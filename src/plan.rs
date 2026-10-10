//! Lyra's side of planning and execution: the `Runtime` the engine runs on
//! (the chat model, lyra's tools, memory and skills as planning context, the
//! UI as the event sink), and the text the `/plan` commands show.

use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::Duration;

use lyra_execution::{
    Engine, ExecutionEvent, Goal, Plan, PlanStep, PlanningContext, ReasonError, Reasoned, Risk, Runtime, StepAction,
    StepStatus, Task, ToolInfo, Uuid, budget, short,
};
use serde_json::{Value, json};

use crate::StreamEvent;
use crate::evolve::Evolution;
use crate::learn::Learning;
use crate::tools::{CallContext, Tools};

/// How risky each tool is. Declared here, by the runtime, never by the model.
/// Composite tools are `Mutating` unless they're only reads: they never
/// contain destructive steps.
pub(crate) fn risk(tool: &str) -> Risk {
    if crate::tools::is_read_only(tool) {
        Risk::ReadOnly
    } else if crate::tools::is_destructive(tool) {
        Risk::Destructive
    } else {
        Risk::Mutating
    }
}

pub struct LyraRuntime {
    pub url: String,
    pub model: String,
    pub tools: Option<Arc<Tools>>,
    pub learning: Option<Arc<Learning>>,
    pub forbidden_tools: Vec<String>,
    /// Evolved behavior settings and workflows.
    pub evolution: Option<Arc<Evolution>>,
    /// Every capability, behind policy; plans call tools through it.
    pub caps: Option<Arc<crate::caps::Caps>>,
    /// Autonomous work (G11): the riskiest capability it may use, and the
    /// level from which every use needs approval (assisted mode).
    pub guard: Option<(lyra_capabilities::RiskLevel, Option<lyra_capabilities::RiskLevel>)>,
    pub tx: Sender<StreamEvent>,
}

impl LyraRuntime {
    fn definitions(&self, allowed: Option<&[String]>) -> Vec<Value> {
        let all = match (&self.caps, &self.tools) {
            (Some(caps), _) => caps.tool_definitions(),
            (None, Some(tools)) => tools.definitions().as_array().cloned().unwrap_or_default(),
            _ => return Vec::new(),
        };
        all.into_iter()
            .filter(|d| {
                let name = d["function"]["name"].as_str().unwrap_or("");
                // Reasoning can't stop for approval, so guarded writes stay out of it.
                !self.forbidden_tools.iter().any(|f| f == name)
                    && self.allowed_by_guard(name)
                    && !self.guard_needs_approval(name)
                    && allowed.is_none_or(|a| a.iter().any(|t| t == name))
            })
            .collect()
    }

    /// `approved`: a person approved this call (an approved tool step).
    /// `verify`: check writes here (off for tool steps; the engine verifies those).
    fn run_tool(&self, name: &str, arguments: &str, call_id: &str, agent: Option<&str>, approved: bool, verify: bool) -> String {
        if self.forbidden_tools.iter().any(|f| f == name) {
            return json!({ "error": format!("tool {name} isn't available") }).to_string();
        }
        // A specialist's step stays within its memory scopes (A10, M16).
        let (write, read) = agent.map_or((None, None), |a| crate::agents::scopes(self.caps.as_deref(), a));
        let write_refs: Option<Vec<&str>> = write.as_ref().map(|w| w.iter().map(String::as_str).collect());
        let read_refs: Option<Vec<&str>> = read.as_ref().map(|r| r.iter().map(String::as_str).collect());
        let ctx = CallContext { run: None, call_id, write_scopes: write_refs.as_deref(), read_scopes: read_refs.as_deref(), agent, member: false };
        crate::actions::because_if_unset(crate::actions::Why { source: "a plan".into(), ..Default::default() });
        match (&self.caps, &self.tools) {
            (Some(caps), _) => caps.invoke(name, arguments, ctx, approved, verify),
            (None, Some(tools)) => tools.run(name, arguments, ctx),
            _ => json!({ "error": format!("tool {name} isn't available") }).to_string(),
        }
    }

    /// Whether the autonomy guard lets plans use this tool at all.
    fn allowed_by_guard(&self, name: &str) -> bool {
        match (&self.guard, &self.caps) {
            (Some((max, _)), Some(caps)) => caps.risk_level(name).is_none_or(|r| r <= *max),
            _ => true,
        }
    }

    /// The guard's approval floor applies to this tool.
    fn guard_needs_approval(&self, name: &str) -> bool {
        match (&self.guard, &self.caps) {
            (Some((_, Some(floor))), Some(caps)) => caps.risk_level(name).is_some_and(|r| r >= *floor),
            _ => false,
        }
    }

    /// Apply the guard to the planner's tool list.
    fn guarded(&self, tools: Vec<ToolInfo>) -> Vec<ToolInfo> {
        tools
            .into_iter()
            .filter(|t| !self.forbidden_tools.contains(&t.name) && self.allowed_by_guard(&t.name))
            .map(|mut t| {
                t.requires_approval |= self.guard_needs_approval(&t.name);
                t
            })
            .collect()
    }

    fn risk_of(&self, name: &str) -> Risk {
        self.caps.as_ref().map_or_else(|| risk(name), |c| c.risk_of(name))
    }

    fn reason_with(&self, task: &Task, system: &str, url: &str, model: &str, options: &ChatOptions, tools: &[Value]) -> Result<Reasoned, ReasonError> {
        let mut messages = vec![json!({ "role": "system", "content": system }), json!({ "role": "user", "content": task.instruction })];
        let mut out = Reasoned::default();
        let failed = |out: &Reasoned, error: String| ReasonError { error, changes: out.changes.clone() };
        // Steps get a few rounds of tool use, not an open-ended conversation.
        let rounds = self.evolution.as_ref().map_or(6, |e| e.behavior().plan_step_rounds);
        // Never more model calls than the plan's budget has left (P11).
        let rounds = task.max_model_calls.map_or(rounds, |left| rounds.min(left)) as usize;
        for _ in 0..rounds {
            let (message, tokens) = chat_with(url, model, &messages, tools, options).map_err(|e| failed(&out, e))?;
            out.model_calls += 1;
            out.tokens += tokens;
            let calls = message["tool_calls"].as_array().cloned().unwrap_or_default();
            if calls.is_empty() {
                out.text = message["content"].as_str().unwrap_or("").trim().to_string();
                if out.text.is_empty() {
                    return Err(failed(&out, "the model returned nothing".into()));
                }
                return Ok(out);
            }
            messages.push(json!({ "role": "assistant", "content": message["content"].as_str().unwrap_or(""), "tool_calls": calls }));
            for call in &calls {
                let name = call["function"]["name"].as_str().unwrap_or("");
                let arguments = call["function"]["arguments"].as_str().unwrap_or("{}");
                let allowed = tools.iter().any(|d| d["function"]["name"] == name);
                let result = if allowed {
                    out.tool_calls += 1;
                    out.tools_used.push(name.to_string());
                    let result = self.run_tool(name, arguments, call["id"].as_str().unwrap_or(""), task.agent.as_deref(), false, true);
                    // Record what changed (successfully), so a retry doesn't redo it.
                    let ok = serde_json::from_str::<Value>(&result).map_or(true, |v| v.get("error").is_none());
                    if self.risk_of(name) != Risk::ReadOnly && ok {
                        out.changes.push(format!("{name} {}", arguments.chars().take(300).collect::<String>()));
                    }
                    result
                } else {
                    json!({ "error": format!("{name} isn't available for this step") }).to_string()
                };
                messages.push(json!({ "role": "tool", "tool_call_id": call["id"], "content": result }));
            }
        }
        Err(failed(&out, format!("the step didn't finish within {rounds} rounds")))
    }
}

impl Runtime for LyraRuntime {
    fn complete(&self, system: &str, user: &str) -> Result<(String, u64), String> {
        // These calls can take a minute with a reasoning model; say what's happening.
        let purpose = if system == lyra_execution::planner::GOAL_PROMPT {
            "working out the goal"
        } else if system.starts_with("You plan") {
            "drafting the plan"
        } else if system.starts_with("A step in an AI agent's plan failed") {
            "revising the plan"
        } else if system == lyra_execution::planner::VERIFY_PROMPT {
            "verifying a step"
        } else if system == lyra_execution::planner::BIND_PROMPT {
            "filling in a step's arguments"
        } else {
            "judging the goal"
        };
        let _ = self.tx.send(StreamEvent::PlanNotes(vec![format!("asking the model: {purpose}…")]));
        let (text, usage) = crate::learn::complete(&self.url, &self.model, system, user)?;
        Ok((text, usage.map_or(0, |u| u.prompt_tokens + u.completion_tokens)))
    }

    fn context(&self, goal: &str) -> PlanningContext {
        let memories = self
            .tools
            .as_ref()
            .and_then(|t| t.mem.recall(None, goal, 6, false).ok())
            .map(|found| found.into_iter().map(|r| r.memory.content).collect())
            .unwrap_or_default();
        // Evolved setting: whether the planner is shown the relevant skills.
        // Workflow steps can name any active skill either way.
        let behavior = self.evolution.as_ref().map(|e| e.behavior()).unwrap_or_default();
        let skills = self
            .learning
            .as_ref()
            .filter(|_| behavior.search_skills_before_planning)
            .and_then(|l| l.relevant(goal).ok())
            .map(|found| found.into_iter().map(|r| (r.skill.name, r.skill.description)).collect())
            .unwrap_or_default();
        let workflows = self
            .learning
            .as_ref()
            .and_then(|l| l.active_skills().ok())
            .map(|all| all.into_iter().map(|s| s.name).collect())
            .unwrap_or_default();
        let agents = crate::agents::plan_agents(self.caps.as_deref());
        PlanningContext {
            memories,
            skills,
            workflows,
            // C3: the capabilities this goal needs, with their track record,
            // prerequisites, approval and verification.
            tools: match &self.caps {
                Some(caps) => self.guarded(caps.plan_tools(goal)),
                None => self.tools(),
            },
            agents,
            forbidden_tools: self.forbidden_tools.clone(),
            budget_note: None,
            guidance: self.evolution.as_ref().and_then(|e| e.planning_guidance(goal)),
        }
    }

    fn tools(&self) -> Vec<ToolInfo> {
        if let Some(caps) = &self.caps {
            return self.guarded(caps.all_tools());
        }
        self.definitions(None)
            .iter()
            .map(|d| {
                let name = d["function"]["name"].as_str().unwrap_or("").to_string();
                ToolInfo::new(&name, d["function"]["description"].as_str().unwrap_or(""), risk(&name), d["function"]["parameters"].clone())
            })
            .collect()
    }

    fn call_tool(&self, tool: &str, arguments: &Value, operation: Option<Uuid>) -> Result<Value, String> {
        let call_id = operation.map(|o| o.to_string()).unwrap_or_default();
        // A tool step runs only after any approval it needs (P14).
        let text = self.run_tool(tool, &arguments.to_string(), &call_id, None, true, false);
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
        match value.get("error").and_then(Value::as_str) {
            Some(e) => Err(e.to_string()),
            None => Ok(value),
        }
    }

    fn reason(&self, task: &Task) -> Result<Reasoned, ReasonError> {
        // Destructive tools only run as their own, approved tool steps; a
        // step marked safe (repeatable) only gets read-only tools.
        let tools: Vec<Value> = self
            .definitions(task.tools.as_deref())
            .into_iter()
            .filter(|d| match self.risk_of(d["function"]["name"].as_str().unwrap_or("")) {
                Risk::ReadOnly => true,
                Risk::Mutating => task.may_change,
                Risk::Destructive => false,
            })
            .collect();
        let mut system = format!(
            "You are carrying out one step of a larger plan. Do only this step, using the tools if \
             it helps, then reply with the result: the facts, findings or text it produced, \
             concisely. If you can't do it, say exactly why.\n\n{}",
            task.context
        );
        // A specialist's step (A12): its instructions, model and record.
        let profile = task.agent.as_deref().and_then(|a| crate::agents::step_profile(self.caps.as_deref(), a));
        let (mut url, mut model, mut options) = (self.url.clone(), self.model.clone(), ChatOptions::default());
        if let Some(p) = &profile {
            system = format!("You are {}: {}\n{}\n\n{system}", p.title, p.role, p.instructions);
            if let Some(u) = &p.model_policy.url {
                url = format!("{}/chat/completions", u.trim_end_matches('/'));
            }
            if let Some(m) = &p.model_policy.model {
                model = m.clone();
            }
            options = ChatOptions { max_tokens: p.model_policy.max_tokens, thinking: p.model_policy.thinking, temperature: p.model_policy.temperature };
            crate::agents::step_started(&self.tx, self.caps.as_deref(), p, &task.instruction);
        }
        let started = std::time::Instant::now();
        let result = self.reason_with(task, &system, &url, &model, &options, &tools);
        if let Some(p) = &profile {
            let (out, calls) = match &result {
                Ok(r) => (Ok(r.text.as_str()), (r.model_calls, r.tool_calls, r.tokens)),
                Err(e) => (Err(e.error.as_str()), (0, 0, 0)),
            };
            crate::agents::step_finished(&self.tx, self.caps.as_deref(), p, &task.instruction, out, started.elapsed().as_millis() as u64, calls);
        }
        result
    }

    fn workflow(&self, name: &str) -> Option<String> {
        self.learning.as_ref()?.instructions(name)
    }

    fn agent_tools(&self, agent: &str) -> Option<Vec<String>> {
        crate::agents::step_tools(self.caps.as_deref(), agent)
    }

    fn decide_yes(&self, state: &str, question: &str) -> Option<(bool, f32)> {
        crate::decide::yes("step check", state, question)
    }

    fn emit(&self, event: &ExecutionEvent) {
        let _ = self.tx.send(StreamEvent::PlanEvent(event.clone()));
    }
}

/// Per-call model settings (a subagent's model policy).
#[derive(Debug, Clone, Default)]
pub struct ChatOptions {
    pub max_tokens: Option<u32>,
    pub thinking: Option<bool>,
    pub temperature: Option<f32>,
}

/// One non-streaming chat completion with tools; returns the message and tokens used.
pub(crate) fn chat(url: &str, model: &str, messages: &[Value], tools: &[Value]) -> Result<(Value, u64), String> {
    chat_with(url, model, messages, tools, &ChatOptions::default())
}

pub(crate) fn chat_with(url: &str, model: &str, messages: &[Value], tools: &[Value], o: &ChatOptions) -> Result<(Value, u64), String> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| e.to_string())?;
    let mut body = json!({ "model": model, "messages": messages });
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools.to_vec());
    }
    if let Some(n) = o.max_tokens {
        body["max_tokens"] = json!(n);
    }
    if o.thinking == Some(false) {
        body["chat_template_kwargs"] = json!({ "enable_thinking": false });
    }
    if let Some(t) = o.temperature {
        body["temperature"] = json!(t);
    }
    let started = std::time::Instant::now();
    let send = |url: &str, body: &Value| -> Result<Value, String> {
        let resp = client.post(url).json(body).send().map_err(|e| e.to_string())?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("{status}: {}", resp.text().unwrap_or_default().chars().take(500).collect::<String>()));
        }
        resp.json().map_err(|e| e.to_string())
    };
    // The main chat model down: the next in line takes the step (not an agent's own model).
    let (reply, used): (Value, String) = crate::fallback::call(url, model, false, |u, m| {
        body["model"] = json!(m);
        send(u, &body)
    })?;
    // Agents' and plans' steps, for whoever this thread works for (as the model that answered).
    crate::usage::record_usage("agent", &used, &reply["usage"], started.elapsed().as_millis() as u64);
    let tokens = reply["usage"]["total_tokens"].as_u64().unwrap_or(0);
    Ok((reply["choices"][0]["message"].clone(), tokens))
}

pub fn icon(status: StepStatus) -> &'static str {
    match status {
        StepStatus::Pending => "○",
        StepStatus::Running => "▸",
        StepStatus::Completed => "✓",
        StepStatus::Failed => "✗",
        StepStatus::Blocked => "⏸",
        StepStatus::Skipped | StepStatus::Cancelled => "⊘",
    }
}

/// `tool memory_recall`, `reasoning`, `subagent researcher`, ...
pub fn action_label(a: &StepAction) -> String {
    match a {
        StepAction::Tool { tool, .. } => format!("tool {tool}"),
        StepAction::Reasoning { .. } => "reasoning".into(),
        StepAction::Workflow { workflow, .. } => format!("workflow {workflow}"),
        StepAction::Subagent { agent, .. } => format!("subagent {agent}"),
    }
}

/// A plan as text for the chat.
pub fn describe(goal: Option<&Goal>, plan: &Plan) -> String {
    let mut out = Vec::new();
    if let Some(g) = goal {
        out.push(format!("📋 Goal: {}", g.description));
        out.push("  success when:".into());
        out.extend(g.success_criteria.iter().map(|c| format!("    - {c}")));
        if !g.constraints.is_empty() {
            out.push(format!("  constraints: {}", g.constraints.join("; ")));
        }
        if !g.ambiguities.is_empty() {
            out.push(format!("  open questions: {}", g.ambiguities.join("; ")));
        }
        if g.destructive {
            out.push("  ⚠ involves destructive or external changes".into());
        }
    }
    out.push(format!("Plan {} v{} ({}):", short(plan.id), plan.version, plan.status));
    for s in &plan.steps {
        out.push(format!("  {} {} {} [{}]{}", icon(s.status), s.key, s.title, action_label(&s.action), step_notes(plan, s)));
    }
    out.push(format!("  budget: {}", budget::describe(&plan.budget, &plan.usage)));
    if let Some(note) = &plan.note {
        out.push(format!("  note: {note}"));
    }
    out.join("\n")
}

fn step_notes(plan: &Plan, s: &PlanStep) -> String {
    let mut notes = Vec::new();
    let deps: Vec<&str> = s.dependencies.iter().filter_map(|d| plan.step(*d)).map(|d| d.key.as_str()).collect();
    if !deps.is_empty() {
        notes.push(format!("after {}", deps.join(", ")));
    }
    if s.needs_approval() {
        notes.push("needs approval".into());
    }
    if s.attempts > 1 {
        notes.push(format!("{} attempts", s.attempts));
    }
    if let Some(e) = s.last_error.as_ref().filter(|e| s.status != StepStatus::Completed && *e != "needs approval") {
        notes.push(e.chars().take(80).collect());
    }
    if notes.is_empty() { String::new() } else { format!(" — {}", notes.join(" · ")) }
}

/// `/plans`: recent plans, one line each.
pub fn list(engine: &Engine) -> Result<String, String> {
    let plans = engine.plans(15)?;
    if plans.is_empty() {
        return Ok("no plans yet — /plan <what you want done>".into());
    }
    Ok(plans
        .iter()
        .map(|p| {
            let goal = engine.goal(p.goal_id).ok().flatten().map(|(g, _)| g);
            let done = p.steps.iter().filter(|s| s.is_settled()).count();
            format!(
                "{} {} v{} · {}/{} steps · goal {} — {}",
                short(p.id),
                p.status,
                p.version,
                done,
                p.steps.len(),
                goal.as_ref().map_or("?".into(), |g| g.status.to_string()),
                goal.map_or(String::new(), |g| g.description)
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

/// What each finished step produced, for memory capture.
pub fn results(plan: &Plan) -> String {
    plan.steps
        .iter()
        .filter_map(|s| {
            let r = s.result.as_ref().filter(|r| r.success)?;
            let text = match &r.output {
                Value::String(t) => t.clone(),
                other => other.to_string(),
            };
            Some(format!("[{} {}] {}", s.key, s.title, text.chars().take(1200).collect::<String>()))
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// `/plan checkpoints`: the recovery points saved before steps that change things.
pub fn checkpoints(engine: &Engine, plan: &Plan) -> Result<String, String> {
    let list = engine.checkpoints(plan.id)?;
    if list.is_empty() {
        return Ok("no checkpoints yet (they're saved before steps that change things)".into());
    }
    Ok(list
        .iter()
        .map(|c| {
            let done: Vec<&str> = c.completed_steps.iter().filter_map(|id| plan.step(*id)).map(|s| s.key.as_str()).collect();
            format!("{} v{} · {} · done: {}", c.created_at.with_timezone(&chrono::Local).format("%H:%M:%S"), c.plan_version, c.reason, if done.is_empty() { "—".into() } else { done.join(", ") })
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

/// `/plan revisions`: how the plan changed when steps failed.
pub fn revisions(engine: &Engine, plan: &Plan) -> Result<String, String> {
    let list = engine.revisions(plan.id)?;
    if list.is_empty() {
        return Ok("never revised".into());
    }
    Ok(list
        .iter()
        .map(|(from, to, r)| {
            let added: Vec<&str> = r.added_steps.iter().map(|s| s.key.as_str()).collect();
            let changed: Vec<&str> = r.modified_steps.iter().map(|s| s.key.as_str()).collect();
            format!(
                "v{from} → v{to}: {} · removed {} · added {} · changed {}",
                r.reason,
                r.removed_steps.len(),
                if added.is_empty() { "—".into() } else { added.join(", ") },
                if changed.is_empty() { "—".into() } else { changed.join(", ") }
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

/// `/plan events`: what happened, in order.
pub fn events(engine: &Engine, plan: &Plan) -> Result<String, String> {
    let events = engine.events(plan.id)?;
    let mut out: Vec<String> =
        events.iter().map(|e| format!("{} {}: {}", e.created_at.format("%H:%M:%S"), e.kind, e.message)).collect();
    let m = engine.metrics(plan)?;
    out.push(format!(
        "model calls {} · tool calls {} · tokens {} · retries {} · replans {} · failed attempts {} · recoveries {} · verification failures {} · {}s",
        m.model_calls, m.tool_calls, m.tokens, m.retries, m.replans, m.failed_attempts, m.recoveries, m.verification_failures, m.seconds
    ));
    Ok(out.join("\n"))
}

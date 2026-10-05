//! Lyra's side of planning and execution: the `Runtime` the engine runs on
//! (the chat model, lyra's tools, memory and skills as planning context, the
//! UI as the event sink), and the text the `/plan` commands show.

use std::sync::Arc;
use std::sync::mpsc::Sender;
use std::time::Duration;

use lyra_execution::{
    AgentInfo, Engine, ExecutionEvent, Goal, Plan, PlanStep, PlanningContext, Reasoned, Risk, Runtime, StepAction,
    StepStatus, Task, ToolInfo, Uuid, budget, short,
};
use serde_json::{Value, json};

use crate::StreamEvent;
use crate::learn::Learning;
use crate::tools::{CallContext, Tools};

/// Steps get a few rounds of tool use, not an open-ended conversation.
const MAX_STEP_ROUNDS: usize = 6;

/// Helper agents for subagent steps (P13): each sees only its task and these tools.
const AGENTS: &[(&str, &str, &[&str])] = &[
    ("researcher", "looks things up in memory and reports what it finds (read-only)", &["memory_recall", "memory_list"]),
    ("archivist", "records findings and decisions in memory", &["memory_recall", "memory_list", "memory_remember", "memory_correct", "memory_supersede"]),
];

/// How risky each tool is. Declared here, by the runtime, never by the model.
fn risk(tool: &str) -> Risk {
    match tool {
        "memory_recall" | "memory_list" => Risk::ReadOnly,
        "memory_forget" => Risk::Destructive,
        _ => Risk::Mutating,
    }
}

pub struct LyraRuntime {
    pub url: String,
    pub model: String,
    pub tools: Option<Arc<Tools>>,
    pub learning: Option<Arc<Learning>>,
    pub forbidden_tools: Vec<String>,
    pub tx: Sender<StreamEvent>,
}

impl LyraRuntime {
    fn definitions(&self, allowed: Option<&[String]>) -> Vec<Value> {
        let Some(tools) = &self.tools else { return Vec::new() };
        tools
            .definitions()
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|d| {
                let name = d["function"]["name"].as_str().unwrap_or("");
                !self.forbidden_tools.iter().any(|f| f == name) && allowed.is_none_or(|a| a.iter().any(|t| t == name))
            })
            .collect()
    }

    fn run_tool(&self, name: &str, arguments: &str, call_id: &str) -> String {
        match &self.tools {
            Some(tools) if !self.forbidden_tools.iter().any(|f| f == name) => {
                tools.run(name, arguments, CallContext { run: None, call_id })
            }
            _ => json!({ "error": format!("tool {name} isn't available") }).to_string(),
        }
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
        let skills = self
            .learning
            .as_ref()
            .and_then(|l| l.relevant(goal).ok())
            .map(|found| found.into_iter().map(|r| (r.skill.name, r.skill.description)).collect())
            .unwrap_or_default();
        let tools = self
            .definitions(None)
            .iter()
            .map(|d| {
                let name = d["function"]["name"].as_str().unwrap_or("").to_string();
                ToolInfo {
                    risk: risk(&name),
                    description: d["function"]["description"].as_str().unwrap_or("").into(),
                    parameters: d["function"]["parameters"].clone(),
                    name,
                }
            })
            .collect();
        let agents = AGENTS.iter().map(|(n, d, _)| AgentInfo { name: n.to_string(), description: d.to_string() }).collect();
        PlanningContext { memories, skills, tools, agents, forbidden_tools: self.forbidden_tools.clone(), budget_note: None }
    }

    fn call_tool(&self, tool: &str, arguments: &Value, operation: Option<Uuid>) -> Result<Value, String> {
        let call_id = operation.map(|o| o.to_string()).unwrap_or_default();
        let text = self.run_tool(tool, &arguments.to_string(), &call_id);
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
        match value.get("error").and_then(Value::as_str) {
            Some(e) => Err(e.to_string()),
            None => Ok(value),
        }
    }

    fn reason(&self, task: &Task) -> Result<Reasoned, String> {
        // Destructive tools only run as their own, approved tool steps.
        let tools: Vec<Value> = self
            .definitions(task.tools.as_deref())
            .into_iter()
            .filter(|d| risk(d["function"]["name"].as_str().unwrap_or("")) != Risk::Destructive)
            .collect();
        let system = format!(
            "You are carrying out one step of a larger plan. Do only this step, using the tools if \
             it helps, then reply with the result: the facts, findings or text it produced, \
             concisely. If you can't do it, say exactly why.\n\n{}",
            task.context
        );
        let mut messages = vec![json!({ "role": "system", "content": system }), json!({ "role": "user", "content": task.instruction })];
        let mut out = Reasoned::default();
        for _ in 0..MAX_STEP_ROUNDS {
            let (message, tokens) = chat(&self.url, &self.model, &messages, &tools)?;
            out.model_calls += 1;
            out.tokens += tokens;
            let calls = message["tool_calls"].as_array().cloned().unwrap_or_default();
            if calls.is_empty() {
                out.text = message["content"].as_str().unwrap_or("").trim().to_string();
                if out.text.is_empty() {
                    return Err("the model returned nothing".into());
                }
                return Ok(out);
            }
            messages.push(json!({ "role": "assistant", "content": message["content"].as_str().unwrap_or(""), "tool_calls": calls }));
            for call in &calls {
                let name = call["function"]["name"].as_str().unwrap_or("");
                let allowed = tools.iter().any(|d| d["function"]["name"] == name);
                let result = if allowed {
                    out.tool_calls += 1;
                    self.run_tool(name, call["function"]["arguments"].as_str().unwrap_or("{}"), call["id"].as_str().unwrap_or(""))
                } else {
                    json!({ "error": format!("{name} isn't available for this step") }).to_string()
                };
                messages.push(json!({ "role": "tool", "tool_call_id": call["id"], "content": result }));
            }
        }
        Err(format!("the step didn't finish within {MAX_STEP_ROUNDS} rounds"))
    }

    fn workflow(&self, name: &str) -> Option<String> {
        self.learning.as_ref()?.instructions(name)
    }

    fn agent_tools(&self, agent: &str) -> Option<Vec<String>> {
        AGENTS.iter().find(|(n, _, _)| *n == agent).map(|(_, _, tools)| tools.iter().map(|t| t.to_string()).collect())
    }

    fn emit(&self, event: &ExecutionEvent) {
        let _ = self.tx.send(StreamEvent::PlanEvent(event.clone()));
    }
}

/// One non-streaming chat completion with tools; returns the message and tokens used.
fn chat(url: &str, model: &str, messages: &[Value], tools: &[Value]) -> Result<(Value, u64), String> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| e.to_string())?;
    let mut body = json!({ "model": model, "messages": messages });
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools.to_vec());
    }
    let resp = client.post(url).json(&body).send().map_err(|e| e.to_string())?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("{status}: {}", resp.text().unwrap_or_default()));
    }
    let reply: Value = resp.json().map_err(|e| e.to_string())?;
    let tokens = reply["usage"]["total_tokens"].as_u64().unwrap_or(0);
    Ok((reply["choices"][0]["message"].clone(), tokens))
}

pub fn icon(status: StepStatus) -> &'static str {
    match status {
        StepStatus::Pending | StepStatus::Ready => "○",
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

//! Lyra's side of subagents (docs/done/sub_agents.md): routing a message to
//! a specialist, running a delegation (scoped context, the agent's own
//! instructions, skills, model, tools and memory, all enforced here), the
//! creation wizard, and the `/agents` commands. The main agent stays in
//! charge: it gets the specialist's result and answers the user itself.

use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::Utc;
use lyra_agents::builder::{AgentCreationDraft, CreationMode, Stage};
use lyra_agents::delegation::{self, AgentBudget, DelegationRequest, DelegationResult, DelegationStatus, OutputContract};
use lyra_agents::router::{self, RouteMethod, RouterSettings, RoutingDecision, SemanticRouter};
use lyra_agents::{AgentProfile, AgentRegistry, DelegationRecord, MAIN};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::runtime::Handle;
use lyra_learning::Uuid;

use crate::StreamEvent;
use crate::caps::Caps;
use crate::learn::Learning;
use crate::tools::{CallContext, Tools};

/// `[agents]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    /// The main agent hands matching requests to specialists on its own.
    pub auto_delegate: bool,
    /// Longest chain of delegations (main → agent counts as 1).
    pub max_depth: u32,
    /// Say "handled with: Writer" under replies that used a specialist.
    pub show_handled_by: bool,
    pub routing: RouterSettings,
    pub budget: AgentBudget,
}

impl Default for Settings {
    fn default() -> Self {
        Self { enabled: true, auto_delegate: true, max_depth: 2, show_handled_by: true, routing: RouterSettings::default(), budget: AgentBudget::default() }
    }
}

/// What the TUI shows while the main agent works with subagents.
#[derive(Debug, Clone)]
pub enum AgentEvent {
    Routed { agent: String, method: RouteMethod, confidence: f32, reason: String },
    Started { agent: String, task: String, depth: u32 },
    /// An agent calls a tool: `id` is unique to this call, `args` as sent.
    Tool { agent: String, tool: String, id: String, args: String },
    /// What that call returned (clipped for the screen).
    ToolResult { agent: String, id: String, output: String },
    /// A long tool's progress (a coding job's steps), shown live under its call.
    ToolProgress { agent: String, id: String, event: Value },
    Finished { agent: String, status: DelegationStatus, confidence: Option<f32>, ms: u64, output: String, depth: u32 },
}

pub struct Agents {
    pub registry: AgentRegistry,
    pub router: SemanticRouter,
    rt: Handle,
    pub settings: Mutex<Settings>,
    /// The agent being created, if any (A3).
    pub wizard: Mutex<Option<AgentCreationDraft>>,
    /// Agents working right now (for the panel).
    pub active: Mutex<Vec<String>>,
    /// Actions the user allowed for the rest of the session ("a").
    pub allowed: Mutex<std::collections::HashSet<String>>,
}

/// The user's answer to an approval request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Yes,
    No,
    /// Yes, and don't ask again for this exact action this session.
    Always,
}

/// An agent waiting for the user's yes before it changes something.
pub struct ApprovalRequest {
    /// Unique in this run (a notification's Allow button names it).
    pub id: u64,
    pub agent: String,
    /// What kind of thing ("run a command on this machine").
    pub what: String,
    /// Exactly what (the command, the path).
    pub detail: String,
    pub why: String,
    pub dangerous: bool,
    pub reply: Sender<Answer>,
}

/// Ask the user (through the TUI) to approve an agent's action, and wait.
/// No answer in time, or lyra closing, is a no.
pub fn approve(env: &Env, profile: &AgentProfile, tool: &str, ask: crate::caps::Ask) -> Result<(), String> {
    let key = format!("{}|{tool}|{}|{}", profile.name, ask.what, ask.detail);
    let action = format!("{}: {}", ask.what, ask.detail.replace('\n', " "));
    if env.agents.allowed.lock().unwrap_or_else(|e| e.into_inner()).contains(&key) {
        return Ok(());
    }
    let timeout = env.caps.as_ref().and_then(|c| c.system.as_ref()).map_or(300, |s| s.settings().approval_timeout_seconds).max(10);
    let (reply, answer) = std::sync::mpsc::channel();
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let request = ApprovalRequest { id, agent: profile.title.clone(), what: ask.what, detail: ask.detail, why: ask.why, dangerous: ask.dangerous, reply };
    env.tx.send(StreamEvent::Approval(request)).map_err(|_| "lyra is closing".to_string())?;
    match answer.recv_timeout(std::time::Duration::from_secs(timeout)) {
        Ok(Answer::Yes) => Ok(()),
        Ok(Answer::Always) => {
            env.agents.allowed.lock().unwrap_or_else(|e| e.into_inner()).insert(key);
            Ok(())
        }
        Ok(Answer::No) => Err(format!("the user declined: {action}")),
        Err(_) => Err(format!("no approval within {timeout}s, so it wasn't done: {action}")),
    }
}

/// What a delegation needs from lyra.
#[derive(Clone)]
pub struct Env {
    pub url: String,
    pub model: String,
    pub caps: Option<Arc<Caps>>,
    pub tools: Option<Arc<Tools>>,
    pub learning: Option<Arc<Learning>>,
    pub agents: Arc<Agents>,
    pub tx: Sender<StreamEvent>,
    /// A machine the user named with `@desktop`: system tools default to it.
    pub machine: Option<String>,
    /// `@all` or `@<group>`: several machines (`fleet_run`).
    pub fleet: Option<String>,
    /// Set when the user stops the run.
    pub cancel: crate::Cancel,
    /// Working for a member, not an admin: no Operator or Coder, no system tools.
    pub member: bool,
    /// That member (their own memories, `user:<id>`, are all the agent sees).
    pub viewer: Option<String>,
}

/// The machine a message names with `@name` (one of `known`, or `server`).
pub fn machine_mention(message: &str, known: &[String]) -> Option<String> {
    message.split_whitespace().filter_map(|w| w.strip_prefix('@')).find_map(|w| {
        let w = w.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_');
        if w.eq_ignore_ascii_case(crate::caps::HERE) {
            return Some(crate::caps::HERE.to_string());
        }
        known.iter().find(|k| k.eq_ignore_ascii_case(w)).cloned()
    })
}

/// `@all` or a `[groups]` name in the message: several machines at once.
pub fn fleet_mention(message: &str, groups: &[String]) -> Option<String> {
    message.split_whitespace().filter_map(|w| w.strip_prefix('@')).find_map(|w| {
        let w = w.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_').to_lowercase();
        (w == "all" || groups.contains(&w)).then_some(w)
    })
}

/// A system call's arguments with the mentioned machine filled in, when the
/// model didn't pick one.
fn with_machine(env: &Env, tool: &str, args: &str) -> String {
    // `@all` / `@web`: fleet_run's machines, when the model didn't say.
    if tool == "fleet_run"
        && let Some(fleet) = &env.fleet
    {
        let mut v: Value = serde_json::from_str(args).unwrap_or(json!({}));
        if v.get("machines").and_then(Value::as_str).is_none_or(str::is_empty)
            && let Some(map) = v.as_object_mut()
        {
            map.insert("machines".into(), json!(fleet));
        }
        return v.to_string();
    }
    let (Some(machine), Some(caps)) = (&env.machine, &env.caps) else { return args.to_string() };
    if tool == "ssh_run" || caps.manager.get(tool).is_none_or(|c| c.source != "system" && c.source != "coding") {
        return args.to_string();
    }
    let mut v: Value = serde_json::from_str(args).unwrap_or(json!({}));
    if v.get("machine").and_then(Value::as_str).is_none_or(str::is_empty)
        && let Some(map) = v.as_object_mut()
    {
        map.insert("machine".into(), json!(machine));
    }
    v.to_string()
}

fn emit(tx: &Sender<StreamEvent>, e: AgentEvent) {
    let _ = tx.send(StreamEvent::Agent(e));
}

impl Agents {
    pub fn open(dir: &std::path::Path, rt: Handle, settings: Settings) -> anyhow::Result<Self> {
        let registry = AgentRegistry::open(dir, rt.clone())?;
        let router = rt.block_on(SemanticRouter::open(&dir.join("index")))?;
        Ok(Self {
            registry,
            router,
            rt,
            settings: Mutex::new(settings),
            wizard: Mutex::new(None),
            active: Mutex::new(Vec::new()),
            allowed: Mutex::new(Default::default()),
        })
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Re-index the agents' routing texts (A7). Returns notes.
    pub fn sync(&self) -> Vec<String> {
        let mut notes = self.registry.sync();
        match self.rt.block_on(self.router.sync(&self.registry.enabled())) {
            Ok(n) if n > 0 => notes.push(format!("indexed {n} routing examples")),
            Ok(_) => {}
            Err(e) => notes.push(format!("agent routing index: {e:#}")),
        }
        notes
    }

    /// Which agent should handle a message (A6, A7): an explicit mention,
    /// the rules, then meaning, and the model only when it's close.
    /// `None` keeps it with the main agent.
    pub fn route(&self, env: &Env, message: &str) -> Option<RoutingDecision> {
        let s = self.settings();
        let agents = self.registry.enabled();
        if let Some(d) = router::explicit(&agents, message) {
            return Some(d);
        }
        if !s.auto_delegate {
            return None;
        }
        if let Some(d) = router::by_rules(&agents, message, s.routing.rule_threshold) {
            return Some(d);
        }
        let ranked = self.rt.block_on(self.router.rank(message)).unwrap_or_default();
        let candidates: Vec<&AgentProfile> = ranked
            .iter()
            .filter(|(_, sim)| *sim >= s.routing.semantic_floor)
            .filter_map(|(name, _)| agents.iter().find(|a| &a.name == name))
            .filter(|a| a.delegation.auto_delegate && !router::excluded(a, message))
            .take(3)
            .collect();
        let top = candidates.first()?;
        let sim = ranked.iter().find(|(n, _)| *n == top.name).map_or(0.0, |r| r.1);
        if sim >= s.routing.semantic_threshold {
            return Some(RoutingDecision { agent: top.name.clone(), confidence: sim, reason: "similar to what it handles".into(), method: RouteMethod::Semantic });
        }
        // The decision model is quick enough to ask whenever routing is unsure.
        if crate::decide::model().is_some() {
            let mut options: Vec<(String, String)> = candidates
                .iter()
                .map(|a| (a.name.clone(), format!("{} Handles: {}.", a.description, a.delegation.intents.join(", "))))
                .collect();
            options.push((MAIN.into(), "The main assistant: anything not clearly one specialist's kind of work.".into()));
            let question = crate::decide::Question::Choice("Which agent should handle this message?".into(), options);
            if let Some(answers) = crate::decide::ask("routing", message, &[("agent".into(), question)])
                && let Some(a) = crate::decide::confident(&answers, "agent")
            {
                return (a.choice != MAIN && candidates.iter().any(|c| c.name == a.choice)).then(|| RoutingDecision {
                    agent: a.choice.clone(),
                    confidence: a.confidence,
                    reason: "the decision model's pick".into(),
                    method: RouteMethod::Model,
                });
            }
        }
        if !s.routing.model_fallback {
            return None;
        }
        let (reply, _) = crate::learn::complete(&env.url, &env.model, router::ROUTE_PROMPT, &router::route_prompt(&candidates, message)).ok()?;
        let d = router::parse_route(&reply, &candidates).ok()?;
        (d.agent != MAIN && d.confidence >= 0.6).then_some(d)
    }

    fn set_active(&self, agent: &str, on: bool) {
        let mut a = self.active.lock().unwrap_or_else(|e| e.into_inner());
        if on {
            a.push(agent.to_string());
        } else if let Some(i) = a.iter().position(|x| x == agent) {
            a.remove(i);
        }
    }
}

/// The tool definition the main agent (and agents allowed to delegate) use
/// to hand work to a specialist.
pub fn delegate_tool(agents: &[AgentProfile]) -> Value {
    let list: Vec<String> = agents.iter().map(|a| format!("{} ({})", a.name, a.description)).collect();
    json!({
        "type": "function",
        "function": {
            "name": "delegate",
            "description": format!("Hand a task to a specialist agent and get its result back to check and use. Agents: {}", list.join("; ")),
            "parameters": {
                "type": "object",
                "properties": {
                    "agent": { "type": "string", "description": "The agent's name." },
                    "task": { "type": "string", "description": "What to do, self-contained." },
                    "input": { "type": "string", "description": "The material to work on (text to rewrite, data, findings)." },
                    "expected_output": { "type": "string", "description": "What to hand back, e.g. \"the rewritten email only\"." },
                },
                "required": ["agent", "task"],
            },
        },
    })
}

/// Run a delegation to `profile` (A5, A8–A11): scoped context in, the
/// agent's instructions, skills and model, only the capabilities it may use
/// (checked again on every call), structured result out. Records it.
#[allow(clippy::too_many_arguments)]
pub fn delegate(
    env: &Env,
    profile: &AgentProfile,
    from: &str,
    task: &str,
    input: Option<&str>,
    expected: Option<&str>,
    depth: u32,
    method: &str,
    confidence: Option<f32>,
    run: Option<Uuid>,
) -> DelegationResult {
    let s = env.agents.settings();
    let start = Instant::now();
    let task_id = Uuid::new_v4();
    emit(&env.tx, AgentEvent::Started { agent: profile.title.clone(), task: task.chars().take(160).collect(), depth });
    env.agents.set_active(&profile.title, true);
    // A10: only the memories this agent may read; for a member, only theirs.
    let mine = env.viewer.as_ref().map(|v| vec![format!("user:{v}")]);
    let read = mine.clone().or_else(|| profile.memory_policy.read_scopes(&profile.name));
    let write = mine.clone().or_else(|| profile.memory_policy.write_scopes(&profile.name));
    let readable = |scope: &str| read.as_ref().is_none_or(|r| r.iter().any(|p| p.strip_suffix('*').map_or(p == scope, |x| scope.starts_with(x))));
    let memories: Vec<String> = match (&env.tools, profile.memory_policy.reads()) {
        (Some(t), true) => t
            .mem
            .recall(mine.as_ref().map(|m| m[0].as_str()), task, 6, false)
            .unwrap_or_default()
            .into_iter()
            .filter(|r| readable(&r.memory.scope))
            .map(|r| r.memory.content)
            .collect(),
        _ => Vec::new(),
    };
    // A11: its own skills and the global ones.
    let mut skills: Vec<(String, String)> = Vec::new();
    if let Some(l) = &env.learning {
        for r in l.relevant_for(Some(&profile.name), task).unwrap_or_default() {
            skills.push((r.skill.name, r.skill.instructions));
        }
        for name in &profile.skills {
            if !skills.iter().any(|(n, _)| n == name)
                && let Some(text) = l.instructions(name)
            {
                skills.push((name.clone(), text));
            }
        }
    }
    let request = DelegationRequest {
        task_id,
        from_agent: from.to_string(),
        to_agent: profile.name.clone(),
        instruction: task.to_string(),
        context: delegation::build_context(task, input, &memories, &[]),
        expected_output: OutputContract { format: "text".into(), description: expected.unwrap_or("the result of the task").into() },
        budget: s.budget,
        depth,
    };
    // A9: the capabilities it may use, enforced again on every call.
    let allowed: Vec<lyra_capabilities::Capability> = env
        .caps
        .as_ref()
        .map(|c| c.manager.usable().into_iter().filter(|x| x.kind.callable() && delegation::allows(profile, x)).collect())
        .unwrap_or_default();
    let mut defs: Vec<Value> = allowed.iter().map(lyra_capabilities::Capability::definition).collect();
    let can_delegate = profile.permission_policy.can_delegate && depth < s.max_depth;
    let others: Vec<AgentProfile> = env.agents.registry.enabled().into_iter().filter(|a| a.name != profile.name).collect();
    if can_delegate && !others.is_empty() {
        defs.push(delegate_tool(&others));
    }
    let url = profile.model_policy.url.clone().map(|u| format!("{}/chat/completions", u.trim_end_matches('/'))).unwrap_or_else(|| env.url.clone());
    let model = profile.model_policy.model.clone().unwrap_or_else(|| env.model.clone());
    let options = crate::plan::ChatOptions {
        max_tokens: profile.model_policy.max_tokens,
        thinking: profile.model_policy.thinking,
        temperature: profile.model_policy.temperature,
    };
    let mut messages = vec![
        json!({ "role": "system", "content": delegation::system_prompt(profile, &skills, &request) }),
        json!({ "role": "user", "content": delegation::user_prompt(&request) }),
    ];
    let (mut model_calls, mut tool_calls, mut tokens) = (0u32, 0u32, 0u64);
    let write_refs: Option<Vec<&str>> = write.as_ref().map(|w| w.iter().map(String::as_str).collect());
    let read_refs: Option<Vec<&str>> = read.as_ref().map(|r| r.iter().map(String::as_str).collect());
    let mut result = DelegationResult {
        task_id,
        status: DelegationStatus::Failed,
        output: Value::String(String::new()),
        confidence: None,
        notes: Some("ran out of model calls".into()),
    };
    for _ in 0..s.budget.max_model_calls.max(1) {
        if crate::stopped(&env.cancel) {
            result.notes = Some("stopped by the user".into());
            break;
        }
        let reply = crate::plan::chat_with(&url, &model, &messages, &defs, &options);
        let (message, used) = match reply {
            Ok(r) => r,
            Err(e) => {
                result.notes = Some(e);
                break;
            }
        };
        model_calls += 1;
        tokens += used;
        let calls = message["tool_calls"].as_array().cloned().unwrap_or_default();
        if calls.is_empty() {
            result = delegation::parse_result(task_id, message["content"].as_str().unwrap_or(""));
            break;
        }
        messages.push(json!({ "role": "assistant", "content": message["content"].as_str().unwrap_or(""), "tool_calls": calls }));
        for call in &calls {
            let name = call["function"]["name"].as_str().unwrap_or("");
            let args = call["function"]["arguments"].as_str().unwrap_or("{}");
            // Shown in the chat as this delegation's own tool card.
            let shown_id = format!("{task_id}-{}", call["id"].as_str().unwrap_or(""));
            emit(&env.tx, AgentEvent::Tool { agent: profile.title.clone(), tool: name.to_string(), id: shown_id.clone(), args: with_machine(env, name, args) });
            let out = if crate::stopped(&env.cancel) {
                json!({ "error": "stopped by the user" }).to_string()
            } else if tool_calls >= s.budget.max_tool_calls {
                json!({ "error": "this delegation's tool budget is used up" }).to_string()
            } else if name == "delegate" && can_delegate {
                tool_calls += 1;
                nested(env, profile, args, depth, run)
            } else if allowed.iter().any(|c| c.name == name) {
                tool_calls += 1;
                let filled = with_machine(env, name, args);
                let args = filled.as_str();
                let ctx = CallContext {
                    run,
                    call_id: call["id"].as_str().unwrap_or(""),
                    write_scopes: write_refs.as_deref(),
                    read_scopes: read_refs.as_deref(),
                    agent: Some(&profile.name),
                    member: env.member,
                };
                match &env.caps {
                    None => json!({ "error": "tools are off" }).to_string(),
                    // Changes wait for the user's yes (asked in the TUI).
                    Some(c) => match c.approval(name, args) {
                        Some(ask) => match approve(env, profile, name, ask) {
                            // A coding job: its steps show live, and stop stops it.
                            Ok(()) if c.manager.get(name).is_some_and(|x| x.source == "coding") => {
                                let progress = |event: Value| emit(&env.tx, AgentEvent::ToolProgress { agent: profile.title.clone(), id: shown_id.clone(), event });
                                crate::coding::run(c, &serde_json::from_str(args).unwrap_or(json!({})), &env.cancel, &progress, Some((&env.url, &env.model))).to_string()
                            }
                            Ok(()) => c.invoke(name, args, ctx, true, true),
                            Err(why) => json!({ "error": why }).to_string(),
                        },
                        None => c.invoke(name, args, ctx, false, true),
                    },
                }
            } else {
                // A9: not in this agent's permissions, whatever the model asked.
                json!({ "error": format!("{} may not use {name}", profile.title) }).to_string()
            };
            let clipped: String = out.chars().take(4000).collect();
            emit(&env.tx, AgentEvent::ToolResult { agent: profile.title.clone(), id: shown_id, output: clipped });
            messages.push(json!({ "role": "tool", "tool_call_id": call["id"], "content": out }));
        }
    }
    let ms = start.elapsed().as_millis() as u64;
    env.agents.set_active(&profile.title, false);
    let record = DelegationRecord {
        id: task_id,
        run_id: run,
        from_agent: from.to_string(),
        agent: profile.name.clone(),
        task: task.chars().take(500).collect(),
        method: method.to_string(),
        confidence,
        status: result.status.as_str().into(),
        output: result.text().chars().take(2000).collect(),
        duration_ms: ms,
        model_calls,
        tool_calls,
        tokens,
        outcome: None,
        created_at: Utc::now(),
    };
    // A wizard trial isn't the agent's track record (it may not even exist yet).
    if method != "test" {
        let _ = env.agents.registry.record_delegation(&record);
    }
    emit(
        &env.tx,
        AgentEvent::Finished { agent: profile.title.clone(), status: result.status, confidence: result.confidence, ms, output: result.text(), depth },
    );
    result
}

/// A delegation an agent makes to another (A22), within the depth limit.
fn nested(env: &Env, from: &AgentProfile, args: &str, depth: u32, run: Option<Uuid>) -> String {
    let a: Value = serde_json::from_str(args).unwrap_or(json!({}));
    let Some(to) = a["agent"].as_str().and_then(|n| env.agents.registry.find(n).ok()).filter(|p| p.enabled && p.name != from.name) else {
        return json!({ "error": "no such agent" }).to_string();
    };
    let r = delegate(env, &to, &from.name, a["task"].as_str().unwrap_or(""), a["input"].as_str(), a["expected_output"].as_str(), depth + 1, "agent", None, run);
    result_json(&to, &r).to_string()
}

/// A result as the delegating agent sees it.
pub fn result_json(p: &AgentProfile, r: &DelegationResult) -> Value {
    json!({ "agent": p.name, "status": r.status.as_str(), "output": r.text(), "confidence": r.confidence, "notes": r.notes })
}

/// The main agent's `delegate` tool.
pub fn delegate_call(env: &Env, args: &str, run: Uuid) -> String {
    let a: Value = serde_json::from_str(args).unwrap_or(json!({}));
    let Some(to) = a["agent"].as_str().and_then(|n| env.agents.registry.find(n).ok()).filter(|p| p.enabled) else {
        return json!({ "error": "no such agent; /agents lists them" }).to_string();
    };
    if env.member && admin_only(&to.name) {
        return json!({ "error": format!("{} works on machines and code: that's for admins", to.title) }).to_string();
    }
    let r = delegate(env, &to, MAIN, a["task"].as_str().unwrap_or(""), a["input"].as_str(), a["expected_output"].as_str(), 1, "tool", None, Some(run));
    result_json(&to, &r).to_string()
}

/// How the main agent sees itself once there are specialists (A1).
pub const MAIN_ROLE: &str = "You are the main agent: you own the conversation, and specialist agents own their specialties. \
Hand a task to one when it's clearly its kind of work, then check its result and answer the user yourself.";

/// The note that keeps the main agent in charge of a routed delegation (A11).
pub const MAIN_NOTE: &str = "A specialist agent already worked on this request; its result is the `delegate` call \
above. Check that it does what the user asked (follow-up instructions, facts, tone), fix anything it got wrong, and \
give the user the result itself, not a description of it. Don't mention the specialist unless asked.";

// ---- the wizard (A3, A23, A24)

/// Ask the model to write the instructions and routing examples, then try
/// the agent on its test task. Blocking; returns the draft in review.
pub fn finish_draft(env: &Env, mut draft: AgentCreationDraft) -> (AgentCreationDraft, Vec<String>) {
    let mut notes = Vec::new();
    let mut profile = draft.profile.clone().unwrap_or_else(|| draft.profile());
    if draft.mode != CreationMode::Expert {
        match crate::learn::complete(&env.url, &env.model, lyra_agents::builder::GENERATE_PROMPT, &lyra_agents::builder::generate_prompt(&draft))
            .and_then(|(reply, _)| lyra_agents::builder::apply_generated(profile.clone(), &reply))
        {
            Ok(p) => profile = p,
            Err(e) => notes.push(format!("couldn't draft instructions ({e}); using the template's")),
        }
    }
    // A24: try it before it exists.
    let test = profile.test_task.clone().unwrap_or_else(|| format!("Show what you can do as {} with a short example.", profile.title));
    let r = delegate(env, &profile, MAIN, &test, delegation::extract_input(&test).as_deref(), None, 1, "test", None, None);
    draft.test_output = Some(format!("Test task: {test}\n\nResult ({}):\n{}", r.status.as_str(), r.text()));
    draft.profile = Some(profile);
    draft.stage = Stage::Review;
    (draft, notes)
}

pub const REVIEW_HELP: &str = "activate · modify <tone|behavior|tools|memory|delegation|model|name|purpose> · test · cancel";

// ---- commands

pub const COMMANDS: &str = "\
/agents [log]                the main agent and its specialists · recent delegations
/agent new [writer|…|expert] create one: a template, a guided interview, or a profile you paste
/agent <name>                its profile, version, track record and skills
/agent ask <name> <task>     hand it a task (or just write @writer … in a message)
/agent edit <name> <field> <value>   instructions, role, description, tools, memory, auto, model, max_risk, example
/agent enable|disable|delete <name> · history <name> · rollback <name> [version]
/agent skill <skill> <agent|global>  make a skill an agent's own, or shared again";

pub fn list(agents: &Agents) -> String {
    let stats = agents.registry.stats();
    let (all, errors) = agents.registry.list();
    let mut out = vec!["main — the main agent: owns the conversation and hands specialties over".to_string()];
    for a in &all {
        let s = stats.get(&a.name).cloned().unwrap_or_default();
        let record = if s.delegations > 0 {
            format!(" · {} delegations, {} corrected, ~{:.1}s", s.delegations, s.corrected, s.average_ms as f32 / 1000.0)
        } else {
            String::new()
        };
        out.push(format!(
            "{}{} v{} — {}{}{} · tools: {} · memory: {}{record}",
            if a.enabled { "" } else { "(disabled) " },
            a.name,
            a.version,
            a.description,
            if a.delegation.auto_delegate { " · takes matching requests on its own" } else { "" },
            a.model_policy.model.as_ref().map_or(String::new(), |m| format!(" · model {m}")),
            if a.tools.is_empty() && a.capabilities.is_empty() { "none".into() } else { [a.tools.clone(), a.capabilities.clone()].concat().join(", ") },
            a.memory_policy.mode.as_str().replace('_', " "),
        ));
    }
    out.extend(errors.into_iter().map(|e| format!("unreadable: {e}")));
    out.push(String::new());
    out.push(COMMANDS.into());
    out.join("\n")
}

pub fn show(agents: &Agents, learning: Option<&Learning>, key: &str) -> Result<String, String> {
    let a = agents.registry.find(key)?;
    let s = agents.registry.stats().get(&a.name).cloned().unwrap_or_default();
    let mut out = vec![
        format!("{} ({}) v{}{} — {}", a.title, a.name, a.version, if a.enabled { "" } else { " · disabled" }, a.description),
        format!("role: {}", a.role),
        format!("instructions:\n{}", a.instructions),
        format!(
            "routing: {}{}",
            if a.delegation.auto_delegate { "takes matching requests on its own" } else { "only when asked" },
            if a.delegation.intents.is_empty() { String::new() } else { format!(" · intents {}", a.delegation.intents.join(", ")) }
        ),
    ];
    if !a.delegation.examples.is_empty() {
        out.push(format!("examples: {}", a.delegation.examples.join(" · ")));
    }
    if !a.delegation.exclusions.is_empty() {
        out.push(format!("not for: {}", a.delegation.exclusions.join(" · ")));
    }
    out.push(format!(
        "may use: {} · up to {} risk{} · delegates: {}",
        if a.tools.is_empty() && a.capabilities.is_empty() { "nothing".into() } else { [a.tools.clone(), a.capabilities.clone()].concat().join(", ") },
        a.permission_policy.max_risk,
        if a.permission_policy.deny.is_empty() { String::new() } else { format!(" · never {}", a.permission_policy.deny.join(", ")) },
        if a.permission_policy.can_delegate { "yes" } else { "no" }
    ));
    out.push(format!(
        "memory: {} · reads {} · writes {}",
        a.memory_policy.mode.as_str().replace('_', " "),
        a.memory_policy.read_scopes(&a.name).map_or("every scope".into(), |r| if r.is_empty() { "nothing".into() } else { r.join(", ") }),
        a.memory_policy.write_scopes(&a.name).map_or("every allowed scope".into(), |w| if w.is_empty() { "nothing".into() } else { w.join(", ") })
    ));
    out.push(format!("model: {}", a.model_policy.model.clone().unwrap_or_else(|| "the main agent's".into())));
    if let Some(l) = learning {
        let own: Vec<String> = l.active_skills().unwrap_or_default().into_iter().filter(|k| k.agent.as_deref() == Some(&a.name)).map(|k| k.name).collect();
        out.push(format!("its skills: {}", if own.is_empty() { "none yet".into() } else { own.join(", ") }));
    }
    out.push(format!(
        "track record: {} delegations · {} failed · {} corrected · {} praised · ~{:.1}s",
        s.delegations,
        s.failed,
        s.corrected,
        s.praised,
        s.average_ms as f32 / 1000.0
    ));
    Ok(out.join("\n"))
}

/// `/agent edit <name> <field> <value>`: a change is a new version (A13).
pub fn edit(agents: &Agents, rest: &str) -> Result<String, String> {
    let mut parts = rest.trim().splitn(3, ' ');
    let (key, field, value) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next().unwrap_or("").trim());
    let mut p = agents.registry.find(key)?;
    let list = |v: &str| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>();
    match field {
        "instructions" => p.instructions = value.into(),
        "role" => p.role = value.into(),
        "description" => p.description = value.into(),
        "tools" => p.tools = list(value),
        "capabilities" => p.capabilities = list(value),
        "deny" => p.permission_policy.deny = list(value),
        "max_risk" => p.permission_policy.max_risk = value.into(),
        "delegates" => p.permission_policy.can_delegate = matches!(value, "yes" | "on" | "true"),
        "auto" => p.delegation.auto_delegate = matches!(value, "yes" | "on" | "true"),
        "example" => p.delegation.examples.push(value.into()),
        "exclusion" => p.delegation.exclusions.push(value.into()),
        "keywords" => p.delegation.keywords = list(value),
        "model" => p.model_policy.model = (!value.is_empty() && value != "default").then(|| value.to_string()),
        "thinking" => p.model_policy.thinking = Some(matches!(value, "yes" | "on" | "true")),
        "memory" => {
            p.memory_policy.mode = serde_json::from_value(json!(value.replace(' ', "_"))).map_err(|_| "memory: none, session_only, shared_read_only, shared_read_write or scoped")?;
        }
        "read" => p.memory_policy.read = list(value),
        "write" => p.memory_policy.write = list(value),
        "skills" => p.skills = list(value),
        _ => return Err("fields: instructions, role, description, tools, capabilities, deny, max_risk, delegates, auto, example, exclusion, keywords, model, thinking, memory, read, write, skills".into()),
    }
    let p = agents.registry.update(p, &format!("{field} changed by the user"))?;
    Ok(format!("{} is now v{} ({field} changed)", p.title, p.version))
}

/// One row of the Agents panel.
#[derive(Debug, Clone)]
pub struct PanelRow {
    pub title: String,
    pub enabled: bool,
    pub auto: bool,
    pub delegations: u32,
    pub corrected: u32,
}

/// What the Agents panel shows (refreshed after changes and delegations).
pub fn panel(agents: &Agents) -> Vec<PanelRow> {
    let stats = agents.registry.stats();
    agents
        .registry
        .list()
        .0
        .into_iter()
        .map(|a| {
            let s = stats.get(&a.name).cloned().unwrap_or_default();
            PanelRow { title: a.title, enabled: a.enabled, auto: a.delegation.auto_delegate, delegations: s.delegations, corrected: s.corrected }
        })
        .collect()
}

/// Route the user's message before the main agent answers it, and if a
/// specialist should take it, run the delegation and put it in the history as
/// the main agent's own `delegate` call (A6, A11). Returns the agent's title.
pub fn auto_delegate(env: &Env, message: &str, run: Uuid, history: &mut Vec<Value>) -> Option<String> {
    // `@desktop …`: the Operator, on that machine.
    let operator = env.agents.registry.get("operator").is_some_and(|p| p.enabled);
    let coder = env.agents.registry.get("coder").is_some_and(|p| p.enabled);
    let d = match (&env.machine, &env.fleet) {
        // Coding work on a machine goes to the Coder, not the Operator.
        (Some(m), _) if coder && crate::coding::looks_like_code(message) => {
            RoutingDecision { agent: "coder".into(), confidence: 1.0, reason: format!("coding work on @{m}"), method: RouteMethod::Explicit }
        }
        (_, Some(f)) if operator => RoutingDecision { agent: "operator".into(), confidence: 1.0, reason: format!("@{f}"), method: RouteMethod::Explicit },
        (Some(m), _) if operator => RoutingDecision { agent: "operator".into(), confidence: 1.0, reason: format!("@{m}"), method: RouteMethod::Explicit },
        _ => env.agents.route(env, message)?,
    };
    let profile = env.agents.registry.get(&d.agent)?;
    // Machines and coding are admins' (the tools refuse them anyway).
    if env.member && admin_only(&profile.name) {
        return None;
    }
    emit(&env.tx, AgentEvent::Routed { agent: profile.title.clone(), method: d.method, confidence: d.confidence, reason: d.reason.clone() });
    let input = delegation::extract_input(message);
    let r = delegate(env, &profile, MAIN, message, input.as_deref(), None, 1, d.method.as_str(), Some(d.confidence), Some(run));
    let id = format!("delegate-{}", &run.to_string()[..8]);
    let args = json!({ "agent": profile.name, "task": message }).to_string();
    history.push(json!({
        "role": "assistant",
        "content": "",
        "tool_calls": [{ "id": id, "type": "function", "function": { "name": "delegate", "arguments": args } }],
    }));
    history.push(json!({ "role": "tool", "tool_call_id": id, "content": result_json(&profile, &r).to_string() }));
    Some(profile.title)
}

/// Agents that work on machines or code: only for admins.
pub fn admin_only(agent: &str) -> bool {
    matches!(agent, "operator" | "coder")
}

/// The wizard's finished draft (built and tested in the background).
pub type Built = Result<Box<(AgentCreationDraft, Vec<String>)>, String>;

impl crate::App {
    pub(crate) fn agent_env(&self) -> Option<Env> {
        let agents = self.agents.clone()?;
        agents.settings().enabled.then(|| Env {
            url: format!("{}/chat/completions", self.base_url.trim_end_matches('/')),
            model: self.model.clone(),
            caps: self.caps.clone(),
            tools: self.tools.clone(),
            learning: self.learning.clone(),
            agents,
            tx: self.tx.clone(),
            machine: None,
            fleet: None,
            cancel: self.cancel.clone(),
            member: !self.admin,
            viewer: (!self.admin).then(|| self.owner.clone()),
        })
    }

    pub(crate) fn refresh_agents(&mut self) {
        self.agents_panel = self.agents.as_deref().map(panel).unwrap_or_default();
    }

    /// Re-index routing in the background after agents change.
    pub(crate) fn sync_agents(&self) {
        let Some(agents) = self.agents.clone() else { return };
        let (tx, caps) = (self.tx.clone(), self.caps.clone());
        std::thread::spawn(move || {
            for note in agents.sync() {
                let _ = tx.send(StreamEvent::Log(format!("agents: {note}")));
            }
            // Plans and /caps see the agents as capabilities.
            if let Some(caps) = caps {
                caps.refresh();
            }
        });
    }

    /// The wizard takes plain input as answers while it's open.
    pub(crate) fn wizard_active(&self) -> bool {
        self.agents.as_ref().is_some_and(|a| a.wizard.lock().unwrap_or_else(|e| e.into_inner()).is_some())
    }

    fn set_wizard(&self, draft: Option<AgentCreationDraft>) {
        let Some(agents) = &self.agents else { return };
        match &draft {
            Some(d) => {
                let _ = agents.registry.save_draft(&serde_json::to_value(d).unwrap_or_default());
            }
            None => agents.registry.clear_draft(),
        }
        *agents.wizard.lock().unwrap_or_else(|e| e.into_inner()) = draft;
    }

    fn wizard(&self) -> Option<AgentCreationDraft> {
        self.agents.as_ref().and_then(|a| a.wizard.lock().unwrap_or_else(|e| e.into_inner()).clone())
    }

    /// Ask the next question, or build and test the agent when there are none.
    fn wizard_next(&mut self, draft: AgentCreationDraft) -> String {
        if let Some(q) = draft.next_question() {
            let text = q.render();
            self.set_wizard(Some(draft));
            return text;
        }
        let Some(env) = self.agent_env() else { return "agents are off".into() };
        self.set_wizard(Some(draft.clone()));
        self.wizard_busy = true;
        self.log(crate::Level::Agent, "building the agent and trying it on a test task…".into());
        std::thread::spawn(move || {
            let built = finish_draft(&env, draft);
            let _ = env.tx.send(StreamEvent::AgentBuilt(Ok(Box::new(built))));
        });
        "Drafting its instructions and trying it on a test task…".into()
    }

    /// A line typed while the wizard is open (A3).
    pub(crate) fn wizard_input(&mut self, text: &str) {
        self.messages.push(crate::Message::new("info", format!("> {text}")));
        let reply = self.wizard_answer(text);
        let (role, text) = match reply {
            Ok(t) => ("info", t),
            Err(e) => ("error", e),
        };
        self.messages.push(crate::Message::new(role, text));
    }

    fn wizard_answer(&mut self, text: &str) -> Result<String, String> {
        if self.wizard_busy {
            return Err("still building the agent — a moment".into());
        }
        let mut draft = self.wizard().ok_or("no agent is being created")?;
        let agents = self.agents.clone().ok_or("agents are off")?;
        if draft.stage == Stage::Review {
            let (cmd, rest) = text.trim().split_once(' ').unwrap_or((text.trim(), ""));
            return match cmd.to_lowercase().as_str() {
                "activate" | "yes" | "save" | "create" => {
                    let profile = draft.profile.clone().ok_or("nothing built yet")?;
                    let p = agents.registry.create(profile, "created with the wizard")?;
                    self.set_wizard(None);
                    self.sync_agents();
                    self.refresh_agents();
                    self.log(crate::Level::Agent, format!("created agent {} (v{})", p.title, p.version));
                    Ok(format!(
                        "{} is active.{} Try: @{} … · /agent {} for its profile",
                        p.title,
                        if p.delegation.auto_delegate { " The main agent will hand it matching requests." } else { "" },
                        p.name,
                        p.name
                    ))
                }
                "modify" | "change" | "edit" => {
                    let key = rest.trim().to_lowercase();
                    if !draft.answered.contains(&key) && !matches!(key.as_str(), "name" | "purpose" | "tools" | "model") {
                        return Err(format!("modify what? {REVIEW_HELP}"));
                    }
                    draft.answered.retain(|k| *k != key);
                    if draft.mode != CreationMode::Guided {
                        // Template and expert drafts ask only what's being changed.
                        draft.mode = CreationMode::Guided;
                        for k in ["name", "purpose", "tone", "behavior", "tools", "memory", "delegation", "model"] {
                            if k != key && !draft.answered.iter().any(|a| a == k) {
                                draft.answered.push(k.into());
                            }
                        }
                    }
                    draft.stage = Stage::Interview;
                    draft.profile = None;
                    Ok(self.wizard_next(draft))
                }
                "test" => {
                    draft.stage = Stage::Interview;
                    Ok(self.wizard_next(draft))
                }
                "cancel" | "no" => {
                    self.set_wizard(None);
                    Ok("discarded the draft".into())
                }
                _ => Err(REVIEW_HELP.into()),
            };
        }
        if draft.mode == CreationMode::Expert {
            let p = lyra_agents::builder::from_expert(text)?;
            draft.name = Some(p.title.clone());
            draft.profile = Some(p);
            return Ok(self.wizard_next(draft));
        }
        draft.answer(text)?;
        Ok(self.wizard_next(draft))
    }

    /// The wizard's draft is built and tested: show it for review.
    pub(crate) fn agent_built(&mut self, built: Built) {
        self.wizard_busy = false;
        match built.map(|b| *b) {
            Ok((draft, notes)) => {
                for n in notes {
                    self.log(crate::Level::Agent, n);
                }
                let text = format!("{}\n\n{}\n\n{REVIEW_HELP}", draft.summary(), draft.test_output.clone().unwrap_or_default());
                self.set_wizard(Some(draft));
                self.messages.push(crate::Message::new("info", text));
            }
            Err(e) => self.messages.push(crate::Message::new("error", e)),
        }
    }

    /// An agent asks to change something: show it, and wait for y / n / a.
    pub(crate) fn approval_requested(&mut self, r: ApprovalRequest) {
        use crate::{Level, Phase};
        self.log(Level::Agent, format!("{} asks to {}: {} ({})", r.agent, r.what, r.detail.replace('\n', " "), r.why));
        // The approval box above the input shows it; keep the chat at the bottom.
        self.scroll = None;
        self.set_phase(Phase::Approval(r.agent.clone()));
        self.approvals.push(r);
    }

    /// The user's answer to the oldest pending approval.
    pub(crate) fn answer_approval(&mut self, text: &str) {
        if let Some(id) = self.approvals.first().map(|r| r.id) {
            self.answer_approval_id(id, text);
        }
    }

    /// Answer a particular approval (from a phone or a notification button).
    /// One that was already answered is ignored.
    pub(crate) fn answer_approval_id(&mut self, id: u64, text: &str) {
        use crate::{Level, Message, Phase};
        let Some(i) = self.approvals.iter().position(|r| r.id == id) else { return };
        let answer = match text.trim().to_lowercase().as_str() {
            "y" | "yes" | "ok" | "approve" => Answer::Yes,
            "n" | "no" | "deny" | "stop" => Answer::No,
            "a" | "always" => Answer::Always,
            // Anything else leaves the question open (the box stays up).
            _ => return,
        };
        let r = self.approvals.remove(i);
        let said = match answer {
            Answer::Yes => "allowed",
            Answer::No => "denied",
            Answer::Always => "allowed for this session",
        };
        let detail = r.detail.replace('\n', " · ");
        self.log(if answer == Answer::No { Level::Error } else { Level::Agent }, format!("{said}: {} — {}: {detail}", r.agent, r.what));
        // A record in the chat of what was asked and what you said.
        self.messages.push(Message::new("approval", format!("{} asked to {}:\n  {detail}\n→ {said}", r.agent, r.what)));
        let _ = r.reply.send(answer);
        if self.approvals.is_empty() {
            self.set_phase(if self.waiting { Phase::Waiting } else { Phase::Idle });
        }
    }

    /// What the TUI does with a subagent event: the chat shows the hand-off
    /// and the result, the phase says who's working, the log has the details.
    pub(crate) fn agent_event(&mut self, e: AgentEvent) {
        use crate::{Level, Message, Phase};
        match e {
            AgentEvent::Routed { agent, method, confidence, reason } => {
                self.log(Level::Agent, format!("routing → {agent} ({}, {confidence:.2}): {reason}", method.as_str()));
            }
            AgentEvent::Started { agent, task, depth } => {
                self.set_phase(Phase::Delegating(agent.clone()));
                let from = if depth > 1 { "an agent" } else { "lyra" };
                self.log(Level::Agent, format!("{from} → {agent}: {task}"));
                self.messages.push(Message::new("agent", format!("{agent} · working on it for {from}{}", if depth > 1 { " (nested)" } else { "" })));
                // Its tool calls and the result go on this card.
                self.agent_cards.push((agent.clone(), self.messages.len() - 1));
                if !self.handled_by.contains(&agent) {
                    self.handled_by.push(agent);
                }
            }
            AgentEvent::Tool { agent, tool, id, args } => {
                self.log(Level::Agent, format!("{agent} ⚙ {tool} {}", args.chars().take(120).collect::<String>()));
                self.set_phase(Phase::Delegating(format!("{agent} · {tool}")));
                if let Some(&(_, i)) = self.agent_cards.iter().rev().find(|(a, _)| *a == agent)
                    && let Some(card) = self.messages.get_mut(i)
                {
                    card.tool_calls.push(crate::ToolCall { id, kind: "function".into(), function: crate::FunctionCall { name: tool, arguments: args } });
                    self.touch(i);
                }
            }
            AgentEvent::ToolResult { agent, id, output } => {
                self.log(Level::Agent, format!("{agent} ↳ {}", output.chars().take(120).collect::<String>()));
                // Shown with its call; never part of the model's history. A live
                // progress message for the same call becomes the result.
                match self.messages.iter().rposition(|m| m.role == "agent_tool" && m.tool_call_id.as_deref() == Some(id.as_str())) {
                    Some(i) => {
                        self.messages[i].content = output;
                        self.touch(i);
                    }
                    None => {
                        let mut m = Message::new("agent_tool", output);
                        m.tool_call_id = Some(id);
                        self.messages.push(m);
                    }
                }
            }
            AgentEvent::ToolProgress { agent, id, event } => {
                let text = event["text"].as_str().unwrap_or("").chars().take(140).collect::<String>();
                if !text.is_empty() && event["kind"] != "text" {
                    self.log(Level::Agent, format!("{agent} · {text}"));
                }
                if event["kind"] == "tool" {
                    self.set_phase(Phase::Delegating(format!("{agent} · {}", text.chars().take(40).collect::<String>())));
                }
                // One live message per call: {"progress": [events…]}.
                let i = match self.messages.iter().rposition(|m| m.role == "agent_tool" && m.tool_call_id.as_deref() == Some(id.as_str())) {
                    Some(i) => i,
                    None => {
                        let mut m = Message::new("agent_tool", json!({ "progress": [] }).to_string());
                        m.tool_call_id = Some(id);
                        self.messages.push(m);
                        self.messages.len() - 1
                    }
                };
                let mut v: Value = serde_json::from_str(&self.messages[i].content).unwrap_or(json!({ "progress": [] }));
                if let Some(list) = v["progress"].as_array_mut() {
                    list.push(event);
                    if list.len() > 60 {
                        list.remove(0);
                    }
                }
                self.messages[i].content = v.to_string();
                self.touch(i);
            }
            AgentEvent::Finished { agent, status, confidence, ms, output, depth } => {
                let conf = confidence.map_or(String::new(), |c| format!(", confidence {c:.2}"));
                let line = format!("{agent} {} ({}{conf}) in {:.1}s", status.as_str(), if depth > 1 { "nested" } else { "to lyra" }, ms as f32 / 1000.0);
                self.log(if status == DelegationStatus::Completed { Level::Agent } else { Level::Error }, line.clone());
                let preview: String = output.lines().take(4).collect::<Vec<_>>().join("\n");
                let more = if output.lines().count() > 4 { "\n…" } else { "" };
                // The card that said "working on it" now says how it went.
                match self.agent_cards.iter().rposition(|(a, _)| *a == agent) {
                    Some(k) => {
                        let (_, i) = self.agent_cards.remove(k);
                        if let Some(card) = self.messages.get_mut(i) {
                            card.content = format!("{line}\n{preview}{more}");
                            self.touch(i);
                        }
                    }
                    None => self.messages.push(Message::new("agent", format!("{line}\n{preview}{more}"))),
                }
                self.set_phase(if self.waiting { Phase::Waiting } else { Phase::Idle });
                self.refresh_agents();
            }
        }
    }

    /// The user's reaction to a reply counts for the agents that worked on
    /// it; a correction is reviewed for a lesson that becomes the agent's
    /// own skill (A14).
    pub(crate) fn judge_delegations(&self, run: Uuid, outcome: lyra_learning::SkillOutcome, message: &str) {
        let Some(agents) = self.agents.clone() else { return };
        let word = match outcome {
            lyra_learning::SkillOutcome::Success => "success",
            lyra_learning::SkillOutcome::Failure => "failure",
            _ => "partial",
        };
        let (learning, tx, message) = (self.learning.clone(), self.tx.clone(), message.to_string());
        let (url, model) = (format!("{}/chat/completions", self.base_url.trim_end_matches('/')), self.model.clone());
        std::thread::spawn(move || {
            let Ok(records) = agents.registry.set_outcome(run, word) else { return };
            if records.is_empty() {
                return;
            }
            let _ = tx.send(StreamEvent::Log(format!("agents: {word} recorded for {}", records.iter().map(|r| r.agent.as_str()).collect::<Vec<_>>().join(", "))));
            let Some(learning) = learning.filter(|_| outcome == lyra_learning::SkillOutcome::Failure) else { return };
            for r in records {
                let transcript = format!("Task given to the {} agent: {}\n\nIts result:\n{}\n\nThe user's reaction: {message}", r.agent, r.task, r.output);
                let review = learning.review(&url, &model, "the user corrected a specialist agent's work", &transcript, Some(run));
                if let Ok(lyra_learning::Applied::Created(skill)) = &review.outcome {
                    let note = match learning.assign(&skill.name, Some(&r.agent)) {
                        Ok(_) => format!("{} is now {}'s own skill", skill.name, r.agent),
                        Err(e) => format!("couldn't give {} to {}: {e}", skill.name, r.agent),
                    };
                    let _ = tx.send(StreamEvent::LearnNotes(vec![note]));
                }
                let _ = tx.send(StreamEvent::Reviewed(review));
            }
        });
    }

    /// `/agents …` and `/agent …`.
    pub(crate) fn agents_command(&mut self, name: &str, arg: &str) -> Result<String, String> {
        let agents = self.agents.clone().ok_or_else(|| self.agents_status.clone().err().unwrap_or_else(|| "agents are off ([agents] enabled)".into()))?;
        let (sub, rest) = arg.trim().split_once(' ').unwrap_or((arg.trim(), ""));
        let rest = rest.trim();
        if name == "/agents" {
            return match sub {
                "" => Ok(list(&agents)),
                "log" => {
                    let rows = agents.registry.delegations(None, 15)?;
                    if rows.is_empty() {
                        return Ok("no delegations yet".into());
                    }
                    Ok(rows
                        .iter()
                        .map(|d| {
                            format!(
                                "{} {} → {} [{}] {} · {:.1}s · {} model / {} tool calls{}\n  {}",
                                d.created_at.format("%m-%d %H:%M"),
                                d.from_agent,
                                d.agent,
                                d.method,
                                d.status,
                                d.duration_ms as f32 / 1000.0,
                                d.model_calls,
                                d.tool_calls,
                                d.outcome.as_ref().map_or(String::new(), |o| format!(" · user: {o}")),
                                d.task.chars().take(100).collect::<String>()
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n"))
                }
                _ => Err("usage: /agents [log]".into()),
            };
        }
        let changed = |app: &mut Self, what: String| {
            app.sync_agents();
            app.refresh_agents();
            app.log(crate::Level::Agent, what.clone());
            Ok(what)
        };
        match sub {
            "" => Ok(list(&agents)),
            "new" | "create" => {
                if self.wizard_active() {
                    let d = self.wizard().expect("active");
                    return Ok(match d.next_question() {
                        Some(q) => format!("(creating {}) {}", d.name.clone().unwrap_or_else(|| "an agent".into()), q.render()),
                        None => format!("{}\n\n{REVIEW_HELP}", d.summary()),
                    });
                }
                if rest.is_empty()
                    && let Some(saved) = agents.registry.draft().and_then(|v| serde_json::from_value::<AgentCreationDraft>(v).ok())
                {
                    if saved.stage == Stage::Review {
                        let text = format!("resuming the agent you were creating\n{}\n\n{REVIEW_HELP}", saved.summary());
                        self.set_wizard(Some(saved));
                        return Ok(text);
                    }
                    return Ok(format!("resuming the agent you were creating\n{}", self.wizard_next(saved)));
                }
                let mut draft = AgentCreationDraft::start(rest);
                if rest.starts_with("expert") {
                    draft.mode = CreationMode::Expert;
                    let body = rest.trim_start_matches("expert").trim();
                    if body.is_empty() {
                        self.set_wizard(Some(draft));
                        return Ok("Paste the profile as TOML or YAML (name, description, instructions, tools, memory, permissions, routing, model) — or 'cancel'".into());
                    }
                    let p = lyra_agents::builder::from_expert(body)?;
                    draft.name = Some(p.title.clone());
                    draft.profile = Some(p);
                }
                let intro = match &draft.template {
                    Some(t) => format!("Starting from the {t} template. Answer each question (or 'skip'); /agent cancel stops.\n\n"),
                    None if draft.mode == CreationMode::Guided => format!(
                        "A few questions to set up the agent ({}). /agent cancel stops.\n\n",
                        lyra_agents::templates::NAMES.join(", ").replace(", custom", "") + " are templates: /agent new writer"
                    ),
                    None => String::new(),
                };
                Ok(format!("{intro}{}", self.wizard_next(draft)))
            }
            "cancel" => {
                self.set_wizard(None);
                Ok("stopped creating the agent (nothing was saved)".into())
            }
            "ask" => {
                let (who, task) = rest.split_once(' ').ok_or("usage: /agent ask <name> <task>")?;
                agents.registry.find(who)?;
                self.pending_input = Some(format!("@{who} {task}"));
                Ok(format!("handing it to {who}…"))
            }
            "edit" => {
                let what = edit(&agents, rest)?;
                changed(self, what)
            }
            "enable" | "disable" => {
                let p = agents.registry.set_enabled(rest, sub == "enable")?;
                changed(self, format!("{} {}d", p.title, sub))
            }
            "delete" => {
                let what = agents.registry.delete(rest)?;
                changed(self, what)
            }
            "history" => {
                let p = agents.registry.find(rest)?;
                let versions = agents.registry.versions(&p.name)?;
                Ok(versions.iter().map(|v| format!("v{} · {} · {}", v.version, v.created_at.format("%Y-%m-%d %H:%M"), v.reason)).collect::<Vec<_>>().join("\n"))
            }
            "rollback" => {
                let (key, to) = rest.split_once(' ').map_or((rest, None), |(k, v)| (k, v.trim().trim_start_matches('v').parse().ok()));
                let p = agents.registry.rollback(key, to)?;
                changed(self, format!("{} rolled back (now v{})", p.title, p.version))
            }
            "skill" => {
                let (skill, who) = rest.split_once(' ').ok_or("usage: /agent skill <skill> <agent|global>")?;
                let learning = self.learning.clone().ok_or("learning is off")?;
                let who = who.trim();
                let agent = if who == "global" || who == MAIN { None } else { Some(agents.registry.find(who)?.name) };
                let s = learning.assign(skill, agent.as_deref())?;
                self.refresh_skills();
                Ok(format!("{} is {}", s.name, agent.map_or("shared by every agent again".into(), |a| format!("{a}'s own skill now"))))
            }
            key => show(&agents, self.learning.as_deref(), key),
        }
    }
}

// ---- plans (A12): the planner can give steps to the specialists

/// The specialists, as the planner sees them.
pub fn plan_agents(caps: Option<&Caps>) -> Vec<lyra_execution::AgentInfo> {
    let Some(agents) = caps.and_then(Caps::agents) else { return Vec::new() };
    agents
        .registry
        .enabled()
        .into_iter()
        .map(|a| lyra_execution::AgentInfo {
            changes_things: delegation::max_risk(&a) != lyra_capabilities::RiskLevel::ReadOnly,
            name: a.name,
            description: a.description,
        })
        .collect()
}

/// The tools a specialist may use in a plan step: what its permissions allow.
pub fn step_tools(caps: Option<&Caps>, agent: &str) -> Option<Vec<String>> {
    let caps = caps?;
    let p = caps.agents()?.registry.get(agent).filter(|p| p.enabled)?;
    Some(caps.manager.usable().into_iter().filter(|c| c.kind.callable() && delegation::allows(&p, c)).map(|c| c.name).collect())
}

/// A specialist's memory scopes: (write, read); `None` is unrestricted.
pub fn scopes(caps: Option<&Caps>, agent: &str) -> (Option<Vec<String>>, Option<Vec<String>>) {
    match caps.and_then(Caps::agents).and_then(|a| a.registry.get(agent)) {
        Some(p) => (p.memory_policy.write_scopes(&p.name), p.memory_policy.read_scopes(&p.name)),
        // Not a known agent: it writes nowhere special.
        None => (Some(Vec::new()), None),
    }
}

/// What a plan step needs from the agent's profile: its instructions and
/// model settings.
pub fn step_profile(caps: Option<&Caps>, agent: &str) -> Option<AgentProfile> {
    caps.and_then(Caps::agents)?.registry.get(agent)
}

/// Show and record a specialist's plan step (method "planner").
pub fn step_started(tx: &Sender<StreamEvent>, caps: Option<&Caps>, p: &AgentProfile, task: &str) {
    if let Some(a) = caps.and_then(Caps::agents) {
        a.set_active(&p.title, true);
    }
    emit(tx, AgentEvent::Started { agent: p.title.clone(), task: task.chars().take(160).collect(), depth: 1 });
}

#[allow(clippy::too_many_arguments)]
pub fn step_finished(tx: &Sender<StreamEvent>, caps: Option<&Caps>, p: &AgentProfile, task: &str, output: Result<&str, &str>, ms: u64, calls: (u32, u32, u64)) {
    let status = if output.is_ok() { DelegationStatus::Completed } else { DelegationStatus::Failed };
    let text = match output {
        Ok(t) | Err(t) => t.to_string(),
    };
    if let Some(a) = caps.and_then(Caps::agents) {
        a.set_active(&p.title, false);
        let _ = a.registry.record_delegation(&DelegationRecord {
            id: Uuid::new_v4(),
            run_id: None,
            from_agent: "planner".into(),
            agent: p.name.clone(),
            task: task.chars().take(500).collect(),
            method: "planner".into(),
            confidence: None,
            status: status.as_str().into(),
            output: text.chars().take(2000).collect(),
            duration_ms: ms,
            model_calls: calls.0,
            tool_calls: calls.1,
            tokens: calls.2,
            outcome: None,
            created_at: Utc::now(),
        });
    }
    emit(tx, AgentEvent::Finished { agent: p.title.clone(), status, confidence: None, ms, output: text, depth: 1 });
}

// ---- evolution (A15)

/// Each agent's track record, for spotting the ones that struggle.
pub fn evolution_records(agents: &Agents) -> Vec<lyra_evolution::detect::AgentRecord> {
    let stats = agents.registry.stats();
    agents
        .registry
        .enabled()
        .into_iter()
        .map(|a| {
            let s = stats.get(&a.name).cloned().unwrap_or_default();
            let examples = agents
                .registry
                .delegations(Some(&a.name), 30)
                .unwrap_or_default()
                .into_iter()
                .filter(|d| d.status != "completed" || d.outcome.as_deref() == Some("failure"))
                .take(4)
                .map(|d| format!("{} → {}{}", d.task.chars().take(150).collect::<String>(), d.status, if d.outcome.as_deref() == Some("failure") { ", corrected by the user" } else { "" }))
                .collect();
            lyra_evolution::detect::AgentRecord { name: a.name, delegations: s.delegations, failed: s.failed, corrected: s.corrected, examples }
        })
        .collect()
}

/// Tasks to benchmark an agent change with: its recent delegations (the
/// troubled ones first) and its test task.
pub fn bench_tasks(agents: &Agents, p: &AgentProfile, limit: usize) -> Vec<(String, String)> {
    let mut recent = agents.registry.delegations(Some(&p.name), 30).unwrap_or_default();
    recent.sort_by_key(|d| d.status == "completed" && d.outcome.as_deref() != Some("failure"));
    let mut out: Vec<(String, String)> = Vec::new();
    for d in recent {
        if !out.iter().any(|(t, _)| *t == d.task) {
            let expect = if d.outcome.as_deref() == Some("failure") {
                "a correct, complete result for the task; an earlier result was corrected by the user, so it must do better"
            } else {
                "a correct, complete result for the task"
            };
            out.push((d.task, expect.into()));
        }
    }
    if let Some(t) = &p.test_task
        && !out.iter().any(|(x, _)| x == t)
    {
        out.push((t.clone(), "a correct, complete result for the task".into()));
    }
    out.truncate(limit.max(1));
    out
}

/// One answer from an agent profile, with no tools (benchmarks change nothing).
pub fn bench_answer(url: &str, model: &str, p: &AgentProfile, task: &str) -> Result<(String, u64), String> {
    let request = DelegationRequest {
        task_id: Uuid::new_v4(),
        from_agent: MAIN.into(),
        to_agent: p.name.clone(),
        instruction: task.to_string(),
        context: delegation::build_context(task, delegation::extract_input(task).as_deref(), &[], &[]),
        expected_output: OutputContract::default(),
        budget: AgentBudget::default(),
        depth: 1,
    };
    let messages = [
        json!({ "role": "system", "content": delegation::system_prompt(p, &[], &request) }),
        json!({ "role": "user", "content": delegation::user_prompt(&request) }),
    ];
    let url = p.model_policy.url.clone().map(|u| format!("{}/chat/completions", u.trim_end_matches('/'))).unwrap_or_else(|| url.to_string());
    let model = p.model_policy.model.clone().unwrap_or_else(|| model.to_string());
    let options = crate::plan::ChatOptions { max_tokens: p.model_policy.max_tokens, thinking: p.model_policy.thinking, temperature: p.model_policy.temperature };
    let (message, tokens) = crate::plan::chat_with(&url, &model, &messages, &[], &options)?;
    let r = delegation::parse_result(request.task_id, message["content"].as_str().unwrap_or(""));
    Ok((r.text(), tokens))
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    use super::*;
    use crate::mem::Mem;
    use lyra_memory::{MemoryManager, Settings as MemorySettings};

    /// A chat server that answers each request with the next canned message
    /// and hands back the request bodies it got.
    fn fake_model(replies: Vec<Value>) -> (String, mpsc::Receiver<Value>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1/chat/completions", listener.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for reply in replies {
                let Ok((mut stream, _)) = listener.accept() else { return };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if let Some(v) = line.to_lowercase().strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap();
                    }
                    if line == "\r\n" {
                        break;
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let _ = tx.send(serde_json::from_slice(&body).unwrap_or(Value::Null));
                let out = json!({ "choices": [{ "message": reply }], "usage": { "total_tokens": 10 } }).to_string();
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{out}", out.len());
            }
        });
        (url, rx)
    }

    struct Fixture {
        _rt: tokio::runtime::Runtime,
        env: Env,
        events: mpsc::Receiver<StreamEvent>,
    }

    fn fixture(url: &str) -> Fixture {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let dir = std::env::temp_dir().join(format!("lyra-agents-test-{}", Uuid::new_v4()));
        let memory = rt.block_on(MemoryManager::open_lance(&dir.join("memory"), "memories", MemorySettings::default())).unwrap();
        let mem = Mem::new(memory, rt.handle().clone(), "test".into(), None);
        let tools = Arc::new(crate::tools::Tools::new(Arc::new(mem)));
        let manager = rt.block_on(lyra_capabilities::CapabilityManager::open(&dir.join("caps"), Default::default())).unwrap();
        let mut caps = Caps::new(manager, rt.handle().clone(), Vec::new(), Vec::new());
        caps.tools = Some(tools.clone());
        let system = lyra_system::Settings { write_roots: vec![dir.join("work").display().to_string()], approval_timeout_seconds: 10, ..Default::default() };
        caps.system = Some(lyra_system::System::new(system, crate::config::expand_path));
        let agents = Arc::new(Agents::open(&dir.join("agents"), rt.handle().clone(), Settings::default()).unwrap());
        agents.registry.create(lyra_agents::templates::template("writer").unwrap(), "test").unwrap();
        agents.registry.create(lyra_agents::templates::template("operator").unwrap(), "test").unwrap();
        caps.set_agents(agents.clone());
        caps.refresh();
        let (tx, events) = mpsc::channel();
        let env = Env { url: url.into(), model: "m".into(), caps: Some(Arc::new(caps)), tools: Some(tools), learning: None, agents, tx, machine: None, fleet: None, cancel: Default::default(), member: false, viewer: None };
        Fixture { _rt: rt, env, events }
    }

    #[test]
    fn a_delegation_stays_within_the_agents_permissions_and_is_recorded() {
        let (url, requests) = fake_model(vec![
            // The writer tries a tool it may not use, then answers.
            json!({ "role": "assistant", "content": "", "tool_calls": [{ "id": "c1", "type": "function", "function": { "name": "memory_forget", "arguments": "{\"id\":\"x\"}" } }] }),
            json!({ "role": "assistant", "content": "Hi Sam, the report will be ready Friday.\nCONFIDENCE: 0.9" }),
        ]);
        let f = fixture(&url);
        let writer = f.env.agents.registry.get("writer").unwrap();
        let r = delegate(&f.env, &writer, MAIN, "Rewrite this email", Some("sam report friday"), None, 1, "rule", Some(0.9), None);
        assert_eq!(r.status, DelegationStatus::Completed);
        assert_eq!(r.confidence, Some(0.9));
        assert!(r.text().contains("ready Friday"));

        let first = requests.recv().unwrap();
        let offered: Vec<&str> = first["tools"].as_array().map(|t| t.iter().filter_map(|d| d["function"]["name"].as_str()).collect()).unwrap_or_default();
        assert!(offered.contains(&"memory_recall") && !offered.contains(&"memory_forget"), "{offered:?}");
        assert!(first["messages"][0]["content"].as_str().unwrap().contains(&writer.instructions));
        let second = requests.recv().unwrap();
        let refused = second["messages"].as_array().unwrap().iter().find(|m| m["role"] == "tool").unwrap();
        assert!(refused["content"].as_str().unwrap().contains("may not use memory_forget"), "{refused}");

        let records = f.env.agents.registry.delegations(Some("writer"), 5).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!((records[0].model_calls, records[0].tool_calls, records[0].method.as_str()), (2, 0, "rule"));
        let events: Vec<StreamEvent> = f.events.try_iter().collect();
        assert!(matches!(events.first(), Some(StreamEvent::Agent(AgentEvent::Started { .. }))));
        assert!(events.iter().any(|e| matches!(e, StreamEvent::Agent(AgentEvent::Tool { tool, .. }) if tool == "memory_forget")));
        assert!(events.iter().any(|e| matches!(e, StreamEvent::Agent(AgentEvent::ToolResult { output, .. }) if output.contains("may not use memory_forget"))), "the result follows the call");
        assert!(matches!(events.last(), Some(StreamEvent::Agent(AgentEvent::Finished { status: DelegationStatus::Completed, .. }))));
        assert!(f.env.agents.active.lock().unwrap().is_empty());
    }

    #[test]
    fn a_routed_message_reaches_the_main_agent_as_its_own_delegate_call() {
        let (url, _requests) = fake_model(vec![json!({ "role": "assistant", "content": "Dear team, …\nCONFIDENCE: 0.8" })]);
        let f = fixture(&url);
        let mut history = vec![json!({ "role": "system", "content": "s" }), json!({ "role": "user", "content": "@writer make this friendlier:\nSend the numbers by noon." })];
        let run = Uuid::new_v4();
        assert_eq!(auto_delegate(&f.env, "@writer make this friendlier:\nSend the numbers by noon.", run, &mut history).as_deref(), Some("Writer"));
        assert_eq!(history[2]["tool_calls"][0]["function"]["name"], "delegate");
        assert_eq!(history[3]["role"], "tool");
        assert_eq!(history[3]["tool_call_id"], history[2]["tool_calls"][0]["id"]);
        assert!(history[3]["content"].as_str().unwrap().contains("Dear team"));
        assert!(matches!(f.events.try_iter().next(), Some(StreamEvent::Agent(AgentEvent::Routed { method: RouteMethod::Explicit, .. }))));
        let records = f.env.agents.registry.set_outcome(run, "failure").unwrap();
        assert_eq!(records.len(), 1, "the user's reaction applies to the run's delegations");
        assert_eq!(f.env.agents.registry.stats()["writer"].corrected, 1);

        // Nothing to route: the main agent keeps it.
        let mut quiet = Vec::new();
        f.env.agents.settings.lock().unwrap().routing.model_fallback = false;
        assert!(auto_delegate(&f.env, "what's 2 + 2?", Uuid::new_v4(), &mut quiet).is_none());
        assert!(quiet.is_empty());
    }

    #[test]
    fn the_operator_reads_freely_and_asks_before_changing_anything() {
        let tool = |id: &str, name: &str, args: Value| {
            json!({ "role": "assistant", "content": "", "tool_calls": [{ "id": id, "type": "function", "function": { "name": name, "arguments": args.to_string() } }] })
        };
        let (url, requests) = fake_model(vec![
            tool("c1", "shell_run", json!({ "command": "echo looked" })),
            tool("c2", "shell_run", json!({ "command": "touch made.txt", "cwd": "/tmp" })),
            tool("c3", "shell_run", json!({ "command": "rm -f made.txt", "cwd": "/tmp" })),
            tool("c4", "shell_run", json!({ "command": "rm -rf /" })),
            json!({ "role": "assistant", "content": "Done.\nCONFIDENCE: 0.9" }),
        ]);
        let f = fixture(&url);
        let operator = f.env.agents.registry.get("operator").unwrap();
        let env = f.env.clone();
        let worker = std::thread::spawn(move || delegate(&env, &operator, MAIN, "tidy up", None, None, 1, "tool", None, None));
        // The TUI's side: the first change is approved, the second denied.
        let mut answers = vec![Answer::No, Answer::Yes];
        let mut asked = Vec::new();
        while let Ok(e) = f.events.recv_timeout(std::time::Duration::from_secs(10)) {
            match e {
                StreamEvent::Approval(r) => {
                    asked.push((format!("{}: {}", r.what, r.detail), r.dangerous));
                    r.reply.send(answers.pop().unwrap()).unwrap();
                }
                StreamEvent::Agent(AgentEvent::Finished { .. }) => break,
                _ => {}
            }
        }
        let r = worker.join().unwrap();
        assert_eq!(r.status, DelegationStatus::Completed);
        assert_eq!(asked.len(), 2, "reading didn't ask; forbidden didn't ask: {asked:?}");
        assert!(asked[0].0.contains("touch made.txt") && !asked[0].1);
        assert!(asked[1].0.contains("rm -f made.txt") && asked[1].1, "deleting is flagged");
        let last = requests.try_iter().last().unwrap();
        let results: Vec<String> =
            last["messages"].as_array().unwrap().iter().filter(|m| m["role"] == "tool").map(|m| m["content"].as_str().unwrap().to_string()).collect();
        assert!(results[0].contains("looked"), "{results:?}");
        assert!(results[1].contains("exit_code"), "approved, so it ran: {}", results[1]);
        assert!(results[2].contains("declined"), "{}", results[2]);
        assert!(results[3].contains("refused"), "{}", results[3]);
        assert!(std::path::Path::new("/tmp/made.txt").exists(), "the approved change happened");
        let _ = std::fs::remove_file("/tmp/made.txt");

        // The main agent never gets system tools, and can't call them.
        let caps = f.env.caps.as_ref().unwrap();
        assert!(!caps.tool_definitions().iter().any(|d| d["function"]["name"] == "shell_run"));
        let direct = caps.invoke("shell_run", r#"{"command":"echo hi"}"#, CallContext::new(None, ""), true, true);
        assert!(direct.contains("only for agents"), "{direct}");
        // Other agents don't have them either.
        let writer = f.env.agents.registry.get("writer").unwrap();
        assert!(!caps.manager.usable().iter().any(|c| c.name == "shell_run" && delegation::allows(&writer, c)));
    }

    #[test]
    fn at_mentions_pick_a_machine() {
        let known = vec!["desktop".to_string(), "web1".to_string()];
        assert_eq!(machine_mention("@desktop how much disk is free?", &known).as_deref(), Some("desktop"));
        assert_eq!(machine_mention("check uptime on @Web1, please", &known).as_deref(), Some("web1"));
        assert_eq!(machine_mention("@server restart nginx", &known).as_deref(), Some("server"));
        assert_eq!(machine_mention("@writer fix this email", &known), None, "agents aren't machines");
        assert_eq!(machine_mention("mail me at a@b.com", &known), None);

        let f = fixture("http://127.0.0.1:9/v1/chat/completions");
        let mut env = f.env.clone();
        env.machine = Some("desktop".into());
        assert_eq!(serde_json::from_str::<Value>(&with_machine(&env, "shell_run", r#"{"command":"ls"}"#)).unwrap()["machine"], "desktop");
        assert_eq!(serde_json::from_str::<Value>(&with_machine(&env, "shell_run", r#"{"command":"ls","machine":"server"}"#)).unwrap()["machine"], "server", "an explicit choice stays");
        assert_eq!(with_machine(&env, "memory_recall", r#"{"query":"x"}"#), r#"{"query":"x"}"#, "only system tools");
        assert_eq!(fleet_mention("update packages on @web please", &["web".into()]).as_deref(), Some("web"));
        assert_eq!(fleet_mention("disk space on @all.", &[]).as_deref(), Some("all"));
        assert_eq!(fleet_mention("email me@all-hands.com", &[]), None);
        env.fleet = Some("web".into());
        assert_eq!(serde_json::from_str::<Value>(&with_machine(&env, "fleet_run", r#"{"command":"uptime"}"#)).unwrap()["machines"], "web");
    }

    #[test]
    fn plans_see_the_registry_and_its_scopes() {
        let f = fixture("http://127.0.0.1:9/v1/chat/completions");
        let caps = f.env.caps.as_deref();
        let names: Vec<String> = plan_agents(caps).into_iter().map(|a| a.name).collect();
        assert!(names.contains(&"writer".to_string()) && names.contains(&"archivist".to_string()), "{names:?}");
        let tools = step_tools(caps, "archivist").unwrap();
        assert!(tools.contains(&"memory_remember".to_string()) && !tools.contains(&"memory_forget".to_string()), "{tools:?}");
        let archivist = scopes(caps, "archivist").0.unwrap();
        assert!(archivist.contains(&"project:*".to_string()) && !archivist.contains(&"user".to_string()), "{archivist:?}");
        let (write, read) = scopes(caps, "writer");
        assert!(read.unwrap().contains(&"user".to_string()));
        assert!(write.is_some());
        assert!(f.env.caps.as_ref().unwrap().manager.get("agent.writer").is_some(), "agents are capabilities");
        f.env.agents.registry.set_enabled("writer", false).unwrap();
        assert!(step_tools(caps, "writer").is_none(), "disabled agents get no plan steps");
    }
}

//! Delegation (A5, A8, A9): a structured request with only the context the
//! task needs, a structured result, and the permission check that decides
//! which capabilities an agent may touch. The runtime enforces it.

use lyra_capabilities::{Capability, RiskLevel};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::model::*;

/// What the result should look like.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputContract {
    /// `text`, `markdown` or `json`.
    pub format: String,
    /// In words: "the rewritten email only".
    pub description: String,
}

impl Default for OutputContract {
    fn default() -> Self {
        Self { format: "text".into(), description: "the result of the task".into() }
    }
}

/// How much one delegation may use.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentBudget {
    pub max_model_calls: u32,
    pub max_tool_calls: u32,
}

impl Default for AgentBudget {
    fn default() -> Self {
        Self { max_model_calls: 6, max_tool_calls: 12 }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DelegationRequest {
    pub task_id: Uuid,
    pub from_agent: String,
    pub to_agent: String,
    pub instruction: String,
    /// The scoped context: the input, relevant preferences, earlier results.
    pub context: Value,
    pub expected_output: OutputContract,
    pub budget: AgentBudget,
    /// How deep in a chain of delegations this is (the main agent's is 1).
    pub depth: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationStatus {
    Completed,
    Partial,
    Failed,
    /// Not the agent's kind of work, or not allowed.
    Refused,
}

impl DelegationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Partial => "partial",
            Self::Failed => "failed",
            Self::Refused => "refused",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DelegationResult {
    pub task_id: Uuid,
    pub status: DelegationStatus,
    pub output: Value,
    pub confidence: Option<f32>,
    pub notes: Option<String>,
}

impl DelegationResult {
    pub fn text(&self) -> String {
        match &self.output {
            Value::String(s) => s.clone(),
            v => v.to_string(),
        }
    }
}

/// The scoped context (A8): the task, its input, the memories this agent
/// may see that bear on it, and what to hand back — not the conversation.
pub fn build_context(task: &str, input: Option<&str>, memories: &[String], previous: &[(String, String)]) -> Value {
    let mut ctx = json!({ "task": task });
    if let Some(input) = input.filter(|i| !i.trim().is_empty()) {
        ctx["input"] = json!(input);
    }
    if !memories.is_empty() {
        ctx["relevant_memories"] = json!(memories);
    }
    if !previous.is_empty() {
        ctx["results_so_far"] = json!(previous.iter().map(|(who, what)| json!({ "from": who, "result": what })).collect::<Vec<_>>());
    }
    ctx
}

/// The longest quoted or pasted part of a message: the thing to work on.
pub fn extract_input(message: &str) -> Option<String> {
    if let Some((_, rest)) = message.split_once("```")
        && let Some((block, _)) = rest.split_once("```")
    {
        let block = block.split_once('\n').map_or(block, |(first, body)| if first.trim().contains(' ') { block } else { body });
        return Some(block.trim().to_string());
    }
    // "Please rewrite this email for me:\n\n<email>"
    if let Some((head, body)) = message.split_once(":\n")
        && head.len() < 200
        && body.trim().len() > 20
    {
        return Some(body.trim().to_string());
    }
    None
}

/// The agent's system prompt: who it is, how it works, its skills, and the
/// contract for its answer.
pub fn system_prompt(p: &AgentProfile, skills: &[(String, String)], r: &DelegationRequest) -> String {
    let mut out = format!("You are {}, a specialist agent. {}\n\n{}", p.title, p.role, p.instructions);
    if !skills.is_empty() {
        out += "\n\n# Your skills\n";
        for (name, text) in skills {
            out += &format!("\n## {name}\n{text}\n");
        }
    }
    out += &format!(
        "\n\nThe main agent gave you a task. Work only on it, with the context given. Return {} as {}. \
         End with a line `CONFIDENCE: <0-1>`; if you can't or shouldn't do it, write `STATUS: refused — <why>` \
         or `STATUS: failed — <why>` instead of an answer.",
        r.expected_output.description, r.expected_output.format
    );
    out
}

pub fn user_prompt(r: &DelegationRequest) -> String {
    format!("{}\n\nContext:\n{}", r.instruction, serde_json::to_string_pretty(&r.context).unwrap_or_default())
}

/// Read the agent's answer into a result.
pub fn parse_result(task_id: Uuid, reply: &str) -> DelegationResult {
    let reply = reply.rsplit_once("</think>").map_or(reply, |(_, a)| a).trim();
    let mut status = DelegationStatus::Completed;
    let mut confidence = None;
    let mut notes = None;
    let mut body = Vec::new();
    for line in reply.lines() {
        let t = line.trim();
        let lower = t.to_lowercase();
        if let Some(c) = lower.strip_prefix("confidence:") {
            confidence = c.trim().trim_end_matches('%').parse::<f32>().ok().map(|c| if c > 1.0 { c / 100.0 } else { c }.clamp(0.0, 1.0));
        } else if let Some(s) = lower.strip_prefix("status:") {
            let s = s.trim();
            status = if s.starts_with("refused") {
                DelegationStatus::Refused
            } else if s.starts_with("failed") {
                DelegationStatus::Failed
            } else if s.starts_with("partial") {
                DelegationStatus::Partial
            } else {
                status
            };
            let why = t[t.find(':').map_or(0, |i| i + 1)..].trim();
            let why = why.trim_start_matches(|c: char| c.is_alphabetic()).trim_start_matches(|c: char| c == '—' || c == '-' || c == ':' || c.is_whitespace());
            if !why.is_empty() {
                notes = Some(why.to_string());
            }
        } else {
            body.push(line);
        }
    }
    let text = body.join("\n").trim().to_string();
    if text.is_empty() && status == DelegationStatus::Completed {
        status = DelegationStatus::Failed;
        notes = Some("the agent returned nothing".into());
    }
    DelegationResult { task_id, status, output: Value::String(text), confidence, notes }
}

/// The riskiest level an agent may use.
pub fn max_risk(p: &AgentProfile) -> RiskLevel {
    serde_json::from_value(json!(p.permission_policy.max_risk)).unwrap_or(RiskLevel::ReadOnly)
}

/// Whether an agent may use a capability (A9): it's in its tools or
/// capability patterns, within its risk ceiling, not denied, and its memory
/// policy allows memory tools of that kind.
pub fn allows(p: &AgentProfile, c: &Capability) -> bool {
    let matches = |pattern: &str| {
        let pattern = pattern.trim();
        match pattern.strip_suffix('*') {
            Some(prefix) => c.id.starts_with(prefix) || c.name.starts_with(prefix) || c.source == prefix.trim_end_matches('.'),
            None => c.id == pattern || c.name == pattern,
        }
    };
    if p.permission_policy.deny.iter().any(|d| matches(d)) || c.risk > max_risk(p) {
        return false;
    }
    if !(p.tools.iter().any(|t| matches(t)) || p.capabilities.iter().any(|t| matches(t))) {
        return false;
    }
    if c.name.starts_with("memory_") {
        let writes = c.risk != RiskLevel::ReadOnly;
        return if writes { p.memory_policy.writes() } else { p.memory_policy.reads() };
    }
    if c.name == "working_memory" {
        return p.memory_policy.mode != MemoryMode::None;
    }
    true
}

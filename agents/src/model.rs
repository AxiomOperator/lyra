//! Agent profiles (A1): every agent, the main one included, is described
//! the same way: role, instructions, what it may use, what it may remember,
//! which model it runs on and when work should be delegated to it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Which model an agent runs on. Unset fields use the main agent's.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelPolicy {
    /// Another model on the same server (or on `url`).
    pub model: Option<String>,
    /// Another OpenAI-compatible server.
    pub url: Option<String>,
    pub max_tokens: Option<u32>,
    /// Let the model think first (reasoning models); `false` is faster.
    pub thinking: Option<bool>,
    pub temperature: Option<f32>,
}

/// What an agent may remember (A10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryMode {
    /// No memory at all.
    None,
    /// Only short-term notes for the task at hand.
    SessionOnly,
    /// Reads the shared memory, writes nothing.
    SharedReadOnly,
    /// Reads and writes the shared memory (within the allowed scopes).
    SharedReadWrite,
    /// Reads and writes only the listed scopes.
    #[default]
    Scoped,
}

impl MemoryMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::SessionOnly => "session_only",
            Self::SharedReadOnly => "shared_read_only",
            Self::SharedReadWrite => "shared_read_write",
            Self::Scoped => "scoped",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryPolicy {
    pub mode: MemoryMode,
    /// Scopes it may read (`user`, `project:*`, …) when scoped.
    pub read: Vec<String>,
    /// Scopes it may write when scoped. Its own scope, `agent:<name>`, is
    /// always writable unless the mode forbids writing.
    pub write: Vec<String>,
}

impl MemoryPolicy {
    pub fn reads(&self) -> bool {
        !matches!(self.mode, MemoryMode::None | MemoryMode::SessionOnly)
    }

    pub fn writes(&self) -> bool {
        matches!(self.mode, MemoryMode::SharedReadWrite | MemoryMode::Scoped)
    }

    /// Scope patterns it may read; `None` means every scope.
    pub fn read_scopes(&self, agent: &str) -> Option<Vec<String>> {
        match self.mode {
            MemoryMode::None | MemoryMode::SessionOnly => Some(Vec::new()),
            MemoryMode::SharedReadOnly | MemoryMode::SharedReadWrite => None,
            MemoryMode::Scoped => {
                let mut scopes = self.read.clone();
                scopes.push(format!("agent:{agent}"));
                Some(scopes)
            }
        }
    }

    /// Scope patterns it may write; `None` means every allowed scope.
    pub fn write_scopes(&self, agent: &str) -> Option<Vec<String>> {
        match self.mode {
            MemoryMode::None | MemoryMode::SessionOnly | MemoryMode::SharedReadOnly => Some(Vec::new()),
            MemoryMode::SharedReadWrite => None,
            MemoryMode::Scoped => {
                let mut scopes = self.write.clone();
                scopes.push(format!("agent:{agent}"));
                Some(scopes)
            }
        }
    }
}

/// What an agent may do (A9). Enforced by the runtime, not the prompt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PermissionPolicy {
    /// The riskiest capability it may use: read_only, low_write, write, destructive.
    pub max_risk: String,
    /// Capabilities it may never use (ids, names or `source.*`).
    pub deny: Vec<String>,
    /// It may hand work to other agents (within the depth limit).
    pub can_delegate: bool,
}

impl Default for PermissionPolicy {
    fn default() -> Self {
        Self { max_risk: "read_only".into(), deny: Vec::new(), can_delegate: false }
    }
}

/// When work should go to an agent (A5).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DelegationProfile {
    /// The main agent hands matching requests over on its own.
    pub auto_delegate: bool,
    /// What it handles, e.g. `rewrite_text`, `draft_email`.
    pub intents: Vec<String>,
    pub keywords: Vec<String>,
    /// Requests it should get.
    pub examples: Vec<String>,
    /// Breaks ties between agents that fit equally well.
    pub priority: i32,
    /// Requests it should not get, even when they look similar.
    pub exclusions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentProfile {
    pub id: Uuid,
    /// Lowercase, `-` between words: `writer`, `project-manager`.
    pub name: String,
    /// For display: `Writer`.
    pub title: String,
    pub description: String,
    pub role: String,
    pub instructions: String,
    /// Capability patterns it may use besides `tools` (`pm.tasks.*`, `memory_*`).
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Tools it may use, by name.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Skills it always gets (its learned skills are found automatically).
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub model_policy: ModelPolicy,
    #[serde(default)]
    pub memory_policy: MemoryPolicy,
    #[serde(default)]
    pub permission_policy: PermissionPolicy,
    #[serde(default)]
    pub delegation: DelegationProfile,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// How it looks in the app: its colour and icon (empty: picked from its kind).
    #[serde(default)]
    pub look: Look,
    /// Members may use it (an admin's choice; the Operator and the Coder never are).
    #[serde(default = "yes")]
    pub shared: bool,
    /// The template it was made from.
    #[serde(default)]
    pub template: Option<String>,
    /// A task and expectation to try it on (the wizard's test step).
    #[serde(default)]
    pub test_task: Option<String>,
    #[serde(default = "one")]
    pub version: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn yes() -> bool {
    true
}

fn one() -> u32 {
    1
}

/// An agent's colour and icon in the app (one of `COLORS`, one of `ICONS`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Look {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub color: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub icon: String,
}

/// The colours an agent can have.
pub const COLORS: &[&str] = &["sky", "teal", "emerald", "amber", "orange", "rose", "violet", "fuchsia", "slate"];
/// The icons an agent can have.
pub const ICONS: &[&str] = &["bot", "pen", "search", "archive", "server", "code", "briefcase", "calendar", "chart", "shield", "book", "sparkles"];

impl AgentProfile {
    /// Its colour: its own, else one picked from its name (the same every time).
    pub fn color(&self) -> &str {
        if COLORS.contains(&self.look.color.as_str()) {
            return &self.look.color;
        }
        let n = self.name.bytes().fold(0usize, |a, b| a.wrapping_mul(31).wrapping_add(b as usize));
        COLORS[n % (COLORS.len() - 1)]
    }

    /// Its icon: its own, else one for its kind.
    pub fn icon(&self) -> &str {
        if ICONS.contains(&self.look.icon.as_str()) {
            return &self.look.icon;
        }
        let kind = self.template.as_deref().unwrap_or(&self.name);
        match kind {
            "writer" => "pen",
            "researcher" => "search",
            "archivist" => "archive",
            "operator" => "server",
            "coder" => "code",
            "project-manager" => "briefcase",
            _ => "bot",
        }
    }

    pub fn new(name: &str, title: &str, description: &str) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v5(&Uuid::NAMESPACE_URL, format!("lyra-agent:{name}").as_bytes()),
            name: name.into(),
            title: title.into(),
            description: description.into(),
            role: String::new(),
            instructions: String::new(),
            capabilities: Vec::new(),
            tools: Vec::new(),
            skills: Vec::new(),
            model_policy: ModelPolicy::default(),
            memory_policy: MemoryPolicy::default(),
            permission_policy: PermissionPolicy::default(),
            delegation: DelegationProfile::default(),
            enabled: true,
            look: Look::default(),
            shared: true,
            template: None,
            test_task: None,
            version: 1,
            created_at: now,
            updated_at: now,
        }
    }

    /// Every way it's described, for routing (A7).
    pub fn routing_texts(&self) -> Vec<String> {
        let mut out = vec![format!("{}: {} {}", self.title, self.description, self.role)];
        out.extend(self.delegation.examples.iter().cloned());
        if !self.delegation.intents.is_empty() {
            out.push(self.delegation.intents.iter().map(|i| i.replace('_', " ")).collect::<Vec<_>>().join(", "));
        }
        out
    }

    /// `writer` from `Writer Agent`, `Project Manager`.
    pub fn slug(text: &str) -> String {
        let lower = text.to_lowercase();
        let trimmed = lower.trim().trim_end_matches(" agent").trim();
        let mut out = String::new();
        for c in trimmed.chars() {
            if c.is_ascii_alphanumeric() {
                out.push(c);
            } else if !out.ends_with('-') && !out.is_empty() {
                out.push('-');
            }
        }
        out.trim_matches('-').to_string()
    }
}

/// The main agent's name: it owns the conversation.
pub const MAIN: &str = "main";

/// A profile at one point (A13).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentVersion {
    pub agent_id: Uuid,
    pub name: String,
    pub version: u32,
    pub profile_snapshot: serde_json::Value,
    pub reason: String,
    pub created_at: DateTime<Utc>,
}

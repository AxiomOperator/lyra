use std::fmt;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One remembered fact or event. (Procedures belong in skills, not here.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub id: Uuid,
    /// Keeps memories apart: `user`, `agent`, `project:arcella`, ...
    pub scope: String,
    pub kind: MemoryKind,
    pub content: String,
    pub tags: Vec<String>,
    pub source: MemorySource,
    /// Where exactly it came from, so the agent can say why it believes it.
    pub provenance: Provenance,
    /// How useful it's likely to be: 0.1 minor detail … 0.9 critical fact.
    pub importance: f32,
    /// How sure we are it's true: stated by the user 1.0 … inferred 0.5.
    pub confidence: f32,
    pub status: MemoryStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_accessed_at: Option<DateTime<Utc>>,
    /// After this it stops being used (and the curator archives it).
    pub expires_at: Option<DateTime<Utc>>,
    /// From the usage table, filled in by the manager.
    #[serde(skip)]
    pub usage: Usage,
}

impl Memory {
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_some_and(|t| t <= now)
    }

    /// Short id used in the UI, the prompt and commands.
    pub fn short_id(&self) -> String {
        self.id.to_string()[..8].to_string()
    }
}

/// The run and tool call a memory came from.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    pub run_id: Option<Uuid>,
    pub tool_call_id: Option<String>,
    /// The conversation (a lyra session) it came from.
    pub conversation_id: Option<Uuid>,
}

/// What a new memory needs; the manager fills in the rest.
#[derive(Debug, Clone)]
pub struct NewMemory {
    pub scope: String,
    pub kind: MemoryKind,
    pub content: String,
    pub tags: Vec<String>,
    pub source: MemorySource,
    pub provenance: Provenance,
    /// Defaults to 0.5.
    pub importance: Option<f32>,
    /// Defaults to what the source suggests.
    pub confidence: Option<f32>,
    pub expires_at: Option<DateTime<Utc>>,
}

impl NewMemory {
    /// A semantic memory with defaults for everything else.
    pub fn fact(scope: &str, content: &str, source: MemorySource) -> Self {
        Self {
            scope: scope.into(),
            kind: MemoryKind::Semantic,
            content: content.into(),
            tags: Vec::new(),
            source,
            provenance: Provenance::default(),
            importance: None,
            confidence: None,
            expires_at: None,
        }
    }
}

/// How a memory has been used.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    /// Times it was put in front of the model.
    pub injected: u64,
    pub helpful: u64,
    pub unhelpful: u64,
}

/// A significant run, summarized. Also stored as an episodic memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    pub id: Uuid,
    pub scope: String,
    pub summary: String,
    pub outcome: String,
    pub entities: Vec<String>,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub source_run_id: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryKind {
    /// Durable facts.
    Semantic,
    /// Summaries of things that happened.
    Episodic,
    /// Short-term notes; they expire quickly.
    Working,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryStatus {
    Active,
    /// Replaced by a newer memory; kept for history.
    Superseded,
    /// Kept for reference, left out of normal recall.
    Archived,
    /// Soft-forgotten: left out of everything but inspection; can be restored.
    Deleted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemorySource {
    User,
    Conversation,
    Tool,
    Document,
    Agent,
    Derived,
}

impl MemorySource {
    /// How much to trust a memory from this source by default: what the user
    /// said outright beats tool output, which beats the agent's inference.
    pub fn default_confidence(self) -> f32 {
        match self {
            MemorySource::User => 1.0,
            MemorySource::Tool => 0.95,
            MemorySource::Conversation | MemorySource::Document => 0.9,
            MemorySource::Derived => 0.7,
            MemorySource::Agent => 0.5,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relationship {
    Supports,
    Contradicts,
    Supersedes,
    DerivedFrom,
    RelatedTo,
}

/// `as_str`, `Display` and `FromStr` for the lowercase names these are stored under.
macro_rules! names {
    ($ty:ident { $($variant:ident = $name:literal),+ $(,)? }) => {
        impl $ty {
            pub fn as_str(self) -> &'static str {
                match self { $($ty::$variant => $name),+ }
            }
        }

        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl FromStr for $ty {
            type Err = anyhow::Error;

            fn from_str(s: &str) -> anyhow::Result<Self> {
                match s {
                    $($name => Ok($ty::$variant),)+
                    _ => anyhow::bail!(concat!("unknown ", stringify!($ty), " {:?}"), s),
                }
            }
        }
    };
}

names!(MemoryKind { Semantic = "semantic", Episodic = "episodic", Working = "working" });
names!(MemoryStatus { Active = "active", Superseded = "superseded", Archived = "archived", Deleted = "deleted" });
names!(MemorySource {
    User = "user",
    Conversation = "conversation",
    Tool = "tool",
    Document = "document",
    Agent = "agent",
    Derived = "derived",
});
names!(Relationship {
    Supports = "supports",
    Contradicts = "contradicts",
    Supersedes = "supersedes",
    DerivedFrom = "derived_from",
    RelatedTo = "related_to",
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for s in [MemoryStatus::Active, MemoryStatus::Superseded, MemoryStatus::Archived, MemoryStatus::Deleted] {
            assert_eq!(s.as_str().parse::<MemoryStatus>().unwrap(), s);
        }
        assert_eq!("derived_from".parse::<Relationship>().unwrap(), Relationship::DerivedFrom);
        assert!("fact".parse::<MemoryKind>().is_err());
    }

    #[test]
    fn inference_is_trusted_less_than_evidence() {
        assert!(MemorySource::User.default_confidence() > MemorySource::Tool.default_confidence());
        assert!(MemorySource::Tool.default_confidence() > MemorySource::Agent.default_confidence());
        assert_eq!(MemorySource::Agent.default_confidence(), 0.5);
    }
}

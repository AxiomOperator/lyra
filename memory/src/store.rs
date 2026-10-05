use std::collections::HashMap;

use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Episode, Memory, MemoryKind, MemoryStatus, Relationship, Usage};

/// Which memories to list.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// Exact scope, or `None` for every scope.
    pub scope: Option<String>,
    /// Any of these statuses; empty means any status.
    pub statuses: Vec<MemoryStatus>,
    pub kind: Option<MemoryKind>,
}

impl Filter {
    pub fn active() -> Self {
        Self { statuses: vec![MemoryStatus::Active], ..Self::default() }
    }
}

/// A past version of a memory's content (from corrections).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Version {
    pub memory_id: Uuid,
    pub version: i64,
    pub content: String,
    pub confidence: f32,
    pub reason: String,
    pub created_at: DateTime<Utc>,
}

/// One entry in the audit log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: Uuid,
    pub memory_id: Option<Uuid>,
    /// e.g. `created`, `reconfirmed`, `corrected`, `superseded`, `archived`, `curated`.
    pub kind: String,
    pub from_state: Option<String>,
    pub to_state: Option<String>,
    pub reason: String,
    pub run_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

impl Event {
    pub fn new(kind: &str, memory_id: Option<Uuid>, reason: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            memory_id,
            kind: kind.into(),
            from_state: None,
            to_state: None,
            reason: reason.into(),
            run_id: None,
            created_at: Utc::now(),
        }
    }

    pub fn states(mut self, from: impl ToString, to: impl ToString) -> Self {
        self.from_state = Some(from.to_string());
        self.to_state = Some(to.to_string());
        self
    }

    pub fn run(mut self, run_id: Option<Uuid>) -> Self {
        self.run_id = run_id;
        self
    }
}

/// A maintenance change waiting for approval.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MemoryChange {
    /// Replace several memories with one consolidated memory.
    Merge { memories: Vec<Uuid>, scope: String, content: String, tags: Vec<String> },
    /// Archive a stale memory.
    Archive { memory: Uuid },
}

impl MemoryChange {
    pub fn kind(&self) -> &'static str {
        match self {
            MemoryChange::Merge { .. } => "merge",
            MemoryChange::Archive { .. } => "archive",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProposalStatus {
    Pending,
    Applied,
    Rejected,
}

impl ProposalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ProposalStatus::Pending => "pending",
            ProposalStatus::Applied => "applied",
            ProposalStatus::Rejected => "rejected",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Proposal {
    pub id: Uuid,
    pub change: MemoryChange,
    pub reason: String,
    pub status: ProposalStatus,
    pub created_at: DateTime<Utc>,
}

impl Proposal {
    pub fn new(change: MemoryChange, reason: String) -> Self {
        Self { id: Uuid::new_v4(), change, reason, status: ProposalStatus::Pending, created_at: Utc::now() }
    }
}

/// Storage for memories and everything around them: LanceDB
/// ([`crate::LanceStore`]) by default, SQLite ([`crate::SqliteStore`]) as an
/// alternative. The agent only ever sees `MemoryManager`, so this can be replaced.
#[async_trait::async_trait]
pub trait MemoryStore: Send + Sync {
    /// `lance` or `sqlite`, for display.
    fn backend(&self) -> &'static str;

    // ---- memories
    async fn create(&self, memory: &Memory) -> Result<()>;
    async fn get(&self, id: Uuid) -> Result<Option<Memory>>;
    /// Overwrite a memory's content, tags, trust, status and dates.
    async fn update(&self, memory: &Memory) -> Result<()>;
    /// Remove a memory for good, with its index entry, vectors, versions,
    /// relationships, usage and episode. Audit events are kept.
    async fn delete(&self, id: Uuid) -> Result<()>;
    /// Keyword search over memories of any status, best first, with a
    /// relevance score (higher is better). `scope` narrows to one scope.
    async fn search(&self, query: &str, scope: Option<&str>, limit: usize) -> Result<Vec<(Memory, f32)>>;
    /// Newest first.
    async fn list(&self, filter: &Filter, limit: usize) -> Result<Vec<Memory>>;
    /// Active memories per scope, largest first.
    async fn scopes(&self) -> Result<Vec<(String, u64)>>;

    // ---- vectors
    async fn set_embedding(&self, id: Uuid, model: &str, vector: &[f32]) -> Result<()>;
    async fn embeddings(&self, model: &str) -> Result<Vec<(Uuid, Vec<f32>)>>;
    /// Active memories without a current vector from `model`.
    async fn missing_embeddings(&self, model: &str, limit: usize) -> Result<Vec<Memory>>;
    /// The memories whose `model` vectors are nearest to `vector`, with cosine
    /// similarity, best first (L12). Exact search unless the store has an index.
    async fn nearest(&self, model: &str, vector: &[f32], limit: usize) -> Result<Vec<(Uuid, f32)>> {
        let mut all: Vec<(Uuid, f32)> =
            self.embeddings(model).await?.into_iter().map(|(id, v)| (id, crate::rank::cosine(vector, &v))).collect();
        all.sort_by(|a, b| b.1.total_cmp(&a.1));
        all.truncate(limit);
        Ok(all)
    }
    /// These memories' `model` vectors.
    async fn embeddings_of(&self, model: &str, ids: &[Uuid]) -> Result<Vec<(Uuid, Vec<f32>)>> {
        Ok(self.embeddings(model).await?.into_iter().filter(|(id, _)| ids.contains(id)).collect())
    }
    /// Get ready for vectors from `model` of `dimensions` (L21): when they no
    /// longer match what's stored, old vectors stop being used and every
    /// memory is due for re-embedding. Returns a note when that happened.
    async fn prepare_embeddings(&self, _model: &str, _dimensions: usize) -> Result<Option<String>> {
        Ok(None)
    }

    // ---- upkeep
    /// Compact storage and build indexes once they pay off (L15). Returns notes.
    async fn maintain(&self, _vector_index_threshold: usize) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
    /// Bytes on disk, if known.
    async fn size_bytes(&self) -> Result<Option<u64>> {
        Ok(None)
    }
    /// Copy everything to `dest`, a new directory (L19).
    async fn backup(&self, _dest: &std::path::Path) -> Result<()> {
        anyhow::bail!("this memory store can't be backed up from lyra; copy its file instead")
    }

    // ---- history
    /// Record a version; returns its number (1, 2, ...).
    async fn add_version(&self, id: Uuid, content: &str, confidence: f32, reason: &str) -> Result<i64>;
    /// Newest first.
    async fn versions(&self, id: Uuid) -> Result<Vec<Version>>;
    async fn relate(&self, from: Uuid, to: Uuid, relationship: Relationship, reason: &str) -> Result<()>;
    /// `(from, to, relationship, reason)` touching `id`, or all of them.
    async fn relationships(&self, id: Option<Uuid>) -> Result<Vec<(Uuid, Uuid, Relationship, String)>>;
    async fn record(&self, event: &Event) -> Result<()>;
    /// Newest first.
    async fn events(&self, id: Option<Uuid>, limit: usize) -> Result<Vec<Event>>;
    async fn last_event(&self, kind: &str) -> Result<Option<Event>>;

    // ---- usage
    async fn record_usage(&self, run: Uuid, ids: &[Uuid], injected: bool) -> Result<()>;
    /// Mark a run's memories helpful or not; returns the memories affected.
    async fn set_helpful(&self, run: Uuid, helpful: bool) -> Result<Vec<Uuid>>;
    async fn usage(&self) -> Result<HashMap<Uuid, Usage>>;

    // ---- episodes
    async fn add_episode(&self, episode: &Episode) -> Result<()>;
    /// Newest first.
    async fn episodes(&self, limit: usize) -> Result<Vec<Episode>>;

    // ---- proposals
    async fn add_proposal(&self, proposal: &Proposal) -> Result<()>;
    async fn pending_proposals(&self) -> Result<Vec<Proposal>>;
    async fn set_proposal_status(&self, id: Uuid, status: ProposalStatus) -> Result<()>;
}

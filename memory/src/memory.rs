use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One remembered fact. The only memory type in V1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub id: Uuid,
    /// Keeps memories apart: `user`, `agent`, `project:arcella`, `conversation:abc123`, ...
    pub scope: String,
    pub content: String,
    pub tags: Vec<String>,
    /// Where it came from: `conversation`, `user`, `tool`, `document`, `agent`, `system`.
    pub source: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

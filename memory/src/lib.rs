//! The agent's memory: what it knows (facts), what happened (episodes) and
//! what it's working on (working memory). Procedures are skills, not memories.
//!
//! - [`MemoryManager`] is the only way in. The model proposes memories (via
//!   tools and the [`capture`] review); the manager owns policy: [`safety`],
//!   scopes, deduplication, superseding, ranking ([`rank`]), the context
//!   compiler and maintenance ([`curator`]).
//! - [`SqliteStore`] (SQLite + FTS5, vectors in a table) is the one
//!   [`MemoryStore`] behind it; it can be replaced without touching the agent.
//! - [`WorkingMemory`] is short-term state that's never persisted.

pub mod capture;
pub mod curator;
pub mod embedding;
mod error;
mod lance;
pub mod metrics;
mod manager;
mod memory;
pub mod rank;
pub mod relate;
pub mod safety;
mod sqlite;
pub mod store;
pub mod text;
mod working;

pub use manager::{
    Budget, CaptureMode, Compiled, Inspection, MaintenanceMode, MemoryManager, Recalled, Reembedded, Remembered,
    Report, Settings, approx_tokens, personal, visible,
};
pub use memory::{
    Episode, Memory, MemoryKind, MemorySource, MemoryStatus, NewMemory, Provenance, Relationship, Usage,
};
pub use embedding::EmbeddingProvider;
pub use error::MemoryStoreError;
pub use lance::{LanceStore, MEMORY_SCHEMA_VERSION, restore};
pub use sqlite::SqliteStore;
pub use store::{Filter, MemoryStore};
pub use uuid::Uuid;
pub use working::WorkingMemory;

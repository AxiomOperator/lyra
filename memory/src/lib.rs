//! Persistent memory for the agent (V1): a notebook of facts in SQLite with
//! FTS5 keyword search, behind four operations: remember, recall, forget, list.
//!
//! The agent talks to [`MemoryManager`] only; [`SqliteStore`] is one
//! [`MemoryStore`] behind it and can be swapped without touching the agent.

mod memory;
mod sqlite;
mod store;

use std::path::Path;

use anyhow::Result;

pub use memory::Memory;
pub use uuid::Uuid;
pub use sqlite::SqliteStore;
pub use store::{ALL_SCOPES, MemoryStore};

pub struct MemoryManager<S: MemoryStore = SqliteStore> {
    store: S,
}

impl MemoryManager<SqliteStore> {
    /// Open the SQLite-backed memory at `path`, creating it if needed.
    pub async fn open(path: &Path) -> Result<Self> {
        Ok(Self::new(SqliteStore::open(path).await?))
    }
}

impl<S: MemoryStore> MemoryManager<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    pub async fn remember(
        &self,
        scope: &str,
        content: &str,
        tags: &[String],
        source: Option<&str>,
    ) -> Result<Memory> {
        self.store.remember(scope, content, tags, source).await
    }

    pub async fn recall(&self, scope: &str, query: &str, limit: usize) -> Result<Vec<Memory>> {
        self.store.recall(scope, query, limit).await
    }

    pub async fn forget(&self, id: Uuid) -> Result<()> {
        self.store.forget(id).await
    }

    pub async fn list(&self, scope: &str, limit: usize) -> Result<Vec<Memory>> {
        self.store.list(scope, limit).await
    }

    /// Number of memories per scope, largest first.
    pub async fn scopes(&self) -> Result<Vec<(String, u64)>> {
        self.store.scopes().await
    }
}

use anyhow::Result;
use uuid::Uuid;

use crate::Memory;

/// Scope value that matches every scope in `recall` and `list`.
pub const ALL_SCOPES: &str = "*";

/// Storage backend for memories. SQLite today; anything later.
#[async_trait::async_trait]
pub trait MemoryStore: Send + Sync {
    async fn remember(
        &self,
        scope: &str,
        content: &str,
        tags: &[String],
        source: Option<&str>,
    ) -> Result<Memory>;

    /// Keyword search within `scope` (or [`ALL_SCOPES`]), best matches first.
    async fn recall(&self, scope: &str, query: &str, limit: usize) -> Result<Vec<Memory>>;

    /// Errors if no memory has this id.
    async fn forget(&self, id: Uuid) -> Result<()>;

    /// Most recent first, within `scope` (or [`ALL_SCOPES`]).
    async fn list(&self, scope: &str, limit: usize) -> Result<Vec<Memory>>;

    /// Number of memories per scope, largest first. For display, not the agent.
    async fn scopes(&self) -> Result<Vec<(String, u64)>>;
}

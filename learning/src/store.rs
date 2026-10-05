use anyhow::Result;
use uuid::Uuid;

use crate::{Skill, SkillStatus};

/// Storage for skill content and status: Markdown files today. Usage numbers
/// live in the ledger; policy (what to use, when to change) lives in the manager.
#[async_trait::async_trait]
pub trait SkillStore: Send + Sync {
    /// Save a new skill. Errors if the name is taken.
    async fn create(&self, skill: Skill) -> Result<Skill>;

    async fn get(&self, id: Uuid) -> Result<Option<Skill>>;

    /// Skills of any status matching `query`, with their keyword relevance, best first.
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<(Skill, f32)>>;

    /// Newest first, optionally only one status.
    async fn list(&self, status: Option<SkillStatus>) -> Result<Vec<Skill>>;

    /// Overwrite an existing skill (matched by id).
    async fn save(&self, skill: &Skill) -> Result<()>;

    /// Errors if no skill has this id.
    async fn delete(&self, id: Uuid) -> Result<()>;
}

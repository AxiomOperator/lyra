use anyhow::Result;
use uuid::Uuid;

use crate::{Skill, SkillStatus};

/// Storage backend for skills: Markdown files today.
#[async_trait::async_trait]
pub trait SkillStore: Send + Sync {
    /// Save a skill as given (status included). Errors if the name is taken.
    async fn learn(&self, skill: Skill) -> Result<Skill>;

    /// Keyword search over *active* skills, best matches first.
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<Skill>>;

    /// Replace a skill's instructions.
    async fn update(&self, id: Uuid, instructions: &str) -> Result<()>;

    /// Delete a skill. Errors if no skill has this id.
    async fn forget(&self, id: Uuid) -> Result<()>;

    /// Newest first, optionally only one status.
    async fn list(&self, status: Option<SkillStatus>, limit: usize) -> Result<Vec<Skill>>;

    /// Approve or reject. Errors if no skill has this id.
    async fn set_status(&self, id: Uuid, status: SkillStatus) -> Result<()>;
}

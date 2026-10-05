//! Self-learning (V1): reusable procedures ("skills") the agent picks up from
//! corrections and successful multi-step work, kept apart from memory (facts).
//!
//! Skills are proposed by an [`evaluator`] review, approved by a person, and
//! then retrieved by keyword search to guide later tasks. The agent talks to
//! [`LearningManager`]; [`SqliteSkillStore`] is the backend behind it.

pub mod evaluator;
mod manager;
mod skill;
mod sqlite;
mod store;

pub use manager::{Learned, LearningManager, Mode};
pub use skill::{Skill, SkillStatus};
pub use sqlite::SqliteSkillStore;
pub use store::SkillStore;
pub use uuid::Uuid;

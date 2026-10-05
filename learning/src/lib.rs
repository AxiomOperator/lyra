//! Self-learning skills: reusable procedures the agent picks up from
//! corrections and successful work, kept apart from memory (facts).
//!
//! - **Store** ([`FileSkillStore`]): one Markdown file per skill, the
//!   canonical content and status, editable by people.
//! - **Ledger** ([`ledger::Ledger`], SQLite): versions, usage and outcomes,
//!   relationships, proposals and the audit log.
//! - **Manager** ([`SkillManager`]): the only way in. The model proposes
//!   (via the [`evaluator`] and [`curator`] prompts); the manager owns policy
//!   ([`scoring`], [`lifecycle`]) and persistence.

pub mod curator;
pub mod evaluator;
mod files;
pub mod ledger;
pub mod lifecycle;
mod manager;
pub mod proposal;
pub mod scoring;
mod skill;
mod store;

pub use files::FileSkillStore;
pub use manager::{Applied, Mode, Ranked, Report, Settings, SkillManager};
pub use skill::{Relationship, Skill, SkillOutcome, SkillStatus, Usage};
pub use store::SkillStore;
pub use uuid::Uuid;

//! Long-lived goals (docs/done/goal_manager.md): what the agent is trying to
//! accomplish over time, what to work on next, and when to resume.
//!
//! A [`Goal`] (G1) lives for days or weeks and is persisted (G2), broken into
//! subgoals (G3) and worked toward by plans, each one attempt (G4). Progress
//! is tracked with a summary (G5); blockers and dependencies stop it from
//! being retried pointlessly (G6). [`GoalManager`] ranks goals (G7), picks
//! what to resume (G8), and wakes goals by schedule and condition (G9, G10).
//! The autonomy policy (G11) bounds what may happen without the user, and
//! [`prompts`] supports decomposition and reviews (G12).

mod manager;
pub mod model;
pub mod prompts;
mod store;

pub use manager::{GoalManager, PlanResult, Settings, parse_duration, parse_when};
pub use model::*;
pub use store::GoalStore;
pub use uuid::Uuid;

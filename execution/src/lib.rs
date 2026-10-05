//! Planning and execution: turn a request into a goal with success
//! criteria, plan it as a dependency graph of steps, and run it: verifying
//! each step, retrying transient failures, replanning only the broken part,
//! pausing for approval and budgets, checkpointing, and resuming safely
//! after a restart. Finally the goal itself is judged.
//!
//! [`Engine`] is the entry point; the application supplies a [`Runtime`]
//! (model calls, tools, memory and skills context). Plans persist in SQLite.

pub mod budget;
mod engine;
pub mod graph;
pub mod model;
pub mod planner;
pub mod retry;
mod store;

pub use engine::{Engine, Metrics, Reasoned, RunOutcome, Runtime, Settings, Task, short};
pub use model::*;
pub use planner::{AgentInfo, Evaluation, PlanningContext, Risk, ToolInfo};
pub use store::PlanStore;
pub use uuid::Uuid;

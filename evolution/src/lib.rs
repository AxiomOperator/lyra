//! Self-evolution: the agent improves how it works, progressively and
//! reversibly, from evidence about its own runs.
//!
//! - **Observe** (E1): every chat turn and plan run is recorded ([`RunRecord`]).
//! - **Review** ([`detect`]): deterministic detectors find problems with evidence.
//! - **Evolve** ([`evolver`]): a separate model role proposes competing,
//!   structured candidates: response guidelines and behavior settings
//!   ([`Behavior`]), workflows ([`WorkflowDef`]), composite tools
//!   ([`CompositeTool`]), skill refinements, and (only with a configured source
//!   repository) code patches tried in a sandbox ([`lab`]).
//! - **Validate and select** ([`fitness`]): candidates are benchmarked against
//!   the baseline and the best one wins.
//! - **Deploy, monitor, roll back** ([`EvolutionManager`]): every change is a
//!   new generation with a full snapshot; regressions roll back.

pub mod behavior;
pub mod composite;
pub mod detect;
pub mod evolver;
pub mod fitness;
pub mod lab;
mod manager;
pub mod model;
mod store;
pub mod workflow;

pub use behavior::Behavior;
pub use composite::CompositeTool;
pub use manager::{EvolutionManager, Mode, Settings, Stats, success_rate};
pub use model::*;
pub use store::EvolutionStore;
pub use uuid::Uuid;
pub use workflow::WorkflowDef;

/// The memory system's credential scanner, reused for guidelines and skills.
pub fn safety_scan(text: &str) -> Option<&'static str> {
    lyra_memory::safety::scan(text)
}

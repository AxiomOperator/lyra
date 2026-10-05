//! Subagents (docs/done/sub_agents.md): user-defined specialists the main agent
//! delegates to. The main agent owns the conversation; subagents own
//! specialties.
//!
//! - [`AgentProfile`] (A1) describes every agent: role, instructions, tools,
//!   skills, model, memory and permission policies, and when to delegate.
//! - [`AgentRegistry`] (A2, A13) keeps profiles as files with every version,
//!   and logs delegations and how they went.
//! - [`builder`] (A3, A4) is the creation wizard, from [`templates`].
//! - [`router`] (A6, A7) picks an agent: rules, then meaning, then the model.
//! - [`delegation`] (A5, A8, A9) is the contract: scoped context in,
//!   structured result out, permissions enforced by the runtime.

pub mod builder;
pub mod delegation;
pub mod model;
mod registry;
pub mod router;
pub mod templates;

pub use model::*;
pub use registry::{AgentRegistry, AgentStats, DelegationRecord, validate};

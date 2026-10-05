//! Capability intelligence (docs/capabilities.md): what the agent can do
//! right now, which capability fits, and whether it's allowed.
//!
//! - [`Capability`] (C1) describes anything the agent can do: native and
//!   composite tools, OpenAPI operations ([`openapi`]), MCP tools ([`mcp`]),
//!   workflows, learned skills and subagents, with risk, permissions,
//!   metadata, requirements and verification.
//! - [`CapabilityManager`] (C2) holds the registry, finds the capabilities
//!   a goal needs (full-text and semantic search in LanceDB, C3/C9), scores
//!   them by relevance and track record (C6), applies the permission
//!   [`Policy`] (C4), records usage (C5) and tracks health (C8).

pub mod index;
mod manager;
pub mod mcp;
pub mod model;
pub mod openapi;
pub mod policy;
pub mod store;

pub use manager::{CapabilityManager, Score, Scored, Settings, Weights};
pub use model::*;
pub use policy::{Policy, Rule};

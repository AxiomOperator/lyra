//! Permissions as policy (C4): what each risk level may do on its own,
//! enforced in Rust before anything runs.

use std::collections::HashMap;

use serde::Deserialize;

use crate::model::{Capability, RiskLevel};

/// What happens when a capability is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rule {
    /// Runs.
    Auto,
    /// Runs once someone approved it (a plan step's approval, or `/caps allow`).
    Approval,
    /// Never runs; not offered to the model at all.
    Deny,
}

impl Rule {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Approval => "approval",
            Self::Deny => "deny",
        }
    }
}

/// `[capabilities.policy]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Policy {
    pub read_only: Rule,
    pub low_write: Rule,
    pub write: Rule,
    pub destructive: Rule,
    pub privileged: Rule,
    /// Per capability id (or `source.*`), overriding its risk level's rule.
    pub overrides: HashMap<String, Rule>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            read_only: Rule::Auto,
            low_write: Rule::Auto,
            write: Rule::Auto,
            destructive: Rule::Approval,
            privileged: Rule::Deny,
            overrides: HashMap::new(),
        }
    }
}

impl Policy {
    pub fn decide(&self, c: &Capability) -> Rule {
        if !c.enabled {
            return Rule::Deny;
        }
        let by_source = format!("{}.*", c.source);
        if let Some(rule) = self.overrides.get(&c.id).or_else(|| self.overrides.get(&c.name)).or_else(|| self.overrides.get(&by_source)) {
            return *rule;
        }
        let rule = match c.risk {
            RiskLevel::ReadOnly => self.read_only,
            RiskLevel::LowWrite => self.low_write,
            RiskLevel::Write => self.write,
            RiskLevel::Destructive => self.destructive,
            RiskLevel::Privileged => self.privileged,
        };
        // A capability that asks for approval gets it, at least.
        if c.metadata.requires_approval && rule == Rule::Auto { Rule::Approval } else { rule }
    }
}

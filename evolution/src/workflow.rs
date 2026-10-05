//! Workflows as data (E4): named phase lists the planner follows for matching
//! requests. Evolution edits these definitions, not Rust code.

use serde::{Deserialize, Serialize};

/// `~/.lyra/workflows/<name>.toml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowDef {
    pub name: String,
    pub description: String,
    /// Words in a request that make this workflow apply.
    #[serde(default)]
    pub triggers: Vec<String>,
    pub phases: Vec<Phase>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Phase {
    pub name: String,
    /// What this phase should achieve, for the planner.
    pub instruction: String,
}

impl WorkflowDef {
    pub fn parse(text: &str) -> Result<Self, String> {
        let w: WorkflowDef = toml::from_str(text).map_err(|e| format!("workflow: {e}"))?;
        w.validate()?;
        Ok(w)
    }

    pub fn render(&self) -> String {
        toml::to_string_pretty(self).unwrap_or_default()
    }

    pub fn validate(&self) -> Result<(), String> {
        if !valid_name(&self.name) {
            return Err(format!("bad workflow name {:?}: use lowercase letters, digits and dashes", self.name));
        }
        if self.phases.is_empty() || self.phases.len() > 12 {
            return Err("a workflow needs 1–12 phases".into());
        }
        if self.triggers.is_empty() {
            return Err("a workflow needs trigger words".into());
        }
        let mut seen = std::collections::HashSet::new();
        for p in &self.phases {
            if p.name.trim().is_empty() || p.instruction.trim().is_empty() {
                return Err("every phase needs a name and an instruction".into());
            }
            if !seen.insert(p.name.to_lowercase()) {
                return Err(format!("phase {} appears twice", p.name));
            }
        }
        Ok(())
    }

    /// How well a request matches the triggers (share of triggers present).
    pub fn matches(&self, request: &str) -> f32 {
        let lower = request.to_lowercase();
        let hits = self.triggers.iter().filter(|t| lower.contains(&t.to_lowercase())).count();
        hits as f32 / self.triggers.len().max(1) as f32
    }

    /// Guidance for the planner.
    pub fn guidance(&self) -> String {
        let phases: Vec<String> =
            self.phases.iter().enumerate().map(|(i, p)| format!("{}. {}: {}", i + 1, p.name, p.instruction)).collect();
        format!("Follow the \"{}\" workflow ({}); plan steps for these phases in order:\n{}", self.name, self.description, phases.join("\n"))
    }
}

/// The workflow that best matches a request, if any matches at all.
pub fn pick<'a>(workflows: &'a [WorkflowDef], request: &str) -> Option<&'a WorkflowDef> {
    workflows
        .iter()
        .map(|w| (w, w.matches(request)))
        .filter(|(_, score)| *score > 0.0)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(w, _)| w)
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 60
        && !name.starts_with('-')
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    const INFRA: &str = r#"
name = "infrastructure-change"
description = "changes to servers and services"
triggers = ["deploy", "server", "service"]

[[phases]]
name = "gather state"
instruction = "inspect the current state first"

[[phases]]
name = "execute"
instruction = "make the change"

[[phases]]
name = "verify"
instruction = "check it worked"
"#;

    #[test]
    fn parses_matches_and_guides() {
        let w = WorkflowDef::parse(INFRA).unwrap();
        assert!(w.matches("deploy the billing service") > 0.5);
        assert_eq!(w.matches("write a poem"), 0.0);
        assert!(w.guidance().contains("1. gather state"));
        assert_eq!(WorkflowDef::parse(&w.render()).unwrap(), w);
        let all = [w];
        assert!(pick(&all, "restart the server").is_some());
        assert!(pick(&all, "hello").is_none());
    }

    #[test]
    fn rejects_bad_definitions() {
        assert!(WorkflowDef::parse(&INFRA.replace("infrastructure-change", "../escape")).is_err());
        assert!(WorkflowDef::parse(&INFRA.replace("name = \"execute\"", "name = \"verify\"")).is_err());
        assert!(WorkflowDef::parse("name = \"x\"\ndescription = \"d\"\ntriggers = [\"a\"]\nphases = []").is_err());
    }
}

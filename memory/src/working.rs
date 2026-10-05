//! Working memory (M7): short-term state for the current conversation (the
//! goal, a plan, scratch values, the things being talked about). It lives in
//! the process and is never persisted on its own.

use std::collections::{BTreeMap, VecDeque};

const MAX_ENTITIES: usize = 12;
const MAX_TOOLS: usize = 5;

#[derive(Debug, Clone, Default)]
pub struct WorkingMemory {
    pub goal: Option<String>,
    pub plan: Vec<String>,
    /// Scratch values the model chose to keep (`current_server = 10.0.0.5`).
    pub values: BTreeMap<String, String>,
    /// Recently mentioned identifiers, newest first.
    pub entities: VecDeque<String>,
    /// Recent tool results, newest first, one line each (for display).
    pub recent_tools: VecDeque<String>,
}

impl WorkingMemory {
    pub fn is_empty(&self) -> bool {
        self.goal.is_none() && self.plan.is_empty() && self.values.is_empty() && self.entities.is_empty() && self.recent_tools.is_empty()
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Pick out identifiers worth tracking: things with digits and dots or
    /// colons (IPs, ports, versions), paths, `code`, CamelCase and
    /// snake/kebab-case names.
    pub fn note_entities(&mut self, text: &str) {
        let mut found: Vec<String> = Vec::new();
        for part in text.split('`').skip(1).step_by(2) {
            if !part.is_empty() && part.len() <= 60 && !part.contains('\n') {
                found.push(part.to_string());
            }
        }
        for w in text.split_whitespace() {
            let w = w.trim_matches(|c: char| !c.is_alphanumeric() && !"/._:-".contains(c));
            let w = w.trim_end_matches(['.', ':', '-']);
            if w.len() < 3 || w.len() > 60 || w.starts_with("http") {
                continue;
            }
            let digits = w.chars().any(|c| c.is_ascii_digit());
            let networky = digits && (w.contains('.') || w.contains(':'));
            let path = w.contains('/') && w.len() > 3;
            let camel = w.chars().next().is_some_and(char::is_uppercase)
                && w.chars().skip(1).any(char::is_uppercase)
                && w.chars().all(char::is_alphanumeric);
            let snake = (w.contains('_') || w.contains('-')) && w.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') && w.len() >= 5;
            if networky || path || camel || snake {
                found.push(w.to_string());
            }
        }
        for entity in found {
            self.entities.retain(|e| e != &entity);
            self.entities.push_front(entity);
        }
        self.entities.truncate(MAX_ENTITIES);
    }

    pub fn note_tool(&mut self, name: &str, result: &str) {
        let line: String = format!("{name}: {}", result.split_whitespace().collect::<Vec<_>>().join(" "))
            .chars()
            .take(120)
            .collect();
        self.recent_tools.push_front(line);
        self.recent_tools.truncate(MAX_TOOLS);
    }

    /// The system prompt section, or `None` when there's nothing to say.
    pub fn render(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut out = String::from(
            "# Working memory\n\nShort-term notes for this conversation (update them with the \
             working_memory tool; they are not saved).",
        );
        if let Some(goal) = &self.goal {
            out += &format!("\n\nGoal: {goal}");
        }
        if !self.plan.is_empty() {
            out += "\n\nPlan:";
            for (i, step) in self.plan.iter().enumerate() {
                out += &format!("\n{}. {step}", i + 1);
            }
        }
        if !self.values.is_empty() {
            out += "\n\nNotes:";
            for (k, v) in &self.values {
                out += &format!("\n- {k}: {v}");
            }
        }
        if !self.entities.is_empty() {
            out += &format!("\n\nRecently mentioned: {}", self.entities.iter().cloned().collect::<Vec<_>>().join(", "));
        }
        if !self.recent_tools.is_empty() {
            out += "\n\nRecent tool results (newest first):";
            for line in &self.recent_tools {
                out += &format!("\n- {line}");
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_out_identifiers() {
        let mut w = WorkingMemory::default();
        w.note_entities("Point PgBouncer at 10.0.0.5:6432 and edit /etc/pgbouncer/userlist.txt, see `max_client_conn`.");
        let e: Vec<&str> = w.entities.iter().map(String::as_str).collect();
        for expected in ["PgBouncer", "10.0.0.5:6432", "/etc/pgbouncer/userlist.txt", "max_client_conn"] {
            assert!(e.contains(&expected), "{expected} missing from {e:?}");
        }
        assert!(!e.contains(&"edit") && !e.contains(&"Point"));
    }

    #[test]
    fn newest_first_without_duplicates() {
        let mut w = WorkingMemory::default();
        w.note_entities("10.0.0.1:80 then 10.0.0.2:80");
        w.note_entities("back to 10.0.0.1:80");
        assert_eq!(w.entities[0], "10.0.0.1:80");
        assert_eq!(w.entities.len(), 2);
    }

    #[test]
    fn renders_only_when_there_is_something() {
        let mut w = WorkingMemory::default();
        assert!(w.render().is_none());
        w.goal = Some("Fix PgBouncer auth".into());
        w.plan = vec!["check perms".into(), "restart".into()];
        w.values.insert("server".into(), "db1".into());
        let text = w.render().unwrap();
        assert!(text.contains("Goal: Fix PgBouncer auth") && text.contains("2. restart") && text.contains("- server: db1"));
        w.clear();
        assert!(w.render().is_none());
    }
}

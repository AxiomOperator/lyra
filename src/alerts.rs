//! What lyra has told people about and not yet said is over: a machine's
//! problems, a machine gone quiet, an endpoint down. Each is told once when it
//! starts and once when it clears, and the list is kept in
//! `~/.lyra/alerts/<name>.json` so a restart doesn't tell it all again.
//! When to tell (twice down in a row, quiet for a while) stays with each
//! kind of alert (`health.rs`, `status.rs`); this only keeps what was told.

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One thing told: what was said, and since when.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Told {
    pub text: String,
    pub since: DateTime<Utc>,
}

/// What's been told and not cleared, by key.
#[derive(Default)]
pub struct Ledger {
    told: HashMap<String, Told>,
    path: Option<PathBuf>,
    changed: bool,
}

impl Ledger {
    /// `alerts/<name>.json` in lyra's home. `legacy` reads a file saved in an
    /// older shape (before this ledger), so an update doesn't tell everything again.
    pub fn load(name: &str, legacy: impl FnOnce(Value) -> HashMap<String, Told>) -> Ledger {
        let path = crate::config::home().map(|h| h.join("alerts").join(format!("{name}.json")));
        match path {
            Some(p) => Ledger::at(p, legacy),
            None => Ledger::default(),
        }
    }

    /// The same, kept at `path`.
    pub fn at(path: PathBuf, legacy: impl FnOnce(Value) -> HashMap<String, Told>) -> Ledger {
        let raw: Value = crate::store::read_json(&path);
        let told = if raw.is_null() { HashMap::new() } else { serde_json::from_value(raw.clone()).unwrap_or_else(|_| legacy(raw)) };
        Ledger { told, path: Some(path), changed: false }
    }

    /// Tell `key`: true when it's news. Told already, it only takes the newer
    /// text (what's said when it clears), without a save of its own.
    pub fn raise(&mut self, key: &str, text: &str, at: DateTime<Utc>) -> bool {
        if let Some(t) = self.told.get_mut(key) {
            t.text = text.to_string();
            return false;
        }
        self.told.insert(key.to_string(), Told { text: text.to_string(), since: at });
        self.changed = true;
        true
    }

    /// `key` is over: what was told, if it was.
    pub fn clear(&mut self, key: &str) -> Option<Told> {
        let gone = self.told.remove(key);
        self.changed |= gone.is_some();
        gone
    }

    pub fn is_told(&self, key: &str) -> bool {
        self.told.contains_key(key)
    }

    /// What's told under `prefix`, by the rest of its key.
    pub fn under(&self, prefix: &str) -> HashMap<String, Told> {
        self.told.iter().filter_map(|(k, t)| Some((k.strip_prefix(prefix)?.to_string(), t.clone()))).collect()
    }

    /// Forget whatever `keep` says no to (e.g. a machine that was unpaired).
    pub fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        let before = self.told.len();
        self.told.retain(|k, _| keep(k));
        self.changed |= self.told.len() != before;
    }

    /// Save, if anything changed since the last save.
    pub fn save(&mut self) {
        if !std::mem::take(&mut self.changed) {
            return;
        }
        if let Some(p) = &self.path {
            let _ = crate::store::write_json(p, &self.told);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn told_once_cleared_once_and_kept_over_a_restart() {
        let dir = std::env::temp_dir().join(format!("lyra-ledger-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("x.json");
        let now = Utc::now();
        let mut l = Ledger::at(path.clone(), |_| HashMap::new());
        assert!(l.raise("web1/disk", "disk full", now));
        assert!(!l.raise("web1/disk", "disk full", now), "once");
        l.save();
        let mut again = Ledger::at(path.clone(), |_| HashMap::new());
        assert!(again.is_told("web1/disk"), "a restart remembers");
        assert_eq!(again.under("web1/").keys().collect::<Vec<_>>(), vec!["disk"]);
        assert_eq!(again.clear("web1/disk").map(|t| t.text), Some("disk full".into()));
        assert!(again.clear("web1/disk").is_none());
        // An older file is read through `legacy`.
        std::fs::write(&path, r#"{"chat":"2026-10-08T12:00:00Z"}"#).unwrap();
        let old = Ledger::at(path, |v| v.as_object().unwrap().keys().map(|k| (k.clone(), Told { text: String::new(), since: now })).collect());
        assert!(old.is_told("chat"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

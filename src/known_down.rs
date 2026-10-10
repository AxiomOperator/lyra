//! Models an admin has marked "known down" (the Status page): lyra stops
//! trying them until the mark is cleared. The chat model goes straight to the
//! fallback, the decision model is skipped (the chat model decides), the
//! vision model says so at once; their "is down" pushes stop. The checks still
//! run quietly and say when a marked one answers again; only a person clears
//! it. Kept in `status/known_down.json` (it survives restarts).

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::store::JsonStore;

/// The checks that can be marked: the models lyra calls.
pub const MARKABLE: &[(&str, &str)] = &[("chat", "Chat model"), ("fallback", "Fallback chat model"), ("fallback2", "Second fallback model"), ("decide", "Decision model"), ("vision", "Vision model")];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Mark {
    /// Who marked it.
    pub by: String,
    pub at: DateTime<Utc>,
    /// What they know ("GPU box being repaired, back Monday").
    #[serde(default)]
    pub note: String,
}

fn store() -> Option<JsonStore<BTreeMap<String, Mark>>> {
    Some(JsonStore::new(crate::config::home()?.join("status").join("known_down.json")))
}

fn path() -> Option<PathBuf> {
    Some(crate::config::home()?.join("status").join("known_down.json"))
}

/// Every mark now.
pub fn all() -> BTreeMap<String, Mark> {
    if cfg!(test) {
        return BTreeMap::new();
    }
    match path() {
        Some(p) if p.exists() => store().map(|s| s.load()).unwrap_or_default(),
        _ => BTreeMap::new(),
    }
}

/// This model is marked down.
pub fn is_down(id: &str) -> bool {
    all().contains_key(id)
}

pub fn mark(id: &str) -> Option<Mark> {
    all().get(id).cloned()
}

/// Mark one down (`down`), or clear the mark.
pub fn set(id: &str, down: bool, by: &str, note: &str) -> Result<(), String> {
    if !MARKABLE.iter().any(|(m, _)| *m == id) {
        return Err(format!("{id} isn't a model lyra calls"));
    }
    store().ok_or("no lyra home")?.update(|all| {
        if down {
            all.insert(id.to_string(), Mark { by: by.to_string(), at: Utc::now(), note: note.trim().chars().take(200).collect() });
        } else {
            all.remove(id);
        }
    })
}

/// What the Status page shows for a marked check.
pub fn describe(m: &Mark) -> String {
    let when = m.at.with_timezone(&chrono::Local).format("%a %H:%M");
    if m.note.is_empty() { format!("marked known down by {} ({when})", m.by) } else { format!("marked known down by {} ({when}): {}", m.by, m.note) }
}

/// For the Status page: every mark, by check id.
pub fn view() -> Value {
    json!(all().iter().map(|(id, m)| (id.clone(), json!({ "by": m.by, "at": m.at, "note": m.note, "text": describe(m) }))).collect::<serde_json::Map<_, _>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_models_can_be_marked_and_marks_read_well() {
        assert!(set("search", true, "owner", "").unwrap_err().contains("isn't a model"));
        let m = Mark { by: "Garrett".into(), at: Utc::now(), note: "GPU box out until Monday".into() };
        assert!(describe(&m).starts_with("marked known down by Garrett (") && describe(&m).ends_with("GPU box out until Monday"));
        assert!(!is_down("chat"), "nothing marked in tests");
    }
}

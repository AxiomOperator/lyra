//! The test set (E7): tasks lyra's changes are measured on, one TOML file each
//! in `evolution/golden/`: what was asked (with the turns it followed), what a
//! good answer does, and a reference answer someone liked. They come from
//! replies the owner rated 👍 and from an admin's "add to tests"; anyone can
//! edit or delete the files.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoldenTask {
    pub id: String,
    pub task: String,
    /// What a good answer must do.
    #[serde(default)]
    pub expect: String,
    /// An answer someone liked (a good answer needn't match it word for word).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// The conversation's turns before it (role, text), for a follow-up.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<(String, String)>,
    /// "rated" (a 👍) or "added" (an admin's "add to tests").
    #[serde(default)]
    pub source: String,
    pub added: DateTime<Utc>,
}

impl GoldenTask {
    pub fn new(task: &str, reference: Option<&str>, history: Vec<(String, String)>, source: &str) -> Self {
        Self {
            id: Uuid::new_v4().simple().to_string()[..8].to_string(),
            task: task.trim().chars().take(2000).collect(),
            expect: "a correct, complete and helpful answer, at least as good as the reference answer".into(),
            reference: reference.map(|r| r.trim().chars().take(4000).collect()),
            history,
            source: source.into(),
            added: Utc::now(),
        }
    }
}

pub fn dir(home: &Path) -> PathBuf {
    home.join("evolution").join("golden")
}

/// Every test, newest first, with the files that couldn't be read.
pub fn list(home: &Path) -> (Vec<GoldenTask>, Vec<String>) {
    let (mut out, mut errors) = (Vec::new(), Vec::new());
    let Ok(entries) = std::fs::read_dir(dir(home)) else { return (out, errors) };
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().is_some_and(|x| x == "toml") {
            match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| toml::from_str::<GoldenTask>(&t).map_err(|e| e.to_string())) {
                Ok(t) if !t.task.trim().is_empty() => out.push(t),
                Ok(_) => {}
                Err(e) => errors.push(format!("{}: {e}", path.display())),
            }
        }
    }
    out.sort_by_key(|t| std::cmp::Reverse(t.added));
    (out, errors)
}

/// Keep a test (one asked already is left as it is). Returns whether it's new.
pub fn add(home: &Path, t: &GoldenTask) -> std::io::Result<bool> {
    let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    if list(home).0.iter().any(|x| norm(&x.task) == norm(&t.task)) {
        return Ok(false);
    }
    let d = dir(home);
    std::fs::create_dir_all(&d)?;
    let text = toml::to_string_pretty(t).map_err(std::io::Error::other)?;
    let tmp = d.join(format!(".{}.tmp", t.id));
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, d.join(format!("{}.toml", t.id)))?;
    Ok(true)
}

pub fn remove(home: &Path, id: &str) -> std::io::Result<bool> {
    let id = id.trim();
    let matches: Vec<GoldenTask> = list(home).0.into_iter().filter(|t| !id.is_empty() && t.id.starts_with(id)).collect();
    match matches.as_slice() {
        [t] => std::fs::remove_file(dir(home).join(format!("{}.toml", t.id))).map(|_| true),
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tests_are_kept_once_listed_and_removed() {
        let home = std::env::temp_dir().join(format!("lyra-golden-{}", Uuid::new_v4()));
        let t = GoldenTask::new("When is the team meeting?", Some("Tuesday at 10."), vec![("user".into(), "hi".into())], "rated");
        assert!(add(&home, &t).unwrap());
        assert!(!add(&home, &GoldenTask::new("when is the  team meeting?", None, vec![], "added")).unwrap(), "asked already");
        let (all, errors) = list(&home);
        assert_eq!((all.len(), errors.len()), (1, 0));
        assert_eq!(all[0], t);
        std::fs::write(dir(&home).join("broken.toml"), "task = ").unwrap();
        assert_eq!(list(&home).1.len(), 1, "a broken file is reported, not fatal");
        assert!(remove(&home, &t.id[..4]).unwrap());
        assert!(list(&home).0.is_empty());
        let _ = std::fs::remove_dir_all(&home);
    }
}

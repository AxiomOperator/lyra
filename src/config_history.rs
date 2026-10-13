//! config.toml's versions: before every change lyra writes (the Settings
//! page, /settings, a restore, the Machines page's rules), the file as it was
//! and as it became go to `config/history/` (readable by its owner only: the
//! file may hold keys), with who changed it and which settings. A change made
//! by hand is noticed at the next reload or save and recorded as such. The
//! Settings page's History shows each version's diff (keys masked) and can
//! put an earlier version back.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Versions kept (the oldest go).
const KEEP: usize = 200;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub at: DateTime<Utc>,
    /// Who changed it ("" when by hand).
    pub by: String,
    /// What happened: "Settings", "restored …", "edited by hand".
    pub how: String,
    /// The settings that changed.
    pub keys: Vec<String>,
}

fn dir(config: &Path) -> PathBuf {
    config.with_file_name("history")
}

fn index(config: &Path) -> PathBuf {
    dir(config).join("index.json")
}

pub fn list_at(config: &Path) -> Vec<Entry> {
    crate::store::read_json(&index(config))
}

fn write_private(path: &Path, text: &str) -> Result<(), String> {
    crate::store::write_atomic(path, text.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Every setting's path and value, flat ("planner.day_end" → "17:00").
fn flat(text: &str) -> Vec<(String, String)> {
    fn walk(t: &toml::Table, at: &str, out: &mut Vec<(String, String)>) {
        for (k, v) in t {
            let path = if at.is_empty() { k.clone() } else { format!("{at}.{k}") };
            match v {
                toml::Value::Table(inner) => walk(inner, &path, out),
                other => out.push((path, other.to_string())),
            }
        }
    }
    let mut out = Vec::new();
    if let Ok(t) = toml::from_str::<toml::Table>(text) {
        walk(&t, "", &mut out);
    }
    out
}

/// The settings that differ between two versions of the file.
pub fn changed(before: &str, after: &str) -> Vec<String> {
    let (a, b) = (flat(before), flat(after));
    let mut keys: Vec<String> = a.iter().filter(|(k, v)| b.iter().find(|(k2, _)| k2 == k).is_none_or(|(_, v2)| v2 != v)).map(|(k, _)| k.clone()).collect();
    keys.extend(b.iter().filter(|(k, _)| !a.iter().any(|(k2, _)| k2 == k)).map(|(k, _)| k.clone()));
    keys.sort();
    keys.dedup();
    keys
}

/// Keep a change: the file before and after it, who and how.
pub fn record_at(config: &Path, before: &str, after: &str, by: &str, how: &str) -> Result<Option<Entry>, String> {
    if before == after {
        return Ok(None);
    }
    let d = dir(config);
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700));
    }
    let at = Utc::now();
    let mut all = list_at(config);
    // Two changes in the same millisecond: each keeps its own files.
    let base = at.format("%Y%m%d-%H%M%S-%3f").to_string();
    let mut id = base.clone();
    let mut n = 1;
    while all.iter().any(|e| e.id == id) || d.join(format!("{id}.before.toml")).exists() {
        id = format!("{base}-{n}");
        n += 1;
    }
    write_private(&d.join(format!("{id}.before.toml")), before)?;
    write_private(&d.join(format!("{id}.after.toml")), after)?;
    let e = Entry { id, at, by: by.to_string(), how: how.to_string(), keys: changed(before, after) };
    all.push(e.clone());
    while all.len() > KEEP {
        let old = all.remove(0);
        let _ = std::fs::remove_file(d.join(format!("{}.before.toml", old.id)));
        let _ = std::fs::remove_file(d.join(format!("{}.after.toml", old.id)));
    }
    crate::store::write_json(&index(config), &all)?;
    Ok(Some(e))
}

/// The file as the last recorded change left it.
fn last_after(config: &Path) -> Option<String> {
    let last = list_at(config).pop()?;
    std::fs::read_to_string(dir(config).join(format!("{}.after.toml", last.id))).ok()
}

/// Changed by hand since lyra last wrote it? Then that's a version too.
pub fn notice_hand_edit_at(config: &Path) -> Option<Entry> {
    let now = std::fs::read_to_string(config).ok()?;
    let before = last_after(config)?;
    record_at(config, &before, &now, "", "edited by hand").ok().flatten()
}

/// A version's text: the file before or after the change.
fn version_text(config: &Path, id: &str, after: bool) -> Result<String, String> {
    if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err("no such version".into());
    }
    std::fs::read_to_string(dir(config).join(format!("{id}.{}.toml", if after { "after" } else { "before" }))).map_err(|_| format!("version {id} isn't kept any more"))
}

/// Keys' values hidden (api_key = "…", tokens): diffs and listings show that they changed, not what to.
fn masked(text: &str) -> String {
    text.lines()
        .map(|l| match l.split_once('=') {
            Some((k, _)) if crate::config_schema::secret(k.trim().trim_start_matches('#').trim()) && !l.trim_start().starts_with('#') => format!("{}= \"••••••\"", k),
            _ => l.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A unified diff (`-` taken out, `+` put in), with a little context.
pub fn diff(before: &str, after: &str) -> Vec<Value> {
    // Whether the last line ends with a newline isn't a change.
    let end = |t: String| if t.ends_with('\n') { t } else { t + "\n" };
    let (a, b) = (end(masked(before)), end(masked(after)));
    let d = similar::TextDiff::from_lines(&a, &b);
    let mut out = Vec::new();
    for (i, group) in d.grouped_ops(3).iter().enumerate() {
        if i > 0 {
            out.push(json!({ "op": "gap", "text": "…" }));
        }
        for op in group {
            for change in d.iter_changes(op) {
                let tag = match change.tag() {
                    similar::ChangeTag::Delete => "-",
                    similar::ChangeTag::Insert => "+",
                    similar::ChangeTag::Equal => " ",
                };
                out.push(json!({ "op": tag, "line": change.new_index().or(change.old_index()).map(|n| n + 1), "text": change.value().trim_end_matches('\n') }));
            }
        }
    }
    out
}

/// One version's diff: what that change did, or (`against_now`) from then to the file now.
pub fn diff_at(config: &Path, id: &str, against_now: bool) -> Result<Value, String> {
    let before = version_text(config, id, false)?;
    let after = if against_now { std::fs::read_to_string(config).map_err(|e| e.to_string())? } else { version_text(config, id, true)? };
    let lines = diff(&before, &after);
    Ok(json!({ "id": id, "against_now": against_now, "lines": lines, "same": lines.is_empty() }))
}

/// Put the file back as it was before a change (that's recorded too).
pub fn restore_at(config: &Path, id: &str, by: &str) -> Result<Entry, String> {
    let text = version_text(config, id, false)?;
    toml::from_str::<crate::config::Config>(&text).map_err(|e| format!("that version doesn't read any more: {e}"))?;
    let now = std::fs::read_to_string(config).unwrap_or_default();
    let perms = std::fs::metadata(config).ok().map(|m| m.permissions());
    crate::store::write_atomic(config, text.as_bytes())?;
    if let Some(p) = perms {
        let _ = std::fs::set_permissions(config, p);
    }
    let when = list_at(config).into_iter().find(|e| e.id == id).map_or_else(|| id.to_string(), |e| e.at.with_timezone(&chrono::Local).format("%b %-d %H:%M:%S").to_string());
    record_at(config, &now, &text, by, &format!("put back as it was before the change of {when}"))?.ok_or_else(|| "it's the same as the file now: nothing to put back".to_string())
}

/// The History tab: every version, newest first, with who changed it.
pub fn page_at(config: &Path) -> Value {
    let names: std::collections::HashMap<String, String> = crate::config::home().map(|h| lyra_web::Users::open(&h.join("web")).list()).unwrap_or_default().into_iter().map(|u| (u.id, u.name)).collect();
    let mut all = list_at(config);
    all.reverse();
    json!(all.iter().map(|e| json!({
        "id": e.id, "at": e.at, "how": e.how, "keys": e.keys,
        "by": if e.by.is_empty() { "by hand".to_string() } else { names.get(&e.by).cloned().unwrap_or_else(|| if e.by == lyra_web::users::OWNER { "the owner".into() } else { e.by.clone() }) },
    })).collect::<Vec<_>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_change_is_kept_with_its_diff_and_can_be_put_back() {
        let dir = std::env::temp_dir().join(format!("lyra-history-{}", lyra_memory::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("config.toml");
        let v1 = "url = \"http://a/v1\"\nmodel = \"m1\"\n[embedding]\nurl = \"http://e/v1\"\napi_key = \"sk-one\"\nmodel = \"e\"\n";
        let v2 = v1.replace("m1", "m2").replace("sk-one", "sk-two");
        std::fs::write(&config, &v2).unwrap();
        let e = record_at(&config, v1, &v2, "owner", "Settings").unwrap().unwrap();
        assert_eq!(e.keys, ["embedding.api_key", "model"]);
        assert!(record_at(&config, &v2, &v2, "owner", "Settings").unwrap().is_none(), "nothing changed: no version");
        let d = diff_at(&config, &e.id, false).unwrap();
        let text = d.to_string();
        assert!(text.contains("model = \\\"m1\\\"") && text.contains("model = \\\"m2\\\""), "{text}");
        assert!(!text.contains("sk-one") && !text.contains("sk-two"), "keys are masked in diffs");
        // By hand: noticed at the next look.
        let v3 = v2.replace("m2", "m3");
        std::fs::write(&config, &v3).unwrap();
        let hand = notice_hand_edit_at(&config).unwrap();
        assert_eq!((hand.how.as_str(), hand.keys.clone()), ("edited by hand", vec!["model".to_string()]));
        // Back to before the first change.
        let back = restore_at(&config, &e.id, "owner").unwrap();
        assert_eq!(std::fs::read_to_string(&config).unwrap(), v1);
        assert!(back.how.starts_with("put back"));
        assert_eq!(list_at(&config).len(), 3);
        assert!(version_text(&config, "../etc", false).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("history").join(format!("{}.before.toml", e.id))).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "versions may hold keys");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}

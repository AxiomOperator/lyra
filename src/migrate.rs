//! One-time moves into the current layout of the lyra home (`~/.lyra`):
//!
//! 1. From before the lyra home existed (`~/.config/lyra` for config and
//!    context files, `~/.local/share/lyra/data` for the memory database).
//!    Files are copied, so the originals stay as a backup.
//! 2. SOUL/USER/AGENT.md from `~/.lyra/config` into `~/.lyra/context`. These
//!    are moved: both places are lyra's own, and a stale copy would mislead.
//! 3. Documents (`documents/`, and each person's) into their notes folder:
//!    notes and documents are one thing now. Moved, each under a name of its
//!    own, its `# Title` first.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

const CONTEXT_FILES: [&str; 3] = ["SOUL.md", "USER.md", "AGENT.md"];

/// Bring older layouts up to date. Returns notes for the activity log.
pub fn run() -> Result<Vec<String>, String> {
    let Some(home) = crate::config::home() else { return Ok(Vec::new()) };
    let notes = [
        migrate(&home, old_config_dir().as_deref(), old_data_dir().as_deref())?,
        context_out_of_config(&home)?,
        documents_into_notes(&home)?,
    ];
    Ok(notes.into_iter().flatten().collect())
}

/// Move each person's documents into their notes folder.
fn documents_into_notes(home: &Path) -> Result<Option<String>, String> {
    let mut places = vec![(home.join("documents"), home.join("notes"))];
    for person in std::fs::read_dir(home.join("users")).into_iter().flatten().flatten() {
        places.push((person.path().join("documents"), person.path().join("notes")));
    }
    let mut moved = 0;
    for (from, to) in places.into_iter().filter(|(f, _)| f.is_dir()) {
        for e in std::fs::read_dir(&from).map_err(|e| format!("reading {}: {e}", from.display()))?.flatten() {
            let p = e.path();
            if p.extension().is_none_or(|x| x != "md") {
                continue;
            }
            let text = std::fs::read_to_string(&p).map_err(|e| format!("reading {}: {e}", p.display()))?;
            let title = crate::documents::title_of(&text);
            // Its title as a heading first (a note's title), the rest after.
            let text = match text.trim_start().strip_prefix('#') {
                Some(_) => text.trim_start().to_string(),
                None => format!("# {title}\n\n{}", text.trim_start().split_once('\n').map_or("", |(_, rest)| rest).trim_start()),
            };
            std::fs::create_dir_all(&to).map_err(|e| format!("creating {}: {e}", to.display()))?;
            let id = crate::documents::free_id(&to, &title, None);
            crate::store::write_text(&to.join(format!("{id}.md")), &text)?;
            std::fs::remove_file(&p).map_err(|e| format!("moving {}: {e}", p.display()))?;
            moved += 1;
        }
        let _ = std::fs::remove_dir(&from);
    }
    Ok((moved > 0).then(|| format!("moved {moved} document{} into notes", if moved == 1 { "" } else { "s" })))
}

/// Move the global SOUL/USER/AGENT.md from `config/` to `context/`, unless a
/// file is already there.
fn context_out_of_config(home: &Path) -> Result<Option<String>, String> {
    let (from, to) = (home.join("config"), home.join("context"));
    let mut moved = Vec::new();
    for name in CONTEXT_FILES {
        let (src, dest) = (from.join(name), to.join(name));
        if !src.is_file() || dest.exists() {
            continue;
        }
        std::fs::create_dir_all(&to).map_err(|e| format!("creating {}: {e}", to.display()))?;
        std::fs::rename(&src, &dest).map_err(|e| format!("moving {}: {e}", src.display()))?;
        moved.push(name);
    }
    Ok((!moved.is_empty()).then(|| {
        format!("moved {} into {}", moved.join(", "), crate::context::show(&to))
    }))
}

/// Where config lived before: `$XDG_CONFIG_HOME/lyra` or `~/.config/lyra`.
fn old_config_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("lyra"))
}

/// Where databases lived before: `$XDG_DATA_HOME/lyra/data` or `~/.local/share/lyra/data`.
fn old_data_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from).or_else(|| {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share"))
    })?;
    Some(base.join("lyra").join("data"))
}

fn migrate(
    home: &Path,
    old_config: Option<&Path>,
    old_data: Option<&Path>,
) -> Result<Option<String>, String> {
    if home.exists() {
        return Ok(None);
    }
    // (source, destination relative to home)
    let mut plan: Vec<(PathBuf, PathBuf)> = Vec::new();
    if let Some(dir) = old_config {
        plan.push((dir.join("config.toml"), Path::new("config").join("config.toml")));
        for name in CONTEXT_FILES {
            plan.push((dir.join(name), Path::new("context").join(name)));
        }
    }
    if let Some(dir) = old_data {
        // A SQLite database is the file plus its write-ahead log and shared memory.
        // (Skills are files now; an old skills.db isn't carried over.)
        for suffix in ["", "-wal", "-shm"] {
            let name = format!("memory.db{suffix}");
            plan.push((dir.join(&name), Path::new("memory").join(name)));
        }
    }
    plan.retain(|(src, _)| src.is_file());
    if plan.is_empty() {
        return Ok(None);
    }

    // Copy into a staging dir and rename at the end, so an interrupted
    // migration never looks like a finished one.
    let mut staging = OsString::from(home.as_os_str());
    staging.push(".migrating");
    let staging = PathBuf::from(staging);
    let fail = |e: std::io::Error, what: &Path| format!("migrating {}: {e}", what.display());
    if staging.exists() {
        std::fs::remove_dir_all(&staging).map_err(|e| fail(e, &staging))?;
    }
    for (src, rel) in &plan {
        let dest = staging.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| fail(e, parent))?;
        }
        std::fs::copy(src, &dest).map_err(|e| fail(e, src))?;
    }
    std::fs::rename(&staging, home).map_err(|e| fail(e, home))?;

    let sources: Vec<String> = [old_config, old_data]
        .into_iter()
        .flatten()
        .filter(|dir| plan.iter().any(|(src, _)| src.starts_with(dir)))
        .map(crate::context::show)
        .collect();
    Ok(Some(format!(
        "migrated {} files from {} to {} (originals left in place)",
        plan.len(),
        sources.join(" and "),
        crate::context::show(home)
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lyra-migrate-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn copies_old_layout_into_home() {
        let root = temp("copy");
        let (config, data, home) = (root.join("config"), root.join("data"), root.join(".lyra"));
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(config.join("config.toml"), "model = \"m\"").unwrap();
        std::fs::write(config.join("SOUL.md"), "soul").unwrap();
        std::fs::write(data.join("memory.db"), "db").unwrap();
        std::fs::write(data.join("memory.db-wal"), "wal").unwrap();
        std::fs::write(data.join("skills.db"), "skills").unwrap();

        let note = migrate(&home, Some(&config), Some(&data)).unwrap().unwrap();
        assert!(note.contains("migrated 4 files"), "{note}");
        let read = |p: &str| std::fs::read_to_string(home.join(p)).unwrap();
        assert_eq!(read("config/config.toml"), "model = \"m\"");
        assert_eq!(read("context/SOUL.md"), "soul");
        assert_eq!(read("memory/memory.db"), "db");
        assert_eq!(read("memory/memory.db-wal"), "wal");
        assert!(!home.join("skills").exists(), "skills.db is not carried over");
        assert!(config.join("SOUL.md").exists(), "originals are kept");
        assert!(!root.join(".lyra.migrating").exists());

        // Second run: home exists, nothing happens.
        assert!(migrate(&home, Some(&config), Some(&data)).unwrap().is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn context_files_move_out_of_config() {
        let home = temp("context");
        std::fs::create_dir_all(home.join("config")).unwrap();
        std::fs::create_dir_all(home.join("context")).unwrap();
        for (name, text) in [("SOUL.md", "soul"), ("USER.md", "old user"), ("config.toml", "x")] {
            std::fs::write(home.join("config").join(name), text).unwrap();
        }
        // Already in context/: left alone, and the config/ copy isn't moved over it.
        std::fs::write(home.join("context/USER.md"), "new user").unwrap();

        let note = context_out_of_config(&home).unwrap().unwrap();
        assert_eq!(note, format!("moved SOUL.md into {}", crate::context::show(&home.join("context"))));
        assert_eq!(std::fs::read_to_string(home.join("context/SOUL.md")).unwrap(), "soul");
        assert!(!home.join("config/SOUL.md").exists());
        assert_eq!(std::fs::read_to_string(home.join("context/USER.md")).unwrap(), "new user");
        assert!(home.join("config/config.toml").exists());
        assert!(context_out_of_config(&home).unwrap().is_none(), "nothing left to move");
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn fresh_install_creates_nothing() {
        let root = temp("fresh");
        let home = root.join(".lyra");
        assert!(migrate(&home, Some(&root.join("none")), Some(&root.join("nada"))).unwrap().is_none());
        assert!(!home.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn leftover_staging_dir_is_replaced() {
        let root = temp("staging");
        let (config, home) = (root.join("config"), root.join(".lyra"));
        std::fs::create_dir_all(&config).unwrap();
        std::fs::write(config.join("USER.md"), "user").unwrap();
        std::fs::create_dir_all(root.join(".lyra.migrating/junk")).unwrap();

        migrate(&home, Some(&config), None).unwrap().unwrap();
        assert_eq!(std::fs::read_to_string(home.join("context/USER.md")).unwrap(), "user");
        assert!(!home.join("junk").exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn documents_become_notes_without_overwriting_one() {
        let home = temp("documents");
        std::fs::create_dir_all(home.join("documents")).unwrap();
        std::fs::create_dir_all(home.join("notes")).unwrap();
        std::fs::create_dir_all(home.join("users/dana/documents")).unwrap();
        std::fs::write(home.join("notes/memo.md"), "# Memo\n\nthe note\n").unwrap();
        std::fs::write(home.join("documents/memo-1a2b.md"), "# Memo\n\nthe document\n").unwrap();
        std::fs::write(home.join("users/dana/documents/x.md"), "Dear all,\nThe office closes early.\n").unwrap();
        let said = documents_into_notes(&home).unwrap();
        assert_eq!(said.as_deref(), Some("moved 2 documents into notes"));
        assert_eq!(std::fs::read_to_string(home.join("notes/memo.md")).unwrap(), "# Memo\n\nthe note\n", "the note stays");
        assert!(std::fs::read_to_string(home.join("notes/memo-2.md")).unwrap().contains("the document"));
        // No heading: its first line is the title.
        assert_eq!(std::fs::read_to_string(home.join("users/dana/notes/dear-all.md")).unwrap(), "# Dear all,\n\nThe office closes early.\n");
        assert!(!home.join("documents").exists() && !home.join("users/dana/documents").exists());
        assert_eq!(documents_into_notes(&home).unwrap(), None, "once");
        let _ = std::fs::remove_dir_all(&home);
    }
}

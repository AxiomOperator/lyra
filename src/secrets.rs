//! Tokens for outside services (`~/.lyra/config/secrets.toml`, root-only,
//! left out of backups): `[pmi] token = "…"`. Never logged, never shown;
//! only whether one is set.

use std::path::{Path, PathBuf};

use toml::Table;

pub fn path() -> Option<PathBuf> {
    Some(crate::config::home()?.join("config").join("secrets.toml"))
}

/// A person's own secrets (`~/.lyra/users/<id>/secrets.toml`); the owner's are the main file.
fn path_for(user: &str) -> Option<PathBuf> {
    if user == lyra_web::users::OWNER { path() } else { Some(crate::context::user_dir(user)?.join("secrets.toml")) }
}

/// One lock for every secrets file: a change reads, edits and writes the
/// whole file, and two at once (Microsoft refreshes from several jobs) lost
/// every other token before.
static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn table_at(path: Option<PathBuf>) -> Table {
    path.and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| t.parse::<Table>().ok()).unwrap_or_default()
}

/// The file as it is, to change: empty only when there's none yet. One that
/// can't be read is an error, never a fresh start (that would drop the rest).
fn table_to_change(path: &Path) -> Result<Table, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => text.parse::<Table>().map_err(|e| format!("{} doesn't read ({e}); not changing it", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Table::new()),
        Err(e) => Err(format!("can't read {}: {e}; not changing it", path.display())),
    }
}

/// Written whole: a private temp file next to it, then renamed over it, so a
/// reader sees the old file or the new one, never half of one.
fn write_whole(path: &Path, text: &str) -> Result<(), String> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.tmp-{}", std::process::id()));
    lyra_node::write_private(&tmp, text)?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("couldn't save {}: {e}", path.display())
    })
}

/// Set (or with an empty token, remove) one service's token in the file at `path`.
fn set_at(path: &Path, service: &str, token: &str) -> Result<(), String> {
    let token = token.trim();
    if token.chars().any(char::is_whitespace) {
        return Err("a token has no spaces in it".into());
    }
    let _held = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = table_to_change(path)?;
    if token.is_empty() {
        t.remove(service);
    } else {
        let mut entry = Table::new();
        entry.insert("token".into(), toml::Value::String(token.into()));
        t.insert(service.into(), toml::Value::Table(entry));
    }
    let text = format!("# Tokens for outside services: readable by this user only, not backed up.\n{}", toml::to_string(&t).map_err(|e| e.to_string())?);
    write_whole(path, &text)
}

/// A service's token (`[service] token`), if one is set.
pub fn token(service: &str) -> Option<String> {
    token_for(service, lyra_web::users::OWNER)
}

/// One person's token for a service (their PMI account).
pub fn token_for(service: &str, user: &str) -> Option<String> {
    table_at(path_for(user)).get(service)?.get("token")?.as_str().map(str::trim).filter(|t| !t.is_empty()).map(str::to_string)
}

/// Set (or with an empty one, remove) a service's token.
pub fn set_token(service: &str, token: &str) -> Result<(), String> {
    set_token_for(service, token, lyra_web::users::OWNER)
}

/// Set (or remove) one person's token for a service.
pub fn set_token_for(service: &str, token: &str, user: &str) -> Result<(), String> {
    set_at(&path_for(user).ok_or("no lyra home")?, service, token)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_at_once_keep_every_token() {
        let dir = std::env::temp_dir().join(format!("lyra-secrets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("secrets.toml");
        set_at(&path, "entra", "app-secret").unwrap();
        set_at(&path, "pmi", "pmi-token").unwrap();
        // Microsoft refreshes from several jobs at once: nothing else is lost.
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for n in 0..25 {
                        set_at(&path, if i % 2 == 0 { "graph" } else { "graph_scope" }, &format!("t{i}-{n}")).unwrap();
                        // A reader in between always sees the whole file.
                        assert!(table_to_change(&path).unwrap().contains_key("entra"));
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let t = table_to_change(&path).unwrap();
        assert!(["entra", "pmi", "graph", "graph_scope"].iter().all(|k| t.contains_key(*k)), "{t:?}");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temp files left");
        // A file that doesn't read is left alone, not replaced by one token.
        std::fs::write(&path, "[pmi\ntoken = ").unwrap();
        assert!(set_at(&path, "graph", "x").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[pmi\ntoken = ");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

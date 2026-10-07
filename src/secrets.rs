//! Tokens for outside services (`~/.lyra/config/secrets.toml`, root-only,
//! left out of backups): `[pmi] token = "…"`. Never logged, never shown;
//! only whether one is set.

use std::path::PathBuf;

use toml::Table;

pub fn path() -> Option<PathBuf> {
    Some(crate::config::home()?.join("config").join("secrets.toml"))
}

fn table() -> Table {
    path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| t.parse::<Table>().ok()).unwrap_or_default()
}

/// A service's token (`[service] token`), if one is set.
pub fn token(service: &str) -> Option<String> {
    table().get(service)?.get("token")?.as_str().map(str::trim).filter(|t| !t.is_empty()).map(str::to_string)
}

/// Set (or with an empty one, remove) a service's token.
pub fn set_token(service: &str, token: &str) -> Result<(), String> {
    let path = path().ok_or("no lyra home")?;
    let mut t = table();
    let token = token.trim();
    if token.is_empty() {
        t.remove(service);
    } else {
        if token.chars().any(char::is_whitespace) {
            return Err("a token has no spaces in it".into());
        }
        let mut entry = Table::new();
        entry.insert("token".into(), toml::Value::String(token.into()));
        t.insert(service.into(), toml::Value::Table(entry));
    }
    let text = format!("# Tokens for outside services: readable by this user only, not backed up.\n{}", toml::to_string(&t).map_err(|e| e.to_string())?);
    lyra_node::write_private(&path, &text)
}

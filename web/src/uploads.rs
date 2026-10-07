//! Files sent from a device (`~/.lyra/uploads/<id>/<name>` + `meta.json`):
//! lyra reads text ones, and the Operator can put any of them on a machine.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Largest file accepted.
pub const MAX_BYTES: usize = 25 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Upload {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub created: chrono::DateTime<chrono::Utc>,
    pub device: String,
    /// Whose it is (only they, an admin or a machine placing it may fetch it).
    #[serde(default)]
    pub user: Option<String>,
}

/// A file name without paths or odd characters.
pub fn clean_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = base.chars().map(|c| if c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ' | '(' | ')') { c } else { '_' }).collect();
    let cleaned = cleaned.trim().trim_start_matches('.').chars().take(120).collect::<String>();
    if cleaned.is_empty() { "file".into() } else { cleaned }
}

pub struct Uploads {
    dir: PathBuf,
}

impl Uploads {
    pub fn new(dir: &Path) -> Self {
        Self { dir: dir.to_path_buf() }
    }

    pub fn save(&self, name: &str, mime: &str, device: &str, user: &str, bytes: &[u8]) -> Result<Upload, String> {
        if bytes.len() > MAX_BYTES {
            return Err(format!("too big: at most {} MB", MAX_BYTES / 1024 / 1024));
        }
        let id = crate::devices::random(12, b"abcdefghijkmnpqrstuvwxyz23456789");
        let up = Upload {
            id: id.clone(),
            name: clean_name(name),
            mime: if mime.is_empty() { "application/octet-stream".into() } else { mime.chars().take(100).collect() },
            size: bytes.len() as u64,
            created: chrono::Utc::now(),
            device: device.to_string(),
            user: Some(user.to_string()),
        };
        let dir = self.dir.join(&id);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(dir.join(&up.name), bytes).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("meta.json"), serde_json::to_string(&up).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        Ok(up)
    }

    /// An upload and where its file is.
    pub fn get(&self, id: &str) -> Option<(Upload, PathBuf)> {
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
            return None;
        }
        let dir = self.dir.join(id);
        let up: Upload = serde_json::from_str(&std::fs::read_to_string(dir.join("meta.json")).ok()?).ok()?;
        let path = dir.join(&up.name);
        path.exists().then_some((up, path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uploads_keep_a_safe_name_and_their_details() {
        let dir = std::env::temp_dir().join(format!("lyra-uploads-{}", std::process::id()));
        let u = Uploads::new(&dir);
        let up = u.save("../../etc/passwd", "text/plain", "phone", "owner", b"hello").unwrap();
        assert_eq!(up.name, "passwd", "no way out of its folder");
        let (back, path) = u.get(&up.id).unwrap();
        assert_eq!((back.size, back.mime.as_str()), (5, "text/plain"));
        assert!(path.starts_with(&dir));
        assert!(u.get("../x").is_none() && u.get("nope").is_none());
        assert_eq!(clean_name(".bashrc"), "bashrc");
        assert_eq!(clean_name("my photo (1).jpg"), "my photo (1).jpg");
        assert!(u.save("big", "", "phone", "owner", &vec![0u8; MAX_BYTES + 1]).is_err());
    }
}

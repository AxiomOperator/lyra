//! Paired devices (`~/.lyra/web/devices.json`) and pairing codes
//! (`~/.lyra/web/pairing.json`). A device pairs once with a short-lived code
//! from `lyra pair` and gets a long random token; only the token's hash is
//! kept. `lyra serve` and `lyra pair` are separate processes, so both go
//! through the files.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::push::Subscription;

fn device_kind() -> String {
    "device".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    /// `device`: a phone or browser (chats, approves). `node`: a machine that
    /// lends lyra its tools (`lyra node`); it can't chat or approve.
    #[serde(default = "device_kind")]
    pub kind: String,
    pub token_hash: String,
    pub created: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    #[serde(default)]
    pub push: Option<Subscription>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Pairing {
    code_hash: String,
    expires: DateTime<Utc>,
    attempts: u32,
}

/// Wrong codes allowed before a pairing code stops working.
const MAX_ATTEMPTS: u32 = 5;

pub fn hash(secret: &str) -> String {
    Sha256::digest(secret.trim().as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn random(len: usize, alphabet: &[u8]) -> String {
    let mut out = String::new();
    while out.len() < len {
        for b in uuid::Uuid::new_v4().into_bytes() {
            // Rejection sampling keeps every character equally likely.
            let limit = 256 - 256 % alphabet.len();
            if (b as usize) < limit && out.len() < len {
                out.push(alphabet[b as usize % alphabet.len()] as char);
            }
        }
    }
    out
}

pub struct Devices {
    dir: PathBuf,
}

/// Write a file only its owner can read, atomically.
fn write_private(path: &Path, text: &str) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("tmp");
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp).map_err(|e| e.to_string())?;
        f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

impl Devices {
    pub fn open(dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        Ok(Self { dir: dir.to_path_buf() })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn list(&self) -> Vec<Device> {
        std::fs::read_to_string(self.dir.join("devices.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    fn save(&self, devices: &[Device]) -> Result<(), String> {
        write_private(&self.dir.join("devices.json"), &serde_json::to_string_pretty(devices).map_err(|e| e.to_string())?)
    }

    /// A new pairing code (valid for `minutes`); replaces any earlier one.
    pub fn new_code(&self, minutes: i64) -> Result<String, String> {
        let code = random(8, b"ABCDEFGHJKMNPQRSTUVWXYZ23456789");
        let p = Pairing { code_hash: hash(&code), expires: Utc::now() + Duration::minutes(minutes), attempts: 0 };
        write_private(&self.dir.join("pairing.json"), &serde_json::to_string(&p).map_err(|e| e.to_string())?)?;
        Ok(code)
    }

    /// Pair a device with a code: returns its token (shown once, never stored).
    pub fn pair(&self, code: &str, name: &str, kind: &str) -> Result<(Device, String), String> {
        if !matches!(kind, "device" | "node") {
            return Err(format!("unknown kind {kind:?}"));
        }
        let path = self.dir.join("pairing.json");
        let mut p: Pairing = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .ok_or("no pairing code is active: run `lyra pair` on the computer")?;
        if Utc::now() > p.expires || p.attempts >= MAX_ATTEMPTS {
            let _ = std::fs::remove_file(&path);
            return Err("that pairing code has expired: run `lyra pair` again".into());
        }
        let code = code.trim().to_uppercase().replace([' ', '-'], "");
        if hash(&code) != p.code_hash {
            p.attempts += 1;
            write_private(&path, &serde_json::to_string(&p).map_err(|e| e.to_string())?)?;
            return Err("wrong pairing code".into());
        }
        let _ = std::fs::remove_file(&path);
        let token = random(43, b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789");
        let name: String = name.trim().chars().take(60).collect();
        let mut all = self.list();
        // A machine's name is how lyra addresses it: one node per name.
        if kind == "node" && all.iter().any(|d| d.kind == "node" && d.name.eq_ignore_ascii_case(&name)) {
            return Err(format!("a machine called {name:?} is already paired (lyra devices remove {name})"));
        }
        let device = Device {
            id: random(10, b"abcdefghijkmnpqrstuvwxyz23456789"),
            name: if name.is_empty() { kind.into() } else { name },
            kind: kind.into(),
            token_hash: hash(&token),
            created: Utc::now(),
            last_seen: Utc::now(),
            push: None,
        };
        all.push(device.clone());
        self.save(&all)?;
        Ok((device, token))
    }

    /// The device a token belongs to (and note that it was seen).
    pub fn authenticate(&self, token: &str) -> Option<Device> {
        if token.trim().len() < 20 {
            return None;
        }
        let h = hash(token);
        let mut all = self.list();
        let d = all.iter_mut().find(|d| d.token_hash == h)?;
        if Utc::now() - d.last_seen > Duration::minutes(5) {
            d.last_seen = Utc::now();
            let found = d.clone();
            let _ = self.save(&all);
            return Some(found);
        }
        Some(d.clone())
    }

    pub fn set_push(&self, id: &str, sub: Option<Subscription>) -> Result<(), String> {
        let mut all = self.list();
        let d = all.iter_mut().find(|d| d.id == id).ok_or("no such device")?;
        d.push = sub;
        self.save(&all)
    }

    pub fn remove(&self, key: &str) -> Result<Device, String> {
        let mut all = self.list();
        let i = all.iter().position(|d| d.id == key || d.name.eq_ignore_ascii_case(key)).ok_or(format!("no device {key:?}"))?;
        let d = all.remove(i);
        self.save(&all)?;
        Ok(d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_gives_a_token_once_and_codes_expire() {
        let dir = std::env::temp_dir().join(format!("lyra-devices-{}", uuid::Uuid::new_v4()));
        let d = Devices::open(&dir).unwrap();
        assert!(d.pair("ABCDEFGH", "phone", "device").unwrap_err().contains("lyra pair"));
        let code = d.new_code(10).unwrap();
        assert_eq!(code.len(), 8);
        assert!(d.pair("WRONGONE", "phone", "device").unwrap_err().contains("wrong"));
        let (device, token) = d.pair(&code.to_lowercase(), "Pixel", "device").unwrap();
        assert_eq!((device.name.as_str(), device.kind.as_str()), ("Pixel", "device"));
        assert!(d.pair(&code, "again", "device").is_err(), "a code works once");
        assert_eq!(d.authenticate(&token).unwrap().id, device.id);
        assert!(d.authenticate("not-a-real-token-at-all-but-long").is_none());
        assert!(!std::fs::read_to_string(dir.join("devices.json")).unwrap().contains(&token), "only the hash is kept");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(dir.join("devices.json")).unwrap().permissions().mode() & 0o777, 0o600);

        let code = d.new_code(10).unwrap();
        for _ in 0..MAX_ATTEMPTS {
            let _ = d.pair("WRONGONE", "x", "device");
        }
        assert!(d.pair(&code, "x", "device").unwrap_err().contains("expired"), "too many wrong guesses");
        let code = d.new_code(10).unwrap();
        let (node, _) = d.pair(&code, "desktop", "node").unwrap();
        assert_eq!(node.kind, "node");
        let code = d.new_code(10).unwrap();
        assert!(d.pair(&code, "Desktop", "node").unwrap_err().contains("already paired"), "one node per name");
        // Devices saved before kinds existed are devices.
        let old: Device = serde_json::from_str(r#"{"id":"a","name":"b","token_hash":"c","created":"2026-01-01T00:00:00Z","last_seen":"2026-01-01T00:00:00Z"}"#).unwrap();
        assert_eq!(old.kind, "device");
        assert_eq!(d.remove("pixel").unwrap().id, device.id);
        assert!(d.authenticate(&token).is_none());
    }
}

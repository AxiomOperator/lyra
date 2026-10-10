//! Files lyra keeps its own state in, written safely. A save goes to a temp
//! file next to the real one and is renamed over it, so a reader never sees a
//! half-written file and a crash never leaves one. Each file has its own lock,
//! so a read-change-write (`update`) can't lose another thread's change. A
//! file that can't be read is set aside (`<name>.bad-<time>`), not
//! overwritten, and the problem is logged.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde::de::DeserializeOwned;

/// One lock per file, for every store in lyra.
static LOCKS: Mutex<Option<HashMap<PathBuf, Arc<Mutex<()>>>>> = Mutex::new(None);

fn lock_for(path: &Path) -> Arc<Mutex<()>> {
    let mut all = LOCKS.lock().unwrap_or_else(|e| e.into_inner());
    all.get_or_insert_with(HashMap::new).entry(path.to_path_buf()).or_default().clone()
}

/// Something went wrong with a file: into lyra's log (the journal under lyra serve).
fn report(what: &str, path: &Path, e: &str) {
    crate::trouble::report(format!("couldn't {what} {}: {e}", path.display()));
}

/// Write `bytes` to `path` atomically (temp file, then rename), making its folder.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = path.parent().ok_or_else(|| format!("{} has no folder", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = dir.join(format!(".{name}.tmp-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    let done = (|| {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if done.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    done.map_err(|e| {
        let e = e.to_string();
        report("save", path, &e);
        e
    })
}

/// Any value as pretty JSON, written atomically (for a store's own lock, see `JsonStore`).
pub fn write_json<V: Serialize + ?Sized>(path: &Path, v: &V) -> Result<(), String> {
    let text = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
    write_atomic(path, text.as_bytes())
}

/// A JSON file's value, or the default when it's missing; a file that
/// doesn't read is moved aside and logged.
pub fn read_json<T: DeserializeOwned + Default>(path: &Path) -> T {
    JsonStore::<T>::new(path).read()
}

/// Change a text file (config.toml, behavior.toml) under its lock: read it
/// (empty when missing), `change` it, write it atomically, keeping its
/// permissions (it may hold keys). Two changes at once can't lose each other.
pub fn update_text(path: &Path, change: impl FnOnce(String) -> Result<String, String>) -> Result<(), String> {
    let lock = lock_for(path);
    let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let out = change(text)?;
    let before = std::fs::metadata(path).ok().map(|m| m.permissions());
    write_atomic(path, out.as_bytes())?;
    if let Some(p) = before {
        let _ = std::fs::set_permissions(path, p);
    }
    Ok(())
}

/// A text file (Markdown, TOML), written atomically.
pub fn write_text(path: &Path, text: &str) -> Result<(), String> {
    write_atomic(path, text.as_bytes())
}

/// A value kept as a JSON file.
pub struct JsonStore<T> {
    path: PathBuf,
    _t: std::marker::PhantomData<fn() -> T>,
}

impl<T> JsonStore<T> {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), _t: std::marker::PhantomData }
    }
}

impl<T: DeserializeOwned + Default> JsonStore<T> {
    fn read(&self) -> T {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return T::default(),
            Err(e) => {
                report("read", &self.path, &e.to_string());
                return T::default();
            }
        };
        match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                let aside = self.path.with_extension(format!("bad-{}", chrono::Utc::now().format("%Y%m%d%H%M%S")));
                report("read", &self.path, &format!("{e} (kept as {})", aside.display()));
                let _ = std::fs::rename(&self.path, &aside);
                T::default()
            }
        }
    }
}

impl<T: Serialize + DeserializeOwned + Default> JsonStore<T> {
    /// What's kept, or the default when there's nothing yet. A file that
    /// doesn't read is moved aside (kept for a look) and the default returned.
    pub fn load(&self) -> T {
        let _guard = lock_for(&self.path);
        let _held = _guard.lock().unwrap_or_else(|e| e.into_inner());
        self.read()
    }

    fn write(&self, v: &T) -> Result<(), String> {
        write_json(&self.path, v)
    }

    /// Read, change and write it back as one step: no other `update` or
    /// `save` of this file comes in between. The change's own result is returned.
    pub fn update<R>(&self, change: impl FnOnce(&mut T) -> R) -> Result<R, String> {
        let lock = lock_for(&self.path);
        let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut v = self.read();
        let out = change(&mut v);
        self.write(&v)?;
        Ok(out)
    }

    /// Like `update`, but the change may refuse (nothing is written then).
    pub fn try_update<R>(&self, change: impl FnOnce(&mut T) -> Result<R, String>) -> Result<R, String> {
        let lock = lock_for(&self.path);
        let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut v = self.read();
        let out = change(&mut v)?;
        self.write(&v)?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::update_text;

    #[test]
    fn text_changes_at_once_lose_nothing_and_keep_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("lyra-update-text-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = std::sync::Arc::new(dir.join("config.toml"));
        std::fs::write(&*path, "").unwrap();
        std::fs::set_permissions(&*path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let threads: Vec<_> = (0..12)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    update_text(&path, |t| {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                        Ok(format!("{t}k{i} = {i}\n"))
                    })
                    .unwrap()
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let text = std::fs::read_to_string(&*path).unwrap();
        assert_eq!(text.lines().count(), 12, "every change kept:\n{text}");
        assert_eq!(std::fs::metadata(&*path).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lyra-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn updates_from_many_threads_are_all_kept() {
        let d = dir("threads");
        let store = Arc::new(JsonStore::<Vec<u32>>::new(d.join("list.json")));
        let handles: Vec<_> = (0..8u32)
            .map(|t| {
                let s = store.clone();
                std::thread::spawn(move || {
                    for i in 0..25 {
                        s.update(|v| v.push(t * 100 + i)).unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(store.load().len(), 200, "no update lost");
        // No temp files left behind.
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_broken_file_is_set_aside_not_overwritten() {
        let d = dir("broken");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("x.json"), "{ not json").unwrap();
        let store = JsonStore::<Vec<u32>>::new(d.join("x.json"));
        assert!(store.load().is_empty());
        let kept: Vec<String> = std::fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert!(kept.iter().any(|n| n.starts_with("x.bad-")), "{kept:?}");
        store.update(|v| v.push(1)).unwrap();
        assert_eq!(store.load(), vec![1]);
        // A refused change writes nothing.
        assert!(store.try_update(|_| Err::<(), _>("no".into())).is_err());
        assert_eq!(store.load(), vec![1]);
        let _ = std::fs::remove_dir_all(&d);
    }
}

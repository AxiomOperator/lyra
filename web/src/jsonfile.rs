//! The web crate's JSON files (devices.json, users.json): read, changed and
//! written safely. One lock per file around a whole read–change–write (so two
//! sign-ins can't lose each other's change), a temp file of its own for each
//! write, flushed before it replaces the real one, and the last good version
//! kept as `.bak`. A file that doesn't read is never taken as "empty": the
//! `.bak` is read instead, and if that fails too, changes are refused rather
//! than saving a near-empty list over it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex};
use std::thread::ThreadId;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// A file's lock: held by one thread at a time, which may take it again.
#[derive(Default)]
struct FileLock {
    held: Mutex<(Option<ThreadId>, usize)>,
    free: Condvar,
}

static LOCKS: LazyLock<Mutex<HashMap<PathBuf, Arc<FileLock>>>> = LazyLock::new(Default::default);

/// While this lives, this thread has the file to itself.
pub struct Held(Arc<FileLock>);

impl Drop for Held {
    fn drop(&mut self) {
        let mut h = self.0.held.lock().unwrap_or_else(|e| e.into_inner());
        h.1 -= 1;
        if h.1 == 0 {
            h.0 = None;
            self.0.free.notify_all();
        }
    }
}

/// Take a file for a read–change–write (again, if this thread has it already).
pub fn hold(path: &Path) -> Held {
    let lock = LOCKS.lock().unwrap_or_else(|e| e.into_inner()).entry(path.to_path_buf()).or_default().clone();
    let me = std::thread::current().id();
    {
        let mut h = lock.held.lock().unwrap_or_else(|e| e.into_inner());
        while h.0.is_some_and(|t| t != me) {
            h = lock.free.wait(h).unwrap_or_else(|e| e.into_inner());
        }
        h.0 = Some(me);
        h.1 += 1;
    }
    Held(lock)
}

fn bak(path: &Path) -> PathBuf {
    let mut p = path.as_os_str().to_owned();
    p.push(".bak");
    PathBuf::from(p)
}

/// The file's contents; nothing yet is the default. A file that doesn't read
/// gives its `.bak`, else an error (never "empty").
pub fn read<T: DeserializeOwned + Default>(path: &Path) -> Result<T, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(e) => return Err(format!("{} can't be read: {e}", path.display())),
    };
    match serde_json::from_str(&text) {
        Ok(v) => Ok(v),
        Err(e) => match std::fs::read_to_string(bak(path)).ok().and_then(|t| serde_json::from_str(&t).ok()) {
            Some(v) => {
                eprintln!("{} doesn't read ({e}): using its last good copy (.bak)", path.display());
                Ok(v)
            }
            None => Err(format!("{} doesn't read ({e}), and there's no good copy: left as it is", path.display())),
        },
    }
}

/// Write it: a temp file of its own, flushed, then put in place (only its
/// owner can read it when `private`). The version it replaces, if it read
/// well, is kept as `.bak`.
/// A file as it was when last looked at: when it was changed, its size and
/// which file it is (a write puts a new one in its place).
type Stamp = (std::time::SystemTime, u64, u64);

fn stamp(path: &Path) -> Option<Stamp> {
    let m = std::fs::metadata(path).ok()?;
    #[cfg(unix)]
    let id = std::os::unix::fs::MetadataExt::ino(&m);
    #[cfg(not(unix))]
    let id = 0;
    Some((m.modified().ok()?, m.len(), id))
}

type Cached = HashMap<(PathBuf, std::any::TypeId), (Stamp, Box<dyn std::any::Any + Send>)>;
static CACHE: LazyLock<Mutex<Cached>> = LazyLock::new(Default::default);

/// `read` the file only when it changed since the last time (files read every
/// few seconds, or on every request, but rarely written); otherwise what it
/// gave then. A failed read isn't kept.
pub fn cached<T: Clone + Send + 'static>(path: &Path, read: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let key = (path.to_path_buf(), std::any::TypeId::of::<T>());
    let Some(now) = stamp(path) else {
        CACHE.lock().unwrap_or_else(|e| e.into_inner()).remove(&key);
        return read();
    };
    if let Some((at, v)) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).get(&key)
        && *at == now
        && let Some(v) = v.downcast_ref::<T>()
    {
        return Ok(v.clone());
    }
    // Stamped before reading: a change in between is read again next time.
    let v = read()?;
    let mut all = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if all.len() > 500 {
        all.clear();
    }
    all.insert(key, (now, Box::new(v.clone())));
    Ok(v)
}

pub fn write<T: Serialize + ?Sized>(path: &Path, value: &T, private: bool) -> Result<(), String> {
    use std::io::Write;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    let tmp = PathBuf::from(tmp);
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create_new(true);
    if private {
        use std::os::unix::fs::OpenOptionsExt;
        open.mode(0o600);
    }
    let result = (|| {
        let mut f = open.open(&tmp).map_err(|e| e.to_string())?;
        f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
        // The one being replaced, kept while it's a good one.
        if std::fs::read_to_string(path).ok().is_some_and(|t| serde_json::from_str::<serde_json::Value>(&t).is_ok()) {
            let _ = std::fs::copy(path, bak(path));
        }
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_read_again_only_once_it_changed() {
        let dir = std::env::temp_dir().join(format!("lyra-jsonfile-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("devices.json");
        write(&file, &vec![1, 2], false).unwrap();
        let reads = std::cell::Cell::new(0);
        let get = || cached(&file, || { reads.set(reads.get() + 1); read::<Vec<i32>>(&file) }).unwrap();
        assert_eq!(get(), vec![1, 2]);
        assert_eq!(get(), vec![1, 2]);
        assert_eq!(reads.get(), 1, "unchanged: not read again");
        // Same size, new contents: still seen.
        write(&file, &vec![3, 4], false).unwrap();
        assert_eq!(get(), vec![3, 4]);
        assert_eq!(reads.get(), 2);
        std::fs::remove_file(&file).unwrap();
        assert_eq!(get(), Vec::<i32>::new(), "gone: the default");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_garbled_file_is_never_read_as_empty() {
        let dir = std::env::temp_dir().join(format!("lyra-jsonfile-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("list.json");
        assert_eq!(read::<Vec<u32>>(&p).unwrap(), Vec::<u32>::new(), "nothing yet");
        write(&p, &vec![1, 2, 3], true).unwrap();
        write(&p, &vec![1, 2, 3, 4], true).unwrap();
        std::fs::write(&p, "[1, 2, ").unwrap();
        assert_eq!(read::<Vec<u32>>(&p).unwrap(), vec![1, 2, 3], "its last good copy");
        std::fs::remove_file(bak(&p)).unwrap();
        assert!(read::<Vec<u32>>(&p).is_err(), "no good copy: an error, not []");
    }

    #[test]
    fn writers_at_once_lose_nothing() {
        let dir = std::env::temp_dir().join(format!("lyra-jsonfile-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = Arc::new(dir.join("list.json"));
        let threads: Vec<_> = (0..16)
            .map(|i| {
                let p = p.clone();
                std::thread::spawn(move || {
                    let _held = hold(&p);
                    let mut all: Vec<u32> = read(&p).unwrap();
                    all.push(i);
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    write(&p, &all, false).unwrap();
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let mut all: Vec<u32> = read(&p).unwrap();
        all.sort();
        assert_eq!(all, (0..16).collect::<Vec<_>>());
        assert!(std::fs::read_dir(&dir).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().contains(".tmp-")), "no temp files left");
    }
}

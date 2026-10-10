//! Backups of lyra itself (`[backup]`): a dated `lyra-<time>.tar.gz` of the
//! whole lyra home (memory, skills, goals, plans, agents, sessions, config,
//! paired devices), made nightly, on `/backup`, or with `lyra backup`; the
//! newest `keep` are kept. SQLite databases are copied with `VACUUM INTO` and
//! the memory store through `MemoryManager::backup`, so a backup made while
//! lyra runs is consistent. `lyra restore` puts one back (lyra stopped),
//! keeping what it replaces.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use serde::Deserialize;

/// `[backup]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    /// Where backups go; empty means `<lyra home>-backups` (e.g. `~/.lyra-backups`).
    /// Point it at another disk or a NAS mount to survive this disk failing.
    pub dir: String,
    /// When the nightly backup runs (local time, `HH:MM`).
    pub at: String,
    /// How many backups to keep.
    pub keep: usize,
    /// Also back up files sent from the app (`uploads/`).
    pub include_uploads: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { enabled: true, dir: String::new(), at: "03:30".into(), keep: 7, include_uploads: false }
    }
}

impl Settings {
    pub fn dir(&self, home: &Path) -> PathBuf {
        if self.dir.trim().is_empty() {
            let name = home.file_name().map_or(".lyra".into(), |n| n.to_string_lossy().to_string());
            home.with_file_name(format!("{name}-backups"))
        } else {
            crate::config::expand_path(self.dir.trim())
        }
    }
}

/// One backup on disk.
#[derive(Debug, Clone, PartialEq)]
pub struct Backup {
    pub path: PathBuf,
    pub name: String,
    pub made: DateTime<Local>,
    pub size: u64,
}

/// Only one backup at a time.
static RUNNING: AtomicBool = AtomicBool::new(false);
/// The newest backup, for the panels (read once at startup, then after each run).
static LAST: std::sync::Mutex<Option<Backup>> = std::sync::Mutex::new(None);

/// The newest backup known.
pub fn last() -> Option<Backup> {
    LAST.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Look at the backup folder again.
pub fn refresh(dir: &Path) {
    *LAST.lock().unwrap_or_else(|e| e.into_inner()) = list(dir).into_iter().next();
}

pub fn running() -> bool {
    RUNNING.load(Ordering::Relaxed)
}

/// The backups in `dir`, newest first.
pub fn list(dir: &Path) -> Vec<Backup> {
    let mut all: Vec<Backup> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let stamp = name.strip_prefix("lyra-")?.strip_suffix(".tar.gz")?;
            let made = Local.from_local_datetime(&NaiveDateTime::parse_from_str(stamp, "%Y%m%d-%H%M%S").ok()?).single()?;
            Some(Backup { path: e.path(), name, made, size: e.metadata().ok()?.len() })
        })
        .collect();
    all.sort_by_key(|b| std::cmp::Reverse(b.made));
    all
}

/// Whether the nightly backup is due: past today's `at` and none since.
pub fn due(settings: &Settings, last: Option<DateTime<Local>>, now: DateTime<Local>) -> bool {
    if !settings.enabled {
        return false;
    }
    let (h, m) = settings.at.split_once(':').and_then(|(h, m)| Some((h.trim().parse::<u32>().ok()?, m.trim().parse::<u32>().ok()?))).unwrap_or((3, 30));
    let Some(today) = now.date_naive().and_hms_opt(h.min(23), m.min(59), 0).and_then(|t| Local.from_local_datetime(&t).single()) else {
        return false;
    };
    now >= today && last.is_none_or(|l| l < today)
}

/// How a file is put in the backup.
enum Copy {
    Skip,
    Sqlite,
    Plain,
}

fn kind(path: &Path) -> Copy {
    let name = path.file_name().map_or(String::new(), |n| n.to_string_lossy().to_string());
    if name == "lyra.pid" || name.ends_with(".lock") || name.ends_with("-wal") || name.ends_with("-shm") || name.ends_with("-journal") || name.ends_with(".tmp") {
        return Copy::Skip;
    }
    let mut head = [0u8; 16];
    let sqlite = std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut head)).is_ok() && &head == b"SQLite format 3\0";
    if sqlite { Copy::Sqlite } else { Copy::Plain }
}

/// A consistent copy of a database that may be in use.
fn copy_sqlite(from: &Path, to: &Path) -> Result<(), String> {
    use sqlx::Connection;
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
    rt.block_on(async {
        let options = sqlx::sqlite::SqliteConnectOptions::new().filename(from).read_only(true);
        let mut c = sqlx::SqliteConnection::connect_with(&options).await.map_err(|e| e.to_string())?;
        sqlx::query("VACUUM INTO ?").bind(to.display().to_string()).execute(&mut c).await.map_err(|e| e.to_string())?;
        c.close().await.map_err(|e| e.to_string())
    })
}

/// Copy `dir` (relative path `rel` inside the home) into `stage`, skipping `skip`.
fn stage_dir(dir: &Path, rel: &Path, stage: &Path, skip: &[PathBuf], notes: &mut Vec<String>) -> Result<usize, String> {
    let mut files = 0;
    std::fs::create_dir_all(stage.join(rel)).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?.flatten() {
        let path = entry.path();
        if skip.contains(&path) {
            continue;
        }
        let rel = rel.join(entry.file_name());
        let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
        if meta.is_dir() {
            files += stage_dir(&path, &rel, stage, skip, notes)?;
        } else if meta.is_file() {
            let to = stage.join(&rel);
            let copied = match kind(&path) {
                Copy::Skip => continue,
                Copy::Sqlite => copy_sqlite(&path, &to).or_else(|e| {
                    notes.push(format!("{}: copied as a file ({e})", rel.display()));
                    std::fs::copy(&path, &to).map(|_| ()).map_err(|e| e.to_string())
                }),
                Copy::Plain => std::fs::copy(&path, &to).map(|_| ()).map_err(|e| e.to_string()),
            };
            match copied {
                Ok(()) => files += 1,
                Err(e) => notes.push(format!("{}: skipped ({e})", rel.display())),
            }
        }
    }
    Ok(files)
}

/// The memory store's path and how to copy it consistently.
pub type Memory<'a> = Option<(&'a Path, &'a dyn Fn(&Path) -> Result<(), String>)>;
/// A finished backup and notes on what couldn't be copied cleanly.
pub type Made = Result<(Backup, Vec<String>), String>;

/// Back up `home` now. `memory` copies the memory store consistently (it's
/// skipped from the plain walk); `memory_path` is where that store lives.
/// Returns the backup and notes on anything that couldn't be copied cleanly.
pub fn run(home: &Path, settings: &Settings, memory: Memory) -> Made {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return Err("a backup is already running".into());
    }
    let result = make(home, settings, memory);
    RUNNING.store(false, Ordering::SeqCst);
    result
}

fn make(home: &Path, settings: &Settings, memory: Memory) -> Made {
    let dir = settings.dir(home);
    if dir.starts_with(home) {
        return Err(format!("[backup] dir {} is inside {}; put it outside", dir.display(), home.display()));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    // What an interrupted one left (one runs at a time, so none of it is live).
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with(".staging-") {
            let _ = std::fs::remove_dir_all(e.path());
        } else if name.ends_with(".partial") {
            let _ = std::fs::remove_file(e.path());
        }
    }
    let stamp = Local::now().format("%Y%m%d-%H%M%S").to_string();
    let stage = dir.join(format!(".staging-{stamp}"));
    let mut notes = Vec::new();
    let mut skip = Vec::new();
    if !settings.include_uploads {
        skip.push(home.join("uploads"));
    }
    if let Some((path, _)) = memory {
        skip.push(path.to_path_buf());
    }
    // Indexes rebuilt on every start (capabilities and agent routing, from the
    // registry and the agent files): a cache, not data. The capabilities one
    // alone made a night's backup 4× bigger (I-12).
    skip.push(home.join("capabilities").join("index"));
    skip.push(home.join("agents").join("index"));
    // Tokens for outside services stay out (everyone's): set them again after a restore.
    skip.push(home.join("config").join("secrets.toml"));
    for person in std::fs::read_dir(home.join("users")).into_iter().flatten().flatten() {
        skip.push(person.path().join("secrets.toml"));
    }
    let staged = (|| {
        let files = stage_dir(home, Path::new(""), &stage, &skip, &mut notes)?;
        if let Some((path, backup)) = memory {
            let rel = path.strip_prefix(home).map_err(|_| format!("the memory store {} isn't inside {}", path.display(), home.display()))?;
            if let Some(parent) = stage.join(rel).parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            backup(&stage.join(rel)).map_err(|e| format!("memory: {e}"))?;
        }
        Ok::<usize, String>(files)
    })();
    let archive = dir.join(format!("lyra-{stamp}.tar.gz"));
    // Written under a name `list` doesn't count, and named a backup only when
    // whole: one cut off part way is never taken for the newest good one.
    let partial = dir.join(format!(".lyra-{stamp}.tar.gz.partial"));
    let packed = staged.and_then(|_| {
        let file = std::fs::File::create(&partial).map_err(|e| e.to_string())?;
        let mut tar = tar::Builder::new(flate2::write::GzEncoder::new(file, flate2::Compression::default()));
        tar.follow_symlinks(false);
        tar.append_dir_all(".", &stage).map_err(|e| e.to_string())?;
        let file = tar.into_inner().and_then(|gz| gz.finish()).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        std::fs::rename(&partial, &archive).map_err(|e| e.to_string())
    });
    let _ = std::fs::remove_dir_all(&stage);
    if let Err(e) = packed {
        let _ = std::fs::remove_file(&partial);
        return Err(e);
    }
    // Only its owner reads it: it holds tokens' hashes, keys and conversations.
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&archive, std::fs::Permissions::from_mode(0o600));
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    for old in list(&dir).into_iter().skip(settings.keep.max(1)) {
        if std::fs::remove_file(&old.path).is_ok() {
            notes.push(format!("removed the old backup {}", old.name));
        }
    }
    refresh(&dir);
    let backup = list(&dir).into_iter().find(|b| b.path == archive).ok_or("the backup vanished")?;
    Ok((backup, notes))
}

/// Put a backup back in place of `home`. lyra must not be running; the
/// current home is kept next to it. Returns where it went.
pub fn restore(home: &Path, archive: &Path) -> Result<String, String> {
    if let Some((pid, mode)) = crate::lock::holder(home) {
        return Err(format!("{mode} is running (pid {pid}); stop it first (systemctl stop lyra)"));
    }
    let file = std::fs::File::open(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    let name = home.file_name().map_or(".lyra".into(), |n| n.to_string_lossy().to_string());
    let stamp = Local::now().format("%Y%m%d-%H%M%S");
    let incoming = home.with_file_name(format!("{name}.restoring-{stamp}"));
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    tar.set_preserve_permissions(true);
    tar.unpack(&incoming).map_err(|e| {
        let _ = std::fs::remove_dir_all(&incoming);
        format!("couldn't unpack {}: {e}", archive.display())
    })?;
    if !incoming.join("config").exists() {
        let _ = std::fs::remove_dir_all(&incoming);
        return Err(format!("{} doesn't look like a lyra backup (no config/)", archive.display()));
    }
    let kept = home.with_file_name(format!("{name}.before-restore-{stamp}"));
    if home.exists() {
        std::fs::rename(home, &kept).map_err(|e| format!("couldn't move {} aside: {e}", home.display()))?;
    }
    std::fs::rename(&incoming, home).map_err(|e| format!("couldn't put the backup in place: {e}"))?;
    Ok(format!(
        "restored {} into {}{}",
        archive.display(),
        home.display(),
        if kept.exists() { format!("; what was there is kept at {}", kept.display()) } else { String::new() }
    ))
}

pub fn size_text(n: u64) -> String {
    match n {
        n if n >= 1024 * 1024 => format!("{:.1} MB", n as f64 / 1048576.0),
        n if n >= 1024 => format!("{} KB", n / 1024),
        n => format!("{n} bytes"),
    }
}

/// `/backup list` and `lyra backup list`.
pub fn describe(dir: &Path, settings: &Settings) -> String {
    let all = list(dir);
    let mut out = vec![format!(
        "backups in {} ({}, keeping {}):",
        crate::context::show(dir),
        if settings.enabled { format!("nightly at {}", settings.at) } else { "nightly backups off".into() },
        settings.keep
    )];
    if all.is_empty() {
        out.push("  none yet — /backup now makes one".into());
    }
    out.extend(all.iter().map(|b| format!("  {}  {}  {}", b.name, b.made.format("%Y-%m-%d %H:%M"), size_text(b.size))));
    out.push("lyra restore <file|latest> puts one back (with lyra stopped)".into());
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_backup_cut_off_part_way_is_never_listed() {
        let dir = std::env::temp_dir().join(format!("lyra-backup-partial-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lyra-20261001-033000.tar.gz"), b"whole").unwrap();
        std::fs::write(dir.join(".lyra-20261009-033000.tar.gz.partial"), b"half").unwrap();
        let names: Vec<String> = list(&dir).into_iter().map(|b| b.name).collect();
        assert_eq!(names, ["lyra-20261001-033000.tar.gz"], "the half-written one isn't a backup");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn nightly_backups_are_due_once_after_their_time() {
        let s = Settings::default();
        let at = |h: u32, m: u32| Local.with_ymd_and_hms(2026, 10, 6, h, m, 0).unwrap();
        assert!(!due(&s, None, at(3, 0)), "not before 03:30");
        assert!(due(&s, None, at(3, 31)));
        assert!(due(&s, Some(at(1, 0)), at(9, 0)), "the last one was before today's time");
        assert!(!due(&s, Some(at(3, 31)), at(9, 0)), "already done today");
        assert!(!due(&Settings { enabled: false, ..s }, None, at(9, 0)));
    }

    #[test]
    fn a_backup_holds_everything_and_restores_in_place() {
        let root = std::env::temp_dir().join(format!("lyra-backup-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join(".lyra");
        for d in ["config", "skills", "uploads/x", "memory/lance"] {
            std::fs::create_dir_all(home.join(d)).unwrap();
        }
        std::fs::write(home.join("config/config.toml"), "model = \"m\"\n").unwrap();
        std::fs::write(home.join("skills/deploy.md"), "# deploy\n").unwrap();
        std::fs::write(home.join("uploads/x/photo.png"), "png").unwrap();
        std::fs::write(home.join("skills/ledger.db-wal"), "wal").unwrap();
        std::fs::write(home.join("lyra.pid"), "1 lyra serve").unwrap();
        std::fs::write(home.join("memory/lance/data"), "vectors").unwrap();
        // Indexes rebuilt on every start: left out.
        for d in ["capabilities/index/capabilities.lance", "agents/index/capabilities.lance"] {
            std::fs::create_dir_all(home.join(d)).unwrap();
            std::fs::write(home.join(d).join("big.lance"), "cache").unwrap();
        }
        // A real SQLite database, copied with VACUUM INTO.
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            use sqlx::Connection;
            let o = sqlx::sqlite::SqliteConnectOptions::new().filename(home.join("skills/ledger.db")).create_if_missing(true);
            let mut c = sqlx::SqliteConnection::connect_with(&o).await.unwrap();
            sqlx::query("CREATE TABLE t (v TEXT); INSERT INTO t VALUES ('kept')").execute(&mut c).await.unwrap();
        });
        let settings = Settings::default();
        let copy_memory = |to: &Path| -> Result<(), String> {
            std::fs::create_dir_all(to).unwrap();
            std::fs::write(to.join("data"), "vectors (consistent copy)").map_err(|e| e.to_string())
        };
        let mem_path = home.join("memory/lance");
        let (b, _) = run(&home, &settings, Some((&mem_path, &copy_memory))).unwrap();
        assert_eq!(b.path.parent().unwrap(), root.join(".lyra-backups"), "next to the home, not inside it");
        assert_eq!(list(&settings.dir(&home)).len(), 1);

        // Lose a skill, then restore.
        std::fs::remove_file(home.join("skills/deploy.md")).unwrap();
        let note = restore(&home, &b.path).unwrap();
        assert!(note.contains("before-restore"), "{note}");
        assert_eq!(std::fs::read_to_string(home.join("skills/deploy.md")).unwrap(), "# deploy\n");
        assert_eq!(std::fs::read_to_string(home.join("memory/lance/data")).unwrap(), "vectors (consistent copy)", "memory through its own backup");
        assert!(!home.join("uploads").exists(), "uploads only when asked for");
        assert!(!home.join("skills/ledger.db-wal").exists(), "no half-written journals");
        assert!(!home.join("lyra.pid").exists(), "not the running lyra's lock");
        assert!(!home.join("capabilities/index").exists() && !home.join("agents/index").exists(), "rebuildable indexes aren't backed up");
        let kept: String = rt.block_on(async {
            use sqlx::Connection;
            let o = sqlx::sqlite::SqliteConnectOptions::new().filename(home.join("skills/ledger.db"));
            let mut c = sqlx::SqliteConnection::connect_with(&o).await.unwrap();
            sqlx::query_scalar("SELECT v FROM t").fetch_one(&mut c).await.unwrap()
        });
        assert_eq!(kept, "kept");
        assert!(restore(&home, &root.join("nope.tar.gz")).is_err());

        // Only `keep` are kept.
        let few = Settings { keep: 2, ..Settings::default() };
        for _ in 0..3 {
            std::thread::sleep(std::time::Duration::from_millis(1100));
            run(&home, &few, None).unwrap();
        }
        assert_eq!(list(&few.dir(&home)).len(), 2);
        let _ = std::fs::remove_dir_all(&root);
    }
}

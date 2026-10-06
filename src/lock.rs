//! One lyra per home (`~/.lyra/lyra.pid`): the TUI and `lyra serve` each
//! keep their own copy of the conversation, so two of them on the same
//! home overwrite each other's work. A stale file (the process is gone) is
//! taken over.

use std::path::{Path, PathBuf};

pub struct Lock {
    path: PathBuf,
}

/// Whether a lyra process with this pid is running.
fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/comm")).is_ok_and(|c| c.trim() == "lyra")
}

/// Take the home for this process (`mode`: "the terminal UI" or "lyra serve").
pub fn acquire(home: &Path, mode: &str) -> Result<Lock, String> {
    let path = home.join("lyra.pid");
    let own = std::process::id();
    if let Ok(text) = std::fs::read_to_string(&path) {
        let (pid, other) = text.trim().split_once(' ').unwrap_or((text.trim(), "lyra"));
        if let Ok(pid) = pid.parse::<u32>()
            && pid != own
            && alive(pid)
        {
            return Err(format!("{other} is already running with {} (pid {pid})", home.display()));
        }
    }
    let _ = std::fs::create_dir_all(home);
    std::fs::write(&path, format!("{own} {mode}")).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Lock { path })
}

impl Drop for Lock {
    fn drop(&mut self) {
        let own = std::process::id().to_string();
        if std::fs::read_to_string(&self.path).is_ok_and(|t| t.split(' ').next() == Some(own.as_str())) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stale_lock_is_taken_over_and_released() {
        let home = std::env::temp_dir().join(format!("lyra-lock-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        // A pid that isn't a running lyra.
        std::fs::write(home.join("lyra.pid"), "999999 lyra serve").unwrap();
        let lock = acquire(&home, "the terminal UI").unwrap();
        assert!(std::fs::read_to_string(home.join("lyra.pid")).unwrap().ends_with("the terminal UI"));
        drop(lock);
        assert!(!home.join("lyra.pid").exists());
    }
}

//! A machine's health, as a small JSON report: disks, memory, load, failed
//! systemd units and pending package updates. The node sends it to lyra
//! every few minutes; `lyra serve` takes the same report of the server.
//! Only reads (`df`, `/proc`, `systemctl`, the package manager's cache).

use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// How often a node reports.
pub const EVERY: Duration = Duration::from_secs(5 * 60);
/// Pending updates are counted less often (it reads the package cache).
const UPDATES_EVERY: Duration = Duration::from_secs(6 * 3600);

fn output(cmd: &str, args: &[&str]) -> Option<(i32, String)> {
    let out = Command::new(cmd).args(args).env("LC_ALL", "C").output().ok()?;
    Some((out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned()))
}

/// Real filesystems from `df -P -k`: mount, percent used, size and free (KB).
pub fn disks(df: &str) -> Vec<Value> {
    df.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 6 {
                return None;
            }
            let (size, used, avail) = (f[1].parse::<u64>().ok()?, f[2].parse::<u64>().ok()?, f[3].parse::<u64>().ok()?);
            let mount = f[5..].join(" ");
            if size == 0 || mount.starts_with("/snap/") || mount.starts_with("/run/") || mount.starts_with("/sys") || mount.starts_with("/proc") {
                return None;
            }
            // df's own rounding: used / (used + available).
            let pct = ((used as f64 / (used + avail).max(1) as f64) * 100.0).ceil() as u64;
            Some(json!({ "mount": mount, "used_pct": pct, "size_kb": size, "avail_kb": avail }))
        })
        .collect()
}

/// `MemTotal` and `MemAvailable` from `/proc/meminfo`, in KB.
pub fn memory(meminfo: &str) -> Option<Value> {
    let field = |name: &str| meminfo.lines().find(|l| l.starts_with(name)).and_then(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok());
    let (total, avail) = (field("MemTotal:")?, field("MemAvailable:")?);
    Some(json!({ "total_kb": total, "available_kb": avail, "used_pct": ((total - avail.min(total)) as f64 / total.max(1) as f64 * 100.0).round() as u64 }))
}

/// Failed units from `systemctl list-units --state=failed --plain --no-legend`.
pub fn failed_units(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|l| l.trim_start().trim_start_matches('●').split_whitespace().next().map(str::to_string))
        .collect()
}

/// Pending updates, from the package manager's cached metadata (no network).
fn count_updates() -> Option<u64> {
    if let Some((code, out)) = output("dnf", &["-q", "--cacheonly", "check-update"]) {
        // 100: updates are available; 0: none.
        return match code {
            0 => Some(0),
            100 => Some(out.lines().filter(|l| l.split_whitespace().count() == 3 && !l.starts_with(' ') && l.contains('.')).count() as u64),
            _ => None,
        };
    }
    if let Some((0, out)) = output("apt-get", &["-s", "-q", "upgrade"]) {
        return Some(out.lines().filter(|l| l.starts_with("Inst ")).count() as u64);
    }
    None
}

static UPDATES: Mutex<Option<(Instant, Option<u64>)>> = Mutex::new(None);

fn updates() -> Option<u64> {
    let mut cached = UPDATES.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, n)) = *cached
        && at.elapsed() < UPDATES_EVERY
    {
        return n;
    }
    let n = count_updates();
    *cached = Some((Instant::now(), n));
    n
}

/// This machine's health now (blocking: a few small commands).
pub fn report() -> Value {
    let df = output("df", &["-P", "-k", "-x", "tmpfs", "-x", "devtmpfs", "-x", "overlay", "-x", "squashfs", "-x", "efivarfs"]).map(|o| o.1).unwrap_or_default();
    let load: Vec<f64> = std::fs::read_to_string("/proc/loadavg").unwrap_or_default().split_whitespace().take(3).filter_map(|x| x.parse().ok()).collect();
    let uptime = std::fs::read_to_string("/proc/uptime").ok().and_then(|u| u.split_whitespace().next()?.parse::<f64>().ok()).map(|u| u as u64);
    let failed = output("systemctl", &["list-units", "--state=failed", "--plain", "--no-legend", "--no-pager"]).map(|o| failed_units(&o.1));
    json!({
        "at": chrono::Utc::now().to_rfc3339(),
        "disks": disks(&df),
        "memory": memory(&std::fs::read_to_string("/proc/meminfo").unwrap_or_default()),
        "load": load,
        "cpus": std::thread::available_parallelism().map_or(1, |n| n.get()),
        "uptime_s": uptime,
        "failed_units": failed,
        "updates": updates(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readings_parse() {
        let df = "Filesystem 1024-blocks Used Available Capacity Mounted on\n\
                  /dev/mapper/root 266000000 75000000 191000000 29% /\n\
                  /dev/vda2 1992552 1800000 70000 97% /boot\n\
                  /dev/loop0 1000 1000 0 100% /snap/core/1\n\
                  nas:/data 1000000 950000 50000 95% /mnt/nas data\n";
        let d = disks(df);
        assert_eq!(d.len(), 3, "snaps are left out");
        assert_eq!((d[1]["mount"].as_str(), d[1]["used_pct"].as_u64()), (Some("/boot"), Some(97)));
        assert_eq!(d[2]["mount"], "/mnt/nas data", "mounts with spaces");
        let m = memory("MemTotal:       16000000 kB\nMemFree: 1 kB\nMemAvailable:    4000000 kB\n").unwrap();
        assert_eq!(m["used_pct"], 75);
        assert_eq!(failed_units("● nginx.service loaded failed failed A web server\nbackup.timer loaded failed failed x\n"), vec!["nginx.service", "backup.timer"]);
        assert!(failed_units("").is_empty());
        let r = report();
        assert!(r["disks"].is_array() && r["cpus"].as_u64().unwrap() >= 1);
    }
}

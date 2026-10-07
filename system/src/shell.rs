//! Shell commands: what a command line does (classified in Rust, never by
//! the model) and running one with a timeout and an output limit.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

/// What a command line does, worst part first.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    /// Only looks: runs without asking.
    ReadOnly,
    /// Changes something: asks first.
    Change(String),
    /// Deletes, kills, runs as root…: asks first, with a warning.
    Dangerous(String),
    /// Never run, whoever approves.
    Forbidden(String),
}

/// Commands that only read (their first word).
const READ_ONLY: &[&str] = &[
    "ls", "cat", "head", "tail", "grep", "egrep", "fgrep", "rg", "wc", "du", "df", "free", "uptime", "uname", "hostname", "whoami", "id", "ps",
    "pwd", "echo", "printf", "date", "stat", "file", "which", "whereis", "type", "tree", "lsblk", "lscpu", "lsusb", "lspci", "lsmem", "nproc",
    "sort", "uniq", "cut", "tr", "basename", "dirname", "realpath", "readlink", "md5sum", "sha1sum", "sha256sum", "diff", "cmp", "w", "who",
    "last", "ss", "netstat", "dig", "nslookup", "host", "traceroute", "tracepath", "journalctl", "test", "true", "false", "column", "jq",
    "nl", "cal", "getent", "groups", "locale", "lsof", "pgrep", "vmstat", "iostat", "mpstat", "sensors", "env", "printenv", "awk", "sed",
    "find", "git", "systemctl", "docker", "podman", "kubectl", "ip", "ping", "top", "curl", "apt", "dnf", "rpm", "dpkg", "uname", "timedatectl",
    "hostnamectl", "loginctl", "id", "seq", "xxd", "od", "strings", "zcat", "less", "more", "ls-files",
    "findmnt", "showmount", "nc", "command", "timeout", "blkid", "lsmod", "dmesg", "resolvectl", "nmcli",
];

/// Commands that delete, stop or take over things.
const DANGEROUS: &[(&str, &str)] = &[
    ("rm", "deletes files"),
    ("rmdir", "deletes directories"),
    ("shred", "destroys files"),
    ("dd", "writes raw data"),
    ("truncate", "empties files"),
    ("mv", "moves or overwrites files"),
    ("kill", "stops processes"),
    ("pkill", "stops processes"),
    ("killall", "stops processes"),
    ("shutdown", "shuts the machine down"),
    ("reboot", "restarts the machine"),
    ("poweroff", "shuts the machine down"),
    ("halt", "stops the machine"),
    ("chmod", "changes permissions"),
    ("chown", "changes ownership"),
    ("chgrp", "changes ownership"),
    ("userdel", "deletes a user"),
    ("useradd", "adds a user"),
    ("passwd", "changes a password"),
    ("iptables", "changes the firewall"),
    ("nft", "changes the firewall"),
    ("ufw", "changes the firewall"),
    ("firewall-cmd", "changes the firewall"),
    ("crontab", "changes scheduled jobs"),
    ("mount", "mounts filesystems"),
    ("umount", "unmounts filesystems"),
    ("fdisk", "changes partitions"),
    ("parted", "changes partitions"),
];

/// Split a command line into simple commands (on `;`, `&&`, `||`, `|`, `&`
/// and newlines outside quotes), each as words with quotes removed.
pub(crate) fn segments(line: &str) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = vec![Vec::new()];
    let mut word = String::new();
    let mut quote: Option<char> = None;
    let mut chars = line.chars().peekable();
    let mut prev = ' ';
    let end_word = |word: &mut String, out: &mut Vec<Vec<String>>| {
        if !word.is_empty() {
            out.last_mut().expect("a segment").push(std::mem::take(word));
        }
    };
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => quote = Some(c),
            (None, '\\') => {
                if let Some(n) = chars.next() {
                    word.push(n);
                }
            }
            (None, ' ' | '\t') => end_word(&mut word, &mut out),
            // `2>&1` and `&>` are redirections, not command separators.
            (None, '&') if prev == '>' || chars.peek() == Some(&'>') => word.push(c),
            (None, ';' | '|' | '&' | '\n') => {
                end_word(&mut word, &mut out);
                while matches!(chars.peek(), Some('|' | '&')) {
                    chars.next();
                }
                out.push(Vec::new());
            }
            (None, c) => word.push(c),
        }
        prev = c;
    }
    end_word(&mut word, &mut out);
    out.into_iter().filter(|s| !s.is_empty()).collect()
}

/// Whether the line writes a file with `>` (outside quotes); redirecting to
/// /dev/null or between streams doesn't count.
pub(crate) fn redirects(line: &str) -> bool {
    let mut quote: Option<char> = None;
    let chars: Vec<char> = line.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '\'' | '"') => quote = Some(c),
            (None, '>') => {
                let rest: String = chars[i + 1..].iter().collect();
                let rest = rest.trim_start_matches('>').trim_start();
                if !(rest.starts_with('&') || rest.starts_with("/dev/null")) {
                    return true;
                }
            }
            _ => {}
        }
    }
    false
}

/// Patterns no approval makes acceptable.
fn forbidden(line: &str) -> Option<String> {
    let squashed = line.split_whitespace().collect::<Vec<_>>().join(" ");
    for seg in segments(line) {
        let cmd = seg.first().map(|w| w.rsplit('/').next().unwrap_or(w)).unwrap_or("");
        let cmd = if matches!(cmd, "sudo" | "doas") { seg.get(1).map(String::as_str).unwrap_or("") } else { cmd };
        let recursive = seg.iter().any(|w| w.starts_with('-') && !w.starts_with("--") && (w.contains('r') || w.contains('R')) || w == "--recursive");
        let roots = ["/", "/*", "~", "~/", "~/*", "$HOME", "$HOME/", "$HOME/*", "--no-preserve-root"];
        if cmd == "rm" && (seg.iter().any(|w| w == "--no-preserve-root") || recursive && seg.iter().any(|w| roots.contains(&w.as_str()))) {
            return Some("would delete the whole system or home directory".into());
        }
    }
    if squashed.contains(":(){") || squashed.contains(":() {") {
        return Some("a fork bomb".into());
    }
    if squashed.split_whitespace().any(|w| w.starts_with("mkfs")) {
        return Some("formats a filesystem".into());
    }
    if squashed.contains("of=/dev/sd") || squashed.contains("of=/dev/nvme") || squashed.contains("> /dev/sd") || squashed.contains(">/dev/sd") {
        return Some("overwrites a disk".into());
    }
    None
}

/// A command's verb: its first word that's neither an option nor a redirection (`2>&1`).
fn verb(args: &[String]) -> Option<&str> {
    args.iter().map(String::as_str).find(|a| !a.starts_with('-') && !a.contains('>') && !a.contains('<'))
}

/// Whether one simple command (first word and the rest) only reads.
fn reads_only(cmd: &str, args: &[String]) -> Result<(), String> {
    let has = |flags: &[&str]| args.iter().any(|a| flags.iter().any(|f| a == f || a.starts_with(&format!("{f}="))));
    let sub = args.first().map(String::as_str).unwrap_or("");
    let ok = match cmd {
        "find" => !has(&["-delete", "-exec", "-execdir", "-ok", "-okdir", "-fprint", "-fprintf", "-fls"]),
        "sed" => !args.iter().any(|a| a.starts_with("-i") || a == "--in-place"),
        "awk" => !args.iter().any(|a| a.contains("system(") || a.contains("print >") || a.contains("| getline")),
        "git" => matches!(sub, "status" | "log" | "diff" | "show" | "branch" | "remote" | "tag" | "blame" | "rev-parse" | "ls-files" | "describe" | "shortlog" | "ls-remote" | "grep"),
        // The verb is the first word that isn't an option; options alone (`--failed`) list units.
        "systemctl" => matches!(
            verb(args).unwrap_or("list-units"),
            "status" | "list-units" | "list-unit-files" | "list-timers" | "list-sockets" | "list-jobs" | "list-dependencies" | "is-active" | "is-enabled" | "is-failed" | "is-system-running" | "get-default" | "show" | "cat"
        ),
        "docker" | "podman" => matches!(sub, "ps" | "images" | "logs" | "inspect" | "version" | "info" | "top" | "port") || (sub == "stats" && has(&["--no-stream"])),
        "kubectl" => matches!(sub, "get" | "describe" | "logs" | "version" | "top" | "explain" | "api-resources"),
        "ip" => matches!(sub, "addr" | "a" | "address" | "route" | "r" | "link" | "l" | "neigh" | "n" | "-br" | "-4" | "-6" | "-s") && !has(&["add", "del", "delete", "set", "flush", "change", "replace"]),
        "ping" => has(&["-c"]),
        "top" => has(&["-b", "-bn1"]) || args.iter().any(|a| a.starts_with("-b")),
        "curl" => !has(&["-X", "--request", "-d", "--data", "--data-raw", "--data-binary", "--data-urlencode", "-F", "--form", "-T", "--upload-file", "-o", "--output", "-O", "--remote-name"]),
        "apt" | "dnf" => matches!(
            verb(args).unwrap_or(""),
            "list" | "search" | "info" | "show" | "history" | "repolist" | "check-update" | "updateinfo" | "repoquery" | "provides"
        ),
        "rpm" => args.iter().all(|a| !a.starts_with("-e") && !a.starts_with("-i") && !a.starts_with("-U")),
        "dpkg" => has(&["-l", "-L", "-s", "--list", "--status", "--listfiles"]),
        "timedatectl" | "hostnamectl" | "loginctl" => sub.is_empty() || matches!(sub, "status" | "show" | "list-sessions" | "list-users"),
        // A port check, not a connection that sends anything.
        "nc" => has(&["-z"]) && !has(&["-l", "-e", "-c", "--exec", "--sh-exec"]),
        // `command -v x`: is x installed?
        "command" => has(&["-v", "-V"]),
        "dmesg" => !has(&["-C", "--clear", "-c", "--read-clear", "-n", "--console-level", "-D", "-E"]),
        "resolvectl" => sub.is_empty() || matches!(sub, "status" | "query" | "statistics" | "dns" | "domain"),
        "nmcli" => !args.iter().any(|a| matches!(a.as_str(), "up" | "down" | "add" | "modify" | "delete" | "connect" | "disconnect" | "reload" | "on" | "off")),
        // `timeout 5 <command>`: as read-only as the command it wraps.
        "timeout" => {
            let mut rest = args.iter().skip_while(|a| a.starts_with('-'));
            let _duration = rest.next();
            let inner: Vec<String> = rest.cloned().collect();
            match inner.split_first() {
                Some((c, a)) => READ_ONLY.contains(&c.as_str()) && c != "timeout" && reads_only(c, a).is_ok(),
                None => false,
            }
        }
        _ => true,
    };
    if ok { Ok(()) } else { Err(format!("{cmd} {sub}").trim().to_string()) }
}

/// Classify a command line. `allowed` are prefixes the user lets run
/// without asking (`[system] allow_commands`).
pub fn classify(line: &str, allowed: &[String]) -> Class {
    let line = line.trim();
    if line.is_empty() {
        return Class::Forbidden("an empty command".into());
    }
    if let Some(why) = forbidden(line) {
        return Class::Forbidden(why);
    }
    if allowed.iter().any(|a| !a.trim().is_empty() && (line == a.trim() || line.starts_with(&format!("{} ", a.trim())))) {
        return Class::ReadOnly;
    }
    let mut worst = Class::ReadOnly;
    let mut raise = |c: Class| {
        if c > worst {
            worst = c.clone();
        }
    };
    if line.contains("$(") || line.contains('`') {
        raise(Class::Change("runs a command inside the command".into()));
    }
    if redirects(line) {
        raise(Class::Change("writes a file (>)".into()));
    }
    for seg in segments(line) {
        let mut words = seg.iter().skip_while(|w| w.contains('=') && !w.starts_with('-'));
        let Some(first) = words.next() else { continue };
        let cmd = first.rsplit('/').next().unwrap_or(first).to_string();
        let args: Vec<String> = words.cloned().collect();
        if matches!(cmd.as_str(), "sudo" | "su" | "doas" | "pkexec") {
            raise(Class::Dangerous("runs as root".into()));
            continue;
        }
        if let Some((_, why)) = DANGEROUS.iter().find(|(c, _)| *c == cmd) {
            raise(Class::Dangerous(format!("{why} ({cmd})")));
            continue;
        }
        let dangerous_sub = match (cmd.as_str(), args.first().map(String::as_str)) {
            ("git", Some("push" | "reset" | "clean" | "checkout" | "restore" | "rebase")) => Some("rewrites or publishes git history"),
            ("docker" | "podman", Some("rm" | "rmi" | "kill" | "stop" | "prune" | "system")) => Some("removes or stops containers"),
            ("systemctl", Some("stop" | "disable" | "mask" | "kill" | "poweroff" | "reboot")) => Some("stops services"),
            ("kubectl", Some("delete" | "drain" | "cordon")) => Some("removes cluster resources"),
            ("apt" | "dnf" | "yum", Some("remove" | "purge" | "autoremove" | "erase")) => Some("removes packages"),
            _ => None,
        };
        if let Some(why) = dangerous_sub {
            raise(Class::Dangerous(format!("{why} ({cmd} {})", args[0])));
            continue;
        }
        if !READ_ONLY.contains(&cmd.as_str()) {
            raise(Class::Change(format!("runs {cmd}")));
            continue;
        }
        if let Err(what) = reads_only(&cmd, &args) {
            raise(Class::Change(format!("changes things ({what})")));
        }
    }
    worst
}

/// Keep at most `max` characters, saying how much was cut.
pub fn clip(text: &str, max: usize) -> (String, bool) {
    if text.chars().count() <= max {
        return (text.to_string(), false);
    }
    let kept: String = text.chars().take(max).collect();
    (format!("{kept}\n… ({} more characters cut)", text.chars().count() - max), true)
}

/// Start a process apart from lyra, so it and what it starts can be stopped
/// together (its own process group; on Windows, no console window).
pub fn own_group(cmd: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
}

/// Stop a process and everything it started.
pub fn kill_tree(pid: u32) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill").args(["-TERM", &format!("-{pid}")]).status();
        std::thread::sleep(Duration::from_millis(300));
        let _ = Command::new("kill").args(["-KILL", &format!("-{pid}")]).status();
    }
    #[cfg(windows)]
    {
        let mut c = Command::new("taskkill");
        c.args(["/T", "/F", "/PID", &pid.to_string()]);
        own_group(&mut c);
        let _ = c.status();
    }
}

/// Run a process with a timeout (its whole process group is killed when it
/// runs over), capturing at most `max` characters of each stream.
pub fn run(mut cmd: Command, timeout: Duration, max: usize) -> Result<Value, String> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    own_group(&mut cmd);
    let start = Instant::now();
    let mut child = cmd.spawn().map_err(|e| format!("couldn't start it: {e}"))?;
    let limit = (max as u64) * 4 + 1024;
    let reader = |stream: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut s) = stream {
                let _ = (&mut s).take(limit).read_to_end(&mut buf);
                // Keep draining so the process isn't stuck on a full pipe.
                let _ = std::io::copy(&mut s, &mut std::io::sink());
            }
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let out = reader(child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>));
    let err = reader(child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>));
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break Some(status);
        }
        if start.elapsed() > timeout {
            timed_out = true;
            kill_tree(child.id());
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let (stdout, cut_out) = clip(out.join().unwrap_or_default().trim_end(), max);
    let (stderr, cut_err) = clip(err.join().unwrap_or_default().trim_end(), max / 2);
    Ok(json!({
        "exit_code": status.and_then(|s| s.code()),
        "stdout": stdout,
        "stderr": stderr,
        "timed_out": timed_out,
        "truncated": cut_out || cut_err,
        "seconds": (start.elapsed().as_secs_f32() * 10.0).round() / 10.0,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class(line: &str) -> Class {
        classify(line, &[])
    }

    #[test]
    fn reading_runs_and_changing_asks() {
        for line in [
            "ls -la /var/log",
            "df -h && free -m",
            "ps aux | grep nginx | head -5",
            "git status",
            "systemctl status nginx",
            "systemctl --failed --no-pager --no-legend",
            "findmnt -T /mnt/dbr2-repo; echo \"findmnt:$?\"",
            "nc -z -w 5 192.1.3.196 2049",
            "timeout 5 ping -c 2 -W 3 192.1.3.196",
            "showmount -e 192.1.3.196",
            "command -v nc",
            "systemctl --failed --no-legend --no-pager 2>&1; echo \"EXIT:$?\"",
            "systemctl --user list-timers",
            "dnf -q check-update",
            "find . -name '*.rs' | wc -l",
            "cat /etc/os-release 2>/dev/null",
            "ping -c 3 172.99.99.11",
            "curl -s http://localhost:8181/v1/models",
            "journalctl -u nginx -n 50 --no-pager",
            "echo 'a > b'",
            "ls -la /tmp/x 2>&1; echo \"exit: $?\"",
            "df -h &> /dev/null && echo ok",
        ] {
            assert_eq!(class(line), Class::ReadOnly, "{line}");
        }
        assert!(matches!(class("touch notes.txt"), Class::Change(_)));
        assert!(matches!(class("echo hi > notes.txt"), Class::Change(w) if w.contains('>')));
        assert!(matches!(class("sed -i s/a/b/ f"), Class::Change(_)));
        assert!(matches!(class("find . -name '*.tmp' -delete"), Class::Change(_)));
        assert!(matches!(class("systemctl restart nginx"), Class::Change(_)));
        assert!(matches!(class("systemctl --now enable nginx"), Class::Change(_)), "options first, then a changing verb");
        assert!(matches!(class("dnf -y upgrade"), Class::Change(_)));
        assert!(!matches!(class("mount /dev/sdb1 /mnt"), Class::ReadOnly), "mounting changes things");
        assert!(!matches!(class("nc -l 4444"), Class::ReadOnly));
        assert!(!matches!(class("timeout 5 touch x"), Class::ReadOnly), "timeout is only as harmless as what it runs");
        assert!(matches!(class("ls $(cat list)"), Class::Change(_)));
        assert!(matches!(class("curl -X POST http://x"), Class::Change(_)));
        assert!(matches!(class("ls && rm -r build"), Class::Dangerous(w) if w.contains("deletes")));
        assert!(matches!(class("sudo ls"), Class::Dangerous(w) if w.contains("root")));
        assert!(matches!(class("git push origin main"), Class::Dangerous(_)));
        assert!(matches!(class("systemctl stop nginx"), Class::Dangerous(_)));
        assert!(matches!(class("rm -rf /"), Class::Forbidden(_)));
        assert!(matches!(class("rm -rf ~"), Class::Forbidden(_)));
        assert!(matches!(class(":(){ :|:& };:"), Class::Forbidden(_)));
        assert!(matches!(class("mkfs.ext4 /dev/sdb1"), Class::Forbidden(_)));
        assert!(matches!(class("dd if=/dev/zero of=/dev/sda"), Class::Forbidden(_)));
        assert_eq!(classify("cargo test", &["cargo test".into()]), Class::ReadOnly, "the user's allow list");
        assert!(matches!(classify("cargo publish", &["cargo test".into()]), Class::Change(_)));
    }

    #[test]
    fn runs_with_a_timeout_and_a_limit() {
        let mut c = Command::new("bash");
        c.arg("-c").arg("echo hello; echo oops >&2; exit 3");
        let r = run(c, Duration::from_secs(5), 1000).unwrap();
        assert_eq!((r["stdout"].as_str(), r["stderr"].as_str(), r["exit_code"].as_i64()), (Some("hello"), Some("oops"), Some(3)));
        let mut slow = Command::new("bash");
        slow.arg("-c").arg("sleep 5 & sleep 5");
        let r = run(slow, Duration::from_millis(300), 1000).unwrap();
        assert_eq!(r["timed_out"], true);
        assert!(r["seconds"].as_f64().unwrap() < 2.0, "the whole group is killed");
        let mut big = Command::new("bash");
        big.arg("-c").arg("seq 1 100000");
        let r = run(big, Duration::from_secs(5), 100).unwrap();
        assert_eq!(r["truncated"], true);
    }
}

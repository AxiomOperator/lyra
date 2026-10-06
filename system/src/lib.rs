//! System access for lyra's agents: shell commands, files, HTTP, servers
//! over SSH, and what the local machine looks like. Every call is checked
//! here, in Rust, before it runs ([`System::check`]): reading runs on its
//! own, changing things needs the user's approval, and some things never
//! run. Credentials are never taken as arguments: SSH uses the user's keys
//! (batch mode, no passwords) and HTTP headers can name environment
//! variables (`$TOKEN`).

pub mod shell;

use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use lyra_capabilities::RiskLevel;
use serde::Deserialize;
use serde_json::{Value, json};

pub use shell::{Class, classify};

/// `[system]`.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    /// The shell commands run in.
    pub shell: String,
    /// The longest a command may run.
    pub timeout_seconds: u64,
    /// Most characters of output handed back.
    pub max_output: usize,
    /// Command prefixes that run without asking (beyond the read-only ones).
    pub allow_commands: Vec<String>,
    /// Folders files may be written in without asking.
    pub write_roots: Vec<String>,
    /// Paths never read or written (keys, credentials, lyra's own config).
    pub deny_paths: Vec<String>,
    /// SSH hosts (names from ~/.ssh/config or user@host) commands may run on.
    pub ssh_hosts: Vec<String>,
    pub http_timeout_seconds: u64,
    /// How long an approval waits for an answer before it counts as no.
    pub approval_timeout_seconds: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            shell: "bash".into(),
            timeout_seconds: 60,
            max_output: 20_000,
            allow_commands: Vec::new(),
            write_roots: Vec::new(),
            deny_paths: ["~/.ssh", "~/.gnupg", "~/.lyra/config", "~/.aws", "~/.kube/config", "~/.docker/config.json", "~/.netrc", "/etc/shadow", "/etc/gshadow", "/etc/sudoers"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            ssh_hosts: Vec::new(),
            http_timeout_seconds: 30,
            approval_timeout_seconds: 300,
        }
    }
}

/// A tool the system offers.
pub struct Spec {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: Value,
    /// Its worst case; each call is checked on its own too.
    pub risk: RiskLevel,
}

pub const TOOLS: &[&str] = &["system_info", "shell_run", "file_read", "file_list", "file_write", "file_delete", "http_request", "ssh_run"];

pub fn specs() -> Vec<Spec> {
    let s = |v: &str| json!({ "type": "string", "description": v });
    vec![
        Spec {
            name: "system_info",
            description: "This machine: OS, kernel, hostname, uptime, load, CPUs, memory, disks and the busiest processes.",
            parameters: json!({ "type": "object", "properties": {} }),
            risk: RiskLevel::ReadOnly,
        },
        Spec {
            name: "shell_run",
            description: "Run a shell command on this machine and get its exit code and output. Read-only commands run at once; anything that changes things waits for the user's approval.",
            parameters: json!({ "type": "object", "properties": {
                "command": s("The command line."),
                "cwd": s("Directory to run it in (default: the home directory)."),
                "timeout_seconds": { "type": "integer", "description": "Stop it after this long (capped by the configured limit)." },
            }, "required": ["command"] }),
            risk: RiskLevel::Write,
        },
        Spec {
            name: "file_read",
            description: "Read a text file (a range of lines for big ones).",
            parameters: json!({ "type": "object", "properties": {
                "path": s("The file."),
                "start_line": { "type": "integer", "description": "First line, from 1." },
                "max_lines": { "type": "integer", "description": "How many lines (default 400)." },
            }, "required": ["path"] }),
            risk: RiskLevel::ReadOnly,
        },
        Spec {
            name: "file_list",
            description: "List a directory: names, kinds and sizes; optionally recursive and filtered by a pattern like *.log.",
            parameters: json!({ "type": "object", "properties": {
                "path": s("The directory."),
                "pattern": s("Only names matching this (* and ? wildcards)."),
                "recursive": { "type": "boolean" },
            }, "required": ["path"] }),
            risk: RiskLevel::ReadOnly,
        },
        Spec {
            name: "file_write",
            description: "Write (or append to) a text file, creating folders as needed. Outside the work folders it waits for the user's approval.",
            parameters: json!({ "type": "object", "properties": {
                "path": s("The file."),
                "content": s("The text to write."),
                "append": { "type": "boolean", "description": "Add to the end instead of replacing." },
            }, "required": ["path", "content"] }),
            risk: RiskLevel::Write,
        },
        Spec {
            name: "file_delete",
            description: "Delete a file, or a directory with recursive=true. Always waits for the user's approval.",
            parameters: json!({ "type": "object", "properties": {
                "path": s("The file or directory."),
                "recursive": { "type": "boolean", "description": "Delete a directory and everything in it." },
            }, "required": ["path"] }),
            risk: RiskLevel::Destructive,
        },
        Spec {
            name: "http_request",
            description: "Make an HTTP(S) request. GET and HEAD run at once; other methods wait for the user's approval. A header value like \"$API_TOKEN\" is read from that environment variable.",
            parameters: json!({ "type": "object", "properties": {
                "url": s("The URL."),
                "method": s("GET (default), HEAD, POST, PUT, PATCH or DELETE."),
                "headers": { "type": "object", "description": "Header names and values.", "additionalProperties": { "type": "string" } },
                "body": s("The request body."),
            }, "required": ["url"] }),
            risk: RiskLevel::Write,
        },
        Spec {
            name: "ssh_run",
            description: "Run a command on a configured server over SSH (the user's keys; no passwords). Read-only commands run at once; others wait for approval.",
            parameters: json!({ "type": "object", "properties": {
                "host": s("One of the configured hosts."),
                "command": s("The command line to run there."),
                "timeout_seconds": { "type": "integer" },
            }, "required": ["host", "command"] }),
            risk: RiskLevel::Write,
        },
    ]
}

/// What a call may do.
#[derive(Debug, Clone, PartialEq)]
pub enum Check {
    /// Runs without asking.
    Auto,
    /// Needs the user's approval; why, and whether it's dangerous.
    Ask { why: String, dangerous: bool },
    /// Never runs.
    Forbidden(String),
}

pub struct System {
    pub settings: Settings,
    expand: fn(&str) -> PathBuf,
}

/// Remove `.` and `..` without touching the disk.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c.as_os_str()),
        }
    }
    out
}

/// `*` and `?` wildcards.
fn wildcard(pattern: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    fn go(p: &[char], n: &[char]) -> bool {
        match (p.first(), n.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], n) || (!n.is_empty() && go(p, &n[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &n[1..]),
            (Some(a), Some(b)) if a == b => go(&p[1..], &n[1..]),
            _ => false,
        }
    }
    go(&p, &n)
}

fn arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args[key].as_str().filter(|s| !s.trim().is_empty()).ok_or_else(|| format!("{key} is required"))
}

impl System {
    pub fn new(settings: Settings, expand: fn(&str) -> PathBuf) -> Self {
        Self { settings, expand }
    }

    /// The home directory (where relative paths and commands start).
    pub fn home(&self) -> PathBuf {
        normalize(&(self.expand)("~/"))
    }

    /// A path as the tools see it: `~` expanded, symlinks resolved where they
    /// exist (so a link can't lead around the deny list), `..` removed.
    pub fn resolve(&self, path: &str) -> PathBuf {
        let path = path.trim();
        let p = if path == "~" { self.home() } else { (self.expand)(path) };
        let p = if p.is_relative() { self.home().join(p) } else { p };
        if let Ok(real) = std::fs::canonicalize(&p) {
            return real;
        }
        // Not there yet: resolve the nearest existing parent.
        let p = normalize(&p);
        let mut existing = p.clone();
        let mut rest = Vec::new();
        while !existing.exists() {
            match (existing.file_name().map(|n| n.to_os_string()), existing.parent().map(Path::to_path_buf)) {
                (Some(name), Some(parent)) => {
                    rest.push(name);
                    existing = parent;
                }
                _ => return p,
            }
        }
        let mut out = std::fs::canonicalize(&existing).unwrap_or(existing);
        out.extend(rest.iter().rev());
        out
    }

    fn under(&self, path: &Path, roots: &[String]) -> bool {
        roots.iter().filter(|r| !r.trim().is_empty()).any(|r| path.starts_with(self.resolve(r)))
    }

    fn denied(&self, path: &Path) -> Option<String> {
        self.under(path, &self.settings.deny_paths).then(|| format!("{} is off limits ([system] deny_paths)", path.display()))
    }

    /// What this call may do, decided before it runs.
    pub fn check(&self, tool: &str, args: &Value) -> Check {
        if !self.settings.enabled {
            return Check::Forbidden("system access is off ([system] enabled)".into());
        }
        let from_class = |c: Class| match c {
            Class::ReadOnly => Check::Auto,
            Class::Change(why) => Check::Ask { why, dangerous: false },
            Class::Dangerous(why) => Check::Ask { why, dangerous: true },
            Class::Forbidden(why) => Check::Forbidden(why),
        };
        match tool {
            "system_info" => Check::Auto,
            "shell_run" => match arg(args, "command") {
                Ok(c) => from_class(classify(c, &self.settings.allow_commands)),
                Err(e) => Check::Forbidden(e),
            },
            "ssh_run" => {
                let host = args["host"].as_str().unwrap_or("").trim();
                if host.is_empty() || host.starts_with('-') || !self.settings.ssh_hosts.iter().any(|h| h == host) {
                    return Check::Forbidden(format!(
                        "{host:?} isn't a configured server; add it to [system] ssh_hosts (configured: {})",
                        if self.settings.ssh_hosts.is_empty() { "none".into() } else { self.settings.ssh_hosts.join(", ") }
                    ));
                }
                match arg(args, "command") {
                    Ok(c) => from_class(classify(c, &self.settings.allow_commands)),
                    Err(e) => Check::Forbidden(e),
                }
            }
            "file_read" | "file_list" | "file_write" | "file_delete" => {
                let Ok(raw) = arg(args, "path") else { return Check::Forbidden("path is required".into()) };
                let path = self.resolve(raw);
                if let Some(why) = self.denied(&path) {
                    return Check::Forbidden(why);
                }
                match tool {
                    "file_write" if self.under(&path, &self.settings.write_roots) => Check::Auto,
                    "file_write" => Check::Ask { why: format!("writes {}", path.display()), dangerous: path.exists() && args["append"] != true },
                    "file_delete" => Check::Ask { why: format!("deletes {}", path.display()), dangerous: true },
                    _ => Check::Auto,
                }
            }
            "http_request" => {
                let method = args["method"].as_str().unwrap_or("GET").to_uppercase();
                if matches!(method.as_str(), "GET" | "HEAD") {
                    Check::Auto
                } else {
                    Check::Ask { why: format!("sends a {method} request"), dangerous: method == "DELETE" }
                }
            }
            _ => Check::Forbidden(format!("{tool} isn't a system tool")),
        }
    }

    /// For the approval prompt: what kind of thing would happen ("run a
    /// command on this machine") and exactly what (the command, the path).
    pub fn describe(&self, tool: &str, args: &Value) -> (String, String) {
        let s = |k: &str| args[k].as_str().unwrap_or("").to_string();
        match tool {
            "shell_run" => {
                let cwd = args["cwd"].as_str().filter(|c| !c.trim().is_empty()).map_or_else(|| self.home(), |c| self.resolve(c));
                ("run a command on this machine".into(), format!("{}\nin {}", s("command"), cwd.display()))
            }
            "ssh_run" => (format!("run a command on the server {}", s("host")), s("command")),
            "file_write" => (
                if args["append"] == true { "add to a file".into() } else { "write a file".into() },
                format!("{} ({} characters)", self.resolve(&s("path")).display(), s("content").chars().count()),
            ),
            "file_delete" if args["recursive"] == true => ("delete a folder and everything in it".into(), self.resolve(&s("path")).display().to_string()),
            "file_delete" => ("delete a file".into(), self.resolve(&s("path")).display().to_string()),
            "http_request" => (format!("send an HTTP {} request", args["method"].as_str().unwrap_or("GET").to_uppercase()), s("url")),
            _ => (format!("use {tool}"), args.to_string()),
        }
    }

    /// Run a call. The caller has checked it (and got approval when needed);
    /// forbidden calls are refused here too.
    pub fn call(&self, tool: &str, args: &Value) -> Result<Value, String> {
        if let Check::Forbidden(why) = self.check(tool, args) {
            return Err(why);
        }
        let timeout = |args: &Value| {
            let max = self.settings.timeout_seconds.max(1);
            Duration::from_secs(args["timeout_seconds"].as_u64().map_or(max, |t| t.clamp(1, max)))
        };
        match tool {
            "system_info" => Ok(self.info()),
            "shell_run" => {
                let mut cmd = Command::new(&self.settings.shell);
                cmd.arg("-c").arg(arg(args, "command")?);
                let cwd = args["cwd"].as_str().filter(|c| !c.trim().is_empty()).map_or_else(|| self.home(), |c| self.resolve(c));
                if !cwd.is_dir() {
                    return Err(format!("{} isn't a directory", cwd.display()));
                }
                cmd.current_dir(cwd);
                shell::run(cmd, timeout(args), self.settings.max_output)
            }
            "ssh_run" => {
                let mut cmd = Command::new("ssh");
                cmd.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-o", "StrictHostKeyChecking=accept-new", "-T"])
                    .arg(arg(args, "host")?)
                    .arg("--")
                    .arg(arg(args, "command")?);
                shell::run(cmd, timeout(args), self.settings.max_output)
            }
            "file_read" => self.read(args),
            "file_list" => self.list(args),
            "file_write" => {
                let path = self.resolve(arg(args, "path")?);
                let content = args["content"].as_str().ok_or("content is required")?;
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| format!("couldn't create {}: {e}", parent.display()))?;
                }
                let result = if args["append"] == true {
                    use std::io::Write;
                    std::fs::OpenOptions::new().create(true).append(true).open(&path).and_then(|mut f| f.write_all(content.as_bytes()))
                } else {
                    std::fs::write(&path, content)
                };
                result.map_err(|e| format!("couldn't write {}: {e}", path.display()))?;
                Ok(json!({ "result": "written", "path": path.display().to_string(), "bytes": content.len() }))
            }
            "file_delete" => {
                let path = self.resolve(arg(args, "path")?);
                let meta = std::fs::symlink_metadata(&path).map_err(|e| format!("{}: {e}", path.display()))?;
                if meta.is_dir() {
                    if args["recursive"] != true {
                        return Err(format!("{} is a directory; recursive=true deletes it and its contents", path.display()));
                    }
                    std::fs::remove_dir_all(&path)
                } else {
                    std::fs::remove_file(&path)
                }
                .map_err(|e| format!("couldn't delete {}: {e}", path.display()))?;
                Ok(json!({ "result": "deleted", "path": path.display().to_string() }))
            }
            "http_request" => self.http(args),
            _ => Err(format!("{tool} isn't a system tool")),
        }
    }

    fn read(&self, args: &Value) -> Result<Value, String> {
        let path = self.resolve(arg(args, "path")?);
        let bytes = std::fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if bytes.iter().take(8000).any(|b| *b == 0) {
            return Ok(json!({ "path": path.display().to_string(), "binary": true, "bytes": bytes.len() }));
        }
        let text = String::from_utf8_lossy(&bytes);
        let total = text.lines().count();
        let start = args["start_line"].as_u64().unwrap_or(1).max(1) as usize;
        let max = args["max_lines"].as_u64().unwrap_or(400).clamp(1, 5000) as usize;
        let chunk: Vec<&str> = text.lines().skip(start - 1).take(max).collect();
        let (content, truncated) = shell::clip(&chunk.join("\n"), self.settings.max_output);
        Ok(json!({
            "path": path.display().to_string(),
            "lines": format!("{start}-{} of {total}", (start - 1 + chunk.len()).max(start)),
            "content": content,
            "truncated": truncated || start - 1 + chunk.len() < total,
        }))
    }

    fn list(&self, args: &Value) -> Result<Value, String> {
        let root = self.resolve(arg(args, "path")?);
        if !root.is_dir() {
            return Err(format!("{} isn't a directory", root.display()));
        }
        let pattern = args["pattern"].as_str().filter(|p| !p.is_empty());
        let recursive = args["recursive"] == true;
        let mut entries = Vec::new();
        let mut stack = vec![(root.clone(), 0)];
        let mut more = false;
        while let Some((dir, depth)) = stack.pop() {
            let Ok(read) = std::fs::read_dir(&dir) else { continue };
            let mut items: Vec<_> = read.flatten().collect();
            items.sort_by_key(|e| e.file_name());
            for e in items {
                let name = e.file_name().to_string_lossy().into_owned();
                let Ok(meta) = e.metadata() else { continue };
                let path = e.path();
                if self.denied(&path).is_some() {
                    continue;
                }
                if recursive && meta.is_dir() && depth < 6 {
                    stack.push((path.clone(), depth + 1));
                }
                if pattern.is_some_and(|p| !wildcard(p, &name)) {
                    continue;
                }
                if entries.len() >= 500 {
                    more = true;
                    break;
                }
                entries.push(json!({
                    "path": path.strip_prefix(&root).unwrap_or(&path).display().to_string(),
                    "kind": if meta.is_dir() { "dir" } else if meta.file_type().is_symlink() { "link" } else { "file" },
                    "size": meta.len(),
                }));
            }
        }
        Ok(json!({ "path": root.display().to_string(), "entries": entries, "truncated": more }))
    }

    fn http(&self, args: &Value) -> Result<Value, String> {
        let url = arg(args, "url")?;
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return Err("only http:// and https:// URLs".into());
        }
        let method = reqwest::Method::from_bytes(args["method"].as_str().unwrap_or("GET").to_uppercase().as_bytes()).map_err(|e| e.to_string())?;
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(self.settings.http_timeout_seconds.max(1)))
            .build()
            .map_err(|e| e.to_string())?;
        let mut req = client.request(method, url);
        if let Some(headers) = args["headers"].as_object() {
            for (k, v) in headers {
                let v = v.as_str().unwrap_or("");
                // "$NAME" comes from the environment, never from the model.
                let v = match v.strip_prefix('$') {
                    Some(var) => std::env::var(var).map_err(|_| format!("environment variable {var} isn't set"))?,
                    None => v.to_string(),
                };
                req = req.header(k.as_str(), v);
            }
        }
        if let Some(body) = args["body"].as_str() {
            req = req.body(body.to_string());
        }
        let resp = req.send().map_err(|e| format!("request failed: {e}"))?;
        let status = resp.status().as_u16();
        let content_type = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        let text = resp.text().unwrap_or_default();
        let (body, truncated) = shell::clip(&text, self.settings.max_output);
        Ok(json!({ "status": status, "content_type": content_type, "body": body, "truncated": truncated }))
    }

    fn info(&self) -> Value {
        let read = |p: &str| std::fs::read_to_string(p).unwrap_or_default();
        let out = |cmd: &str| {
            let mut c = Command::new("sh");
            c.arg("-c").arg(cmd);
            shell::run(c, Duration::from_secs(10), 4000).ok().and_then(|v| v["stdout"].as_str().map(str::to_string)).unwrap_or_default()
        };
        let os = read("/etc/os-release").lines().find_map(|l| l.strip_prefix("PRETTY_NAME=")).unwrap_or("").trim_matches('"').to_string();
        let meminfo = read("/proc/meminfo");
        let mem = |key: &str| meminfo.lines().find(|l| l.starts_with(key)).and_then(|l| l.split_whitespace().nth(1)).and_then(|k| k.parse::<u64>().ok()).map(|kb| kb / 1024);
        let uptime_hours = read("/proc/uptime").split_whitespace().next().and_then(|s| s.parse::<f64>().ok()).map(|s| (s / 360.0).round() / 10.0);
        json!({
            "hostname": out("hostname"),
            "os": os,
            "kernel": out("uname -sr"),
            "user": std::env::var("USER").unwrap_or_default(),
            "uptime_hours": uptime_hours,
            "load": read("/proc/loadavg").split_whitespace().take(3).collect::<Vec<_>>().join(" "),
            "cpus": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),
            "memory_mb": { "total": mem("MemTotal:"), "available": mem("MemAvailable:") },
            "disks": out("df -h -x tmpfs -x devtmpfs -x squashfs -x overlay 2>/dev/null"),
            "top_processes": out("ps -eo pid,comm,%cpu,%mem --sort=-%cpu 2>/dev/null | head -8"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expand(p: &str) -> PathBuf {
        match p.strip_prefix('~') {
            Some(rest) => PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(rest.trim_start_matches('/')),
            None => PathBuf::from(p),
        }
    }

    fn system(dir: &Path) -> System {
        let settings = Settings { write_roots: vec![dir.join("work").display().to_string()], ssh_hosts: vec!["web1".into()], ..Settings::default() };
        System::new(settings, expand)
    }

    #[test]
    fn calls_are_checked_before_they_run() {
        let dir = std::env::temp_dir().join(format!("lyra-system-{}", std::process::id()));
        let s = system(&dir);
        assert!(s.home().is_absolute() && s.home().is_dir(), "{}", s.home().display());
        assert_eq!(s.resolve("~"), s.home().canonicalize().unwrap());
        assert_eq!(s.check("system_info", &json!({})), Check::Auto);
        assert_eq!(s.check("shell_run", &json!({ "command": "df -h" })), Check::Auto);
        assert!(matches!(s.check("shell_run", &json!({ "command": "rm -r x" })), Check::Ask { dangerous: true, .. }));
        assert!(matches!(s.check("shell_run", &json!({ "command": "rm -rf /" })), Check::Forbidden(_)));
        assert!(matches!(s.check("file_read", &json!({ "path": "~/.ssh/id_ed25519" })), Check::Forbidden(_)));
        assert!(matches!(s.check("file_read", &json!({ "path": "~/.ssh/../.ssh/config" })), Check::Forbidden(_)), "no way around with ..");
        assert_eq!(s.check("file_write", &json!({ "path": dir.join("work/a.txt").display().to_string(), "content": "x" })), Check::Auto);
        assert!(matches!(s.check("file_write", &json!({ "path": dir.join("elsewhere.txt").display().to_string(), "content": "x" })), Check::Ask { .. }));
        assert!(matches!(s.check("file_delete", &json!({ "path": dir.join("work/a.txt").display().to_string() })), Check::Ask { dangerous: true, .. }));
        assert_eq!(s.check("http_request", &json!({ "url": "http://x" })), Check::Auto);
        assert!(matches!(s.check("http_request", &json!({ "url": "http://x", "method": "post" })), Check::Ask { .. }));
        assert!(matches!(s.check("ssh_run", &json!({ "host": "db9", "command": "ls" })), Check::Forbidden(w) if w.contains("ssh_hosts")));
        assert!(matches!(s.check("ssh_run", &json!({ "host": "-oProxyCommand=x", "command": "ls" })), Check::Forbidden(_)));
        assert_eq!(s.check("ssh_run", &json!({ "host": "web1", "command": "uptime" })), Check::Auto);
        let off = System::new(Settings { enabled: false, ..Settings::default() }, expand);
        assert!(matches!(off.check("system_info", &json!({})), Check::Forbidden(_)));
        // Forbidden calls are refused by call() too, whatever the caller did.
        assert!(s.call("shell_run", &json!({ "command": "rm -rf /" })).is_err());
    }

    #[test]
    fn files_round_trip() {
        let dir = std::env::temp_dir().join(format!("lyra-system-files-{}", std::process::id()));
        let s = system(&dir);
        let file = dir.join("work/notes/today.txt").display().to_string();
        s.call("file_write", &json!({ "path": file, "content": "one\ntwo\n" })).unwrap();
        s.call("file_write", &json!({ "path": file, "content": "three\n", "append": true })).unwrap();
        let r = s.call("file_read", &json!({ "path": file, "start_line": 2 })).unwrap();
        assert_eq!(r["content"], "two\nthree");
        let l = s.call("file_list", &json!({ "path": dir.join("work").display().to_string(), "recursive": true, "pattern": "*.txt" })).unwrap();
        assert_eq!(l["entries"][0]["path"], "notes/today.txt");
        assert!(s.call("file_delete", &json!({ "path": dir.join("work/notes").display().to_string() })).unwrap_err().contains("recursive"));
        s.call("file_delete", &json!({ "path": dir.join("work").display().to_string(), "recursive": true })).unwrap();
        assert!(!dir.join("work").exists());
        let shell = s.call("shell_run", &json!({ "command": "echo hi", "cwd": "/tmp" })).unwrap();
        assert_eq!(shell["stdout"], "hi");
        let info = s.call("system_info", &json!({})).unwrap();
        assert!(info["cpus"].as_u64().unwrap() >= 1);
        assert!(wildcard("*.log", "app.log") && !wildcard("*.log", "app.txt") && wildcard("a?c", "abc"));
    }
}

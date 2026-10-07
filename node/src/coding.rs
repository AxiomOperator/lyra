//! Coding harnesses on this machine: Claude Code (`claude -p … --output-format
//! stream-json`) and OpenCode (`opencode run --format json`), run headless in
//! a project folder for lyra. Their JSON events become small progress lines;
//! the result says what changed (git before/after), what it cost and the
//! session to continue. They run on their own (the user chose full auto: the
//! harness approves its own steps; lyra's approval to start is the gate), but
//! never `git push` unless `allow_push`.

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Harness {
    Claude,
    OpenCode,
}

impl Harness {
    pub fn parse(s: &str) -> Option<Harness> {
        match s.trim().to_lowercase().replace([' ', '-', '_'], "").as_str() {
            "claude" | "claudecode" | "cc" => Some(Harness::Claude),
            "opencode" | "oc" => Some(Harness::OpenCode),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::OpenCode => "opencode",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Harness::Claude => "Claude Code",
            Harness::OpenCode => "OpenCode",
        }
    }

    /// Where its program is: on PATH, else where its installer puts it.
    pub fn program(self) -> Option<PathBuf> {
        let name = self.id();
        let on_path = std::env::var_os("PATH").into_iter().flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).map(|d| d.join(name)).find(|p| p.is_file());
        on_path.or_else(|| {
            let home = crate::home();
            let known: &[&str] = match self {
                Harness::Claude => &[".local/bin/claude", ".claude/local/claude", ".npm-global/bin/claude"],
                Harness::OpenCode => &[".opencode/bin/opencode", ".local/bin/opencode"],
            };
            known.iter().map(|p| home.join(p)).find(|p| p.is_file())
        })
    }
}

/// The harnesses here and their versions, for lyra's Machines page.
pub fn available() -> Value {
    let mut out = serde_json::Map::new();
    for h in [Harness::Claude, Harness::OpenCode] {
        if let Some(p) = h.program() {
            let version = Command::new(&p)
                .arg("--version")
                .stdin(Stdio::null())
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().lines().next().unwrap_or("").to_string())
                .unwrap_or_default();
            out.insert(h.id().into(), json!(version));
        }
    }
    Value::Object(out)
}

/// One job.
#[derive(Debug, Clone)]
pub struct Job {
    pub harness: Harness,
    pub dir: PathBuf,
    pub task: String,
    /// Plan only: read and propose, change nothing.
    pub plan: bool,
    /// Continue this harness session.
    pub session: Option<String>,
    pub allow_push: bool,
    pub timeout: Duration,
}

impl Job {
    pub fn from_request(v: &Value) -> Result<Job, String> {
        let harness = Harness::parse(v["harness"].as_str().unwrap_or("")).ok_or("harness is claude or opencode")?;
        let dir = crate::expand(v["dir"].as_str().filter(|d| !d.trim().is_empty()).ok_or("dir (the project folder) is required")?.trim());
        let task = v["task"].as_str().filter(|t| !t.trim().is_empty()).ok_or("task is required")?.to_string();
        Ok(Job {
            harness,
            dir,
            task,
            plan: v["mode"] == "plan",
            session: v["session"].as_str().filter(|s| !s.is_empty()).map(str::to_string),
            allow_push: v["allow_push"] == true,
            timeout: Duration::from_secs(v["timeout_minutes"].as_u64().unwrap_or(30).clamp(1, 240) * 60),
        })
    }
}

const NOTE: &str = "lyra (the user's assistant) asked for this and it runs unattended: work only in this project, \
                    don't commit unless the task asks you to, never push, and finish with a short summary of what you changed and why (or what stopped you).";

/// The command line for a job: program arguments and extra environment.
pub fn command(job: &Job) -> (Vec<String>, Vec<(String, String)>) {
    let mut args: Vec<String> = Vec::new();
    let mut env = Vec::new();
    match job.harness {
        Harness::Claude => {
            args.extend(["-p".into(), job.task.clone(), "--output-format".into(), "stream-json".into(), "--verbose".into()]);
            args.extend(["--permission-mode".into(), if job.plan { "plan".into() } else { "bypassPermissions".into() }]);
            if !job.allow_push {
                args.extend(["--disallowedTools".into(), "Bash(git push:*)".into()]);
            }
            args.extend(["--append-system-prompt".into(), NOTE.into()]);
            if let Some(s) = &job.session {
                args.extend(["--resume".into(), s.clone()]);
            }
        }
        Harness::OpenCode => {
            args.extend(["run".into(), "--format".into(), "json".into(), "--dir".into(), job.dir.display().to_string()]);
            if job.plan {
                args.extend(["--agent".into(), "plan".into()]);
            } else {
                args.push("--auto".into());
            }
            if let Some(s) = &job.session {
                args.extend(["--session".into(), s.clone()]);
            }
            args.push(format!("{}\n\n({NOTE})", job.task));
            let mut bash = serde_json::Map::new();
            if !job.allow_push {
                bash.insert("git push*".into(), json!("deny"));
            }
            let mut permission = json!({ "bash": bash });
            if job.plan {
                permission["edit"] = json!("deny");
            }
            env.push(("OPENCODE_CONFIG_CONTENT".into(), json!({ "permission": permission }).to_string()));
        }
    }
    (args, env)
}

/// What a run told so far.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct State {
    pub session: Option<String>,
    pub model: Option<String>,
    /// Its last words: the summary.
    pub text: String,
    pub error: Option<String>,
    pub cost_usd: Option<f64>,
    pub turns: u64,
    pub tokens: u64,
}

/// A tool step in a line: "Edit src/main.rs", "Bash cargo test".
fn step(name: &str, input: &Value) -> String {
    let path = ["file_path", "filePath", "path", "notebook_path"].iter().find_map(|k| input[*k].as_str()).map(|p| p.rsplit('/').take(2).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("/"));
    let detail = input["command"]
        .as_str()
        .map(str::to_string)
        .or(path)
        .or_else(|| input["pattern"].as_str().map(str::to_string))
        .or_else(|| {
            // apply_patch: the files it touches.
            input["patchText"].as_str().map(|p| {
                p.lines().filter_map(|l| l.split_once(" File: ").map(|(_, f)| f.rsplit('/').next().unwrap_or(f).to_string())).collect::<Vec<_>>().join(", ")
            })
        })
        .or_else(|| input["description"].as_str().map(str::to_string))
        .unwrap_or_default();
    let detail: String = detail.lines().next().unwrap_or("").chars().take(140).collect();
    format!("{name} {detail}").trim().to_string()
}

/// One Claude Code `stream-json` line: a progress event, if worth showing.
pub fn claude_line(line: &str, s: &mut State) -> Option<Value> {
    let e: Value = serde_json::from_str(line).ok()?;
    match e["type"].as_str()? {
        "system" if e["subtype"] == "init" => {
            s.session = e["session_id"].as_str().map(str::to_string);
            s.model = e["model"].as_str().map(str::to_string);
            Some(json!({ "kind": "start", "session": s.session, "model": s.model }))
        }
        "assistant" => {
            let mut out = None;
            for c in e["message"]["content"].as_array().into_iter().flatten() {
                match c["type"].as_str() {
                    Some("tool_use") => out = Some(json!({ "kind": "tool", "text": step(c["name"].as_str().unwrap_or("tool"), &c["input"]) })),
                    Some("text") => {
                        let t = c["text"].as_str().unwrap_or("").trim();
                        if !t.is_empty() {
                            s.text = t.to_string();
                            out = Some(json!({ "kind": "text", "text": t.chars().take(300).collect::<String>() }));
                        }
                    }
                    _ => {}
                }
            }
            out
        }
        "result" => {
            s.session = e["session_id"].as_str().map(str::to_string).or(s.session.take());
            if let Some(r) = e["result"].as_str().filter(|r| !r.trim().is_empty()) {
                s.text = r.trim().to_string();
            }
            s.cost_usd = e["total_cost_usd"].as_f64();
            s.turns = e["num_turns"].as_u64().unwrap_or(s.turns);
            if e["is_error"] == true || e["subtype"].as_str().is_some_and(|t| t != "success") {
                s.error = Some(e["subtype"].as_str().unwrap_or("error").to_string());
            }
            None
        }
        _ => None,
    }
}

/// One OpenCode `--format json` line.
pub fn opencode_line(line: &str, s: &mut State) -> Option<Value> {
    let e: Value = serde_json::from_str(line).ok()?;
    if s.session.is_none()
        && let Some(id) = e["sessionID"].as_str()
    {
        s.session = Some(id.to_string());
        return Some(json!({ "kind": "start", "session": id, "model": s.model }));
    }
    let part = &e["part"];
    match e["type"].as_str()? {
        "tool_use" => Some(json!({ "kind": "tool", "text": step(part["tool"].as_str().unwrap_or("tool"), &part["state"]["input"]) })),
        "text" => {
            let t = part["text"].as_str().unwrap_or("").trim();
            if t.is_empty() {
                return None;
            }
            s.text = t.to_string();
            Some(json!({ "kind": "text", "text": t.chars().take(300).collect::<String>() }))
        }
        "step_finish" => {
            s.turns += 1;
            s.tokens += part["tokens"]["total"].as_u64().unwrap_or(0);
            if let Some(c) = part["cost"].as_f64() {
                s.cost_usd = Some(s.cost_usd.unwrap_or(0.0) + c);
            }
            None
        }
        "error" => {
            let msg = e["error"]["data"]["message"].as_str().or(e["error"]["message"].as_str()).or(part["error"].as_str()).unwrap_or("error").to_string();
            s.error = Some(msg.clone());
            Some(json!({ "kind": "error", "text": msg }))
        }
        _ => None,
    }
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).stdin(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// What the project looks like now: HEAD (if it's a git repo).
fn head(dir: &Path) -> Option<String> {
    git(dir, &["rev-parse", "HEAD"])
}

/// What changed since `before`: files (tracked and new), a diff stat, new commits.
pub fn changes(dir: &Path, before: Option<&str>) -> Value {
    let Some(before) = before else { return json!({ "git": false }) };
    let mut files: Vec<String> = git(dir, &["diff", "--name-only", before]).unwrap_or_default().lines().map(str::to_string).collect();
    for l in git(dir, &["status", "--porcelain"]).unwrap_or_default().lines() {
        if let Some(f) = l.strip_prefix("?? ")
            && !files.iter().any(|x| x == f)
        {
            files.push(f.to_string());
        }
    }
    let stat = git(dir, &["diff", "--stat", before]).unwrap_or_default();
    let commits: Vec<String> = git(dir, &["log", "--oneline", &format!("{before}..HEAD")]).unwrap_or_default().lines().map(str::to_string).collect();
    json!({ "git": true, "files": files, "diff_stat": stat.lines().last().unwrap_or("").trim(), "commits": commits })
}

/// Stop a process and everything it started (its own process group).
fn kill_group(pid: u32) {
    let _ = Command::new("kill").args(["-TERM", &format!("-{pid}")]).status();
    std::thread::sleep(Duration::from_millis(500));
    let _ = Command::new("kill").args(["-KILL", &format!("-{pid}")]).status();
}

/// Run a job to the end (or `cancel`/timeout), telling `progress` as it goes.
pub fn run(job: &Job, cancel: &AtomicBool, progress: &dyn Fn(Value)) -> Result<Value, String> {
    use std::os::unix::process::CommandExt;
    if !job.dir.is_dir() {
        return Err(format!("{} isn't a folder here", job.dir.display()));
    }
    let program = job.harness.program().ok_or_else(|| format!("{} isn't installed here", job.harness.title()))?;
    let before = head(&job.dir);
    let (args, env) = command(job);
    let started = Instant::now();
    let mut child = Command::new(&program)
        .args(&args)
        .envs(env)
        .current_dir(&job.dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|e| format!("couldn't start {}: {e}", job.harness.title()))?;
    let pid = child.id();
    let stdout = child.stdout.take().ok_or("no output")?;
    let mut stderr = child.stderr.take().ok_or("no errors stream")?;
    let errors = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let (lines_tx, lines) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if lines_tx.send(line).is_err() {
                break;
            }
        }
    });
    let mut state = State::default();
    let mut stopped = None;
    loop {
        match lines.recv_timeout(Duration::from_millis(300)) {
            Ok(line) => {
                let event = match job.harness {
                    Harness::Claude => claude_line(&line, &mut state),
                    Harness::OpenCode => opencode_line(&line, &mut state),
                };
                if let Some(e) = event {
                    progress(e);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if cancel.load(Ordering::SeqCst) {
            stopped = Some("stopped by the user");
        } else if started.elapsed() > job.timeout {
            stopped = Some("timed out");
        }
        if stopped.is_some() {
            kill_group(pid);
            break;
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let stderr_text = errors.join().unwrap_or_default();
    let mut result = json!({
        "harness": job.harness.id(),
        "dir": job.dir.display().to_string(),
        "mode": if job.plan { "plan" } else { "edit" },
        "session": state.session,
        "model": state.model,
        "summary": state.text,
        "seconds": started.elapsed().as_secs(),
        "turns": state.turns,
        "cost_usd": state.cost_usd,
        "tokens": if state.tokens > 0 { Some(state.tokens) } else { None },
    });
    let error = stopped
        .map(str::to_string)
        .or(state.error.clone())
        .or_else(|| (!status.success()).then(|| format!("{} exited with {status}: {}", job.harness.title(), stderr_text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").chars().take(300).collect::<String>())));
    result["ok"] = json!(error.is_none());
    if let Some(e) = error {
        result["error"] = json!(e);
    }
    if let (Some(obj), Some(ch)) = (result.as_object_mut(), changes(&job.dir, before.as_deref()).as_object()) {
        for (k, v) in ch {
            obj.insert(k.clone(), v.clone());
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn replay(file: &str, f: fn(&str, &mut State) -> Option<Value>) -> (State, Vec<Value>) {
        let text = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(file)).unwrap();
        let mut s = State::default();
        let events = text.lines().filter_map(|l| f(l, &mut s)).collect();
        (s, events)
    }

    #[test]
    fn claude_code_runs_read_as_steps_and_a_result() {
        let (s, events) = replay("claude-stream.jsonl", claude_line);
        assert_eq!(s.session.as_deref(), Some("4034d1bf-44cc-48ad-a583-fcc9a43a2978"));
        assert_eq!(s.model.as_deref(), Some("claude-opus-5-5"));
        assert!(s.text.contains("README.md"), "{}", s.text);
        assert_eq!((s.turns, s.error.is_none()), (3, true));
        assert!(s.cost_usd.unwrap() > 0.0);
        let steps: Vec<&str> = events.iter().filter(|e| e["kind"] == "tool").filter_map(|e| e["text"].as_str()).collect();
        assert_eq!(steps[0], "Bash cat -n README.md");
        assert_eq!(events[0]["kind"], "start");
    }

    #[test]
    fn opencode_runs_read_as_steps_and_a_result() {
        let (s, events) = replay("opencode-run.jsonl", opencode_line);
        assert_eq!(s.session.as_deref(), Some("ses_eec16939cffeO95pnzAZSbD4ZU"));
        assert!(s.text.starts_with("Fixed the typo"), "{}", s.text);
        assert_eq!(s.turns, 3);
        assert!(s.tokens > 0);
        let steps: Vec<&str> = events.iter().filter(|e| e["kind"] == "tool").filter_map(|e| e["text"].as_str()).collect();
        assert_eq!(steps, vec!["read coderepo/README.md", "apply_patch README.md"]);
    }

    #[test]
    fn full_auto_but_never_push_and_plan_changes_nothing() {
        let job = |h: Harness, plan: bool| Job { harness: h, dir: "/tmp".into(), task: "fix it".into(), plan, session: Some("s1".into()), allow_push: false, timeout: Duration::from_secs(60) };
        let (a, _) = command(&job(Harness::Claude, false));
        let joined = a.join(" ");
        assert!(joined.contains("--permission-mode bypassPermissions") && joined.contains("--disallowedTools Bash(git push:*)") && joined.contains("--resume s1"));
        assert!(command(&job(Harness::Claude, true)).0.join(" ").contains("--permission-mode plan"));
        let (a, env) = command(&job(Harness::OpenCode, false));
        assert!(a.contains(&"--auto".to_string()) && a.join(" ").contains("--session s1"));
        assert!(env[0].1.contains("\"git push*\":\"deny\""));
        let (a, env) = command(&job(Harness::OpenCode, true));
        assert!(!a.contains(&"--auto".to_string()) && a.join(" ").contains("--agent plan") && env[0].1.contains("\"edit\":\"deny\""));
        assert_eq!(Harness::parse("Claude Code"), Some(Harness::Claude));
        assert_eq!(Harness::parse("open-code"), Some(Harness::OpenCode));
    }

    #[test]
    fn changes_are_read_from_git() {
        let dir = std::env::temp_dir().join(format!("lyra-coding-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let g = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&dir).args(args).output().unwrap().status.success(), "{args:?}");
        g(&["init", "-q"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        g(&["add", "-A"]);
        g(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-qm", "init"]);
        let before = head(&dir);
        std::fs::write(dir.join("a.txt"), "two\n").unwrap();
        std::fs::write(dir.join("b.txt"), "new\n").unwrap();
        let c = changes(&dir, before.as_deref());
        assert_eq!(c["files"], json!(["a.txt", "b.txt"]));
        assert!(c["diff_stat"].as_str().unwrap().contains("1 file changed"));
        assert_eq!(changes(&dir, None)["git"], false);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

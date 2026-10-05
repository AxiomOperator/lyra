//! The code lab (E6): source patches are tried in a throwaway git worktree at
//! a known commit, never on the running binary or the user's working copy.
//! Approval only creates a local branch with the commit; nothing is merged
//! or pushed.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A check run in the sandbox: a name and a command.
#[derive(Debug, Clone)]
pub struct Check {
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
}

impl Check {
    pub fn new(name: &str, program: &str, args: &[&str]) -> Self {
        Self { name: name.into(), program: program.into(), args: args.iter().map(|a| a.to_string()).collect() }
    }
}

/// The checks a Rust patch must pass. (No `cargo fmt --check`: lyra's code
/// isn't rustfmt-formatted, so it would fail every patch.)
pub fn cargo_checks() -> Vec<Check> {
    vec![
        Check::new("cargo clippy", "cargo", &["clippy", "--all-targets", "--workspace", "--", "-D", "warnings"]),
        Check::new("cargo test", "cargo", &["test", "--workspace"]),
    ]
}

pub struct CodeLab {
    /// The source repository (a git checkout).
    pub repo: PathBuf,
    /// Where sandbox worktrees go.
    pub work: PathBuf,
    /// Shared build directory, so sandbox builds reuse compiled dependencies.
    pub target: PathBuf,
    pub timeout: Duration,
}

impl CodeLab {
    pub fn new(repo: PathBuf, home: &Path) -> Self {
        let root = home.join("evolution");
        Self { repo, work: root.join("sandbox"), target: root.join("target"), timeout: Duration::from_secs(20 * 60) }
    }

    fn git(&self, dir: &Path, args: &[&str]) -> Result<String, String> {
        let out = Command::new("git").arg("-C").arg(dir).args(args).output().map_err(|e| format!("git: {e}"))?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            Err(format!("git {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()))
        }
    }

    /// The commit patches are made against.
    pub fn head(&self) -> Result<String, String> {
        self.git(&self.repo, &["rev-parse", "HEAD"])
    }

    /// Source files the evolver may look at.
    pub fn files(&self) -> Result<Vec<String>, String> {
        Ok(self
            .git(&self.repo, &["ls-files"])?
            .lines()
            .filter(|f| f.ends_with(".rs") || f.ends_with("Cargo.toml") || f.ends_with(".sql"))
            .map(str::to_string)
            .collect())
    }

    /// A tracked file's content at `base` (only files from `files()`).
    pub fn read(&self, base: &str, path: &str) -> Result<String, String> {
        if !self.files()?.iter().any(|f| f == path) {
            return Err(format!("{path} isn't a tracked source file"));
        }
        self.git(&self.repo, &["show", &format!("{base}:{path}")])
    }

    /// A file at `base` in one line, for picking files to change:
    /// `path (N lines): fn a, fn b, …` (Rust function names only).
    pub fn outline(&self, base: &str, path: &str) -> String {
        let Ok(text) = self.read(base, path) else { return path.to_string() };
        let fns: Vec<&str> = text
            .lines()
            .filter_map(|l| {
                let l = l.trim_start();
                let l = l.strip_prefix("pub ").or_else(|| l.strip_prefix("pub(crate) ")).unwrap_or(l);
                let l = l.strip_prefix("async ").unwrap_or(l);
                let rest = l.strip_prefix("fn ")?;
                rest.split(['(', '<']).next().filter(|n| !n.is_empty())
            })
            .take(60)
            .collect();
        let lines = text.lines().count();
        if fns.is_empty() { format!("{path} ({lines} lines)") } else { format!("{path} ({lines} lines): {}", fns.join(", ")) }
    }

    /// Apply the patch in a fresh worktree at `base` and run the checks.
    /// Returns `(check, passed, output tail)`; the worktree is removed after.
    pub fn validate(&self, id: &str, base: &str, diff: &str, checks: &[Check]) -> Vec<(String, bool, String)> {
        let mut results = Vec::new();
        let dir = self.work.join(id);
        if let Err(e) = self.prepare(&dir, base, None) {
            return vec![("worktree".into(), false, e)];
        }
        match self.apply(&dir, id, diff) {
            Ok(()) => results.push(("git apply".into(), true, String::new())),
            Err(e) => {
                results.push(("git apply".into(), false, e));
                self.cleanup(&dir);
                return results;
            }
        }
        for check in checks {
            let (ok, output) = self.run(&dir, check);
            results.push((check.name.clone(), ok, output));
            if !ok {
                break;
            }
        }
        self.cleanup(&dir);
        results
    }

    /// Commit the patch on a new local branch `evolution/<id>` at `base`.
    /// The user's checkout and branches are untouched; nothing is pushed.
    pub fn branch(&self, id: &str, base: &str, diff: &str, message: &str) -> Result<String, String> {
        let name = format!("evolution/{id}");
        let dir = self.work.join(format!("{id}-branch"));
        self.prepare(&dir, base, Some(&name))?;
        let result = self.apply(&dir, id, diff).and_then(|()| {
            self.git(&dir, &["add", "-A"])?;
            self.git(&dir, &["commit", "-q", "-m", message])
        });
        self.cleanup(&dir);
        result.map(|_| name)
    }

    fn prepare(&self, dir: &Path, base: &str, branch: Option<&str>) -> Result<(), String> {
        std::fs::create_dir_all(&self.work).map_err(|e| format!("creating {}: {e}", self.work.display()))?;
        self.cleanup(dir);
        let dir_s = dir.to_string_lossy();
        let mut args = vec!["worktree", "add", "-q"];
        match branch {
            Some(b) => args.extend(["-b", b]),
            None => args.push("--detach"),
        }
        args.extend([dir_s.as_ref(), base]);
        self.git(&self.repo, &args).map(|_| ())
    }

    fn apply(&self, dir: &Path, id: &str, diff: &str) -> Result<(), String> {
        let patch = self.work.join(format!("{id}.patch"));
        std::fs::write(&patch, diff).map_err(|e| e.to_string())?;
        let p = patch.to_string_lossy().to_string();
        // Model-written hunk headers often miscount lines; recount them from the hunks.
        let result = self
            .git(dir, &["apply", "--recount", "--check", &p])
            .and_then(|_| self.git(dir, &["apply", "--recount", &p]).map(|_| ()));
        let _ = std::fs::remove_file(&patch);
        result
    }

    fn cleanup(&self, dir: &Path) {
        if dir.exists() {
            let _ = self.git(&self.repo, &["worktree", "remove", "--force", &dir.to_string_lossy()]);
            let _ = std::fs::remove_dir_all(dir);
        }
        let _ = self.git(&self.repo, &["worktree", "prune"]);
    }

    /// Run a check with a timeout; returns whether it passed and the output's tail.
    fn run(&self, dir: &Path, check: &Check) -> (bool, String) {
        let child = Command::new(&check.program)
            .args(&check.args)
            .current_dir(dir)
            .env("CARGO_TARGET_DIR", &self.target)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match child {
            Ok(c) => c,
            Err(e) => return (false, format!("couldn't start {}: {e}", check.program)),
        };
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let mut out = String::new();
                    if let Some(mut s) = child.stdout.take() {
                        let _ = s.read_to_string(&mut out);
                    }
                    if let Some(mut s) = child.stderr.take() {
                        let _ = s.read_to_string(&mut out);
                    }
                    let tail: String = out.chars().rev().take(1500).collect::<Vec<_>>().into_iter().rev().collect();
                    return (status.success(), tail);
                }
                Ok(None) if start.elapsed() > self.timeout => {
                    let _ = child.kill();
                    return (false, format!("timed out after {}s", self.timeout.as_secs()));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(200)),
                Err(e) => return (false, e.to_string()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch git repo with one committed file.
    fn repo() -> (PathBuf, String) {
        let root = std::env::temp_dir().join(format!("lyra-lab-{}", uuid::Uuid::new_v4()));
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&repo).args(args).output().unwrap().status.success(), "{args:?}");
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "test"]);
        std::fs::write(repo.join("lib.rs"), "fn a() {}\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "init"]);
        let head = String::from_utf8(Command::new("git").arg("-C").arg(&repo).args(["rev-parse", "HEAD"]).output().unwrap().stdout).unwrap();
        (root, head.trim().to_string())
    }

    const DIFF: &str = "--- a/lib.rs\n+++ b/lib.rs\n@@ -1 +1 @@\n-fn a() {}\n+fn b() {}\n";
    /// The same change with a wrong hunk header, as models often write them.
    const MISCOUNTED: &str = "--- a/lib.rs\n+++ b/lib.rs\n@@ -1,7 +1,7 @@\n-fn a() {}\n+fn b() {}\n";

    #[test]
    fn patches_are_checked_in_a_sandbox_and_branched_without_touching_the_checkout() {
        let (root, head) = repo();
        let lab = CodeLab::new(root.join("repo"), &root);
        assert_eq!(lab.files().unwrap(), ["lib.rs"]);
        assert_eq!(lab.read(&head, "lib.rs").unwrap(), "fn a() {}");
        assert_eq!(lab.outline(&head, "lib.rs"), "lib.rs (1 lines): a");
        assert!(lab.read(&head, "../etc/passwd").is_err());

        let pass = [Check::new("has b", "grep", &["-q", "fn b", "lib.rs"])];
        let results = lab.validate("c1", &head, DIFF, &pass);
        assert!(results.iter().all(|(_, ok, _)| *ok), "{results:?}");
        let results = lab.validate("c0", &head, MISCOUNTED, &pass);
        assert!(results.iter().all(|(_, ok, _)| *ok), "{results:?}");
        let fail = [Check::new("has a", "grep", &["-q", "fn a", "lib.rs"])];
        assert!(!lab.validate("c2", &head, DIFF, &fail).last().unwrap().1);
        let bad = lab.validate("c3", &head, "--- a/lib.rs\n+++ b/lib.rs\n@@ -1 +1 @@\n-nope\n+x\n", &pass);
        assert_eq!(bad[0].0, "git apply");
        assert!(!bad[0].1);

        let branch = lab.branch("c1", &head, DIFF, "evolution: rename a to b").unwrap();
        assert_eq!(branch, "evolution/c1");
        assert_eq!(std::fs::read_to_string(root.join("repo/lib.rs")).unwrap(), "fn a() {}\n", "checkout untouched");
        let shown = Command::new("git").arg("-C").arg(root.join("repo")).args(["show", "evolution/c1:lib.rs"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&shown.stdout).trim(), "fn b() {}");
        assert!(!root.join("evolution/sandbox/c1").exists(), "worktrees cleaned up");
        let _ = std::fs::remove_dir_all(root);
    }
}

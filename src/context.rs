//! Context files that make up the system prompt:
//!
//! - `SOUL.md`  — identity: personality, tone, boundaries.
//! - `USER.md`  — profile of the person being helped.
//! - `AGENT.md` — operating rules and instructions.
//!
//! Each is looked up in the global config dir (`~/.config/lyra`), then in every
//! directory from `/` down to the working directory. The nearest `SOUL.md` and
//! `USER.md` replace any further out; every `AGENT.md` found is stacked, outermost
//! first, so more specific instructions come last and take precedence.

use std::path::{Path, PathBuf};

pub struct Source {
    pub path: PathBuf,
    pub text: String,
}

#[derive(Default)]
pub struct Context {
    pub soul: Option<Source>,
    pub user: Option<Source>,
    pub agent: Vec<Source>,
}

impl Context {
    /// Load from the global config dir and the working directory's ancestors.
    pub fn load() -> Self {
        let mut dirs: Vec<PathBuf> = Vec::new();
        dirs.extend(crate::config::dir());
        if let Ok(cwd) = std::env::current_dir() {
            let mut chain: Vec<PathBuf> = cwd.ancestors().map(Path::to_path_buf).collect();
            chain.reverse();
            dirs.extend(chain);
        }
        Self::load_from(&dirs)
    }

    /// Load from `dirs`, ordered least to most specific.
    fn load_from(dirs: &[PathBuf]) -> Self {
        let mut context = Context::default();
        let mut seen: Vec<PathBuf> = Vec::new();
        for dir in dirs {
            // Skip a dir listed twice (e.g. the config dir is also an ancestor).
            let canonical = dir.canonicalize().unwrap_or_else(|_| dir.clone());
            if seen.contains(&canonical) {
                continue;
            }
            seen.push(canonical);

            if let Some(source) = read(dir, "SOUL.md") {
                context.soul = Some(source);
            }
            if let Some(source) = read(dir, "USER.md") {
                context.user = Some(source);
            }
            context.agent.extend(read(dir, "AGENT.md"));
        }
        context
    }

    pub fn is_empty(&self) -> bool {
        self.soul.is_none() && self.user.is_none() && self.agent.is_empty()
    }

    /// The system prompt, or `None` if no context files were found.
    pub fn system_prompt(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut sections = Vec::new();
        if let Some(soul) = &self.soul {
            sections.push(format!("# Identity (SOUL.md)\n\n{}", soul.text));
        }
        if !self.agent.is_empty() {
            let mut agent = String::from(
                "# Operating instructions (AGENT.md)\n\n\
                 Layers are ordered general to specific; later layers take precedence.",
            );
            for source in &self.agent {
                agent += &format!("\n\n## {}\n\n{}", source.path.display(), source.text);
            }
            sections.push(agent);
        }
        if let Some(user) = &self.user {
            sections.push(format!("# The person you are helping (USER.md)\n\n{}", user.text));
        }
        Some(sections.join("\n\n"))
    }

    /// One line for the UI listing what was loaded.
    pub fn summary(&self) -> String {
        if self.is_empty() {
            return "no SOUL.md / USER.md / AGENT.md found".into();
        }
        let mut parts = Vec::new();
        parts.extend(self.soul.as_ref().map(|s| show(&s.path)));
        parts.extend(self.user.as_ref().map(|s| show(&s.path)));
        parts.extend(self.agent.iter().map(|s| show(&s.path)));
        format!("loaded {}", parts.join(", "))
    }
}

/// Read `dir/name`, skipping missing, unreadable or blank files.
fn read(dir: &Path, name: &str) -> Option<Source> {
    let path = dir.join(name);
    let text = std::fs::read_to_string(&path).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| Source { path, text: text.to_string() })
}

/// Path with the home directory shortened to `~`.
fn show(path: &Path) -> String {
    if let Some(home) = std::env::var_os("HOME")
        && let Ok(rest) = path.strip_prefix(home)
    {
        return format!("~/{}", rest.display());
    }
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh, empty directory under the system temp dir.
    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lyra-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, text: &str) {
        std::fs::write(dir.join(name), text).unwrap();
    }

    #[test]
    fn nearest_soul_and_user_win_and_agents_stack() {
        let global = temp_dir("global");
        let project = temp_dir("project");
        let sub = project.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        write(&global, "SOUL.md", "global soul");
        write(&global, "USER.md", "global user");
        write(&global, "AGENT.md", "global rules");
        write(&project, "SOUL.md", "project soul");
        write(&project, "AGENT.md", "project rules");
        write(&sub, "AGENT.md", "sub rules");

        let c = Context::load_from(&[global, project, sub]);
        assert_eq!(c.soul.unwrap().text, "project soul");
        assert_eq!(c.user.unwrap().text, "global user");
        let agent: Vec<_> = c.agent.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(agent, ["global rules", "project rules", "sub rules"]);
    }

    #[test]
    fn duplicate_dirs_and_blank_files_are_skipped() {
        let dir = temp_dir("dup");
        write(&dir, "AGENT.md", "rules");
        write(&dir, "SOUL.md", "  \n");
        let c = Context::load_from(&[dir.clone(), dir]);
        assert_eq!(c.agent.len(), 1);
        assert!(c.soul.is_none());
    }

    #[test]
    fn prompt_orders_soul_agent_user() {
        let dir = temp_dir("prompt");
        write(&dir, "SOUL.md", "S");
        write(&dir, "USER.md", "U");
        write(&dir, "AGENT.md", "A");
        let prompt = Context::load_from(&[dir]).system_prompt().unwrap();
        let (s, a, u) = (prompt.find("\nS").unwrap(), prompt.find("\nA").unwrap(), prompt.find("\nU").unwrap());
        assert!(s < a && a < u);
    }

    #[test]
    fn no_files_means_no_prompt() {
        let c = Context::load_from(&[temp_dir("empty")]);
        assert!(c.system_prompt().is_none());
    }
}

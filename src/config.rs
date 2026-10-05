use std::path::PathBuf;

use serde::Deserialize;

use crate::retrieval::Endpoint;

#[derive(Deserialize)]
#[serde(default)]
pub struct Config {
    pub url: String,
    pub model: String,
    /// Price per million prompt tokens (0 for a free local model).
    pub input_cost_per_mtok: f64,
    /// Price per million cached prompt tokens; defaults to the input price.
    pub cached_input_cost_per_mtok: Option<f64>,
    /// Price per million generated tokens.
    pub output_cost_per_mtok: f64,
    /// Symbol shown before costs.
    pub currency: String,
    /// Most tokens (reasoning included) for lyra's internal JSON calls: planning,
    /// verification, memory capture, skill reviews, curation. Bounds how long a
    /// reasoning model can think about them. 0 means no limit.
    pub structured_max_tokens: u32,
    /// Let the model think before answering those calls. `false` asks the server
    /// to skip thinking (`chat_template_kwargs.enable_thinking`, understood by
    /// llama.cpp and vLLM for Qwen3-style models), which is much faster.
    pub structured_thinking: bool,
    /// `[embedding]` table: embedding model endpoint.
    pub embedding: Option<Endpoint>,
    /// `[reranker]` table: reranker model endpoint.
    pub reranker: Option<Endpoint>,
    /// `[memory]` table: persistent memory tools.
    pub memory: MemoryConfig,
    /// `[learning]` table: self-learned skills.
    pub learning: LearningConfig,
    /// `[planning]` table: goals, plans and their execution.
    pub planning: PlanningConfig,
    /// `[evolution]` table: self-evolution from run telemetry.
    pub evolution: EvolutionConfig,
}

#[derive(Deserialize)]
#[serde(default)]
pub struct EvolutionConfig {
    pub enabled: bool,
    /// When to look for improvements on its own: `manual`, `daily` or `weekly`.
    pub review: Schedule,
    /// Recent tasks replayed (sandboxed) to compare a candidate with the baseline.
    pub benchmark_tasks: usize,
    /// A git checkout of lyra's source. Only with this set can evolution
    /// propose code changes, which are tried in a throwaway worktree and, when
    /// approved, committed to a local `evolution/<id>` branch (never merged or pushed).
    pub source_repo: Option<String>,
    /// `mode`, `window`, `monitor_runs`, `monitor_drop` and the
    /// `[evolution.thresholds]` and `[evolution.fitness]` tables.
    #[serde(flatten)]
    pub settings: lyra_evolution::Settings,
}

impl Default for EvolutionConfig {
    fn default() -> Self {
        Self { enabled: true, review: Schedule::Manual, benchmark_tasks: 3, source_repo: None, settings: Default::default() }
    }
}

impl EvolutionConfig {
    pub fn source_repo(&self) -> Option<PathBuf> {
        self.source_repo.as_deref().and_then(expand)
    }
}

#[derive(Deserialize)]
#[serde(default)]
pub struct PlanningConfig {
    pub enabled: bool,
    /// SQLite file; defaults to `~/.lyra/plans/plans.db`.
    pub path: Option<String>,
    /// Most plan steps run at once.
    pub max_parallel: usize,
    /// Tools plan steps may never use.
    pub forbidden_tools: Vec<String>,
    /// `[planning.budget]`: limits per plan.
    pub budget: lyra_execution::Budget,
}

impl Default for PlanningConfig {
    fn default() -> Self {
        Self { enabled: true, path: None, max_parallel: 3, forbidden_tools: Vec::new(), budget: Default::default() }
    }
}

impl PlanningConfig {
    pub fn path(&self) -> Option<PathBuf> {
        data_file(self.path.as_deref(), "plans", "plans.db")
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct LearningConfig {
    /// Folder of skill files (`<name>.md`); defaults to `~/.lyra/skills`.
    pub dir: Option<String>,
    /// When to curate the collection on its own: `manual`, `daily` or `weekly`.
    pub curate: Schedule,
    /// `mode`, `min_confidence`, `max_skills`, `duplicate_threshold` and the
    /// `[learning.scoring]` and `[learning.lifecycle]` tables.
    #[serde(flatten)]
    pub settings: lyra_learning::Settings,
}

#[derive(Deserialize, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Schedule {
    #[default]
    Manual,
    Daily,
    Weekly,
}

impl Schedule {
    /// Days between automatic runs, or `None` for manual only.
    pub fn every_days(self) -> Option<i64> {
        match self {
            Schedule::Manual => None,
            Schedule::Daily => Some(1),
            Schedule::Weekly => Some(7),
        }
    }
}

impl LearningConfig {
    pub fn dir(&self) -> Option<PathBuf> {
        match self.dir.as_deref() {
            Some(dir) => expand(dir),
            None => Some(home()?.join("skills")),
        }
    }
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum MemoryBackend {
    #[default]
    Lance,
    Sqlite,
}

#[derive(Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    /// Offer the memory tools to the model. Needs a server with tool calling.
    pub enabled: bool,
    /// Where memories are stored: `lance` (LanceDB, the default) or `sqlite`.
    pub backend: MemoryBackend,
    /// The LanceDB directory (default `~/.lyra/memory/lance`) or SQLite file
    /// (default `~/.lyra/memory/memory.db`). A leading `~/` is expanded.
    pub path: Option<String>,
    /// LanceDB table for memories.
    pub table: String,
    /// Collections with this many vectors get a vector index; below it,
    /// search is exact (L15). 0 means never index.
    pub vector_index_threshold: usize,
    /// When to curate the collection on its own: `manual`, `daily` or `weekly`.
    pub curate: Schedule,
    /// The project being worked on: its `project:<name>` memories are
    /// recalled, other projects' aren't. `auto` (the default) uses the name of
    /// the git checkout lyra was started in; `none` turns it off.
    pub project: Option<String>,
    /// `default_scope`, `allowed_scopes`, `capture`, `maintenance`, `inject`, the
    /// `[memory.context]`, `[memory.ranking]` and `[memory.half_life_days]` tables, ...
    #[serde(flatten)]
    pub settings: lyra_memory::Settings,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            backend: MemoryBackend::Lance,
            path: None,
            table: "memories".into(),
            vector_index_threshold: 10_000,
            curate: Schedule::Manual,
            project: None,
            settings: Default::default(),
        }
    }
}

impl MemoryConfig {
    pub fn path(&self) -> Option<PathBuf> {
        match self.backend {
            MemoryBackend::Lance if self.points_at_sqlite() => data_file(None, "memory", "lance"),
            MemoryBackend::Lance => data_file(self.path.as_deref(), "memory", "lance"),
            MemoryBackend::Sqlite => data_file(self.path.as_deref(), "memory", "memory.db"),
        }
    }

    /// An older config's `path` naming the SQLite file, which LanceDB (a
    /// directory) can't use; the default LanceDB directory is used instead.
    pub fn points_at_sqlite(&self) -> bool {
        self.backend == MemoryBackend::Lance
            && self.path.as_deref().and_then(expand).is_some_and(|p| p.extension().is_some_and(|e| e == "db") || p.is_file())
    }

    /// The current project's name, if any (see `project`).
    pub fn project(&self) -> Option<String> {
        match self.project.as_deref().map(str::trim) {
            None | Some("auto") => detect_project(),
            Some("" | "none") => None,
            Some(name) => Some(name.to_string()),
        }
    }
}

/// The git checkout containing the working directory, by folder name.
fn detect_project() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let root = cwd.ancestors().find(|d| d.join(".git").exists())?;
    let name = root.file_name()?.to_string_lossy().to_lowercase();
    let name: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    (!name.is_empty()).then_some(name)
}

/// `custom` with a leading `~/` expanded, or `<lyra home>/<folder>/<name>`.
fn data_file(custom: Option<&str>, folder: &str, name: &str) -> Option<PathBuf> {
    match custom {
        Some(p) => expand(p),
        None => Some(home()?.join(folder).join(name)),
    }
}

/// A configured path, with a leading `~/` expanded.
fn expand(path: &str) -> Option<PathBuf> {
    match path.strip_prefix("~/") {
        Some(rest) => Some(user_home()?.join(rest)),
        None => Some(PathBuf::from(path)),
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            url: "http://localhost:11434/v1".into(),
            model: "llama3.2".into(),
            input_cost_per_mtok: 0.0,
            cached_input_cost_per_mtok: None,
            output_cost_per_mtok: 0.0,
            currency: "$".into(),
            structured_max_tokens: 8192,
            structured_thinking: true,
            embedding: None,
            reranker: None,
            memory: MemoryConfig::default(),
            learning: LearningConfig::default(),
            planning: PlanningConfig::default(),
            evolution: EvolutionConfig::default(),
        }
    }
}

impl Config {
    /// Load from the config file (if present), then apply env var overrides.
    pub fn load() -> Result<Self, String> {
        let mut config = match path() {
            Some(path) if path.exists() => {
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?
            }
            _ => Config::default(),
        };
        if let Ok(url) = std::env::var("LYRA_URL") {
            config.url = url;
        }
        if let Ok(model) = std::env::var("LYRA_MODEL") {
            config.model = model;
        }
        Ok(config)
    }
}

fn user_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Where everything lyra keeps lives: `$LYRA_HOME`, or `~/.lyra`.
///
/// ```text
/// ~/.lyra/
/// ├── config/    config.toml, behavior.toml (evolved behavior)
/// ├── context/   SOUL.md, USER.md, AGENT.md
/// ├── evolution/ evolution.db (runs, candidates, generations)
/// ├── memory/    lance/ (memories, LanceDB), backups
/// ├── plans/     plans.db
/// ├── skills/    <name>.md, one per skill
/// ├── tools/     <name>.toml, composite tools
/// └── workflows/ <name>.toml
/// ```
pub fn home() -> Option<PathBuf> {
    match std::env::var_os("LYRA_HOME") {
        Some(dir) => Some(PathBuf::from(dir)),
        None => Some(user_home()?.join(".lyra")),
    }
}

/// `<lyra home>/config`: config.toml.
pub fn dir() -> Option<PathBuf> {
    Some(home()?.join("config"))
}

/// `<lyra home>/context`: the global SOUL.md, USER.md and AGENT.md.
pub fn context_dir() -> Option<PathBuf> {
    Some(home()?.join("context"))
}

/// `<config dir>/config.toml`.
pub fn path() -> Option<PathBuf> {
    Some(dir()?.join("config.toml"))
}

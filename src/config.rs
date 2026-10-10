use std::path::PathBuf;

use serde::Deserialize;

use crate::retrieval::Endpoint;

#[derive(Deserialize)]
#[serde(default)]
pub struct Config {
    pub url: String,
    pub model: String,
    /// The model can see images (attached photos go to it as images).
    pub vision: bool,
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
    /// `[decide]` table: a decision model (clef-flash) for yes/no and
    /// pick-one questions; without it the chat model answers them.
    pub decide: Option<crate::decide::Settings>,
    /// `[vision_model]` table: a model that looks at pictures and scanned
    /// PDFs and answers in text (for a chat model that can't see).
    pub vision_model: Option<crate::vision::Settings>,
    /// Answers in the chat model's place while it can't be reached.
    #[serde(default)]
    pub fallback_model: Option<crate::fallback::Settings>,
    /// `[coding]` table: coding work handed to Claude Code / OpenCode.
    pub coding: crate::coding::Settings,
    /// `[diagnose]` table: problems researched by themselves.
    pub diagnose: crate::diagnose::Settings,
    /// `[status]` table: checking what lyra depends on.
    pub status: crate::status::Settings,
    /// `[health]` table: machine health limits and alerts.
    pub health: crate::health::Settings,
    /// `[briefing]` table: the daily briefing (lyra serve).
    pub briefing: crate::briefing::Settings,
    /// `[planner]` table: plan my day (focus blocks, working hours, quiet times).
    pub planner: crate::planner::Settings,
    /// `[proactive]` table: meeting prep, mail triage (tasks, flags, drafts), follow-ups.
    pub proactive: crate::proactive::Settings,
    /// `[recap]`: the end-of-day recap.
    pub recap: crate::recap::Settings,
    /// `[email]`: email from lyra to each person (its service's key is in secrets.toml).
    pub email: crate::mailout::Settings,
    /// `[pmi]` table: PMI, the project-management app (its token is in secrets.toml).
    pub pmi: crate::pmi::Settings,
    /// `[groups]` table: named sets of machines (web = ["web1", "web2"]) for @group and fleet_run.
    pub groups: std::collections::HashMap<String, Vec<String>>,
    /// `[backup]` table: nightly backups of the lyra home.
    pub backup: crate::backup::Settings,
    /// `[memory]` table: persistent memory tools.
    pub memory: MemoryConfig,
    /// `[learning]` table: self-learned skills.
    pub learning: LearningConfig,
    /// `[planning]` table: goals, plans and their execution.
    pub planning: PlanningConfig,
    /// `[evolution]` table: self-evolution from run telemetry.
    pub evolution: EvolutionConfig,
    /// `[capabilities]` table: discovery, policy and external tool providers.
    pub capabilities: CapabilitiesConfig,
    /// `[goals]` table: long-lived goals, their scheduling and autonomy.
    pub goals: GoalsConfig,
    /// `[agents]` table: specialist subagents and how work is handed to them.
    pub agents: crate::agents::Settings,
    /// `[system]` table: shell, files, network and servers for agents.
    pub system: lyra_system::Settings,
    /// `[web]` table: `lyra serve` for phones and browsers.
    pub web: lyra_web::Settings,
    /// `[search]` table: web search (SearXNG) and reading pages.
    pub search: crate::websearch::Settings,
}

#[derive(Deserialize)]
#[serde(default)]
pub struct GoalsConfig {
    pub enabled: bool,
    /// Seconds between checks of triggers, blockers and autonomous work.
    pub tick_seconds: u64,
    /// `stale_days`, `[goals.autonomy]` and `[goals.priority]`.
    #[serde(flatten)]
    pub settings: lyra_goals::Settings,
}

impl Default for GoalsConfig {
    fn default() -> Self {
        Self { enabled: true, tick_seconds: 60, settings: Default::default() }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
pub struct CapabilitiesConfig {
    /// `[[capabilities.openapi]]`: OpenAPI specs whose operations become capabilities.
    pub openapi: Vec<lyra_capabilities::openapi::OpenApiConfig>,
    /// `[[capabilities.mcp]]`: MCP servers (stdio) whose tools become capabilities.
    pub mcp: Vec<lyra_capabilities::mcp::McpConfig>,
    /// `max_tools`, `discovery_limit`, `[capabilities.policy]` and `[capabilities.scoring]`.
    #[serde(flatten)]
    pub settings: lyra_capabilities::Settings,
}

/// A configured path with `~/` expanded (for capability specs).
pub fn expand_path(path: &str) -> PathBuf {
    expand(path).unwrap_or_else(|| PathBuf::from(path))
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
            vision: false,
            input_cost_per_mtok: 0.0,
            cached_input_cost_per_mtok: None,
            output_cost_per_mtok: 0.0,
            currency: "$".into(),
            structured_max_tokens: 8192,
            structured_thinking: true,
            embedding: None,
            reranker: None,
            decide: None,
            vision_model: None,
            fallback_model: None,
            backup: Default::default(),
            groups: Default::default(),
            health: Default::default(),
            briefing: Default::default(),
            planner: Default::default(),
            proactive: Default::default(),
            recap: Default::default(),
            email: Default::default(),
            pmi: Default::default(),
            status: Default::default(),
            diagnose: Default::default(),
            coding: Default::default(),
            memory: MemoryConfig::default(),
            learning: LearningConfig::default(),
            planning: PlanningConfig::default(),
            evolution: EvolutionConfig::default(),
            capabilities: CapabilitiesConfig::default(),
            goals: GoalsConfig::default(),
            agents: crate::agents::Settings::default(),
            system: lyra_system::Settings::default(),
            web: lyra_web::Settings::default(),
            search: crate::websearch::Settings::default(),
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
/// ├── capabilities/ capabilities.db (usage), index/ (discovery, LanceDB)
/// ├── evolution/ evolution.db (runs, candidates, generations)
/// ├── goals/     goals.db (long-lived goals, their plans, blockers, triggers)
/// ├── memory/    lance/ (memories, LanceDB), backups
/// ├── plans/     plans.db
/// ├── skills/    <name>.md, one per skill
/// ├── tools/     <name>.toml, composite tools
/// └── workflows/ <name>.toml
/// ```
pub fn home() -> Option<PathBuf> {
    // Tests never touch a real home: their own folder, or one for this run.
    #[cfg(test)]
    {
        return Some(TEST_HOME.with(|h| h.borrow().clone()).unwrap_or_else(|| std::env::temp_dir().join(format!("lyra-test-home-{}", std::process::id()))));
    }
    #[allow(unreachable_code)]
    match std::env::var_os("LYRA_HOME") {
        Some(dir) => Some(PathBuf::from(dir)),
        None => Some(user_home()?.join(".lyra")),
    }
}

#[cfg(test)]
thread_local! {
    static TEST_HOME: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Run `f` with lyra's home in a fresh folder of its own (tests: per-person
/// stores, routines, email, written for real and looked at).
#[cfg(test)]
pub fn with_test_home<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
    let dir = std::env::temp_dir().join(format!("lyra-home-{}", lyra_memory::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("a test home");
    let before = TEST_HOME.with(|h| h.replace(Some(dir.clone())));
    let out = f(&dir);
    TEST_HOME.with(|h| *h.borrow_mut() = before);
    let _ = std::fs::remove_dir_all(&dir);
    out
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

/// Change values in config.toml from inside lyra (`/model`, the app's rules
/// pages), keeping the rest of the file and its comments as they are.
/// `edit` gets the document; tables it names are created when missing.
pub fn update(edit: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<(), String>) -> Result<PathBuf, String> {
    let path = path().ok_or("no home directory")?;
    update_file(&path, edit)?;
    Ok(path)
}

pub fn update_file(path: &std::path::Path, edit: impl FnOnce(&mut toml_edit::DocumentMut) -> Result<(), String>) -> Result<(), String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("{} doesn't parse: {e}", path.display()))?;
    edit(&mut doc)?;
    let out = doc.to_string();
    // Never write something lyra couldn't read back.
    toml::from_str::<Config>(&out).map_err(|e| format!("the change would break {}: {e}", path.display()))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, out).map_err(|e| format!("couldn't write {}: {e}", tmp.display()))?;
    // It may hold keys: keep its permissions.
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("couldn't replace {}: {e}", path.display()))
}

/// A list of strings as a TOML array.
pub fn strings(list: &[String]) -> toml_edit::Item {
    toml_edit::value(list.iter().map(String::as_str).collect::<toml_edit::Array>())
}

/// `[system]` written from settings, keeping comments around the table.
pub fn set_system(doc: &mut toml_edit::DocumentMut, s: &lyra_system::Settings) {
    let table = doc.entry("system").or_insert(toml_edit::table());
    if let Some(t) = table.as_table_mut() {
        t["enabled"] = toml_edit::value(s.enabled);
        t["shell"] = toml_edit::value(s.shell.as_str());
        t["timeout_seconds"] = toml_edit::value(s.timeout_seconds as i64);
        t["max_output"] = toml_edit::value(s.max_output as i64);
        t["allow_commands"] = strings(&s.allow_commands);
        t["write_roots"] = strings(&s.write_roots);
        t["deny_paths"] = strings(&s.deny_paths);
        t["ssh_hosts"] = strings(&s.ssh_hosts);
        t["http_timeout_seconds"] = toml_edit::value(s.http_timeout_seconds as i64);
        t["approval_timeout_seconds"] = toml_edit::value(s.approval_timeout_seconds as i64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// config.example.toml with its settings uncommented (`# [table]`, `# key = value`).
    fn example_uncommented() -> String {
        let text = include_str!("../config.example.toml");
        let setting = |l: &str| {
            let l = l.trim_start();
            l.starts_with('[') || l.split_once(" = ").is_some_and(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '"' || c == '.' || c == '-'))
        };
        text.lines().map(|l| match l.strip_prefix("# ") {
            Some(rest) if setting(rest) => rest,
            _ => l,
        }).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn the_example_config_reads_and_shows_every_section() {
        // Every setting in it, turned on, still reads as lyra's config.
        let text = example_uncommented();
        let c: Config = toml::from_str(&text).unwrap_or_else(|e| panic!("config.example.toml, uncommented: {e}"));
        assert!(c.vision_model.is_some() && c.decide.is_some(), "its commented tables come through");
        // Every model's own prices read (whole numbers as well as decimals).
        let priced: Config = toml::from_str("[embedding]\nurl = \"u\"\nmodel = \"e\"\ninput_cost_per_mtok = 1\n[decide]\nurl = \"u\"\noutput_cost_per_mtok = 0.5\n").unwrap();
        assert_eq!(priced.embedding.unwrap().price.input_cost_per_mtok, 1.0);
        assert_eq!(priced.decide.unwrap().price.output_cost_per_mtok, 0.5);
        // Every table of `Config` (a field that isn't a plain value) has a `[name]` in it.
        let src = include_str!("config.rs");
        let body = &src[src.find("pub struct Config {").unwrap()..];
        let body = &body[..body.find("\n}").unwrap()];
        let plain = ["String", "bool", "f64", "u32", "u64", "usize", "Option<f64>", "Option<String>"];
        let missing: Vec<&str> = body
            .lines()
            .filter_map(|l| l.trim().strip_prefix("pub ")?.split_once(": "))
            .filter(|(_, ty)| !plain.contains(&ty.trim_end_matches(',')))
            .map(|(name, _)| name)
            .filter(|name| !text.contains(&format!("[{name}]")) && !text.contains(&format!("[{name}.")) && !text.contains(&format!("[[{name}.")))
            .collect();
        assert!(missing.is_empty(), "config.example.toml has no section for: {missing:?}");
    }

    #[test]
    fn edits_keep_the_rest_of_config_toml() {
        let dir = std::env::temp_dir().join(format!("lyra-config-edit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, "# my endpoint\nurl = \"http://x/v1\"\nmodel = \"a\"   # the usual one\n\n[search]\nmax_results = 3\n").unwrap();
        update_file(&path, |doc| {
            doc["model"] = toml_edit::value("b");
            set_system(doc, &lyra_system::Settings { allow_commands: vec!["git status".into()], ..Default::default() });
            Ok(())
        })
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my endpoint") && text.contains("max_results = 3"), "{text}");
        let c: Config = toml::from_str(&text).unwrap();
        assert_eq!((c.model.as_str(), c.system.allow_commands.len()), ("b", 1));
        assert!(update_file(&path, |doc| {
            doc["model"] = toml_edit::value(3);
            Ok(())
        })
        .is_err(), "a change lyra couldn't read back isn't written");
        assert!(std::fs::read_to_string(&path).unwrap().contains("model = \"b\""));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

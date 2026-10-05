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
    /// `[embedding]` table: embedding model endpoint.
    pub embedding: Option<Endpoint>,
    /// `[reranker]` table: reranker model endpoint.
    pub reranker: Option<Endpoint>,
    /// `[memory]` table: persistent memory tools.
    pub memory: MemoryConfig,
    /// `[learning]` table: self-learned skills.
    pub learning: LearningConfig,
}

#[derive(Deserialize)]
#[serde(default)]
pub struct LearningConfig {
    /// `off`, `propose` (lessons wait for /approve) or `auto` (used straight away).
    pub mode: String,
    /// SQLite file; defaults to `<data dir>/lyra/data/skills.db`.
    pub path: Option<String>,
    /// Lessons the reviewer is less sure of than this are dropped.
    pub min_confidence: f32,
    /// Most skills added to the system prompt per message.
    pub max_skills: usize,
}

impl Default for LearningConfig {
    fn default() -> Self {
        Self { mode: "propose".into(), path: None, min_confidence: 0.6, max_skills: 3 }
    }
}

impl LearningConfig {
    pub fn path(&self) -> Option<PathBuf> {
        data_file(self.path.as_deref(), "skills.db")
    }
}

#[derive(Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    /// Offer the memory tools to the model. Needs a server with tool calling.
    pub enabled: bool,
    /// SQLite file; defaults to `$XDG_DATA_HOME/lyra/data/memory.db`
    /// (`~/.local/share/lyra/data/memory.db`). A leading `~/` is expanded.
    pub path: Option<String>,
    /// Scope for `memory_remember` when the model doesn't give one.
    pub default_scope: String,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self { enabled: true, path: None, default_scope: "user".into() }
    }
}

impl MemoryConfig {
    pub fn path(&self) -> Option<PathBuf> {
        data_file(self.path.as_deref(), "memory.db")
    }
}

/// `custom` with a leading `~/` expanded, or `$XDG_DATA_HOME/lyra/data/<name>`
/// (`~/.local/share/lyra/data/<name>`).
fn data_file(custom: Option<&str>, name: &str) -> Option<PathBuf> {
    let home = || std::env::var_os("HOME").map(PathBuf::from);
    match custom {
        Some(p) => match p.strip_prefix("~/") {
            Some(rest) => Some(home()?.join(rest)),
            None => Some(PathBuf::from(p)),
        },
        None => {
            let base = std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .or_else(|| Some(home()?.join(".local").join("share")))?;
            Some(base.join("lyra").join("data").join(name))
        }
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
            embedding: None,
            reranker: None,
            memory: MemoryConfig::default(),
            learning: LearningConfig::default(),
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
        config.learning.mode.parse::<lyra_learning::Mode>()?;
        Ok(config)
    }
}

/// `$XDG_CONFIG_HOME/lyra`, falling back to `~/.config/lyra`.
pub fn dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("lyra"))
}

/// `<config dir>/config.toml`.
pub fn path() -> Option<PathBuf> {
    Some(dir()?.join("config.toml"))
}

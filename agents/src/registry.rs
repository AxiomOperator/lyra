//! The agent registry (A2): profiles as TOML files in `~/.lyra/agents`
//! (readable, editable, shareable), every change versioned (A13), and the
//! log of delegations with their outcomes (for self-learning and evolution).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use tokio::runtime::Handle;
use uuid::Uuid;

use crate::model::*;

fn time(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn parse_time(s: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(s)?.with_timezone(&Utc))
}

/// One delegation, as logged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DelegationRecord {
    pub id: Uuid,
    pub run_id: Option<Uuid>,
    pub from_agent: String,
    pub agent: String,
    pub task: String,
    pub method: String,
    pub confidence: Option<f32>,
    pub status: String,
    pub output: String,
    pub duration_ms: u64,
    pub model_calls: u32,
    pub tool_calls: u32,
    pub tokens: u64,
    pub outcome: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// How an agent has been doing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentStats {
    pub delegations: u32,
    pub failed: u32,
    /// The user corrected what it produced.
    pub corrected: u32,
    pub praised: u32,
    pub average_ms: u64,
}

pub struct AgentRegistry {
    dir: PathBuf,
    pool: SqlitePool,
    rt: Handle,
}

impl AgentRegistry {
    /// Profiles in `dir`, history in `dir/agents.db`. Installs the default
    /// agents when there are none.
    pub fn open(dir: &Path, rt: Handle) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let db = dir.join("agents.db");
        let pool = rt.block_on(async {
            let options = SqliteConnectOptions::new().filename(&db).create_if_missing(true).journal_mode(SqliteJournalMode::Wal);
            let pool = SqlitePoolOptions::new().connect_with(options).await?;
            sqlx::migrate!("./migrations").run(&pool).await?;
            anyhow::Ok(pool)
        })?;
        let r = Self { dir: dir.to_path_buf(), pool, rt };
        if r.files()?.is_empty() {
            for name in crate::templates::DEFAULTS {
                if let Some(p) = crate::templates::template(name) {
                    r.create(p, "installed by default").map_err(anyhow::Error::msg)?;
                }
            }
        }
        Ok(r)
    }

    fn db<T>(&self, f: impl Future<Output = Result<T>>) -> Result<T, String> {
        self.rt.block_on(f).map_err(|e| format!("{e:#}"))
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.toml"))
    }

    fn files(&self) -> Result<Vec<PathBuf>> {
        Ok(std::fs::read_dir(&self.dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "toml"))
            .collect())
    }

    fn read(path: &Path) -> Result<AgentProfile, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut p: AgentProfile = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        // The file name is the agent's name.
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            p.name = stem.to_string();
        }
        Ok(p)
    }

    fn write(&self, p: &AgentProfile) -> Result<(), String> {
        let text = toml::to_string_pretty(p).map_err(|e| e.to_string())?;
        // Atomically: a temp file, flushed, renamed over it.
        let path = self.path(&p.name);
        let tmp = path.with_extension(format!("toml.tmp-{}", std::process::id()));
        let done = (|| {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(text.as_bytes())?;
            f.sync_all()?;
            std::fs::rename(&tmp, &path)
        })();
        if done.is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
        done.map_err(|e| e.to_string())
    }

    /// Every agent (the main one isn't a file), by name; files that don't
    /// parse are reported in `errors`.
    pub fn list(&self) -> (Vec<AgentProfile>, Vec<String>) {
        let mut agents = Vec::new();
        let mut errors = Vec::new();
        for f in self.files().unwrap_or_default() {
            match Self::read(&f) {
                Ok(p) => agents.push(p),
                Err(e) => errors.push(e),
            }
        }
        agents.sort_by(|a, b| a.name.cmp(&b.name));
        (agents, errors)
    }

    pub fn enabled(&self) -> Vec<AgentProfile> {
        self.list().0.into_iter().filter(|a| a.enabled).collect()
    }

    pub fn get(&self, name: &str) -> Option<AgentProfile> {
        Self::read(&self.path(name)).ok()
    }

    /// By name, title or a unique prefix.
    pub fn find(&self, key: &str) -> Result<AgentProfile, String> {
        let slug = AgentProfile::slug(key);
        if let Some(p) = self.get(&slug) {
            return Ok(p);
        }
        let all = self.list().0;
        let matches: Vec<&AgentProfile> = all.iter().filter(|a| a.name.starts_with(&slug) || a.title.to_lowercase() == key.trim().to_lowercase()).collect();
        match matches.len() {
            1 => Ok(matches[0].clone()),
            0 => Err(format!("no agent {key:?} — /agents lists them")),
            n => Err(format!("{n} agents match {key:?}")),
        }
    }

    /// Add a new agent (version 1).
    pub fn create(&self, mut p: AgentProfile, reason: &str) -> Result<AgentProfile, String> {
        validate(&p)?;
        if self.path(&p.name).exists() {
            return Err(format!("an agent named {} already exists", p.name));
        }
        let now = Utc::now();
        p.version = 1;
        p.created_at = now;
        p.updated_at = now;
        self.write(&p)?;
        self.record_version(&p, reason)?;
        Ok(p)
    }

    /// Save a changed agent as a new version (A13).
    pub fn update(&self, mut p: AgentProfile, reason: &str) -> Result<AgentProfile, String> {
        validate(&p)?;
        let current = self.get(&p.name).ok_or_else(|| format!("no agent {}", p.name))?;
        p.version = self.latest_version(&p.name)?.max(current.version) + 1;
        p.created_at = current.created_at;
        p.updated_at = Utc::now();
        self.write(&p)?;
        self.record_version(&p, reason)?;
        Ok(p)
    }

    /// Files edited by hand since their last recorded version get a version.
    pub fn sync(&self) -> Vec<String> {
        let mut notes = Vec::new();
        for p in self.list().0 {
            let latest = self.versions(&p.name).ok().and_then(|v| v.into_iter().next());
            let changed = latest.as_ref().is_none_or(|v| {
                let mut snap: AgentProfile = match serde_json::from_value(v.profile_snapshot.clone()) {
                    Ok(s) => s,
                    Err(_) => return true,
                };
                snap.updated_at = p.updated_at;
                snap != p
            });
            if changed {
                let mut p2 = p.clone();
                p2.version = latest.map_or(1, |v| v.version + 1);
                if self.write(&p2).is_ok() && self.record_version(&p2, "edited by hand").is_ok() {
                    notes.push(format!("agent {} edited by hand: now v{}", p2.name, p2.version));
                }
            }
        }
        notes
    }

    pub fn set_enabled(&self, key: &str, enabled: bool) -> Result<AgentProfile, String> {
        let mut p = self.find(key)?;
        p.enabled = enabled;
        self.update(p, if enabled { "enabled" } else { "disabled" })
    }

    /// Remove the file; its versions stay, so it can be restored.
    pub fn delete(&self, key: &str) -> Result<String, String> {
        let p = self.find(key)?;
        std::fs::remove_file(self.path(&p.name)).map_err(|e| e.to_string())?;
        Ok(p.name)
    }

    fn record_version(&self, p: &AgentProfile, reason: &str) -> Result<(), String> {
        let snapshot = serde_json::to_string(p).map_err(|e| e.to_string())?;
        self.db(async {
            sqlx::query("INSERT INTO agent_versions (name, agent_id, version, snapshot, reason, created_at) VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT (name, version) DO UPDATE SET agent_id = excluded.agent_id, snapshot = excluded.snapshot, reason = excluded.reason, created_at = excluded.created_at")
                .bind(&p.name)
                .bind(p.id.to_string())
                .bind(p.version as i64)
                .bind(&snapshot)
                .bind(reason)
                .bind(time(Utc::now()))
                .execute(&self.pool)
                .await?;
            Ok(())
        })
    }

    fn latest_version(&self, name: &str) -> Result<u32, String> {
        Ok(self.versions(name)?.first().map_or(0, |v| v.version))
    }

    /// Newest first.
    pub fn versions(&self, name: &str) -> Result<Vec<AgentVersion>, String> {
        self.db(async {
            let rows = sqlx::query("SELECT * FROM agent_versions WHERE name = ? ORDER BY version DESC").bind(name).fetch_all(&self.pool).await?;
            rows.iter()
                .map(|r| {
                    Ok(AgentVersion {
                        agent_id: Uuid::parse_str(r.try_get("agent_id")?)?,
                        name: r.try_get("name")?,
                        version: r.try_get::<i64, _>("version")? as u32,
                        profile_snapshot: serde_json::from_str(r.try_get("snapshot")?)?,
                        reason: r.try_get("reason")?,
                        created_at: parse_time(r.try_get("created_at")?)?,
                    })
                })
                .collect()
        })
    }

    /// Restore an earlier version (the previous one by default) as a new version.
    pub fn rollback(&self, key: &str, to: Option<u32>) -> Result<AgentProfile, String> {
        let name = match self.find(key) {
            Ok(p) => p.name,
            Err(_) => AgentProfile::slug(key),
        };
        let versions = self.versions(&name)?;
        let current = versions.first().ok_or_else(|| format!("no history for {name}"))?.version;
        let target = to.unwrap_or(current.saturating_sub(1));
        let v = versions.iter().find(|v| v.version == target).ok_or_else(|| format!("{name} has no version {target}"))?;
        let p: AgentProfile = serde_json::from_value(v.profile_snapshot.clone()).map_err(|e| e.to_string())?;
        if self.get(&name).is_none() {
            // Restoring a deleted agent.
            self.write(&p)?;
        }
        self.update(p, &format!("rolled back to v{target}"))
    }

    // ---- delegations

    pub fn record_delegation(&self, d: &DelegationRecord) -> Result<(), String> {
        self.db(async {
            sqlx::query(
                "INSERT INTO delegations (id, run_id, from_agent, agent, task, method, confidence, status, output, duration_ms, model_calls, tool_calls, tokens, outcome, created_at)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(d.id.to_string())
            .bind(d.run_id.map(|r| r.to_string()))
            .bind(&d.from_agent)
            .bind(&d.agent)
            .bind(&d.task)
            .bind(&d.method)
            .bind(d.confidence)
            .bind(&d.status)
            .bind(&d.output)
            .bind(d.duration_ms as i64)
            .bind(d.model_calls as i64)
            .bind(d.tool_calls as i64)
            .bind(d.tokens as i64)
            .bind(&d.outcome)
            .bind(time(d.created_at))
            .execute(&self.pool)
            .await?;
            Ok(())
        })
    }

    /// The user's reaction to a run applies to its delegations (A14).
    pub fn set_outcome(&self, run: Uuid, outcome: &str) -> Result<Vec<DelegationRecord>, String> {
        self.db(async {
            sqlx::query("UPDATE delegations SET outcome = ? WHERE run_id = ?").bind(outcome).bind(run.to_string()).execute(&self.pool).await?;
            Ok(())
        })?;
        Ok(self.delegations(None, 500)?.into_iter().filter(|d| d.run_id == Some(run)).collect())
    }

    /// Newest first; one agent's or all.
    pub fn delegations(&self, agent: Option<&str>, limit: usize) -> Result<Vec<DelegationRecord>, String> {
        self.db(async {
            let rows = sqlx::query("SELECT * FROM delegations WHERE ? IS NULL OR agent = ? ORDER BY created_at DESC LIMIT ?")
                .bind(agent)
                .bind(agent)
                .bind(limit as i64)
                .fetch_all(&self.pool)
                .await?;
            rows.iter()
                .map(|r| {
                    Ok(DelegationRecord {
                        id: Uuid::parse_str(r.try_get("id")?)?,
                        run_id: r.try_get::<Option<String>, _>("run_id")?.map(|s| Uuid::parse_str(&s)).transpose()?,
                        from_agent: r.try_get("from_agent")?,
                        agent: r.try_get("agent")?,
                        task: r.try_get("task")?,
                        method: r.try_get("method")?,
                        confidence: r.try_get("confidence")?,
                        status: r.try_get("status")?,
                        output: r.try_get("output")?,
                        duration_ms: r.try_get::<i64, _>("duration_ms")? as u64,
                        model_calls: r.try_get::<i64, _>("model_calls")? as u32,
                        tool_calls: r.try_get::<i64, _>("tool_calls")? as u32,
                        tokens: r.try_get::<i64, _>("tokens")? as u64,
                        outcome: r.try_get("outcome")?,
                        created_at: parse_time(r.try_get("created_at")?)?,
                    })
                })
                .collect()
        })
    }

    pub fn stats(&self) -> HashMap<String, AgentStats> {
        let mut out: HashMap<String, (AgentStats, u64)> = HashMap::new();
        for d in self.delegations(None, 5000).unwrap_or_default() {
            let (s, total) = out.entry(d.agent.clone()).or_default();
            s.delegations += 1;
            *total += d.duration_ms;
            if d.status != "completed" {
                s.failed += 1;
            }
            match d.outcome.as_deref() {
                Some("failure") => s.corrected += 1,
                Some("success") => s.praised += 1,
                _ => {}
            }
        }
        out.into_iter()
            .map(|(k, (mut s, total))| {
                s.average_ms = total / s.delegations.max(1) as u64;
                (k, s)
            })
            .collect()
    }

    // ---- the wizard's draft (A3: resumable)

    pub fn save_draft(&self, draft: &serde_json::Value) -> Result<(), String> {
        self.db(async {
            sqlx::query("INSERT INTO agent_drafts (id, draft, updated_at) VALUES ('current', ?, ?) ON CONFLICT (id) DO UPDATE SET draft = excluded.draft, updated_at = excluded.updated_at")
                .bind(draft.to_string())
                .bind(time(Utc::now()))
                .execute(&self.pool)
                .await?;
            Ok(())
        })
    }

    pub fn draft(&self) -> Option<serde_json::Value> {
        self.db(async {
            let row = sqlx::query("SELECT draft FROM agent_drafts WHERE id = 'current'").fetch_optional(&self.pool).await?;
            Ok(row.and_then(|r| r.try_get::<String, _>("draft").ok()).and_then(|t| serde_json::from_str(&t).ok()))
        })
        .ok()
        .flatten()
    }

    pub fn clear_draft(&self) {
        let _ = self.db(async {
            sqlx::query("DELETE FROM agent_drafts").execute(&self.pool).await?;
            Ok(())
        });
    }
}

/// What every profile must satisfy.
pub fn validate(p: &AgentProfile) -> Result<(), String> {
    if p.name.is_empty() || p.name == MAIN || !p.name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err(format!("bad agent name {:?}: lowercase letters, digits and dashes (and not \"main\")", p.name));
    }
    if p.description.trim().is_empty() {
        return Err("an agent needs a description".into());
    }
    let risks = ["read_only", "low_write", "write", "destructive"];
    if !risks.contains(&p.permission_policy.max_risk.as_str()) {
        return Err(format!("max_risk must be one of {}", risks.join(", ")));
    }
    if lyra_memory::safety::scan(&p.instructions).is_some() {
        return Err("the instructions look like they contain a secret".into());
    }
    Ok(())
}

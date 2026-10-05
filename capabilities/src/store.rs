//! Usage history (C5), in SQLite with the rest of the agent's operational state.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::model::{CapabilityStats, CapabilityUsage};

/// Uses that count as "recent" for health.
pub const RECENT: usize = 10;

pub struct UsageStore {
    pool: SqlitePool,
}

impl UsageStore {
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let options = SqliteConnectOptions::new().filename(path).create_if_missing(true).journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new().connect_with(options).await.with_context(|| format!("opening {}", path.display()))?;
        Self::migrate(pool).await
    }

    pub async fn in_memory() -> Result<Self> {
        let pool = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await?;
        Self::migrate(pool).await
    }

    async fn migrate(pool: SqlitePool) -> Result<Self> {
        sqlx::migrate!("./migrations").run(&pool).await.context("running capability migrations")?;
        Ok(Self { pool })
    }

    pub async fn record(&self, u: &CapabilityUsage) -> Result<()> {
        sqlx::query(
            "INSERT INTO capability_usage (id, capability_id, run_id, success, duration_ms, retries, error_code, error, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(&u.capability_id)
        .bind(u.run_id.map(|r| r.to_string()))
        .bind(u.success)
        .bind(u.duration_ms as i64)
        .bind(u.retries as i64)
        .bind(&u.error_code)
        .bind(&u.error)
        .bind(Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Statistics for every capability that has been used.
    pub async fn stats(&self) -> Result<HashMap<String, CapabilityStats>> {
        let rows = sqlx::query(
            "SELECT capability_id, success, duration_ms, error_code FROM capability_usage ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await?;
        // Per capability: stats so far, total milliseconds, recent outcomes, error counts.
        type Acc = (CapabilityStats, u64, Vec<bool>, HashMap<String, u32>);
        let mut out: HashMap<String, Acc> = HashMap::new();
        for r in rows {
            let id: String = r.try_get("capability_id")?;
            let entry = out.entry(id).or_default();
            let success: bool = r.try_get("success")?;
            entry.0.uses += 1;
            entry.0.successes += success as u64;
            entry.1 += r.try_get::<i64, _>("duration_ms")? as u64;
            if entry.2.len() < RECENT {
                entry.2.push(success);
            }
            if let Some(code) = r.try_get::<Option<String>, _>("error_code")? {
                *entry.3.entry(code).or_default() += 1;
            }
        }
        Ok(out
            .into_iter()
            .map(|(id, (mut s, total_ms, recent, errors))| {
                s.average_latency_ms = (s.uses > 0).then(|| total_ms / s.uses);
                s.recent_success_rate = (!recent.is_empty()).then(|| recent.iter().filter(|x| **x).count() as f32 / recent.len() as f32);
                s.common_error = errors.into_iter().max_by_key(|(_, n)| *n).map(|(code, _)| code);
                (id, s)
            })
            .collect())
    }

    /// The most recent use of a capability in a run, if any (to count retries).
    pub async fn last_in_run(&self, capability: &str, run: Uuid) -> Result<Option<(bool, u32)>> {
        let row = sqlx::query(
            "SELECT success, retries FROM capability_usage WHERE capability_id = ? AND run_id = ? ORDER BY created_at DESC LIMIT 1",
        )
        .bind(capability)
        .bind(run.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| Ok((r.try_get("success")?, r.try_get::<i64, _>("retries")? as u32))).transpose()
    }
}

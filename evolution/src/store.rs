//! Persistence: run telemetry, candidates, generations and the append-only
//! evolution history, in one SQLite file.

use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::model::*;

pub struct EvolutionStore {
    pool: SqlitePool,
}

impl EvolutionStore {
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let options =
            SqliteConnectOptions::new().filename(path).create_if_missing(true).journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new().connect_with(options).await.with_context(|| format!("opening {}", path.display()))?;
        Self::migrate(pool).await
    }

    pub async fn in_memory() -> Result<Self> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(SqliteConnectOptions::from_str("sqlite::memory:")?)
            .await?;
        Self::migrate(pool).await
    }

    async fn migrate(pool: SqlitePool) -> Result<Self> {
        sqlx::migrate!("./migrations").run(&pool).await.context("running evolution migrations")?;
        Ok(Self { pool })
    }

    // ---- runs (E1)

    pub async fn save_run(&self, r: &RunRecord) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO runs (id, kind, outcome, corrected, generation, record, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(r.id.to_string())
        .bind(r.kind.as_str())
        .bind(r.outcome.as_str())
        .bind(r.corrected)
        .bind(r.generation as i64)
        .bind(serde_json::to_string(r)?)
        .bind(time(r.created_at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn run(&self, id: Uuid) -> Result<Option<RunRecord>> {
        let row = sqlx::query("SELECT record FROM runs WHERE id = ?").bind(id.to_string()).fetch_optional(&self.pool).await?;
        row.map(|r| Ok(serde_json::from_str(r.try_get("record")?)?)).transpose()
    }

    /// Newest first.
    pub async fn runs(&self, limit: usize) -> Result<Vec<RunRecord>> {
        let rows = sqlx::query("SELECT record FROM runs ORDER BY created_at DESC LIMIT ?")
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(|r| Ok(serde_json::from_str(r.try_get("record")?)?)).collect()
    }

    // ---- candidates

    pub async fn save_candidate(&self, c: &Candidate) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO candidates (id, grp, status, candidate, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(c.id.to_string())
        .bind(c.group.to_string())
        .bind(c.status.as_str())
        .bind(serde_json::to_string(c)?)
        .bind(time(c.created_at))
        .bind(time(c.updated_at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Newest first.
    pub async fn candidates(&self, limit: usize) -> Result<Vec<Candidate>> {
        let rows = sqlx::query("SELECT candidate FROM candidates ORDER BY created_at DESC LIMIT ?")
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(|r| Ok(serde_json::from_str(r.try_get("candidate")?)?)).collect()
    }

    // ---- generations

    pub async fn add_generation(&self, g: &Generation) -> Result<()> {
        sqlx::query("INSERT INTO generations (id, number, parent, generation, created_at) VALUES (?, ?, ?, ?, ?)")
            .bind(g.id.to_string())
            .bind(g.number as i64)
            .bind(g.parent.map(|p| p.to_string()))
            .bind(serde_json::to_string(g)?)
            .bind(time(g.created_at))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Newest first.
    pub async fn generations(&self) -> Result<Vec<Generation>> {
        let rows = sqlx::query("SELECT generation FROM generations ORDER BY number DESC").fetch_all(&self.pool).await?;
        rows.iter().map(|r| Ok(serde_json::from_str(r.try_get("generation")?)?)).collect()
    }

    // ---- history (append-only)

    pub async fn record(&self, e: &EvolutionEvent) -> Result<()> {
        sqlx::query(
            "INSERT INTO evolution_events (id, candidate, kind, description, old_generation, new_generation, fitness_before, fitness_after, category, evidence, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(e.id.to_string())
        .bind(e.candidate.map(|c| c.to_string()))
        .bind(&e.kind)
        .bind(&e.description)
        .bind(e.old_generation.map(|g| g as i64))
        .bind(e.new_generation.map(|g| g as i64))
        .bind(e.fitness_before)
        .bind(e.fitness_after)
        .bind(e.category.map(|c| c.as_str()))
        .bind(serde_json::to_string(&e.evidence)?)
        .bind(time(e.created_at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Newest first.
    pub async fn events(&self, limit: usize) -> Result<Vec<EvolutionEvent>> {
        let rows = sqlx::query("SELECT * FROM evolution_events ORDER BY created_at DESC, rowid DESC LIMIT ?")
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| {
                Ok(EvolutionEvent {
                    id: Uuid::parse_str(r.try_get("id")?)?,
                    candidate: r.try_get::<Option<&str>, _>("candidate")?.map(Uuid::parse_str).transpose()?,
                    kind: r.try_get("kind")?,
                    description: r.try_get("description")?,
                    old_generation: r.try_get::<Option<i64>, _>("old_generation")?.map(|g| g as u32),
                    new_generation: r.try_get::<Option<i64>, _>("new_generation")?.map(|g| g as u32),
                    fitness_before: r.try_get("fitness_before")?,
                    fitness_after: r.try_get("fitness_after")?,
                    category: r.try_get::<Option<&str>, _>("category")?.map(str::parse).transpose()?,
                    evidence: serde_json::from_str(r.try_get("evidence")?)?,
                    created_at: parse_time(r.try_get("created_at")?)?,
                })
            })
            .collect()
    }
}

fn time(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn parse_time(text: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text).map(|t| t.with_timezone(&Utc)).map_err(|e| anyhow!("bad timestamp {text:?}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_history_is_append_only() {
        let store = EvolutionStore::in_memory().await.unwrap();
        let e = EvolutionEvent {
            id: Uuid::new_v4(),
            candidate: Some(Uuid::new_v4()),
            kind: "proposed".into(),
            description: "x".into(),
            old_generation: None,
            new_generation: Some(1),
            fitness_before: None,
            fitness_after: None,
            category: Some(Category::Prompt),
            evidence: vec![Uuid::new_v4()],
            created_at: Utc::now(),
        };
        store.record(&e).await.unwrap();
        let back = store.events(5).await.unwrap();
        assert_eq!((back[0].category, back[0].evidence.clone()), (e.category, e.evidence.clone()));
        let update = sqlx::query("UPDATE evolution_events SET description = 'changed'").execute(&store.pool).await;
        assert!(update.unwrap_err().to_string().contains("append-only"));
        assert!(sqlx::query("DELETE FROM evolution_events").execute(&store.pool).await.is_err());
    }
}

//! Goal persistence (G2), in SQLite with the agent's other operational state.

use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::model::*;

pub struct GoalStore {
    pool: SqlitePool,
}

fn time(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn parse_time(s: &str) -> Result<DateTime<Utc>> {
    Ok(DateTime::parse_from_rfc3339(s)?.with_timezone(&Utc))
}

fn opt_time(r: &SqliteRow, col: &str) -> Result<Option<DateTime<Utc>>> {
    r.try_get::<Option<String>, _>(col)?.map(|s| parse_time(&s)).transpose()
}

impl GoalStore {
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
        sqlx::migrate!("./migrations").run(&pool).await.context("running goal migrations")?;
        Ok(Self { pool })
    }

    // ---- goals

    pub async fn save(&self, g: &Goal) -> Result<()> {
        sqlx::query(
            "INSERT INTO goals (id, parent_id, status, goal, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)
             ON CONFLICT(id) DO UPDATE SET parent_id = excluded.parent_id, status = excluded.status,
                 goal = excluded.goal, updated_at = excluded.updated_at",
        )
        .bind(g.id.to_string())
        .bind(g.parent_goal_id.map(|p| p.to_string()))
        .bind(g.status.as_str())
        .bind(serde_json::to_string(g)?)
        .bind(time(g.created_at))
        .bind(time(Utc::now()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn goal(&self, id: Uuid) -> Result<Option<Goal>> {
        let row = sqlx::query("SELECT goal FROM goals WHERE id = ?").bind(id.to_string()).fetch_optional(&self.pool).await?;
        row.map(|r| Ok(serde_json::from_str(r.try_get("goal")?)?)).transpose()
    }

    /// Every goal, oldest first.
    pub async fn goals(&self) -> Result<Vec<Goal>> {
        let rows = sqlx::query("SELECT goal FROM goals ORDER BY created_at").fetch_all(&self.pool).await?;
        rows.iter().map(|r| Ok(serde_json::from_str(r.try_get("goal")?)?)).collect()
    }

    /// When the goal last changed.
    pub async fn updated_at(&self, id: Uuid) -> Result<Option<DateTime<Utc>>> {
        let row = sqlx::query("SELECT updated_at FROM goals WHERE id = ?").bind(id.to_string()).fetch_optional(&self.pool).await?;
        row.map(|r| parse_time(r.try_get("updated_at")?)).transpose()
    }

    // ---- plans (G4)

    pub async fn add_plan(&self, p: &GoalPlan) -> Result<()> {
        sqlx::query(
            "INSERT INTO goal_plans (goal_id, plan_id, attempt, outcome, summary, tokens, autonomous, started_at, finished_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(p.goal_id.to_string())
        .bind(p.plan_id.to_string())
        .bind(p.attempt as i64)
        .bind(&p.outcome)
        .bind(&p.summary)
        .bind(p.tokens as i64)
        .bind(p.autonomous)
        .bind(time(p.started_at))
        .bind(p.finished_at.map(time))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn finish_plan(&self, plan: Uuid, outcome: &str, summary: &str, tokens: u64) -> Result<Option<Uuid>> {
        let row = sqlx::query(
            "UPDATE goal_plans SET outcome = ?, summary = ?, tokens = ?, finished_at = ? WHERE plan_id = ? RETURNING goal_id",
        )
        .bind(outcome)
        .bind(summary)
        .bind(tokens as i64)
        .bind(time(Utc::now()))
        .bind(plan.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|r| Ok(Uuid::parse_str(r.try_get("goal_id")?)?)).transpose()
    }

    fn plan_from(r: &SqliteRow) -> Result<GoalPlan> {
        Ok(GoalPlan {
            goal_id: Uuid::parse_str(r.try_get("goal_id")?)?,
            plan_id: Uuid::parse_str(r.try_get("plan_id")?)?,
            attempt: r.try_get::<i64, _>("attempt")? as u32,
            outcome: r.try_get("outcome")?,
            summary: r.try_get("summary")?,
            tokens: r.try_get::<i64, _>("tokens")? as u64,
            autonomous: r.try_get("autonomous")?,
            started_at: parse_time(r.try_get("started_at")?)?,
            finished_at: opt_time(r, "finished_at")?,
        })
    }

    /// A goal's plans, oldest first.
    pub async fn plans(&self, goal: Uuid) -> Result<Vec<GoalPlan>> {
        let rows = sqlx::query("SELECT * FROM goal_plans WHERE goal_id = ? ORDER BY attempt").bind(goal.to_string()).fetch_all(&self.pool).await?;
        rows.iter().map(Self::plan_from).collect()
    }

    /// The goal a plan works toward, if any.
    pub async fn plan_goal(&self, plan: Uuid) -> Result<Option<GoalPlan>> {
        let row = sqlx::query("SELECT * FROM goal_plans WHERE plan_id = ?").bind(plan.to_string()).fetch_optional(&self.pool).await?;
        row.as_ref().map(Self::plan_from).transpose()
    }

    /// Plans not finished yet, across goals.
    pub async fn open_plans(&self) -> Result<Vec<GoalPlan>> {
        let rows = sqlx::query("SELECT * FROM goal_plans WHERE finished_at IS NULL").fetch_all(&self.pool).await?;
        rows.iter().map(Self::plan_from).collect()
    }

    // ---- blockers (G6)

    pub async fn add_blocker(&self, b: &GoalBlocker) -> Result<()> {
        sqlx::query("INSERT INTO goal_blockers (id, goal_id, blocker_type, reason, created_at) VALUES (?, ?, ?, ?, ?)")
            .bind(b.id.to_string())
            .bind(b.goal_id.to_string())
            .bind(b.blocker_type.as_str())
            .bind(&b.reason)
            .bind(time(b.created_at))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn resolve_blockers(&self, goal: Uuid) -> Result<u64> {
        Ok(sqlx::query("UPDATE goal_blockers SET resolved_at = ? WHERE goal_id = ? AND resolved_at IS NULL")
            .bind(time(Utc::now()))
            .bind(goal.to_string())
            .execute(&self.pool)
            .await?
            .rows_affected())
    }

    /// A goal's blockers (open ones only, or all), newest first.
    pub async fn blockers(&self, goal: Option<Uuid>, open_only: bool) -> Result<Vec<GoalBlocker>> {
        let rows = sqlx::query(
            "SELECT * FROM goal_blockers WHERE (? IS NULL OR goal_id = ?) AND (? = 0 OR resolved_at IS NULL) ORDER BY created_at DESC",
        )
        .bind(goal.map(|g| g.to_string()))
        .bind(goal.map(|g| g.to_string()))
        .bind(open_only)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(GoalBlocker {
                    id: Uuid::parse_str(r.try_get("id")?)?,
                    goal_id: Uuid::parse_str(r.try_get("goal_id")?)?,
                    blocker_type: r.try_get::<&str, _>("blocker_type")?.parse().map_err(anyhow::Error::msg)?,
                    reason: r.try_get("reason")?,
                    created_at: parse_time(r.try_get("created_at")?)?,
                    resolved_at: opt_time(r, "resolved_at")?,
                })
            })
            .collect()
    }

    // ---- triggers (G9, G10)

    pub async fn add_trigger(&self, t: &GoalTrigger) -> Result<()> {
        sqlx::query("INSERT INTO goal_triggers (id, goal_id, trigger, last_fired, active, created_at) VALUES (?, ?, ?, ?, ?, ?)")
            .bind(t.id.to_string())
            .bind(t.goal_id.to_string())
            .bind(serde_json::to_string(&t.trigger)?)
            .bind(t.last_fired.map(time))
            .bind(t.active)
            .bind(time(Utc::now()))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn set_trigger(&self, id: Uuid, last_fired: Option<DateTime<Utc>>, active: bool) -> Result<()> {
        sqlx::query("UPDATE goal_triggers SET last_fired = ?, active = ? WHERE id = ?")
            .bind(last_fired.map(time))
            .bind(active)
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn triggers(&self, goal: Option<Uuid>) -> Result<Vec<GoalTrigger>> {
        let rows = sqlx::query("SELECT * FROM goal_triggers WHERE ? IS NULL OR goal_id = ? ORDER BY created_at")
            .bind(goal.map(|g| g.to_string()))
            .bind(goal.map(|g| g.to_string()))
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| {
                Ok(GoalTrigger {
                    id: Uuid::parse_str(r.try_get("id")?)?,
                    goal_id: Uuid::parse_str(r.try_get("goal_id")?)?,
                    trigger: serde_json::from_str(r.try_get("trigger")?)?,
                    last_fired: opt_time(r, "last_fired")?,
                    active: r.try_get("active")?,
                })
            })
            .collect()
    }

    // ---- events

    pub async fn record(&self, e: &GoalEvent) -> Result<()> {
        sqlx::query("INSERT INTO goal_events (id, goal_id, kind, message, created_at) VALUES (?, ?, ?, ?, ?)")
            .bind(e.id.to_string())
            .bind(e.goal_id.to_string())
            .bind(&e.kind)
            .bind(&e.message)
            .bind(time(e.created_at))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Newest first; one goal's or all.
    pub async fn events(&self, goal: Option<Uuid>, limit: usize) -> Result<Vec<GoalEvent>> {
        let rows = sqlx::query("SELECT * FROM goal_events WHERE ? IS NULL OR goal_id = ? ORDER BY created_at DESC, rowid DESC LIMIT ?")
            .bind(goal.map(|g| g.to_string()))
            .bind(goal.map(|g| g.to_string()))
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| {
                Ok(GoalEvent {
                    id: Uuid::parse_str(r.try_get("id")?)?,
                    goal_id: Uuid::parse_str(r.try_get("goal_id")?)?,
                    kind: r.try_get("kind")?,
                    message: r.try_get("message")?,
                    created_at: parse_time(r.try_get("created_at")?)?,
                })
            })
            .collect()
    }
}

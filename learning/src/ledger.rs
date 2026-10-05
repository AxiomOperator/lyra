//! The skills ledger (SQLite): version history, usage and outcomes,
//! relationships, pending proposals and the audit log. Skill *content* lives
//! in the Markdown files; this is the evidence and history around it.

use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::proposal::{Change, Proposal, ProposalStatus};
use crate::{Relationship, SkillOutcome, Usage};

/// A snapshot of a skill's text.
#[derive(Debug, Clone)]
pub struct Version {
    pub skill_id: Uuid,
    pub version: i64,
    pub name: String,
    pub description: String,
    pub instructions: String,
    pub change_reason: String,
    pub run_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

/// One entry in the audit log.
#[derive(Debug, Clone)]
pub struct Event {
    pub id: Uuid,
    pub skill_id: Option<Uuid>,
    /// e.g. `created`, `updated`, `promoted`, `deprecated`, `rolled_back`, `curated`.
    pub kind: String,
    pub from_state: Option<String>,
    pub to_state: Option<String>,
    pub reason: String,
    pub evidence: String,
    pub run_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

impl Event {
    pub fn new(kind: &str, skill_id: Option<Uuid>, reason: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            skill_id,
            kind: kind.into(),
            from_state: None,
            to_state: None,
            reason: reason.into(),
            evidence: String::new(),
            run_id: None,
            created_at: Utc::now(),
        }
    }

    pub fn states(mut self, from: impl ToString, to: impl ToString) -> Self {
        self.from_state = Some(from.to_string());
        self.to_state = Some(to.to_string());
        self
    }

    pub fn evidence(mut self, evidence: impl Into<String>) -> Self {
        self.evidence = evidence.into();
        self
    }

    pub fn run(mut self, run_id: Option<Uuid>) -> Self {
        self.run_id = run_id;
        self
    }
}

pub struct Ledger {
    pool: SqlitePool,
}

impl Ledger {
    /// Open (creating if needed) the ledger at `path` and run migrations.
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        }
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal);
        let pool = SqlitePoolOptions::new()
            .connect_with(options)
            .await
            .with_context(|| format!("opening {}", path.display()))?;
        Self::migrate(pool).await
    }

    /// A private in-memory ledger, for tests.
    pub async fn in_memory() -> Result<Self> {
        // Each connection to :memory: is its own database, so keep exactly one.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(SqliteConnectOptions::from_str("sqlite::memory:")?)
            .await?;
        Self::migrate(pool).await
    }

    async fn migrate(pool: SqlitePool) -> Result<Self> {
        sqlx::migrate!("./migrations").run(&pool).await.context("running ledger migrations")?;
        Ok(Self { pool })
    }

    // ---- versions

    /// Record a snapshot; returns its version number (1, 2, ...).
    pub async fn add_version(
        &self,
        skill_id: Uuid,
        name: &str,
        description: &str,
        instructions: &str,
        change_reason: &str,
        run_id: Option<Uuid>,
    ) -> Result<i64> {
        let mut tx = self.pool.begin().await?;
        let next: i64 = sqlx::query(
            "SELECT COALESCE(MAX(version), 0) + 1 AS next FROM skill_versions WHERE skill_id = ?",
        )
        .bind(skill_id.to_string())
        .fetch_one(&mut *tx)
        .await?
        .try_get("next")?;
        sqlx::query(
            "INSERT INTO skill_versions
               (id, skill_id, version, name, description, instructions, change_reason, run_id, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(skill_id.to_string())
        .bind(next)
        .bind(name)
        .bind(description)
        .bind(instructions)
        .bind(change_reason)
        .bind(run_id.map(|r| r.to_string()))
        .bind(rfc3339(Utc::now()))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(next)
    }

    /// Newest first.
    pub async fn versions(&self, skill_id: Uuid) -> Result<Vec<Version>> {
        let rows = sqlx::query("SELECT * FROM skill_versions WHERE skill_id = ? ORDER BY version DESC")
            .bind(skill_id.to_string())
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(version_from_row).collect()
    }

    pub async fn latest_version(&self, skill_id: Uuid) -> Result<Option<Version>> {
        Ok(self.versions(skill_id).await?.into_iter().next())
    }

    // ---- usage

    /// Note that `skills` were in the prompt for `run_id`.
    pub async fn record_usage(&self, run_id: Uuid, skills: &[Uuid]) -> Result<()> {
        let now = rfc3339(Utc::now());
        for skill in skills {
            sqlx::query(
                "INSERT INTO skill_usage (id, skill_id, run_id, used_at, outcome) VALUES (?, ?, ?, ?, 'unknown')",
            )
            .bind(Uuid::new_v4().to_string())
            .bind(skill.to_string())
            .bind(run_id.to_string())
            .bind(&now)
            .execute(&self.pool)
            .await?;
        }
        Ok(())
    }

    /// Set the outcome of a run's skill uses. Inferred outcomes only fill in
    /// unknowns; explicit ones (`overwrite`) replace whatever was there.
    /// Returns the skills affected.
    pub async fn set_outcome(&self, run_id: Uuid, outcome: SkillOutcome, overwrite: bool) -> Result<Vec<Uuid>> {
        let rows = sqlx::query(
            "UPDATE skill_usage SET outcome = ?
             WHERE run_id = ? AND (? OR outcome = 'unknown')
             RETURNING skill_id",
        )
        .bind(outcome.as_str())
        .bind(run_id.to_string())
        .bind(overwrite)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(|r| Ok(Uuid::parse_str(r.try_get("skill_id")?)?)).collect()
    }

    /// Usage numbers for every skill that has been used.
    pub async fn usage(&self) -> Result<HashMap<Uuid, Usage>> {
        let rows = sqlx::query(
            "SELECT skill_id,
                    COUNT(*) AS uses,
                    SUM(outcome = 'success') AS successes,
                    SUM(outcome = 'failure') AS failures,
                    SUM(outcome = 'partial') AS partials,
                    MAX(used_at) AS last_used
             FROM skill_usage GROUP BY skill_id",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = HashMap::new();
        for r in rows {
            let id = Uuid::parse_str(r.try_get("skill_id")?)?;
            out.insert(
                id,
                Usage {
                    use_count: r.try_get::<i64, _>("uses")? as u64,
                    success_count: r.try_get::<i64, _>("successes")? as u64,
                    failure_count: r.try_get::<i64, _>("failures")? as u64,
                    partial_count: r.try_get::<i64, _>("partials")? as u64,
                    last_used_at: r.try_get::<Option<&str>, _>("last_used")?.map(timestamp).transpose()?,
                },
            );
        }
        Ok(out)
    }

    // ---- relationships

    pub async fn relate(&self, from: Uuid, to: Uuid, kind: Relationship, reason: &str) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO skill_relationships (from_id, to_id, kind, reason, created_at)
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(from.to_string())
        .bind(to.to_string())
        .bind(kind.as_str())
        .bind(reason)
        .bind(rfc3339(Utc::now()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// All relationships as `(from, to, kind, reason)`.
    pub async fn relationships(&self) -> Result<Vec<(Uuid, Uuid, Relationship, String)>> {
        let rows = sqlx::query("SELECT * FROM skill_relationships ORDER BY created_at")
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| {
                Ok((
                    Uuid::parse_str(r.try_get("from_id")?)?,
                    Uuid::parse_str(r.try_get("to_id")?)?,
                    r.try_get::<&str, _>("kind")?.parse()?,
                    r.try_get("reason")?,
                ))
            })
            .collect()
    }

    // ---- proposals

    pub async fn add_proposal(&self, p: &Proposal) -> Result<()> {
        sqlx::query(
            "INSERT INTO skill_proposals
               (id, kind, skill_id, change, reason, evidence, confidence, run_id, status, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(p.id.to_string())
        .bind(p.change.kind())
        .bind(p.change.subject().map(|s| s.to_string()))
        .bind(serde_json::to_string(&p.change)?)
        .bind(&p.reason)
        .bind(&p.evidence)
        .bind(p.confidence)
        .bind(p.run_id.map(|r| r.to_string()))
        .bind(p.status.as_str())
        .bind(rfc3339(p.created_at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Newest first, optionally only one status.
    pub async fn proposals(&self, status: Option<ProposalStatus>) -> Result<Vec<Proposal>> {
        let status = status.map(ProposalStatus::as_str);
        let rows = sqlx::query(
            "SELECT * FROM skill_proposals WHERE ? IS NULL OR status = ? ORDER BY created_at DESC",
        )
        .bind(status)
        .bind(status)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(proposal_from_row).collect()
    }

    pub async fn set_proposal_status(&self, id: Uuid, status: ProposalStatus) -> Result<()> {
        let updated = sqlx::query("UPDATE skill_proposals SET status = ?, resolved_at = ? WHERE id = ?")
            .bind(status.as_str())
            .bind(rfc3339(Utc::now()))
            .bind(id.to_string())
            .execute(&self.pool)
            .await?
            .rows_affected();
        if updated == 0 {
            return Err(anyhow!("no proposal with id {id}"));
        }
        Ok(())
    }

    // ---- audit log

    pub async fn record(&self, e: &Event) -> Result<()> {
        sqlx::query(
            "INSERT INTO skill_events
               (id, skill_id, kind, from_state, to_state, reason, evidence, run_id, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(e.id.to_string())
        .bind(e.skill_id.map(|s| s.to_string()))
        .bind(&e.kind)
        .bind(&e.from_state)
        .bind(&e.to_state)
        .bind(&e.reason)
        .bind(&e.evidence)
        .bind(e.run_id.map(|r| r.to_string()))
        .bind(rfc3339(e.created_at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Newest first: one skill's events, or all of them.
    pub async fn events(&self, skill_id: Option<Uuid>, limit: usize) -> Result<Vec<Event>> {
        let skill = skill_id.map(|s| s.to_string());
        let rows = sqlx::query(
            "SELECT * FROM skill_events WHERE ? IS NULL OR skill_id = ?
             ORDER BY created_at DESC, rowid DESC LIMIT ?",
        )
        .bind(&skill)
        .bind(&skill)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(event_from_row).collect()
    }

    /// The most recent event of a kind, e.g. the last curation.
    pub async fn last_event(&self, kind: &str) -> Result<Option<Event>> {
        let row = sqlx::query(
            "SELECT * FROM skill_events WHERE kind = ? ORDER BY created_at DESC, rowid DESC LIMIT 1",
        )
        .bind(kind)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(event_from_row).transpose()
    }
}

fn uuid_opt(row: &SqliteRow, col: &str) -> Result<Option<Uuid>> {
    Ok(row.try_get::<Option<&str>, _>(col)?.map(Uuid::parse_str).transpose()?)
}

fn version_from_row(r: &SqliteRow) -> Result<Version> {
    Ok(Version {
        skill_id: Uuid::parse_str(r.try_get("skill_id")?)?,
        version: r.try_get("version")?,
        name: r.try_get("name")?,
        description: r.try_get("description")?,
        instructions: r.try_get("instructions")?,
        change_reason: r.try_get("change_reason")?,
        run_id: uuid_opt(r, "run_id")?,
        created_at: timestamp(r.try_get("created_at")?)?,
    })
}

fn proposal_from_row(r: &SqliteRow) -> Result<Proposal> {
    let change: Change = serde_json::from_str(r.try_get("change")?)?;
    Ok(Proposal {
        id: Uuid::parse_str(r.try_get("id")?)?,
        change,
        reason: r.try_get("reason")?,
        evidence: r.try_get("evidence")?,
        confidence: r.try_get("confidence")?,
        run_id: uuid_opt(r, "run_id")?,
        status: ProposalStatus::parse(r.try_get("status")?)?,
        created_at: timestamp(r.try_get("created_at")?)?,
    })
}

fn event_from_row(r: &SqliteRow) -> Result<Event> {
    Ok(Event {
        id: Uuid::parse_str(r.try_get("id")?)?,
        skill_id: uuid_opt(r, "skill_id")?,
        kind: r.try_get("kind")?,
        from_state: r.try_get("from_state")?,
        to_state: r.try_get("to_state")?,
        reason: r.try_get("reason")?,
        evidence: r.try_get("evidence")?,
        run_id: uuid_opt(r, "run_id")?,
        created_at: timestamp(r.try_get("created_at")?)?,
    })
}

/// Fixed-width UTC timestamps, so they sort correctly as text.
fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn timestamp(text: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|e| anyhow!("bad timestamp {text:?}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn versions_count_up_per_skill() {
        let l = Ledger::in_memory().await.unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        assert_eq!(l.add_version(a, "a", "d", "v1", "created", None).await.unwrap(), 1);
        assert_eq!(l.add_version(a, "a", "d", "v2", "refined", None).await.unwrap(), 2);
        assert_eq!(l.add_version(b, "b", "d", "x", "created", None).await.unwrap(), 1);
        let versions = l.versions(a).await.unwrap();
        assert_eq!(versions.iter().map(|v| v.version).collect::<Vec<_>>(), [2, 1]);
        assert_eq!(l.latest_version(a).await.unwrap().unwrap().instructions, "v2");
    }

    #[tokio::test]
    async fn usage_and_outcomes() {
        let l = Ledger::in_memory().await.unwrap();
        let (skill, other) = (Uuid::new_v4(), Uuid::new_v4());
        let (run1, run2, run3) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        l.record_usage(run1, &[skill, other]).await.unwrap();
        l.record_usage(run2, &[skill]).await.unwrap();
        l.record_usage(run3, &[skill]).await.unwrap();

        assert_eq!(l.set_outcome(run1, SkillOutcome::Success, false).await.unwrap().len(), 2);
        l.set_outcome(run2, SkillOutcome::Failure, false).await.unwrap();
        // Inferred outcomes don't overwrite; explicit ones do.
        assert!(l.set_outcome(run2, SkillOutcome::Success, false).await.unwrap().is_empty());
        l.set_outcome(run2, SkillOutcome::Partial, true).await.unwrap();

        let usage = l.usage().await.unwrap();
        let u = usage[&skill];
        assert_eq!((u.use_count, u.success_count, u.failure_count, u.partial_count), (3, 1, 0, 1));
        assert!(u.last_used_at.is_some());
        assert_eq!(usage[&other].success_count, 1);
    }

    #[tokio::test]
    async fn proposals_round_trip() {
        let l = Ledger::in_memory().await.unwrap();
        let skill = Uuid::new_v4();
        let change = Change::Update { skill, description: "d".into(), instructions: "i".into() };
        let p = Proposal::new(change.clone(), "why".into(), "because".into(), 0.8, None);
        l.add_proposal(&p).await.unwrap();

        let pending = l.proposals(Some(ProposalStatus::Pending)).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].change, change);
        l.set_proposal_status(p.id, ProposalStatus::Applied).await.unwrap();
        assert!(l.proposals(Some(ProposalStatus::Pending)).await.unwrap().is_empty());
        assert!(l.set_proposal_status(Uuid::new_v4(), ProposalStatus::Applied).await.is_err());
    }

    #[tokio::test]
    async fn events_and_relationships() {
        let l = Ledger::in_memory().await.unwrap();
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        l.record(&Event::new("created", Some(a), "learned").states("none", "proposed")).await.unwrap();
        l.record(&Event::new("promoted", Some(a), "2 successes").states("proposed", "active")).await.unwrap();
        l.record(&Event::new("curated", None, "weekly")).await.unwrap();

        let events = l.events(Some(a), 10).await.unwrap();
        assert_eq!(events.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(), ["promoted", "created"]);
        assert_eq!(events[0].to_state.as_deref(), Some("active"));
        assert_eq!(l.last_event("curated").await.unwrap().unwrap().kind, "curated");
        assert!(l.last_event("nope").await.unwrap().is_none());

        l.relate(b, a, Relationship::Supersedes, "merged").await.unwrap();
        let rels = l.relationships().await.unwrap();
        assert_eq!(rels, [(b, a, Relationship::Supersedes, "merged".to_string())]);
    }
}

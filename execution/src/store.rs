//! Persistence (P8, P9, P16): goals, plans, every step attempt, verification,
//! revisions, checkpoints, events and approvals, in one SQLite file.

use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::model::*;

pub struct PlanStore {
    pool: SqlitePool,
}

/// One recorded attempt at a step.
#[derive(Debug, Clone)]
pub struct Attempt {
    pub step_id: Uuid,
    pub plan_version: u32,
    pub attempt: u32,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub success: bool,
    pub error: Option<String>,
    pub failure_class: Option<FailureClass>,
}

impl PlanStore {
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
        sqlx::migrate!("./migrations").run(&pool).await.context("running plan migrations")?;
        Ok(Self { pool })
    }

    // ---- goals

    pub async fn save_goal(&self, g: &Goal) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO goals (id, request, description, success_criteria, constraints, outputs,
                destructive, ambiguities, status, evaluation, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, (SELECT evaluation FROM goals WHERE id = ?), ?, ?)",
        )
        .bind(g.id.to_string())
        .bind(&g.request)
        .bind(&g.description)
        .bind(serde_json::to_string(&g.success_criteria)?)
        .bind(serde_json::to_string(&g.constraints)?)
        .bind(serde_json::to_string(&g.outputs)?)
        .bind(g.destructive)
        .bind(serde_json::to_string(&g.ambiguities)?)
        .bind(g.status.as_str())
        .bind(g.id.to_string())
        .bind(time(g.created_at))
        .bind(time(Utc::now()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn set_goal_outcome(&self, id: Uuid, status: GoalStatus, evaluation: &Value) -> Result<()> {
        sqlx::query("UPDATE goals SET status = ?, evaluation = ?, updated_at = ? WHERE id = ?")
            .bind(status.as_str())
            .bind(evaluation.to_string())
            .bind(time(Utc::now()))
            .bind(id.to_string())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn goal(&self, id: Uuid) -> Result<Option<(Goal, Option<Value>)>> {
        let row = sqlx::query("SELECT * FROM goals WHERE id = ?").bind(id.to_string()).fetch_optional(&self.pool).await?;
        row.map(|r| {
            let json = |c: &str| -> Result<Vec<String>> { Ok(serde_json::from_str(r.try_get(c)?)?) };
            let goal = Goal {
                id: Uuid::parse_str(r.try_get("id")?)?,
                request: r.try_get("request")?,
                description: r.try_get("description")?,
                success_criteria: json("success_criteria")?,
                constraints: json("constraints")?,
                outputs: json("outputs")?,
                destructive: r.try_get("destructive")?,
                ambiguities: json("ambiguities")?,
                status: r.try_get::<&str, _>("status")?.parse()?,
                created_at: parse_time(r.try_get("created_at")?)?,
            };
            let evaluation = r.try_get::<Option<&str>, _>("evaluation")?.map(serde_json::from_str).transpose()?;
            Ok((goal, evaluation))
        })
        .transpose()
    }

    // ---- plans

    /// Insert or update (persist before and after every change).
    pub async fn save_plan(&self, p: &Plan) -> Result<()> {
        sqlx::query(
            "INSERT OR REPLACE INTO plans (id, goal_id, version, status, steps, budget, usage, note, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(p.id.to_string())
        .bind(p.goal_id.to_string())
        .bind(p.version as i64)
        .bind(p.status.as_str())
        .bind(serde_json::to_string(&p.steps)?)
        .bind(serde_json::to_string(&p.budget)?)
        .bind(serde_json::to_string(&p.usage)?)
        .bind(&p.note)
        .bind(time(p.created_at))
        .bind(time(p.updated_at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn plan(&self, id: Uuid) -> Result<Option<Plan>> {
        let row = sqlx::query("SELECT * FROM plans WHERE id = ?").bind(id.to_string()).fetch_optional(&self.pool).await?;
        row.as_ref().map(plan_from_row).transpose()
    }

    /// Newest first.
    pub async fn plans(&self, limit: usize) -> Result<Vec<Plan>> {
        let rows = sqlx::query("SELECT * FROM plans ORDER BY created_at DESC LIMIT ?")
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(plan_from_row).collect()
    }

    /// Plans that were running or paused when the process stopped.
    pub async fn unfinished(&self) -> Result<Vec<Plan>> {
        let rows = sqlx::query("SELECT * FROM plans WHERE status IN ('running', 'paused') ORDER BY created_at")
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(plan_from_row).collect()
    }

    // ---- records

    pub async fn record_attempt(&self, plan: &Plan, step: &PlanStep, started: DateTime<Utc>, result: &StepResult) -> Result<()> {
        sqlx::query(
            "INSERT INTO step_executions (id, plan_id, step_id, plan_version, attempt, started_at, finished_at,
                success, output, error, failure_class, operation_id)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(plan.id.to_string())
        .bind(step.id.to_string())
        .bind(plan.version as i64)
        .bind(step.attempts as i64)
        .bind(time(started))
        .bind(time(Utc::now()))
        .bind(result.success)
        .bind(result.output.to_string())
        .bind(&result.error)
        .bind(step.failure_class.map(|c| c.as_str()))
        .bind(step.operation_id.map(|o| o.to_string()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn attempts(&self, plan: Uuid) -> Result<Vec<Attempt>> {
        let rows = sqlx::query("SELECT * FROM step_executions WHERE plan_id = ? ORDER BY started_at")
            .bind(plan.to_string())
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| {
                Ok(Attempt {
                    step_id: Uuid::parse_str(r.try_get("step_id")?)?,
                    plan_version: r.try_get::<i64, _>("plan_version")? as u32,
                    attempt: r.try_get::<i64, _>("attempt")? as u32,
                    started_at: parse_time(r.try_get("started_at")?)?,
                    finished_at: parse_time(r.try_get("finished_at")?)?,
                    success: r.try_get("success")?,
                    error: r.try_get("error")?,
                    failure_class: r.try_get::<Option<&str>, _>("failure_class")?.map(str::parse).transpose()?,
                })
            })
            .collect()
    }

    pub async fn record_verification(&self, plan: Uuid, step: &PlanStep, v: &VerificationResult) -> Result<()> {
        sqlx::query(
            "INSERT INTO verification_results (id, plan_id, step_id, attempt, verified, evidence, reason, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(plan.to_string())
        .bind(step.id.to_string())
        .bind(step.attempts as i64)
        .bind(v.verified)
        .bind(serde_json::to_string(&v.evidence)?)
        .bind(&v.reason)
        .bind(time(Utc::now()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn record_revision(&self, plan: Uuid, from: u32, to: u32, r: &PlanRevision) -> Result<()> {
        sqlx::query("INSERT INTO plan_revisions (id, plan_id, from_version, to_version, revision, created_at) VALUES (?, ?, ?, ?, ?, ?)")
            .bind(Uuid::new_v4().to_string())
            .bind(plan.to_string())
            .bind(from as i64)
            .bind(to as i64)
            .bind(serde_json::to_string(r)?)
            .bind(time(Utc::now()))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn revisions(&self, plan: Uuid) -> Result<Vec<(u32, u32, PlanRevision)>> {
        let rows = sqlx::query("SELECT * FROM plan_revisions WHERE plan_id = ? ORDER BY to_version")
            .bind(plan.to_string())
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| {
                Ok((
                    r.try_get::<i64, _>("from_version")? as u32,
                    r.try_get::<i64, _>("to_version")? as u32,
                    serde_json::from_str(r.try_get("revision")?)?,
                ))
            })
            .collect()
    }

    pub async fn save_checkpoint(&self, c: &Checkpoint) -> Result<()> {
        sqlx::query(
            "INSERT INTO checkpoints (id, plan_id, plan_version, completed_steps, runtime_state, reason, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(c.id.to_string())
        .bind(c.plan_id.to_string())
        .bind(c.plan_version as i64)
        .bind(serde_json::to_string(&c.completed_steps)?)
        .bind(c.runtime_state.to_string())
        .bind(&c.reason)
        .bind(time(c.created_at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn checkpoints(&self, plan: Uuid) -> Result<Vec<Checkpoint>> {
        let rows = sqlx::query("SELECT * FROM checkpoints WHERE plan_id = ? ORDER BY created_at")
            .bind(plan.to_string())
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| {
                Ok(Checkpoint {
                    id: Uuid::parse_str(r.try_get("id")?)?,
                    plan_id: Uuid::parse_str(r.try_get("plan_id")?)?,
                    plan_version: r.try_get::<i64, _>("plan_version")? as u32,
                    completed_steps: serde_json::from_str(r.try_get("completed_steps")?)?,
                    runtime_state: serde_json::from_str(r.try_get("runtime_state")?)?,
                    reason: r.try_get("reason")?,
                    created_at: parse_time(r.try_get("created_at")?)?,
                })
            })
            .collect()
    }

    pub async fn record_event(&self, e: &ExecutionEvent) -> Result<()> {
        sqlx::query("INSERT INTO execution_events (id, plan_id, step_id, kind, message, data, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)")
            .bind(e.id.to_string())
            .bind(e.plan_id.to_string())
            .bind(e.step_id.map(|s| s.to_string()))
            .bind(e.kind.as_str())
            .bind(&e.message)
            .bind(e.data.to_string())
            .bind(time(e.created_at))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Oldest first.
    pub async fn events(&self, plan: Uuid) -> Result<Vec<ExecutionEvent>> {
        let rows = sqlx::query("SELECT * FROM execution_events WHERE plan_id = ? ORDER BY created_at, rowid")
            .bind(plan.to_string())
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| {
                Ok(ExecutionEvent {
                    id: Uuid::parse_str(r.try_get("id")?)?,
                    plan_id: Uuid::parse_str(r.try_get("plan_id")?)?,
                    step_id: r.try_get::<Option<&str>, _>("step_id")?.map(Uuid::parse_str).transpose()?,
                    kind: r.try_get::<&str, _>("kind")?.parse()?,
                    message: r.try_get("message")?,
                    data: serde_json::from_str(r.try_get("data")?)?,
                    created_at: parse_time(r.try_get("created_at")?)?,
                })
            })
            .collect()
    }

    pub async fn record_approval(&self, plan: Uuid, step: Uuid, fingerprint: &str) -> Result<()> {
        sqlx::query("INSERT INTO approvals (id, plan_id, step_id, fingerprint, approved_at) VALUES (?, ?, ?, ?, ?)")
            .bind(Uuid::new_v4().to_string())
            .bind(plan.to_string())
            .bind(step.to_string())
            .bind(fingerprint)
            .bind(time(Utc::now()))
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

fn plan_from_row(r: &SqliteRow) -> Result<Plan> {
    Ok(Plan {
        id: Uuid::parse_str(r.try_get("id")?)?,
        goal_id: Uuid::parse_str(r.try_get("goal_id")?)?,
        version: r.try_get::<i64, _>("version")? as u32,
        status: r.try_get::<&str, _>("status")?.parse()?,
        steps: serde_json::from_str(r.try_get("steps")?).context("reading plan steps")?,
        budget: serde_json::from_str(r.try_get("budget")?)?,
        usage: serde_json::from_str(r.try_get("usage")?)?,
        note: r.try_get("note")?,
        created_at: parse_time(r.try_get("created_at")?)?,
        updated_at: parse_time(r.try_get("updated_at")?)?,
    })
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
    use crate::graph::tests::step;
    use crate::planner::tests::plan_from;

    #[tokio::test]
    async fn plans_and_records_round_trip() {
        let s = PlanStore::in_memory().await.unwrap();
        let a = step("s1", &[]);
        let b = step("s2", &[&a]);
        let mut plan = plan_from(vec![a, b]);
        let goal = Goal {
            id: plan.goal_id,
            request: "r".into(),
            description: "d".into(),
            success_criteria: vec!["c".into()],
            constraints: vec![],
            outputs: vec![],
            destructive: false,
            ambiguities: vec![],
            status: GoalStatus::Active,
            created_at: Utc::now(),
        };
        s.save_goal(&goal).await.unwrap();
        s.save_plan(&plan).await.unwrap();

        plan.steps[0].status = StepStatus::Completed;
        plan.steps[0].attempts = 1;
        plan.usage.model_calls = 3;
        s.save_plan(&plan).await.unwrap();
        let back = s.plan(plan.id).await.unwrap().unwrap();
        assert_eq!(back.steps[0].status, StepStatus::Completed);
        assert_eq!(back.usage.model_calls, 3);
        assert_eq!(s.unfinished().await.unwrap().len(), 1);

        let result = StepResult { success: true, output: Value::String("ok".into()), error: None, metadata: Value::Null };
        s.record_attempt(&plan, &plan.steps[0], Utc::now(), &result).await.unwrap();
        assert!(s.attempts(plan.id).await.unwrap()[0].success);
        s.record_event(&ExecutionEvent::new(plan.id, None, EventKind::PlanStarted, "go")).await.unwrap();
        assert_eq!(s.events(plan.id).await.unwrap()[0].kind, EventKind::PlanStarted);

        s.set_goal_outcome(goal.id, GoalStatus::Completed, &serde_json::json!({"summary": "done"})).await.unwrap();
        let (g, eval) = s.goal(goal.id).await.unwrap().unwrap();
        assert_eq!(g.status, GoalStatus::Completed);
        assert_eq!(eval.unwrap()["summary"], "done");
        s.save_goal(&g).await.unwrap();
        assert!(s.goal(goal.id).await.unwrap().unwrap().1.is_some(), "saving the goal keeps its evaluation");
    }
}

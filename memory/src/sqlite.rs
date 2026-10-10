use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::store::{Event, Filter, MemoryChange, Proposal, ProposalStatus, Version};
use crate::{Episode, Memory, MemoryStore, Provenance, Relationship, Usage};

/// Memories in a single SQLite file, searched with FTS5.
pub struct SqliteStore {
    pool: SqlitePool,
}

impl SqliteStore {
    /// Open (creating if needed) the database at `path` and run migrations.
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

    /// A private in-memory database, for tests.
    pub async fn in_memory() -> Result<Self> {
        // Each connection to :memory: is its own database, so keep exactly one.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(SqliteConnectOptions::from_str("sqlite::memory:")?)
            .await?;
        Self::migrate(pool).await
    }

    async fn migrate(pool: SqlitePool) -> Result<Self> {
        sqlx::migrate!("./migrations").run(&pool).await.context("running migrations")?;
        Ok(Self { pool })
    }
}

#[async_trait::async_trait]
impl MemoryStore for SqliteStore {
    fn backend(&self) -> &'static str {
        "sqlite"
    }

    async fn create(&self, m: &Memory) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO memories (id, scope, kind, content, tags, source, importance, confidence, status,
                                   created_at, updated_at, last_accessed_at, expires_at, run_id, tool_call_id, conversation_id)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(m.id.to_string())
        .bind(&m.scope)
        .bind(m.kind.as_str())
        .bind(&m.content)
        .bind(serde_json::to_string(&m.tags)?)
        .bind(m.source.as_str())
        .bind(m.importance)
        .bind(m.confidence)
        .bind(m.status.as_str())
        .bind(rfc3339(m.created_at))
        .bind(rfc3339(m.updated_at))
        .bind(m.last_accessed_at.map(rfc3339))
        .bind(m.expires_at.map(rfc3339))
        .bind(m.provenance.run_id.map(|r| r.to_string()))
        .bind(&m.provenance.tool_call_id)
        .bind(m.provenance.conversation_id.map(|c| c.to_string()))
        .execute(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO memories_fts (id, content) VALUES (?, ?)")
            .bind(m.id.to_string())
            .bind(&m.content)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn get(&self, id: Uuid) -> Result<Option<Memory>> {
        let row = sqlx::query(
            "SELECT m.id, m.scope, m.kind, m.content, m.tags, m.source, m.importance, m.confidence,
                    m.status, m.created_at, m.updated_at, m.last_accessed_at, m.expires_at, m.run_id, m.tool_call_id, m.conversation_id
             FROM memories m WHERE m.id = ?",
        )
        .bind(id.to_string())
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(from_row).transpose()
    }

    async fn update(&self, m: &Memory) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let updated = sqlx::query(
            "UPDATE memories SET content = ?, tags = ?, importance = ?, confidence = ?, status = ?,
                                 updated_at = ?, last_accessed_at = ?, expires_at = ?
             WHERE id = ?",
        )
        .bind(&m.content)
        .bind(serde_json::to_string(&m.tags)?)
        .bind(m.importance)
        .bind(m.confidence)
        .bind(m.status.as_str())
        .bind(rfc3339(m.updated_at))
        .bind(m.last_accessed_at.map(rfc3339))
        .bind(m.expires_at.map(rfc3339))
        .bind(m.id.to_string())
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if updated == 0 {
            bail!("no memory with id {}", m.id);
        }
        sqlx::query("UPDATE memories_fts SET content = ? WHERE id = ?")
            .bind(&m.content)
            .bind(m.id.to_string())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn delete(&self, id: Uuid) -> Result<()> {
        let id = id.to_string();
        let mut tx = self.pool.begin().await?;
        let deleted =
            sqlx::query("DELETE FROM memories WHERE id = ?").bind(&id).execute(&mut *tx).await?.rows_affected();
        if deleted == 0 {
            bail!("no memory with id {id}");
        }
        for sql in [
            "DELETE FROM memories_fts WHERE id = ?",
            "DELETE FROM memory_embeddings WHERE memory_id = ?",
            "DELETE FROM memory_versions WHERE memory_id = ?",
            "DELETE FROM memory_usage WHERE memory_id = ?",
            "DELETE FROM episodes WHERE id = ?",
        ] {
            sqlx::query(sql).bind(&id).execute(&mut *tx).await?;
        }
        sqlx::query("DELETE FROM memory_relationships WHERE from_id = ? OR to_id = ?")
            .bind(&id)
            .bind(&id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn search(&self, query: &str, scope: Option<&str>, limit: usize) -> Result<Vec<(Memory, f32)>> {
        let Some(fts) = fts_query(query) else { return Ok(Vec::new()) };
        let rows = sqlx::query(
            "SELECT m.id, m.scope, m.kind, m.content, m.tags, m.source, m.importance, m.confidence,
                    m.status, m.created_at, m.updated_at, m.last_accessed_at, m.expires_at, m.run_id, m.tool_call_id, m.conversation_id,
                    bm25(memories_fts) AS rank
             FROM memories_fts JOIN memories m ON m.id = memories_fts.id
             WHERE memories_fts MATCH ? AND (? IS NULL OR m.scope = ?)
             ORDER BY rank LIMIT ?",
        )
        .bind(fts)
        .bind(scope)
        .bind(scope)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        // bm25 is negative, more negative being a better match.
        rows.iter().map(|r| Ok((from_row(r)?, -r.try_get::<f64, _>("rank")? as f32))).collect()
    }

    async fn list(&self, filter: &Filter, limit: usize) -> Result<Vec<Memory>> {
        let statuses: Vec<&str> = filter.statuses.iter().map(|s| s.as_str()).collect();
        let rows = sqlx::query(
            "SELECT m.id, m.scope, m.kind, m.content, m.tags, m.source, m.importance, m.confidence,
                    m.status, m.created_at, m.updated_at, m.last_accessed_at, m.expires_at, m.run_id, m.tool_call_id, m.conversation_id
             FROM memories m
             WHERE (? IS NULL OR m.scope = ?)
               AND (? = '' OR instr(?, ',' || m.status || ',') > 0)
               AND (? IS NULL OR m.kind = ?)
             ORDER BY m.created_at DESC, m.id DESC LIMIT ?",
        )
        .bind(&filter.scope)
        .bind(&filter.scope)
        .bind(statuses.join(","))
        .bind(format!(",{},", statuses.join(",")))
        .bind(filter.kind.map(|k| k.as_str()))
        .bind(filter.kind.map(|k| k.as_str()))
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(from_row).collect()
    }

    async fn scopes(&self) -> Result<Vec<(String, u64)>> {
        let rows = sqlx::query(
            "SELECT scope, COUNT(*) AS n FROM memories WHERE status = 'active'
             GROUP BY scope ORDER BY n DESC, scope",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(|r| Ok((r.try_get("scope")?, r.try_get::<i64, _>("n")? as u64))).collect()
    }

    async fn set_embedding(&self, id: Uuid, model: &str, vector: &[f32]) -> Result<()> {
        let blob: Vec<u8> = vector.iter().flat_map(|x| x.to_le_bytes()).collect();
        sqlx::query(
            "INSERT INTO memory_embeddings (memory_id, model, dims, vector, created_at)
             VALUES (?, ?, ?, ?, ?) ON CONFLICT (memory_id, model) DO UPDATE SET dims = excluded.dims, vector = excluded.vector, created_at = excluded.created_at",
        )
        .bind(id.to_string())
        .bind(model)
        .bind(vector.len() as i64)
        .bind(blob)
        .bind(rfc3339(Utc::now()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn embeddings(&self, model: &str) -> Result<Vec<(Uuid, Vec<f32>)>> {
        let rows = sqlx::query("SELECT memory_id, vector FROM memory_embeddings WHERE model = ?")
            .bind(model)
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| {
                let bytes: Vec<u8> = r.try_get("vector")?;
                let vector = bytes.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect();
                Ok((Uuid::parse_str(r.try_get("memory_id")?)?, vector))
            })
            .collect()
    }

    async fn missing_embeddings(&self, model: &str, limit: usize) -> Result<Vec<Memory>> {
        let rows = sqlx::query(
            "SELECT m.id, m.scope, m.kind, m.content, m.tags, m.source, m.importance, m.confidence,
                    m.status, m.created_at, m.updated_at, m.last_accessed_at, m.expires_at, m.run_id, m.tool_call_id, m.conversation_id
             FROM memories m
             WHERE m.status = 'active'
               AND NOT EXISTS (SELECT 1 FROM memory_embeddings e WHERE e.memory_id = m.id AND e.model = ?)
             ORDER BY m.created_at DESC LIMIT ?",
        )
        .bind(model)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(from_row).collect()
    }

    async fn add_version(&self, id: Uuid, content: &str, confidence: f32, reason: &str) -> Result<i64> {
        let mut tx = self.pool.begin().await?;
        let next: i64 =
            sqlx::query("SELECT COALESCE(MAX(version), 0) + 1 AS next FROM memory_versions WHERE memory_id = ?")
                .bind(id.to_string())
                .fetch_one(&mut *tx)
                .await?
                .try_get("next")?;
        sqlx::query(
            "INSERT INTO memory_versions (id, memory_id, version, content, confidence, reason, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(id.to_string())
        .bind(next)
        .bind(content)
        .bind(confidence)
        .bind(reason)
        .bind(rfc3339(Utc::now()))
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(next)
    }

    async fn versions(&self, id: Uuid) -> Result<Vec<Version>> {
        let rows = sqlx::query("SELECT * FROM memory_versions WHERE memory_id = ? ORDER BY version DESC")
            .bind(id.to_string())
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| {
                Ok(Version {
                    memory_id: Uuid::parse_str(r.try_get("memory_id")?)?,
                    version: r.try_get("version")?,
                    content: r.try_get("content")?,
                    confidence: r.try_get("confidence")?,
                    reason: r.try_get("reason")?,
                    created_at: timestamp(r.try_get("created_at")?)?,
                })
            })
            .collect()
    }

    async fn relate(&self, from: Uuid, to: Uuid, relationship: Relationship, reason: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO memory_relationships (from_id, to_id, relationship, reason, created_at)
             VALUES (?, ?, ?, ?, ?) ON CONFLICT (from_id, to_id, relationship) DO UPDATE SET reason = excluded.reason, created_at = excluded.created_at",
        )
        .bind(from.to_string())
        .bind(to.to_string())
        .bind(relationship.as_str())
        .bind(reason)
        .bind(rfc3339(Utc::now()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn relationships(&self, id: Option<Uuid>) -> Result<Vec<(Uuid, Uuid, Relationship, String)>> {
        let id = id.map(|i| i.to_string());
        let rows = sqlx::query(
            "SELECT * FROM memory_relationships WHERE ? IS NULL OR from_id = ? OR to_id = ? ORDER BY created_at",
        )
        .bind(&id)
        .bind(&id)
        .bind(&id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok((
                    Uuid::parse_str(r.try_get("from_id")?)?,
                    Uuid::parse_str(r.try_get("to_id")?)?,
                    r.try_get::<&str, _>("relationship")?.parse()?,
                    r.try_get("reason")?,
                ))
            })
            .collect()
    }

    async fn record(&self, e: &Event) -> Result<()> {
        sqlx::query(
            "INSERT INTO memory_events (id, memory_id, kind, from_state, to_state, reason, run_id, created_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(e.id.to_string())
        .bind(e.memory_id.map(|m| m.to_string()))
        .bind(&e.kind)
        .bind(&e.from_state)
        .bind(&e.to_state)
        .bind(&e.reason)
        .bind(e.run_id.map(|r| r.to_string()))
        .bind(rfc3339(e.created_at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn events(&self, id: Option<Uuid>, limit: usize) -> Result<Vec<Event>> {
        let id = id.map(|i| i.to_string());
        let rows = sqlx::query(
            "SELECT * FROM memory_events WHERE ? IS NULL OR memory_id = ?
             ORDER BY created_at DESC, id DESC LIMIT ?",
        )
        .bind(&id)
        .bind(&id)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(event_from_row).collect()
    }

    async fn last_event(&self, kind: &str) -> Result<Option<Event>> {
        let row =
            sqlx::query("SELECT * FROM memory_events WHERE kind = ? ORDER BY created_at DESC, id DESC LIMIT 1")
                .bind(kind)
                .fetch_optional(&self.pool)
                .await?;
        row.as_ref().map(event_from_row).transpose()
    }

    async fn record_usage(&self, run: Uuid, ids: &[Uuid], injected: bool) -> Result<()> {
        let now = rfc3339(Utc::now());
        let mut tx = self.pool.begin().await?;
        for id in ids {
            sqlx::query(
                "INSERT INTO memory_usage (id, memory_id, run_id, retrieved_at, injected) VALUES (?, ?, ?, ?, ?)",
            )
            .bind(Uuid::new_v4().to_string())
            .bind(id.to_string())
            .bind(run.to_string())
            .bind(&now)
            .bind(injected)
            .execute(&mut *tx)
            .await?;
            sqlx::query("UPDATE memories SET last_accessed_at = ? WHERE id = ?")
                .bind(&now)
                .bind(id.to_string())
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn set_helpful(&self, run: Uuid, helpful: bool) -> Result<Vec<Uuid>> {
        let rows =
            sqlx::query("UPDATE memory_usage SET helpful = ? WHERE run_id = ? AND injected = 1 RETURNING memory_id")
                .bind(helpful)
                .bind(run.to_string())
                .fetch_all(&self.pool)
                .await?;
        let mut ids: Vec<Uuid> =
            rows.iter().map(|r| Ok(Uuid::parse_str(r.try_get("memory_id")?)?)).collect::<Result<_>>()?;
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    async fn usage(&self) -> Result<HashMap<Uuid, Usage>> {
        let rows = sqlx::query(
            "SELECT memory_id,
                    SUM(injected) AS injected,
                    SUM(helpful = 1) AS helpful,
                    SUM(helpful = 0) AS unhelpful
             FROM memory_usage GROUP BY memory_id",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = HashMap::new();
        for r in rows {
            let n = |col: &str| -> Result<u64> { Ok(r.try_get::<Option<i64>, _>(col)?.unwrap_or(0) as u64) };
            out.insert(
                Uuid::parse_str(r.try_get("memory_id")?)?,
                Usage { injected: n("injected")?, helpful: n("helpful")?, unhelpful: n("unhelpful")? },
            );
        }
        Ok(out)
    }

    async fn add_episode(&self, e: &Episode) -> Result<()> {
        sqlx::query(
            "INSERT INTO episodes (id, scope, summary, outcome, entities, started_at, ended_at, source_run_id)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(e.id.to_string())
        .bind(&e.scope)
        .bind(&e.summary)
        .bind(&e.outcome)
        .bind(serde_json::to_string(&e.entities)?)
        .bind(rfc3339(e.started_at))
        .bind(rfc3339(e.ended_at))
        .bind(e.source_run_id.map(|r| r.to_string()))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn episodes(&self, limit: usize) -> Result<Vec<Episode>> {
        let rows = sqlx::query("SELECT * FROM episodes ORDER BY ended_at DESC LIMIT ?")
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| {
                Ok(Episode {
                    id: Uuid::parse_str(r.try_get("id")?)?,
                    scope: r.try_get("scope")?,
                    summary: r.try_get("summary")?,
                    outcome: r.try_get("outcome")?,
                    entities: serde_json::from_str(r.try_get("entities")?)?,
                    started_at: timestamp(r.try_get("started_at")?)?,
                    ended_at: timestamp(r.try_get("ended_at")?)?,
                    source_run_id: uuid_opt(r, "source_run_id")?,
                })
            })
            .collect()
    }

    async fn add_proposal(&self, p: &Proposal) -> Result<()> {
        sqlx::query(
            "INSERT INTO memory_proposals (id, kind, change, reason, status, created_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(p.id.to_string())
        .bind(p.change.kind())
        .bind(serde_json::to_string(&p.change)?)
        .bind(&p.reason)
        .bind(p.status.as_str())
        .bind(rfc3339(p.created_at))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn pending_proposals(&self) -> Result<Vec<Proposal>> {
        let rows = sqlx::query("SELECT * FROM memory_proposals WHERE status = 'pending' ORDER BY created_at DESC")
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| {
                let change: MemoryChange = serde_json::from_str(r.try_get("change")?)?;
                Ok(Proposal {
                    id: Uuid::parse_str(r.try_get("id")?)?,
                    change,
                    reason: r.try_get("reason")?,
                    status: ProposalStatus::Pending,
                    created_at: timestamp(r.try_get("created_at")?)?,
                })
            })
            .collect()
    }

    async fn set_proposal_status(&self, id: Uuid, status: ProposalStatus) -> Result<()> {
        let updated = sqlx::query("UPDATE memory_proposals SET status = ?, resolved_at = ? WHERE id = ?")
            .bind(status.as_str())
            .bind(rfc3339(Utc::now()))
            .bind(id.to_string())
            .execute(&self.pool)
            .await?
            .rows_affected();
        if updated == 0 {
            bail!("no proposal with id {id}");
        }
        Ok(())
    }
}

/// Turn free text into an FTS5 query: each content word quoted (so punctuation
/// and FTS syntax can't break it) and OR-ed, letting bm25 rank memories that
/// match more or rarer words higher. `None` if the text has no content words.
fn fts_query(text: &str) -> Option<String> {
    let terms: Vec<String> = crate::text::content_words(text).into_iter().map(|w| format!("\"{w}\"")).collect();
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

fn uuid_opt(row: &SqliteRow, col: &str) -> Result<Option<Uuid>> {
    Ok(row.try_get::<Option<&str>, _>(col)?.map(Uuid::parse_str).transpose()?)
}

fn time_opt(row: &SqliteRow, col: &str) -> Result<Option<DateTime<Utc>>> {
    row.try_get::<Option<&str>, _>(col)?.map(timestamp).transpose()
}

fn from_row(row: &SqliteRow) -> Result<Memory> {
    let tags: Option<String> = row.try_get("tags")?;
    // Older rows may hold free-text sources; treat unknown ones as conversation.
    let source = row.try_get::<Option<&str>, _>("source")?.and_then(|s| s.parse().ok());
    Ok(Memory {
        id: Uuid::parse_str(row.try_get("id")?)?,
        scope: row.try_get("scope")?,
        kind: row.try_get::<&str, _>("kind")?.parse()?,
        content: row.try_get("content")?,
        tags: match tags {
            Some(json) => serde_json::from_str(&json)?,
            None => Vec::new(),
        },
        source: source.unwrap_or(crate::MemorySource::Conversation),
        provenance: Provenance {
            run_id: uuid_opt(row, "run_id")?,
            tool_call_id: row.try_get("tool_call_id")?,
            conversation_id: uuid_opt(row, "conversation_id")?,
        },
        importance: row.try_get("importance")?,
        confidence: row.try_get("confidence")?,
        status: row.try_get::<&str, _>("status")?.parse()?,
        created_at: timestamp(row.try_get("created_at")?)?,
        updated_at: timestamp(row.try_get("updated_at")?)?,
        last_accessed_at: time_opt(row, "last_accessed_at")?,
        expires_at: time_opt(row, "expires_at")?,
        usage: Usage::default(),
    })
}

fn event_from_row(r: &SqliteRow) -> Result<Event> {
    Ok(Event {
        id: Uuid::parse_str(r.try_get("id")?)?,
        memory_id: uuid_opt(r, "memory_id")?,
        kind: r.try_get("kind")?,
        from_state: r.try_get("from_state")?,
        to_state: r.try_get("to_state")?,
        reason: r.try_get("reason")?,
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
pub(crate) mod tests {
    use super::*;
    use crate::{MemoryKind, MemorySource, MemoryStatus};

    pub(crate) fn memory(scope: &str, content: &str) -> Memory {
        let now = Utc::now();
        Memory {
            id: Uuid::new_v4(),
            scope: scope.into(),
            kind: MemoryKind::Semantic,
            content: content.into(),
            tags: vec!["t".into()],
            source: MemorySource::User,
            provenance: Provenance::default(),
            importance: 0.5,
            confidence: 1.0,
            status: MemoryStatus::Active,
            created_at: now,
            updated_at: now,
            last_accessed_at: None,
            expires_at: None,
            usage: Usage::default(),
        }
    }

    #[tokio::test]
    async fn create_search_update_and_round_trip() {
        let s = SqliteStore::in_memory().await.unwrap();
        let mut m = memory("project:arcella", "The agent runtime will be written in Rust.");
        m.provenance = Provenance { run_id: Some(Uuid::new_v4()), tool_call_id: Some("call_1".into()), conversation_id: Some(Uuid::new_v4()) };
        s.create(&m).await.unwrap();
        s.create(&memory("project:arcella", "Bananas are yellow.")).await.unwrap();

        let found = s.search("agent runtime language", Some("project:arcella"), 5).await.unwrap();
        assert_eq!(found.len(), 1);
        assert!(found[0].1 > 0.0);
        let back = &found[0].0;
        assert_eq!(back.provenance, m.provenance);
        assert_eq!(
            (back.kind, back.source, back.status),
            (MemoryKind::Semantic, MemorySource::User, MemoryStatus::Active)
        );
        assert!(s.search("agent runtime", Some("project:other"), 5).await.unwrap().is_empty());

        m.content = "The agent runtime is written in Rust 2024.".into();
        m.status = MemoryStatus::Archived;
        s.update(&m).await.unwrap();
        let back = s.get(m.id).await.unwrap().unwrap();
        assert_eq!(back.status, MemoryStatus::Archived);
        assert_eq!(s.search("2024", None, 5).await.unwrap().len(), 1, "index follows updates");
    }

    #[tokio::test]
    async fn queries_with_fts_syntax_or_only_stopwords_are_safe() {
        let s = SqliteStore::in_memory().await.unwrap();
        s.create(&memory("user", "C++ is fine, but \"Rust\" is better.")).await.unwrap();
        assert_eq!(s.search("AND OR NOT \" * ( rust: ^", None, 5).await.unwrap().len(), 1);
        assert!(s.search("  ?! ", None, 5).await.unwrap().is_empty());
        assert!(s.search("what is the", None, 5).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn list_filters() {
        let s = SqliteStore::in_memory().await.unwrap();
        let mut a = memory("user", "a");
        a.status = MemoryStatus::Archived;
        let mut b = memory("user", "b");
        b.kind = MemoryKind::Episodic;
        s.create(&a).await.unwrap();
        s.create(&b).await.unwrap();
        s.create(&memory("project:x", "c")).await.unwrap();
        assert_eq!(s.list(&Filter::default(), 10).await.unwrap().len(), 3);
        assert_eq!(s.list(&Filter::active(), 10).await.unwrap().len(), 2);
        let user = Filter { scope: Some("user".into()), ..Filter::default() };
        assert_eq!(s.list(&user, 10).await.unwrap().len(), 2);
        let episodic = Filter { kind: Some(MemoryKind::Episodic), ..Filter::default() };
        assert_eq!(s.list(&episodic, 10).await.unwrap()[0].content, "b");
        assert_eq!(s.scopes().await.unwrap(), [("project:x".into(), 1), ("user".into(), 1)]);
    }

    #[tokio::test]
    async fn embeddings_round_trip_and_track_missing() {
        let s = SqliteStore::in_memory().await.unwrap();
        let (a, b) = (memory("user", "a"), memory("user", "b"));
        s.create(&a).await.unwrap();
        s.create(&b).await.unwrap();
        s.set_embedding(a.id, "emb", &[0.5, -1.25, 3.0]).await.unwrap();
        assert_eq!(s.embeddings("emb").await.unwrap(), [(a.id, vec![0.5, -1.25, 3.0])]);
        let missing = s.missing_embeddings("emb", 10).await.unwrap();
        assert_eq!(missing.iter().map(|m| m.id).collect::<Vec<_>>(), [b.id]);
        assert_eq!(s.missing_embeddings("other", 10).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn usage_versions_and_hard_delete_cascade() {
        let s = SqliteStore::in_memory().await.unwrap();
        let (a, b) = (memory("user", "alpha fact"), memory("user", "beta fact"));
        s.create(&a).await.unwrap();
        s.create(&b).await.unwrap();
        let run = Uuid::new_v4();
        s.record_usage(run, &[a.id, b.id], true).await.unwrap();
        assert_eq!(s.set_helpful(run, true).await.unwrap().len(), 2);
        assert_eq!(s.usage().await.unwrap()[&a.id], Usage { injected: 1, helpful: 1, unhelpful: 0 });
        assert!(s.get(a.id).await.unwrap().unwrap().last_accessed_at.is_some());

        assert_eq!(s.add_version(a.id, "alpha", 1.0, "created").await.unwrap(), 1);
        assert_eq!(s.add_version(a.id, "alpha fact", 1.0, "typo").await.unwrap(), 2);
        s.relate(b.id, a.id, Relationship::Supersedes, "newer").await.unwrap();
        s.set_embedding(a.id, "emb", &[1.0]).await.unwrap();

        s.delete(a.id).await.unwrap();
        assert!(s.get(a.id).await.unwrap().is_none());
        assert!(s.search("alpha", None, 5).await.unwrap().is_empty());
        assert!(s.versions(a.id).await.unwrap().is_empty());
        assert!(s.relationships(Some(b.id)).await.unwrap().is_empty());
        assert!(s.embeddings("emb").await.unwrap().is_empty());
        assert!(!s.usage().await.unwrap().contains_key(&a.id));
        assert!(s.delete(a.id).await.is_err());
    }

    #[tokio::test]
    async fn events_episodes_and_proposals() {
        let s = SqliteStore::in_memory().await.unwrap();
        let id = Uuid::new_v4();
        s.record(&Event::new("created", Some(id), "x")).await.unwrap();
        s.record(&Event::new("curated", None, "weekly")).await.unwrap();
        assert_eq!(s.events(Some(id), 10).await.unwrap().len(), 1);
        assert!(s.last_event("curated").await.unwrap().is_some());

        let now = Utc::now();
        let e = Episode {
            id,
            scope: "user".into(),
            summary: "Fixed PgBouncer auth".into(),
            outcome: "success".into(),
            entities: vec!["pgbouncer".into()],
            started_at: now,
            ended_at: now,
            source_run_id: None,
        };
        s.add_episode(&e).await.unwrap();
        assert_eq!(s.episodes(5).await.unwrap()[0].entities, ["pgbouncer"]);

        let p = Proposal::new(MemoryChange::Archive { memory: id }, "stale".into());
        s.add_proposal(&p).await.unwrap();
        assert_eq!(s.pending_proposals().await.unwrap()[0].change, p.change);
        s.set_proposal_status(p.id, ProposalStatus::Applied).await.unwrap();
        assert!(s.pending_proposals().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn persists_across_reopen() {
        let dir = std::env::temp_dir().join(format!("lyra-memory-test-{}", Uuid::new_v4()));
        let path = dir.join("memory").join("memory.db");
        {
            let s = SqliteStore::open(&path).await.unwrap();
            s.create(&memory("user", "Survives restarts.")).await.unwrap();
        }
        let s = SqliteStore::open(&path).await.unwrap();
        assert_eq!(s.search("restarts", None, 5).await.unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}

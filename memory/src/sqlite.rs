use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::{Memory, MemoryStore};

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
    async fn remember(
        &self,
        scope: &str,
        content: &str,
        tags: &[String],
        source: Option<&str>,
    ) -> Result<Memory> {
        let content = content.trim();
        if content.is_empty() {
            bail!("memory content is empty");
        }
        let now = Utc::now();
        let memory = Memory {
            id: Uuid::new_v4(),
            scope: scope.to_string(),
            content: content.to_string(),
            tags: tags.to_vec(),
            source: source.map(str::to_string),
            created_at: now,
            updated_at: now,
        };

        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO memories (id, scope, content, tags, source, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(memory.id.to_string())
        .bind(&memory.scope)
        .bind(&memory.content)
        .bind(serde_json::to_string(&memory.tags)?)
        .bind(&memory.source)
        .bind(rfc3339(memory.created_at))
        .bind(rfc3339(memory.updated_at))
        .execute(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO memories_fts (id, content) VALUES (?, ?)")
            .bind(memory.id.to_string())
            .bind(&memory.content)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(memory)
    }

    async fn recall(&self, scope: &str, query: &str, limit: usize) -> Result<Vec<Memory>> {
        let Some(fts) = fts_query(query) else { return Ok(Vec::new()) };
        // '*' is ALL_SCOPES.
        let rows = sqlx::query(
            "SELECT m.id, m.scope, m.content, m.tags, m.source, m.created_at, m.updated_at
             FROM memories_fts JOIN memories m ON m.id = memories_fts.id
             WHERE memories_fts MATCH ? AND (? = '*' OR m.scope = ?)
             ORDER BY bm25(memories_fts) LIMIT ?",
        )
            .bind(fts)
            .bind(scope)
            .bind(scope)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(from_row).collect()
    }

    async fn forget(&self, id: Uuid) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let deleted = sqlx::query("DELETE FROM memories WHERE id = ?")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if deleted == 0 {
            bail!("no memory with id {id}");
        }
        sqlx::query("DELETE FROM memories_fts WHERE id = ?")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn list(&self, scope: &str, limit: usize) -> Result<Vec<Memory>> {
        let rows = sqlx::query(
            "SELECT m.id, m.scope, m.content, m.tags, m.source, m.created_at, m.updated_at
             FROM memories m
             WHERE ? = '*' OR m.scope = ?
             ORDER BY m.created_at DESC, m.rowid DESC LIMIT ?",
        )
            .bind(scope)
            .bind(scope)
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(from_row).collect()
    }

    async fn scopes(&self) -> Result<Vec<(String, u64)>> {
        let rows = sqlx::query(
            "SELECT scope, COUNT(*) AS n FROM memories GROUP BY scope ORDER BY n DESC, scope",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| Ok((r.try_get("scope")?, r.try_get::<i64, _>("n")? as u64)))
            .collect()
    }
}

/// Turn free text into an FTS5 query: each word quoted (so punctuation and FTS
/// syntax can't break it) and OR-ed, letting bm25 rank memories that match more
/// or rarer words higher. `None` if the text has no words.
fn fts_query(text: &str) -> Option<String> {
    let terms: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| format!("\"{}\"", w.to_lowercase()))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

fn from_row(row: &SqliteRow) -> Result<Memory> {
    let tags: Option<String> = row.try_get("tags")?;
    Ok(Memory {
        id: Uuid::parse_str(row.try_get("id")?)?,
        scope: row.try_get("scope")?,
        content: row.try_get("content")?,
        tags: match tags {
            Some(json) => serde_json::from_str(&json)?,
            None => Vec::new(),
        },
        source: row.try_get("source")?,
        created_at: timestamp(row.try_get("created_at")?)?,
        updated_at: timestamp(row.try_get("updated_at")?)?,
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
    use crate::ALL_SCOPES;

    #[test]
    fn all_scopes_matches_the_sql_literal() {
        assert_eq!(ALL_SCOPES, "*");
    }

    fn tags(t: &[&str]) -> Vec<String> {
        t.iter().map(|s| s.to_string()).collect()
    }

    #[tokio::test]
    async fn remember_then_recall_by_keywords() {
        let store = SqliteStore::in_memory().await.unwrap();
        store
            .remember(
                "project:arcella",
                "The agent runtime will be written in Rust.",
                &tags(&["architecture", "rust"]),
                Some("conversation"),
            )
            .await
            .unwrap();
        store.remember("project:arcella", "Bananas are yellow.", &[], None).await.unwrap();

        // "language" appears in no memory; OR-ed terms still find the right one.
        let found = store.recall("project:arcella", "agent runtime language", 5).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].content, "The agent runtime will be written in Rust.");
        assert_eq!(found[0].tags, ["architecture", "rust"]);
        assert_eq!(found[0].source.as_deref(), Some("conversation"));
    }

    #[tokio::test]
    async fn recall_is_scoped() {
        let store = SqliteStore::in_memory().await.unwrap();
        store.remember("user", "Prefers dark mode.", &[], None).await.unwrap();
        store.remember("project:x", "Uses dark theme tokens.", &[], None).await.unwrap();

        assert_eq!(store.recall("user", "dark", 5).await.unwrap().len(), 1);
        assert_eq!(store.recall("project:y", "dark", 5).await.unwrap().len(), 0);
        assert_eq!(store.recall(ALL_SCOPES, "dark", 5).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn recall_ranks_better_matches_first_and_stems() {
        let store = SqliteStore::in_memory().await.unwrap();
        store.remember("user", "We use Postgres for analytics.", &[], None).await.unwrap();
        store.remember("user", "We decided on SQLite for the memory database.", &[], None).await.unwrap();

        let found = store.recall("user", "which database did we decide", 5).await.unwrap();
        assert_eq!(found[0].content, "We decided on SQLite for the memory database.");
    }

    #[tokio::test]
    async fn queries_with_fts_syntax_are_safe() {
        let store = SqliteStore::in_memory().await.unwrap();
        store.remember("user", "C++ is fine, but \"Rust\" is better.", &[], None).await.unwrap();
        let found = store.recall("user", "AND OR NOT \" * ( rust: ^", 5).await.unwrap();
        assert_eq!(found.len(), 1);
        assert!(store.recall("user", "  ?! ", 5).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn forget_removes_from_table_and_search() {
        let store = SqliteStore::in_memory().await.unwrap();
        let m = store.remember("user", "Temporary fact.", &[], None).await.unwrap();
        store.forget(m.id).await.unwrap();
        assert!(store.recall("user", "temporary", 5).await.unwrap().is_empty());
        assert!(store.list("user", 5).await.unwrap().is_empty());
        assert!(store.forget(m.id).await.is_err());
    }

    #[tokio::test]
    async fn list_is_newest_first_and_limited() {
        let store = SqliteStore::in_memory().await.unwrap();
        for i in 0..3 {
            store.remember("user", &format!("fact {i}"), &[], None).await.unwrap();
        }
        let listed = store.list("user", 2).await.unwrap();
        let contents: Vec<_> = listed.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, ["fact 2", "fact 1"]);
    }

    #[tokio::test]
    async fn scopes_counts_per_scope() {
        let store = SqliteStore::in_memory().await.unwrap();
        for (scope, content) in [("user", "a"), ("user", "b"), ("project:x", "c")] {
            store.remember(scope, content, &[], None).await.unwrap();
        }
        let scopes = store.scopes().await.unwrap();
        assert_eq!(scopes, [("user".to_string(), 2), ("project:x".to_string(), 1)]);
    }

    #[tokio::test]
    async fn empty_content_is_rejected() {
        let store = SqliteStore::in_memory().await.unwrap();
        assert!(store.remember("user", "   ", &[], None).await.is_err());
    }

    #[tokio::test]
    async fn persists_across_reopen() {
        let dir = std::env::temp_dir().join(format!("lyra-memory-test-{}", Uuid::new_v4()));
        let path = dir.join("data").join("memory.db");
        {
            let store = SqliteStore::open(&path).await.unwrap();
            store.remember("user", "Survives restarts.", &[], None).await.unwrap();
        }
        let store = SqliteStore::open(&path).await.unwrap();
        assert_eq!(store.recall("user", "restarts", 5).await.unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}

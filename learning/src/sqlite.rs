use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow};
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use crate::{Skill, SkillStatus, SkillStore};

/// Skills in a single SQLite file, searched with FTS5.
pub struct SqliteSkillStore {
    pool: SqlitePool,
}

impl SqliteSkillStore {
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
impl SkillStore for SqliteSkillStore {
    async fn learn(&self, skill: Skill) -> Result<Skill> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO skills
               (id, name, description, instructions, source, confidence, status, created_at, updated_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(skill.id.to_string())
        .bind(&skill.name)
        .bind(&skill.description)
        .bind(&skill.instructions)
        .bind(&skill.source)
        .bind(skill.confidence)
        .bind(skill.status.as_str())
        .bind(rfc3339(skill.created_at))
        .bind(rfc3339(skill.updated_at))
        .execute(&mut *tx)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(d) if d.is_unique_violation() => {
                anyhow!("a skill named {:?} already exists", skill.name)
            }
            _ => e.into(),
        })?;
        sqlx::query("INSERT INTO skills_fts (id, name, description, instructions) VALUES (?, ?, ?, ?)")
            .bind(skill.id.to_string())
            .bind(&skill.name)
            .bind(&skill.description)
            .bind(&skill.instructions)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(skill)
    }

    async fn search(&self, query: &str, limit: usize) -> Result<Vec<Skill>> {
        let Some(fts) = fts_query(query) else { return Ok(Vec::new()) };
        let rows = sqlx::query(
            "SELECT s.* FROM skills_fts JOIN skills s ON s.id = skills_fts.id
             WHERE skills_fts MATCH ? AND s.status = 'active'
             ORDER BY bm25(skills_fts) LIMIT ?",
        )
        .bind(fts)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(from_row).collect()
    }

    async fn update(&self, id: Uuid, instructions: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let updated = sqlx::query("UPDATE skills SET instructions = ?, updated_at = ? WHERE id = ?")
            .bind(instructions)
            .bind(rfc3339(Utc::now()))
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if updated == 0 {
            bail!("no skill with id {id}");
        }
        sqlx::query("UPDATE skills_fts SET instructions = ? WHERE id = ?")
            .bind(instructions)
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn forget(&self, id: Uuid) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let deleted = sqlx::query("DELETE FROM skills WHERE id = ?")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?
            .rows_affected();
        if deleted == 0 {
            bail!("no skill with id {id}");
        }
        sqlx::query("DELETE FROM skills_fts WHERE id = ?")
            .bind(id.to_string())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    async fn list(&self, status: Option<SkillStatus>, limit: usize) -> Result<Vec<Skill>> {
        let status = status.map(SkillStatus::as_str);
        let rows = sqlx::query(
            "SELECT * FROM skills WHERE ? IS NULL OR status = ?
             ORDER BY created_at DESC, rowid DESC LIMIT ?",
        )
        .bind(status)
        .bind(status)
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(from_row).collect()
    }

    async fn set_status(&self, id: Uuid, status: SkillStatus) -> Result<()> {
        let updated = sqlx::query("UPDATE skills SET status = ?, updated_at = ? WHERE id = ?")
            .bind(status.as_str())
            .bind(rfc3339(Utc::now()))
            .bind(id.to_string())
            .execute(&self.pool)
            .await?
            .rows_affected();
        if updated == 0 {
            bail!("no skill with id {id}");
        }
        Ok(())
    }
}

/// Words too common to say anything about which skill applies.
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "but", "by", "can", "could", "do", "does", "for",
    "from", "have", "how", "i", "in", "is", "it", "its", "me", "my", "of", "on", "or", "please",
    "should", "so", "that", "the", "then", "this", "to", "us", "was", "we", "what", "when",
    "where", "which", "will", "with", "would", "you", "your",
];

/// Free text to an FTS5 query: content words quoted and OR-ed, so bm25 ranks
/// skills sharing more (and rarer) words higher. `None` if no content words.
fn fts_query(text: &str) -> Option<String> {
    let terms: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|w| !w.is_empty() && !STOPWORDS.contains(&w.as_str()))
        .map(|w| format!("\"{w}\""))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

fn from_row(row: &SqliteRow) -> Result<Skill> {
    Ok(Skill {
        id: Uuid::parse_str(row.try_get("id")?)?,
        name: row.try_get("name")?,
        description: row.try_get("description")?,
        instructions: row.try_get("instructions")?,
        source: row.try_get("source")?,
        confidence: row.try_get("confidence")?,
        status: row.try_get::<&str, _>("status")?.parse()?,
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

    fn skill(name: &str, instructions: &str, status: SkillStatus) -> Skill {
        let now = Utc::now();
        Skill {
            id: Uuid::new_v4(),
            name: name.into(),
            description: format!("about {name}"),
            instructions: instructions.into(),
            source: "conversation".into(),
            confidence: 0.8,
            status,
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn search_finds_only_active_skills() {
        let store = SqliteSkillStore::in_memory().await.unwrap();
        let steps = "Implement ModelProvider, register it with ModelRouter, add config validation.";
        store.learn(skill("add-model-provider", steps, SkillStatus::Active)).await.unwrap();
        store.learn(skill("provider-draft", steps, SkillStatus::Proposed)).await.unwrap();

        let found = store.search("Add another model provider please", 5).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "add-model-provider");
        assert_eq!(found[0].status, SkillStatus::Active);
    }

    #[tokio::test]
    async fn stopword_only_queries_match_nothing() {
        let store = SqliteSkillStore::in_memory().await.unwrap();
        store.learn(skill("x", "do the thing with it", SkillStatus::Active)).await.unwrap();
        assert!(store.search("what is the", 5).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn names_are_unique() {
        let store = SqliteSkillStore::in_memory().await.unwrap();
        store.learn(skill("dup", "a", SkillStatus::Proposed)).await.unwrap();
        let err = store.learn(skill("dup", "b", SkillStatus::Proposed)).await.unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
    }

    #[tokio::test]
    async fn approve_update_and_forget() {
        let store = SqliteSkillStore::in_memory().await.unwrap();
        let s = store.learn(skill("deploy", "run deploy.sh", SkillStatus::Proposed)).await.unwrap();
        assert!(store.search("deploy", 5).await.unwrap().is_empty());

        store.set_status(s.id, SkillStatus::Active).await.unwrap();
        store.update(s.id, "run deploy.sh --dry-run first, then for real").await.unwrap();
        let found = store.search("dry run", 5).await.unwrap();
        assert_eq!(found[0].instructions, "run deploy.sh --dry-run first, then for real");

        store.forget(s.id).await.unwrap();
        assert!(store.list(None, 10).await.unwrap().is_empty());
        assert!(store.search("deploy", 5).await.unwrap().is_empty());
        assert!(store.forget(s.id).await.is_err());
        assert!(store.set_status(s.id, SkillStatus::Active).await.is_err());
    }

    #[tokio::test]
    async fn list_filters_by_status() {
        let store = SqliteSkillStore::in_memory().await.unwrap();
        store.learn(skill("a", "x", SkillStatus::Proposed)).await.unwrap();
        store.learn(skill("b", "y", SkillStatus::Active)).await.unwrap();
        store.learn(skill("c", "z", SkillStatus::Rejected)).await.unwrap();
        assert_eq!(store.list(None, 10).await.unwrap().len(), 3);
        let proposed = store.list(Some(SkillStatus::Proposed), 10).await.unwrap();
        assert_eq!(proposed.len(), 1);
        assert_eq!(proposed[0].name, "a");
    }
}

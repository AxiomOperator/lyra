//! The agent's only way into memory. The model proposes memories; this owns
//! the policy: what's safe and allowed to store, what's a duplicate, what
//! supersedes what, what's worth putting in the prompt, and the history of it all.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use uuid::Uuid;

use crate::capture;
use crate::curator::{self, Stats, overlap};
use crate::rank::{self, HalfLives, Score, Weights};
use crate::safety;
use crate::store::{Event, Filter, MemoryChange, Proposal, ProposalStatus, Version};
use crate::{
    Episode, Memory, MemoryKind, MemorySource, MemoryStatus, MemoryStore, NewMemory, Provenance, Relationship,
    SqliteStore,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CaptureMode {
    Off,
    /// Review turns that look durable and store what the model proposes (the default).
    #[default]
    Auto,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MaintenanceMode {
    /// No curation beyond expiring memories past their date.
    Off,
    /// Consolidations and archiving wait for approval (the default).
    #[default]
    Propose,
    /// Consolidations and archiving apply on their own (all reversible).
    Auto,
}

/// The context compiler's limits (M12).
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct Budget {
    /// Most tokens of memories added to a prompt.
    pub max_tokens: usize,
    pub max_memories: usize,
    /// Memories scoring below this aren't worth the space.
    pub min_score: f32,
    /// ...and a memory must actually match the message: by meaning (vector
    /// similarity) or keywords, at least this much. Importance and recency
    /// alone don't earn a place.
    pub min_relevance: f32,
}

impl Default for Budget {
    fn default() -> Self {
        Self { max_tokens: 600, max_memories: 8, min_score: 0.35, min_relevance: 0.45 }
    }
}

/// Memory policy, from `[memory]` in the config.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Scope for new memories when none is given.
    pub default_scope: String,
    /// Scopes this agent may read and write: exact names, `prefix:*` or `*` (M16).
    pub allowed_scopes: Vec<String>,
    pub capture: CaptureMode,
    pub maintenance: MaintenanceMode,
    /// Add relevant memories to each prompt (the context compiler).
    pub inject: bool,
    pub context: Budget,
    pub ranking: Weights,
    pub half_life_days: HalfLives,
    /// Word overlap at which two memories count as the same (reconfirmed, not re-saved).
    pub same_wording: f32,
    /// Vector similarity at which two memories count as the same.
    pub same_meaning: f32,
    /// Word overlap for the curator's duplicate candidates.
    pub duplicate_wording: f32,
    /// Unused this many days (and not important) counts as stale.
    pub stale_days: i64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            default_scope: "user".into(),
            allowed_scopes: vec!["*".into()],
            capture: CaptureMode::Auto,
            maintenance: MaintenanceMode::Propose,
            inject: true,
            context: Budget::default(),
            ranking: Weights::default(),
            half_life_days: HalfLives::default(),
            same_wording: 0.85,
            same_meaning: 0.95,
            duplicate_wording: 0.6,
            stale_days: 180,
        }
    }
}

/// A query's vector, and the model that made it.
#[derive(Debug, Clone, Copy)]
pub struct QueryVector<'a> {
    pub model: &'a str,
    pub vector: &'a [f32],
}

/// What `remember` did.
#[derive(Debug)]
pub enum Remembered {
    Created(Memory),
    /// Already known: the existing memory was reconfirmed instead of duplicated.
    Reconfirmed(Memory),
}

impl Remembered {
    pub fn memory(&self) -> &Memory {
        match self {
            Remembered::Created(m) | Remembered::Reconfirmed(m) => m,
        }
    }
}

/// A recalled memory and why it ranked there.
#[derive(Debug, Clone)]
pub struct Recalled {
    pub memory: Memory,
    pub score: Score,
}

/// What the context compiler chose for a prompt.
#[derive(Debug, Default)]
pub struct Compiled {
    /// The system prompt section, if anything made the cut.
    pub section: Option<String>,
    pub used: Vec<Recalled>,
    pub tokens: usize,
}

/// A memory with everything known about it (M15).
#[derive(Debug)]
pub struct Inspection {
    pub memory: Memory,
    pub versions: Vec<Version>,
    pub events: Vec<Event>,
    /// Readable relationship lines.
    pub relationships: Vec<String>,
    pub episode: Option<Episode>,
}

/// Findings of the curator's local checks.
#[derive(Debug, Default)]
pub struct Report {
    /// `(short id, short id, similarity)`.
    pub duplicates: Vec<(String, String, f32)>,
    pub stale: Vec<Uuid>,
}

pub struct MemoryManager<S: MemoryStore = SqliteStore> {
    store: S,
    settings: Settings,
}

impl MemoryManager<SqliteStore> {
    /// Open the SQLite-backed memory at `path`, creating it if needed.
    pub async fn open(path: &Path, settings: Settings) -> Result<Self> {
        Ok(Self::new(SqliteStore::open(path).await?, settings))
    }
}

/// About 4 characters per token, for budgets and display.
pub fn approx_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

impl<S: MemoryStore> MemoryManager<S> {
    pub fn new(store: S, settings: Settings) -> Self {
        Self { store, settings }
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    /// Whether this agent may use `scope` (M16).
    pub fn allowed(&self, scope: &str) -> bool {
        self.settings.allowed_scopes.iter().any(|pattern| match pattern.strip_suffix('*') {
            Some(prefix) => scope.starts_with(prefix),
            None => pattern == scope,
        })
    }

    fn check_scope(&self, scope: &str) -> Result<()> {
        if scope.trim().is_empty() {
            bail!("scope is empty");
        }
        if !self.allowed(scope) {
            bail!("scope {scope:?} isn't allowed ([memory] allowed_scopes)");
        }
        Ok(())
    }

    async fn with_usage(&self, mut memories: Vec<Memory>) -> Result<Vec<Memory>> {
        let usage = self.store.usage().await?;
        for m in &mut memories {
            m.usage = usage.get(&m.id).copied().unwrap_or_default();
        }
        Ok(memories)
    }

    // ---- reading

    pub async fn get(&self, id: Uuid) -> Result<Option<Memory>> {
        let Some(m) = self.store.get(id).await? else { return Ok(None) };
        Ok(self.with_usage(vec![m]).await?.pop())
    }

    /// A memory by id prefix.
    pub async fn find(&self, key: &str) -> Result<Memory> {
        let key = key.trim().trim_start_matches('[').trim_end_matches(']');
        if key.len() < 4 {
            bail!("give at least 4 characters of a memory id");
        }
        let matches: Vec<Memory> = self
            .store
            .list(&Filter::default(), usize::MAX >> 1)
            .await?
            .into_iter()
            .filter(|m| m.id.to_string().starts_with(key))
            .collect();
        match matches.len() {
            0 => bail!("no memory matches {key:?}"),
            1 => Ok(self.with_usage(matches).await?.pop().unwrap()),
            n => bail!("{n} memories match {key:?}; use more of the id"),
        }
    }

    /// Newest first, within allowed scopes.
    pub async fn list(&self, filter: &Filter, limit: usize) -> Result<Vec<Memory>> {
        let all = self.store.list(filter, limit.saturating_mul(4).max(limit)).await?;
        let allowed: Vec<Memory> = all.into_iter().filter(|m| self.allowed(&m.scope)).take(limit).collect();
        self.with_usage(allowed).await
    }

    pub async fn scopes(&self) -> Result<Vec<(String, u64)>> {
        Ok(self.store.scopes().await?.into_iter().filter(|(s, _)| self.allowed(s)).collect())
    }

    /// Hybrid recall (M5): keyword and vector candidates, filtered to usable
    /// memories in allowed scopes, ranked by every signal.
    pub async fn recall(
        &self,
        scope: Option<&str>,
        query: &str,
        query_vector: Option<QueryVector<'_>>,
        limit: usize,
        include_archived: bool,
    ) -> Result<Vec<Recalled>> {
        if let Some(scope) = scope {
            self.check_scope(scope)?;
        }
        let now = Utc::now();
        // id -> (memory, lexical score)
        let mut candidates: HashMap<Uuid, (Memory, f32)> = HashMap::new();
        for (m, lexical) in self.store.search(query, scope, 40).await? {
            candidates.insert(m.id, (m, lexical));
        }
        let mut similarity: HashMap<Uuid, f32> = HashMap::new();
        if let Some(q) = query_vector {
            let vectors: HashMap<Uuid, Vec<f32>> = self.store.embeddings(q.model).await?.into_iter().collect();
            let mut by_meaning: Vec<(Uuid, f32)> =
                vectors.iter().map(|(id, v)| (*id, rank::cosine(q.vector, v))).collect();
            by_meaning.sort_by(|a, b| b.1.total_cmp(&a.1));
            for (id, sim) in by_meaning.iter().take(40) {
                similarity.insert(*id, *sim);
                if !candidates.contains_key(id)
                    && let Some(m) = self.store.get(*id).await?
                {
                    candidates.insert(*id, (m, 0.0));
                }
            }
            for (id, v) in &vectors {
                similarity.entry(*id).or_insert_with(|| rank::cosine(q.vector, v));
            }
        }
        let usable = |m: &Memory| {
            let status_ok = m.status == MemoryStatus::Active || (include_archived && m.status == MemoryStatus::Archived);
            status_ok && !m.is_expired(now) && self.allowed(&m.scope) && scope.is_none_or(|s| s == m.scope)
        };
        let relationships = self.store.relationships(None).await?;
        let supported: HashSet<Uuid> =
            relationships.iter().filter(|r| r.2 == Relationship::Supports).map(|r| r.1).collect();
        let contradicted: HashSet<Uuid> = relationships
            .iter()
            .filter(|r| r.2 == Relationship::Contradicts)
            .flat_map(|r| [r.0, r.1])
            .collect();
        let usage = self.store.usage().await?;
        let (w, h) = (&self.settings.ranking, &self.settings.half_life_days);
        let mut ranked: Vec<Recalled> = candidates
            .into_values()
            .filter(|(m, _)| usable(m))
            .map(|(mut m, _bm25)| {
                m.usage = usage.get(&m.id).copied().unwrap_or_default();
                // Keyword search finds candidates; how well they match is
                // measured absolutely, so a weak best match stays weak.
                let lexical = crate::text::coverage(query, &m.content);
                let semantic = query_vector.and_then(|_| similarity.get(&m.id).copied());
                let relationship = if contradicted.contains(&m.id) {
                    0.0
                } else if supported.contains(&m.id) {
                    1.0
                } else {
                    0.5
                };
                let score = rank::score(&m, lexical, semantic, relationship, w, h, now);
                Recalled { memory: m, score }
            })
            .collect();
        ranked.sort_by(|a, b| b.score.total.total_cmp(&a.score.total));
        ranked.truncate(limit);
        Ok(ranked)
    }

    /// The context compiler (M12): pick the few memories worth the prompt
    /// space for this message, within the token budget, and record their use.
    pub async fn compile(&self, query: &str, query_vector: Option<QueryVector<'_>>, run: Uuid) -> Result<Compiled> {
        if !self.settings.inject {
            return Ok(Compiled::default());
        }
        let b = self.settings.context;
        let candidates = self.recall(None, query, query_vector, 20, false).await?;
        let mut used: Vec<Recalled> = Vec::new();
        let mut tokens = 0;
        for c in candidates {
            if used.len() >= b.max_memories {
                break;
            }
            let relevance = c.score.semantic.unwrap_or(0.0).max(c.score.lexical);
            if c.score.total < b.min_score || relevance < b.min_relevance {
                continue;
            }
            // Two memories saying the same thing waste space.
            if used.iter().any(|u| overlap(&u.memory.content, &c.memory.content) >= 0.8) {
                continue;
            }
            let cost = approx_tokens(&c.memory.content) + 12;
            if tokens + cost > b.max_tokens {
                continue;
            }
            tokens += cost;
            used.push(c);
        }
        if used.is_empty() {
            return Ok(Compiled::default());
        }
        let ids: Vec<Uuid> = used.iter().map(|u| u.memory.id).collect();
        self.store.record_usage(run, &ids, true).await?;
        let mut section = String::from(
            "# Relevant memories\n\nThings remembered from earlier conversations that may help with \
             this message. Each has an id for memory_correct, memory_supersede and memory_forget. \
             Ones marked unsure were inferred and may be wrong.\n",
        );
        for u in &used {
            let m = &u.memory;
            let unsure = if m.confidence < 0.7 { "; unsure" } else { "" };
            section += &format!("\n- [{}] {} ({}{unsure})", m.short_id(), m.content, m.scope);
        }
        Ok(Compiled { section: Some(section), used, tokens })
    }

    /// Note that a run retrieved these memories itself (the recall tool).
    pub async fn record_recall(&self, run: Uuid, ids: &[Uuid]) -> Result<()> {
        self.store.record_usage(run, ids, true).await
    }

    /// The user's reaction to a run: were its memories helpful? (M13)
    pub async fn feedback(&self, run: Uuid, helpful: bool) -> Result<usize> {
        Ok(self.store.set_helpful(run, helpful).await?.len())
    }

    // ---- writing (M1, M3, M6, M14, M16, M17)

    /// Store a memory, unless it's unsafe, not allowed, or already known (in
    /// which case the existing one is reconfirmed).
    pub async fn remember(&self, new: NewMemory, vector: Option<QueryVector<'_>>) -> Result<Remembered> {
        let content = new.content.trim().to_string();
        if content.is_empty() {
            bail!("memory content is empty");
        }
        self.check_scope(&new.scope)?;
        if let Some(why) = safety::scan(&content) {
            self.store.record(&Event::new("rejected", None, format!("not stored: {why}")).run(new.provenance.run_id)).await?;
            bail!("not stored: {why}");
        }
        if let Some(existing) = self.same_as(&new.scope, &content, vector).await? {
            return Ok(Remembered::Reconfirmed(self.reconfirm(existing, &new).await?));
        }
        let now = Utc::now();
        let memory = Memory {
            id: Uuid::new_v4(),
            scope: new.scope,
            kind: new.kind,
            content,
            tags: new.tags,
            source: new.source,
            provenance: new.provenance,
            importance: new.importance.unwrap_or(0.5).clamp(0.0, 1.0),
            confidence: new.confidence.unwrap_or_else(|| new.source.default_confidence()).clamp(0.0, 1.0),
            status: MemoryStatus::Active,
            created_at: now,
            updated_at: now,
            last_accessed_at: None,
            expires_at: new.expires_at.or_else(|| {
                // Working notes expire on their own.
                (new.kind == MemoryKind::Working).then(|| now + Duration::days(1))
            }),
            usage: Default::default(),
        };
        self.store.create(&memory).await?;
        self.store.add_version(memory.id, &memory.content, memory.confidence, "created").await?;
        if let Some(v) = vector {
            self.store.set_embedding(memory.id, v.model, v.vector).await?;
        }
        let event = Event::new("created", Some(memory.id), format!("from {}", memory.source))
            .states("none", MemoryStatus::Active)
            .run(memory.provenance.run_id);
        self.store.record(&event).await?;
        Ok(Remembered::Created(memory))
    }

    /// An active memory in `scope` that already says `content`.
    async fn same_as(&self, scope: &str, content: &str, vector: Option<QueryVector<'_>>) -> Result<Option<Memory>> {
        for (m, _) in self.store.search(content, Some(scope), 5).await? {
            if m.status == MemoryStatus::Active && overlap(&m.content, content) >= self.settings.same_wording {
                return Ok(Some(m));
            }
        }
        if let Some(q) = vector {
            for (id, v) in self.store.embeddings(q.model).await? {
                if rank::cosine(q.vector, &v) >= self.settings.same_meaning
                    && let Some(m) = self.store.get(id).await?
                    && m.status == MemoryStatus::Active
                    && m.scope == scope
                {
                    return Ok(Some(m));
                }
            }
        }
        Ok(None)
    }

    /// Hearing something again makes it more trustworthy, not a second copy.
    async fn reconfirm(&self, mut m: Memory, new: &NewMemory) -> Result<Memory> {
        let confidence = new.confidence.unwrap_or_else(|| new.source.default_confidence());
        m.confidence = m.confidence.max(confidence);
        m.importance = m.importance.max(new.importance.unwrap_or(0.0));
        for tag in &new.tags {
            if !m.tags.contains(tag) {
                m.tags.push(tag.clone());
            }
        }
        m.updated_at = Utc::now();
        self.store.update(&m).await?;
        self.store
            .record(&Event::new("reconfirmed", Some(m.id), format!("heard again from {}", new.source)).run(new.provenance.run_id))
            .await?;
        Ok(m)
    }

    /// Fix a memory's content (a typo, a missing detail): same fact, new
    /// version (M17). Returns the version number. Changes over time should
    /// supersede instead.
    pub async fn correct(&self, id: Uuid, content: &str, reason: &str, run: Option<Uuid>) -> Result<i64> {
        let mut m = self.get(id).await?.ok_or_else(|| anyhow!("no memory with id {id}"))?;
        self.check_scope(&m.scope)?;
        let content = content.trim();
        if content.is_empty() {
            bail!("memory content is empty");
        }
        if let Some(why) = safety::scan(content) {
            bail!("not stored: {why}");
        }
        if self.store.versions(id).await?.is_empty() {
            self.store.add_version(id, &m.content, m.confidence, "before correction").await?;
        }
        m.content = content.to_string();
        m.updated_at = Utc::now();
        self.store.update(&m).await?;
        let version = self.store.add_version(id, content, m.confidence, reason).await?;
        self.store
            .record(&Event::new("corrected", Some(id), reason).states(format!("v{}", version - 1), format!("v{version}")).run(run))
            .await?;
        Ok(version)
    }

    /// Replace a memory that's no longer true with a new one; the old one is
    /// kept, marked superseded and linked (M3, M14).
    pub async fn supersede(&self, old: Uuid, new: NewMemory, reason: &str, vector: Option<QueryVector<'_>>) -> Result<Memory> {
        let mut previous = self.get(old).await?.ok_or_else(|| anyhow!("no memory with id {old}"))?;
        self.check_scope(&previous.scope)?;
        let run = new.provenance.run_id;
        let created = match self.remember(new, vector).await? {
            Remembered::Created(m) => m,
            Remembered::Reconfirmed(m) if m.id == old => bail!("that's what the memory already says"),
            Remembered::Reconfirmed(m) => m,
        };
        self.store.relate(created.id, old, Relationship::Supersedes, reason).await?;
        let from = previous.status;
        previous.status = MemoryStatus::Superseded;
        previous.updated_at = Utc::now();
        self.store.update(&previous).await?;
        self.store
            .record(&Event::new("superseded", Some(old), format!("by {}: {reason}", created.short_id())).states(from, MemoryStatus::Superseded).run(run))
            .await?;
        Ok(created)
    }

    async fn set_status(&self, id: Uuid, status: MemoryStatus, kind: &str, reason: &str, run: Option<Uuid>) -> Result<Memory> {
        let mut m = self.get(id).await?.ok_or_else(|| anyhow!("no memory with id {id}"))?;
        self.check_scope(&m.scope)?;
        if m.status == status {
            bail!("memory {} is already {status}", m.short_id());
        }
        let from = m.status;
        m.status = status;
        m.updated_at = Utc::now();
        self.store.update(&m).await?;
        self.store.record(&Event::new(kind, Some(id), reason).states(from, status).run(run)).await?;
        Ok(m)
    }

    /// Keep for reference but leave out of normal recall (M11).
    pub async fn archive(&self, id: Uuid, reason: &str, run: Option<Uuid>) -> Result<Memory> {
        self.set_status(id, MemoryStatus::Archived, "archived", reason, run).await
    }

    /// Soft forget: out of everything but inspection; `restore` undoes it (M11).
    pub async fn forget(&self, id: Uuid, reason: &str, run: Option<Uuid>) -> Result<Memory> {
        self.set_status(id, MemoryStatus::Deleted, "forgotten", reason, run).await
    }

    /// Bring back an archived, forgotten or superseded memory.
    pub async fn restore(&self, id: Uuid) -> Result<Memory> {
        self.set_status(id, MemoryStatus::Active, "restored", "restored by the user", None).await
    }

    /// Hard delete, with everything attached; only the audit log remembers (M11).
    pub async fn purge(&self, id: Uuid) -> Result<()> {
        let m = self.get(id).await?.ok_or_else(|| anyhow!("no memory with id {id}"))?;
        self.store.delete(id).await?;
        self.store.record(&Event::new("purged", Some(id), "deleted for good").states(m.status, "none")).await
    }

    /// Archive memories past their expiry date. Returns notes.
    pub async fn expire_due(&self) -> Result<Vec<String>> {
        let now = Utc::now();
        let mut notes = Vec::new();
        for m in self.store.list(&Filter::active(), usize::MAX >> 1).await? {
            if m.is_expired(now) {
                self.set_status(m.id, MemoryStatus::Archived, "expired", "past its expiry date", None).await?;
                notes.push(format!("expired [{}] {}", m.short_id(), truncate(&m.content, 60)));
            }
        }
        Ok(notes)
    }

    // ---- vectors (M5)

    pub async fn needs_embedding(&self, model: &str, limit: usize) -> Result<Vec<Memory>> {
        self.store.missing_embeddings(model, limit).await
    }

    pub async fn set_embedding(&self, id: Uuid, model: &str, vector: &[f32]) -> Result<()> {
        self.store.set_embedding(id, model, vector).await
    }

    // ---- episodes (M8)

    /// Record a significant run as an episode (also an episodic memory).
    pub async fn add_episode(&self, scope: &str, summary: &str, outcome: &str, entities: Vec<String>, run: Option<Uuid>) -> Result<Memory> {
        let content = format!("{} Outcome: {}", summary.trim(), outcome.trim());
        let new = NewMemory {
            kind: MemoryKind::Episodic,
            tags: vec!["episode".into()],
            provenance: Provenance { run_id: run, tool_call_id: None },
            importance: Some(0.5),
            ..NewMemory::fact(scope, &content, MemorySource::Derived)
        };
        let m = match self.remember(new, None).await? {
            Remembered::Created(m) => m,
            Remembered::Reconfirmed(m) => return Ok(m),
        };
        let now = Utc::now();
        let episode = Episode {
            id: m.id,
            scope: scope.into(),
            summary: summary.trim().into(),
            outcome: outcome.trim().into(),
            entities,
            started_at: now,
            ended_at: now,
            source_run_id: run,
        };
        self.store.add_episode(&episode).await?;
        Ok(m)
    }

    pub async fn episodes(&self, limit: usize) -> Result<Vec<Episode>> {
        self.store.episodes(limit).await
    }

    // ---- automatic capture (M2, M3, M8)

    /// Apply the capture reviewer's plan: create, update or supersede
    /// memories and record an episode, each through the usual checks.
    /// `similar` are the memories the reviewer was shown (ids it may target).
    /// Returns notes for the log.
    pub async fn apply_capture(&self, plan: capture::Plan, similar: &[Memory], run: Option<Uuid>) -> Result<Vec<String>> {
        let mut notes = Vec::new();
        let target = |key: &str| similar.iter().find(|m| !key.is_empty() && m.id.to_string().starts_with(key.trim_matches(['[', ']'])));
        for item in plan.memories {
            let action = item.action.as_str();
            if action == "ignore" || item.content.trim().is_empty() {
                continue;
            }
            let new = NewMemory {
                scope: if item.scope.trim().is_empty() { self.settings.default_scope.clone() } else { item.scope.trim().to_string() },
                kind: item.kind.parse().unwrap_or(MemoryKind::Semantic),
                content: item.content.clone(),
                tags: item.tags.clone(),
                source: MemorySource::Conversation,
                provenance: Provenance { run_id: run, tool_call_id: None },
                importance: item.importance,
                confidence: item.confidence,
                expires_at: item.expires_in_days.filter(|d| *d > 0.0).map(|d| Utc::now() + Duration::minutes((d * 1440.0) as i64)),
            };
            let result = match (action, target(&item.target)) {
                ("update", Some(old)) => self
                    .correct(old.id, &item.content, if item.reason.is_empty() { "refined" } else { &item.reason }, run)
                    .await
                    .map(|v| format!("updated [{}] to v{v}: {}", old.short_id(), truncate(&item.content, 60))),
                ("supersede", Some(old)) => self
                    .supersede(old.id, new, if item.reason.is_empty() { "changed" } else { &item.reason }, None)
                    .await
                    .map(|m| format!("[{}] supersedes [{}]: {}", m.short_id(), old.short_id(), truncate(&m.content, 60))),
                _ => self.remember(new, None).await.map(|r| match r {
                    Remembered::Created(m) => format!("remembered [{}] {}", m.short_id(), truncate(&m.content, 60)),
                    Remembered::Reconfirmed(m) => format!("reconfirmed [{}] {}", m.short_id(), truncate(&m.content, 60)),
                }),
            };
            notes.push(result.unwrap_or_else(|e| format!("skipped: {e:#}")));
        }
        if let Some(ep) = plan.episode.filter(|e| !e.summary.trim().is_empty()) {
            let scope = self.settings.default_scope.clone();
            match self.add_episode(&scope, &ep.summary, &ep.outcome, ep.entities, run).await {
                Ok(m) => notes.push(format!("episode [{}] {}", m.short_id(), truncate(&ep.summary, 60))),
                Err(e) => notes.push(format!("episode skipped: {e:#}")),
            }
        }
        Ok(notes)
    }

    // ---- maintenance (M9, M18)

    /// The checks that need no model: near-duplicates and stale memories.
    pub async fn review_collection(&self, model: Option<&str>) -> Result<Report> {
        let all = self.list(&Filter::active(), usize::MAX >> 2).await?;
        let vectors: HashMap<Uuid, Vec<f32>> = match model {
            Some(model) => self.store.embeddings(model).await?.into_iter().collect(),
            None => HashMap::new(),
        };
        let short = |id: Uuid| id.to_string()[..8].to_string();
        Ok(Report {
            duplicates: curator::duplicates(&all, &vectors, self.settings.duplicate_wording, self.settings.same_meaning - 0.03)
                .into_iter()
                .map(|(a, b, sim)| (short(a), short(b), sim))
                .collect(),
            stale: curator::stale(&all, self.settings.stale_days, Utc::now()),
        })
    }

    /// Act on a curation: consolidations and stale archiving apply in auto
    /// mode and become proposals in propose mode; contradictions are always
    /// flagged, never resolved silently. Returns notes.
    pub async fn apply_curation(&self, plan: curator::Plan, stale: &[Uuid], run: Option<Uuid>) -> Result<Vec<String>> {
        let mode = self.settings.maintenance;
        let mut notes = Vec::new();
        if mode != MaintenanceMode::Off {
            let all = self.list(&Filter::active(), usize::MAX >> 2).await?;
            let resolve = |keys: &[String]| -> Option<Vec<&Memory>> {
                keys.iter().map(|k| all.iter().find(|m| m.id.to_string().starts_with(k.trim_matches(['[', ']'])))).collect()
            };
            for c in plan.consolidations {
                let Some(found) = resolve(&c.memories).filter(|f| f.len() >= 2) else { continue };
                // Only facts are consolidated, and never across scopes.
                let mixed = found.iter().any(|m| m.scope != found[0].scope || m.kind != MemoryKind::Semantic);
                if mixed || c.content.trim().is_empty() {
                    notes.push(format!("skipped a consolidation of {}: different scopes or not facts", c.memories.join(" + ")));
                    continue;
                }
                let mut tags: Vec<String> = found.iter().flat_map(|m| m.tags.clone()).collect();
                tags.sort();
                tags.dedup();
                let change = MemoryChange::Merge {
                    memories: found.iter().map(|m| m.id).collect(),
                    scope: found[0].scope.clone(),
                    content: c.content.trim().to_string(),
                    tags,
                };
                notes.push(self.propose_or_apply(change, &c.reason, run).await?);
            }
            for id in stale {
                notes.push(self.propose_or_apply(MemoryChange::Archive { memory: *id }, "unused and not important", run).await?);
            }
            for c in plan.contradictions {
                let Some(found) = resolve(&c.memories).filter(|f| f.len() == 2) else { continue };
                self.store.relate(found[0].id, found[1].id, Relationship::Contradicts, &c.reason).await?;
                self.store.record(&Event::new("contradiction", Some(found[0].id), c.reason.clone()).run(run)).await?;
                notes.push(format!("contradiction: [{}] vs [{}] — {}", found[0].short_id(), found[1].short_id(), c.reason));
            }
        }
        self.store.record(&Event::new("curated", None, format!("{} findings", notes.len())).run(run)).await?;
        Ok(notes)
    }

    async fn propose_or_apply(&self, change: MemoryChange, reason: &str, run: Option<Uuid>) -> Result<String> {
        if self.settings.maintenance == MaintenanceMode::Auto {
            return self.apply_change(&change, reason, run).await;
        }
        let pending = self.store.pending_proposals().await?;
        if pending.iter().any(|p| p.change == change) {
            return Ok(format!("already proposed: {}", change.kind()));
        }
        let p = Proposal::new(change, reason.to_string());
        self.store.add_proposal(&p).await?;
        Ok(format!("proposed {} [{}]: {reason}", p.change.kind(), &p.id.to_string()[..8]))
    }

    async fn apply_change(&self, change: &MemoryChange, reason: &str, run: Option<Uuid>) -> Result<String> {
        match change {
            MemoryChange::Merge { memories, scope, content, tags } => {
                let sources: Vec<Memory> = {
                    let mut v = Vec::new();
                    for id in memories {
                        v.push(self.get(*id).await?.ok_or_else(|| anyhow!("memory {id} no longer exists"))?);
                    }
                    v
                };
                let now = Utc::now();
                let merged = Memory {
                    id: Uuid::new_v4(),
                    scope: scope.clone(),
                    kind: sources[0].kind,
                    content: content.clone(),
                    tags: tags.clone(),
                    source: MemorySource::Derived,
                    provenance: Provenance { run_id: run, tool_call_id: None },
                    importance: sources.iter().map(|m| m.importance).fold(0.0, f32::max),
                    // Repeated observations strengthen the consolidated memory.
                    confidence: (sources.iter().map(|m| m.confidence).fold(0.0, f32::max) + 0.05).min(1.0),
                    status: MemoryStatus::Active,
                    created_at: now,
                    updated_at: now,
                    last_accessed_at: None,
                    expires_at: None,
                    usage: Default::default(),
                };
                if let Some(why) = safety::scan(&merged.content) {
                    bail!("not stored: {why}");
                }
                self.store.create(&merged).await?;
                self.store.add_version(merged.id, &merged.content, merged.confidence, "consolidated").await?;
                self.store.record(&Event::new("created", Some(merged.id), format!("consolidated: {reason}")).states("none", "active").run(run)).await?;
                for s in &sources {
                    self.store.relate(merged.id, s.id, Relationship::DerivedFrom, reason).await?;
                    self.store.relate(merged.id, s.id, Relationship::Supersedes, reason).await?;
                    if s.status == MemoryStatus::Active {
                        self.set_status(s.id, MemoryStatus::Superseded, "superseded", &format!("consolidated into {}", merged.short_id()), run).await?;
                    }
                }
                Ok(format!("consolidated {} memories into [{}]", sources.len(), merged.short_id()))
            }
            MemoryChange::Archive { memory } => {
                let m = self.archive(*memory, reason, run).await?;
                Ok(format!("archived [{}] {}", m.short_id(), truncate(&m.content, 50)))
            }
        }
    }

    pub async fn proposals(&self) -> Result<Vec<Proposal>> {
        self.store.pending_proposals().await
    }

    async fn find_proposal(&self, key: &str) -> Result<Proposal> {
        let key = key.trim();
        let matches: Vec<Proposal> =
            self.proposals().await?.into_iter().filter(|p| p.id.to_string().starts_with(key)).collect();
        match matches.len() {
            0 => bail!("no pending memory proposal matches {key:?}"),
            1 => Ok(matches.into_iter().next().unwrap()),
            n => bail!("{n} proposals match {key:?}; use more of the id"),
        }
    }

    pub async fn approve(&self, key: &str) -> Result<String> {
        let p = self.find_proposal(key).await?;
        let note = self.apply_change(&p.change, &format!("approved: {}", p.reason), None).await?;
        self.store.set_proposal_status(p.id, ProposalStatus::Applied).await?;
        Ok(note)
    }

    pub async fn reject(&self, key: &str) -> Result<String> {
        let p = self.find_proposal(key).await?;
        self.store.set_proposal_status(p.id, ProposalStatus::Rejected).await?;
        Ok(format!("rejected the proposed {}", p.change.kind()))
    }

    pub async fn last_curated(&self) -> Result<Option<DateTime<Utc>>> {
        Ok(self.store.last_event("curated").await?.map(|e| e.created_at))
    }

    // ---- introspection (M15)

    pub async fn inspect(&self, id: Uuid) -> Result<Inspection> {
        let memory = self.get(id).await?.ok_or_else(|| anyhow!("no memory with id {id}"))?;
        let all = self.store.list(&Filter::default(), usize::MAX >> 2).await?;
        let short = |id: Uuid| all.iter().find(|m| m.id == id).map_or(id.to_string()[..8].to_string(), |m| m.short_id());
        let relationships = self
            .store
            .relationships(Some(id))
            .await?
            .into_iter()
            .map(|(from, to, rel, reason)| {
                if from == id {
                    format!("{rel} [{}]: {reason}", short(to))
                } else {
                    format!("[{}] {rel} this: {reason}", short(from))
                }
            })
            .collect();
        let episode = self.store.episodes(usize::MAX >> 2).await?.into_iter().find(|e| e.id == id);
        Ok(Inspection {
            versions: self.store.versions(id).await?,
            events: self.store.events(Some(id), 50).await?,
            relationships,
            episode,
            memory,
        })
    }

    /// Recent audit entries across all memories.
    pub async fn recent_events(&self, limit: usize) -> Result<Vec<Event>> {
        self.store.events(None, limit).await
    }

    pub async fn stats(&self, model: Option<&str>) -> Result<Stats> {
        let all = self.list(&Filter::default(), usize::MAX >> 2).await?;
        let now = Utc::now();
        let active: Vec<&Memory> = all.iter().filter(|m| m.status == MemoryStatus::Active).collect();
        let count = |pred: &dyn Fn(&Memory) -> bool| all.iter().filter(|m| pred(m)).count();
        let by_kind = [MemoryKind::Semantic, MemoryKind::Episodic, MemoryKind::Working]
            .into_iter()
            .map(|k| (k, active.iter().filter(|m| m.kind == k).count()))
            .filter(|(_, n)| *n > 0)
            .collect();
        let by_status = [MemoryStatus::Active, MemoryStatus::Superseded, MemoryStatus::Archived, MemoryStatus::Deleted]
            .into_iter()
            .map(|s| (s, count(&|m: &Memory| m.status == s)))
            .filter(|(_, n)| *n > 0)
            .collect();
        let embedded: HashSet<Uuid> = match model {
            Some(model) => self.store.embeddings(model).await?.into_iter().map(|(id, _)| id).collect(),
            None => HashSet::new(),
        };
        let contradictions = self
            .store
            .relationships(None)
            .await?
            .iter()
            .filter(|r| r.2 == Relationship::Contradicts)
            .count();
        let report = self.review_collection(model).await?;
        Ok(Stats {
            total: all.len(),
            active: active.len(),
            by_kind,
            by_status,
            by_scope: self.scopes().await?,
            unused: active.iter().filter(|m| m.usage.injected == 0).count(),
            expired: active.iter().filter(|m| m.is_expired(now)).count(),
            contradictions,
            duplicate_candidates: report.duplicates.len(),
            average_confidence: (!active.is_empty())
                .then(|| active.iter().map(|m| m.confidence).sum::<f32>() / active.len() as f32),
            embedded: active.iter().filter(|m| embedded.contains(&m.id)).count(),
            episodes: self.store.episodes(usize::MAX >> 2).await?.len(),
            pending_proposals: self.proposals().await?.len(),
        })
    }
}

fn truncate(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        flat
    } else {
        format!("{}…", flat.chars().take(max - 1).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::{EpisodeItem, Item};
    use crate::curator::{Consolidation, Contradiction};

    async fn manager(settings: Settings) -> MemoryManager {
        MemoryManager::new(SqliteStore::in_memory().await.unwrap(), settings)
    }

    fn fact(scope: &str, content: &str) -> NewMemory {
        NewMemory::fact(scope, content, MemorySource::User)
    }

    #[tokio::test]
    async fn remember_recall_and_scope_isolation() {
        let m = manager(Settings::default()).await;
        m.remember(fact("project:arcella", "The agent runtime will be written in Rust."), None).await.unwrap();
        m.remember(fact("project:other", "The agent runtime is Go."), None).await.unwrap();

        let found = m.recall(Some("project:arcella"), "agent runtime language", None, 5, false).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].memory.confidence, 1.0, "the user said it");
        assert_eq!(m.recall(None, "agent runtime", None, 5, false).await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn duplicates_are_reconfirmed_not_stored_twice() {
        let m = manager(Settings::default()).await;
        let mut first = NewMemory::fact("user", "Garrett prefers Rust for the agent runtime.", MemorySource::Agent);
        first.tags = vec!["preference".into()];
        let a = m.remember(first, None).await.unwrap();
        assert_eq!(a.memory().confidence, 0.5, "inferred");
        let b = m.remember(fact("user", "Garrett prefers Rust for the agent runtime"), None).await.unwrap();
        assert!(matches!(b, Remembered::Reconfirmed(_)));
        assert_eq!(b.memory().id, a.memory().id);
        assert_eq!(b.memory().confidence, 1.0, "confirmed by the user");
        assert_eq!(m.list(&Filter::default(), 10).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn vector_duplicates_are_caught_too() {
        let m = manager(Settings::default()).await;
        let v = QueryVector { model: "emb", vector: &[1.0, 0.0, 0.0] };
        m.remember(fact("user", "Deploys happen on Fridays"), Some(v)).await.unwrap();
        let close = QueryVector { model: "emb", vector: &[0.99, 0.02, 0.0] };
        let r = m.remember(fact("user", "Releases go out at the end of the week"), Some(close)).await.unwrap();
        assert!(matches!(r, Remembered::Reconfirmed(_)));
    }

    #[tokio::test]
    async fn secrets_and_disallowed_scopes_are_refused() {
        let settings = Settings { allowed_scopes: vec!["user".into(), "project:*".into()], ..Settings::default() };
        let m = manager(settings).await;
        let err = m.remember(fact("user", "The prod db password is hunter2"), None).await.unwrap_err();
        assert!(err.to_string().contains("not stored"), "{err}");
        assert!(m.remember(fact("agent", "x"), None).await.is_err(), "agent scope not allowed");
        assert!(m.remember(fact("project:arcella", "Uses Rust"), None).await.is_ok());
        assert!(m.recall(Some("agent"), "x", None, 5, false).await.is_err());
    }

    #[tokio::test]
    async fn superseding_keeps_history() {
        let m = manager(Settings::default()).await;
        let go = m.remember(fact("project:a", "The agent runtime language is Go."), None).await.unwrap();
        let go = go.memory().clone();
        let rust = m
            .supersede(go.id, fact("project:a", "The agent runtime language is Rust."), "we switched", None)
            .await
            .unwrap();
        let found = m.recall(None, "runtime language", None, 5, false).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].memory.id, rust.id);
        let old = m.inspect(go.id).await.unwrap();
        assert_eq!(old.memory.status, MemoryStatus::Superseded);
        assert!(old.relationships.iter().any(|r| r.contains("supersedes this")), "{:?}", old.relationships);
    }

    #[tokio::test]
    async fn corrections_are_versioned() {
        let m = manager(Settings::default()).await;
        let r = m.remember(fact("project:a", "The API uses port 8000."), None).await.unwrap();
        let id = r.memory().id;
        assert_eq!(m.correct(id, "The API uses port 8080.", "typo", None).await.unwrap(), 2);
        let i = m.inspect(id).await.unwrap();
        assert_eq!(i.memory.content, "The API uses port 8080.");
        assert_eq!(i.versions.iter().map(|v| v.content.as_str()).collect::<Vec<_>>(), ["The API uses port 8080.", "The API uses port 8000."]);
        assert!(m.correct(id, "token: ghp_aBcDeFgHiJkLmNoPqRsTuVwX", "x", None).await.is_err());
    }

    #[tokio::test]
    async fn forgetting_archiving_expiry_and_purge() {
        let m = manager(Settings::default()).await;
        let a = m.remember(fact("user", "Likes green tea"), None).await.unwrap().memory().clone();
        m.forget(a.id, "asked", None).await.unwrap();
        assert!(m.recall(None, "tea", None, 5, false).await.unwrap().is_empty());
        m.restore(a.id).await.unwrap();
        m.archive(a.id, "old", None).await.unwrap();
        assert!(m.recall(None, "tea", None, 5, false).await.unwrap().is_empty());
        assert_eq!(m.recall(None, "tea", None, 5, true).await.unwrap().len(), 1, "archived is still findable");

        let mut temp = fact("user", "Currently debugging server db1");
        temp.expires_at = Some(Utc::now() - Duration::minutes(1));
        let t = m.remember(temp, None).await.unwrap().memory().clone();
        assert!(m.recall(None, "debugging", None, 5, false).await.unwrap().is_empty(), "expired");
        assert_eq!(m.expire_due().await.unwrap().len(), 1);
        assert_eq!(m.get(t.id).await.unwrap().unwrap().status, MemoryStatus::Archived);

        m.purge(a.id).await.unwrap();
        assert!(m.get(a.id).await.unwrap().is_none());
        assert_eq!(m.recent_events(1).await.unwrap()[0].kind, "purged");
    }

    #[tokio::test]
    async fn context_compiler_picks_few_within_budget_and_records_use() {
        let settings = Settings { context: Budget { max_tokens: 60, max_memories: 8, min_score: 0.0, min_relevance: 0.0 }, ..Settings::default() };
        let m = manager(settings).await;
        m.remember(fact("user", "The production database runs PostgreSQL 16."), None).await.unwrap();
        m.remember(fact("user", "The production database runs PostgreSQL 16 on db1."), None).await.unwrap();
        m.remember(fact("user", "The staging database runs PostgreSQL 15 on a small VM with little memory."), None).await.unwrap();
        let run = Uuid::new_v4();
        let c = m.compile("which postgres version does the production database run", None, run).await.unwrap();
        assert!(c.tokens <= 60);
        assert!(!c.used.is_empty() && c.used.len() < 3, "near-duplicates and budget trim the list");
        assert!(c.section.unwrap().contains("# Relevant memories"));

        assert_eq!(m.feedback(run, true).await.unwrap(), c.used.len());
        let used = m.get(c.used[0].memory.id).await.unwrap().unwrap();
        assert_eq!((used.usage.injected, used.usage.helpful), (1, 1));
    }

    #[tokio::test]
    async fn unrelated_memories_stay_out_of_the_prompt() {
        let m = manager(Settings::default()).await;
        let port = m.remember(fact("user", "The billing API listens on port 8080"), None).await.unwrap().memory().clone();
        m.set_embedding(port.id, "emb", &[0.0, 1.0]).await.unwrap();
        let deploy = m.remember(fact("user", "Deploys happen every Friday afternoon"), None).await.unwrap().memory().clone();
        m.set_embedding(deploy.id, "emb", &[1.0, 0.1]).await.unwrap();
        let q = [0.95, 0.3];
        let c = m.compile("when do we ship releases", Some(QueryVector { model: "emb", vector: &q }), Uuid::new_v4()).await.unwrap();
        let ids: Vec<Uuid> = c.used.iter().map(|r| r.memory.id).collect();
        assert_eq!(ids, [deploy.id], "the port memory is important and fresh, but not relevant");
    }

    #[tokio::test]
    async fn semantic_recall_finds_what_keywords_miss() {
        let m = manager(Settings::default()).await;
        let v = |x: f32, y: f32| vec![x, y];
        let deploy = m.remember(fact("user", "Releases ship every Friday afternoon"), None).await.unwrap().memory().clone();
        m.set_embedding(deploy.id, "emb", &v(1.0, 0.1)).await.unwrap();
        let tea = m.remember(fact("user", "Likes green tea"), None).await.unwrap().memory().clone();
        m.set_embedding(tea.id, "emb", &v(0.0, 1.0)).await.unwrap();
        let q = v(0.95, 0.15);
        let found = m.recall(None, "when do we deploy", Some(QueryVector { model: "emb", vector: &q }), 2, false).await.unwrap();
        assert_eq!(found[0].memory.id, deploy.id, "no shared keywords, same meaning");
        assert!(found[0].score.semantic.unwrap() > 0.9);
    }

    #[tokio::test]
    async fn capture_plans_create_update_supersede_and_record_episodes() {
        let m = manager(Settings::default()).await;
        let port = m.remember(fact("project:api", "The API listens on port 8000."), None).await.unwrap().memory().clone();
        let typo = m.remember(fact("project:api", "Deploys use Kubernetis."), None).await.unwrap().memory().clone();
        let plan = capture::Plan {
            memories: vec![
                Item { action: "supersede".into(), target: port.short_id(), content: "The API listens on port 8080.".into(), scope: "project:api".into(), ..Item::default() },
                Item { action: "update".into(), target: typo.short_id(), content: "Deploys use Kubernetes.".into(), ..Item::default() },
                Item { action: "create".into(), content: "The user's name is Garrett.".into(), scope: "user".into(), confidence: Some(1.0), ..Item::default() },
                Item { action: "create".into(), content: "Root password is hunter2".into(), ..Item::default() },
                Item { action: "ignore".into(), content: "small talk".into(), ..Item::default() },
            ],
            episode: Some(EpisodeItem { summary: "Moved the API to port 8080.".into(), outcome: "Done.".into(), entities: vec!["api".into()] }),
        };
        let notes = m.apply_capture(plan, &[port.clone(), typo.clone()], None).await.unwrap();
        assert_eq!(notes.len(), 5, "{notes:?}");
        assert!(notes[3].starts_with("skipped: not stored"), "{notes:?}");
        assert_eq!(m.get(port.id).await.unwrap().unwrap().status, MemoryStatus::Superseded);
        assert_eq!(m.get(typo.id).await.unwrap().unwrap().content, "Deploys use Kubernetes.");
        assert_eq!(m.episodes(5).await.unwrap().len(), 1);
        let stats = m.stats(None).await.unwrap();
        assert_eq!(stats.episodes, 1);
    }

    #[tokio::test]
    async fn curation_proposes_consolidation_and_flags_contradictions() {
        let m = manager(Settings::default()).await;
        let a = m.remember(fact("project:a", "Project uses Rust."), None).await.unwrap().memory().clone();
        let b = m.remember(fact("project:a", "Agent runtime is implemented in Rust."), None).await.unwrap().memory().clone();
        let c = m.remember(fact("project:a", "Deploys happen on Fridays."), None).await.unwrap().memory().clone();
        let d = m.remember(fact("project:a", "Deploys never happen on Fridays."), None).await.unwrap().memory().clone();
        let plan = curator::Plan {
            consolidations: vec![Consolidation { memories: vec![a.short_id(), b.short_id()], content: "The agent runtime uses Rust.".into(), reason: "same fact".into() }],
            contradictions: vec![Contradiction { memories: vec![c.short_id(), d.short_id()], reason: "Friday deploys".into() }],
        };
        let episode = m.add_episode("project:a", "Discussed the runtime language", "Chose Rust", vec![], None).await.unwrap();
        let mixed = curator::Plan {
            consolidations: vec![Consolidation { memories: vec![a.short_id(), episode.short_id()], content: "x".into(), reason: "r".into() }],
            ..Default::default()
        };
        let skipped = m.apply_curation(mixed, &[], None).await.unwrap();
        assert!(skipped[0].starts_with("skipped"), "episodes aren't merged into facts: {skipped:?}");

        let notes = m.apply_curation(plan, &[], None).await.unwrap();
        assert_eq!(notes.len(), 2, "{notes:?}");
        assert_eq!(m.get(a.id).await.unwrap().unwrap().status, MemoryStatus::Active, "propose mode waits");

        let p = m.proposals().await.unwrap().remove(0);
        let note = m.approve(&p.id.to_string()[..8]).await.unwrap();
        assert!(note.starts_with("consolidated 2 memories"), "{note}");
        assert_eq!(m.get(a.id).await.unwrap().unwrap().status, MemoryStatus::Superseded);
        let found = m.recall(None, "rust runtime", None, 5, false).await.unwrap();
        let facts: Vec<&Recalled> = found.iter().filter(|r| r.memory.kind == MemoryKind::Semantic).collect();
        assert_eq!(facts.len(), 1, "the two originals are superseded");
        assert_eq!(facts[0].memory.content, "The agent runtime uses Rust.");
        assert!(facts[0].memory.confidence >= 1.0);

        // Contradicted memories rank below otherwise equal ones.
        assert!(m.stats(None).await.unwrap().contradictions == 1);
        assert!(m.last_curated().await.unwrap().is_some());
    }

    #[tokio::test]
    async fn auto_maintenance_archives_stale_memories() {
        let m = manager(Settings { maintenance: MaintenanceMode::Auto, ..Settings::default() }).await;
        let a = m.remember(fact("user", "Old detail"), None).await.unwrap().memory().clone();
        let notes = m.apply_curation(curator::Plan::default(), &[a.id], None).await.unwrap();
        assert!(notes[0].starts_with("archived"), "{notes:?}");
        assert_eq!(m.get(a.id).await.unwrap().unwrap().status, MemoryStatus::Archived);
        m.restore(a.id).await.unwrap();
        assert_eq!(m.get(a.id).await.unwrap().unwrap().status, MemoryStatus::Active, "reversible");
    }
}

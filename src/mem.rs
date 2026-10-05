//! Lyra's side of memory: wraps the `MemoryManager` with what it can't do on
//! its own (embedding text with the configured model, asking the chat model
//! to capture or curate) and backs the context compiler, the memory panel and
//! the `/memory` commands. Policy stays in the manager.

use std::sync::{Mutex, RwLock};

use lyra_memory::curator::Stats;
use lyra_memory::store::{Filter, MemoryChange};
use lyra_memory::{
    Compiled, Memory, MemoryKind, MemoryManager, MemoryStatus, QueryVector, Recalled, Uuid, WorkingMemory, capture,
    curator,
};
use tokio::runtime::Handle;

use crate::learn::{Review, complete};
use crate::retrieval::{self, Endpoint};

/// What the memory panel shows.
pub struct MemorySnapshot {
    pub stats: Stats,
    pub recent: Vec<Memory>,
    pub working: WorkingMemory,
    /// Pending maintenance changes, one line each.
    pub proposals: Vec<String>,
    pub vectors: bool,
    /// The current project, if any.
    pub project: Option<String>,
}

pub struct Mem {
    pub manager: MemoryManager,
    runtime: Handle,
    /// The embedding model, for hybrid recall; `None` means keywords only.
    /// Replaceable by a config reload.
    embedding: RwLock<Option<Endpoint>>,
    /// The project being worked on: its memories (`project:<name>`) are
    /// recalled, other projects' aren't unless asked for by scope.
    project: RwLock<Option<String>>,
    pub working: Mutex<WorkingMemory>,
    /// Where the database is, for display.
    pub path: String,
}

/// A vector and the model that made it.
pub struct Embedded {
    pub model: String,
    pub vector: Vec<f32>,
}

/// The manager's view of an optional vector.
pub fn qv(e: &Option<Embedded>) -> Option<QueryVector<'_>> {
    e.as_ref().map(|e| QueryVector { model: &e.model, vector: &e.vector })
}

impl Mem {
    pub fn new(manager: MemoryManager, runtime: Handle, embedding: Option<Endpoint>, path: String, project: Option<String>) -> Self {
        Self {
            manager,
            runtime,
            embedding: RwLock::new(embedding),
            project: RwLock::new(project),
            working: Mutex::new(WorkingMemory::default()),
            path,
        }
    }

    pub fn run<T, E: std::fmt::Display>(&self, f: impl Future<Output = Result<T, E>>) -> Result<T, String> {
        self.runtime.block_on(f).map_err(|e| format!("{e:#}"))
    }

    fn endpoint(&self) -> Option<Endpoint> {
        self.embedding.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// New settings and embedding endpoint (a config reload).
    pub fn reconfigure(&self, settings: lyra_memory::Settings, embedding: Option<Endpoint>) {
        self.manager.set_settings(settings);
        *self.embedding.write().unwrap_or_else(|e| e.into_inner()) = embedding;
    }

    pub fn project(&self) -> Option<String> {
        self.project.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_project(&self, project: Option<String>) {
        *self.project.write().unwrap_or_else(|e| e.into_inner()) = project;
    }

    pub fn model(&self) -> Option<String> {
        self.endpoint().map(|e| e.model)
    }

    /// A vector for stored text, if an embedding model is configured and answers.
    pub fn embed_text(&self, text: &str) -> Option<Embedded> {
        let e = self.endpoint()?;
        let vector = retrieval::embed(&e, &[text]).ok()?.vectors.pop()?;
        Some(Embedded { model: e.model, vector })
    }

    /// A vector for a search query (with the retrieval instruction).
    pub fn embed_query(&self, text: &str) -> Option<Embedded> {
        let e = self.endpoint()?;
        let vector = retrieval::embed_query(&e, text).ok()?;
        Some(Embedded { model: e.model, vector })
    }

    pub fn working(&self) -> std::sync::MutexGuard<'_, WorkingMemory> {
        self.working.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The memories worth adding to the prompt for `message` (M12).
    pub fn compile(&self, message: &str, run: Uuid) -> Result<Compiled, String> {
        let vector = self.embed_query(message);
        self.run(self.manager.compile(message, qv(&vector), run, self.project().as_deref()))
    }

    /// Recall in one scope, or (`None`) in everything visible from the
    /// current project.
    pub fn recall(&self, scope: Option<&str>, query: &str, limit: usize, archived: bool) -> Result<Vec<Recalled>, String> {
        let vector = self.embed_query(query);
        match scope {
            Some(_) => self.run(self.manager.recall(scope, query, qv(&vector), limit, archived)),
            None => self.run(self.manager.recall_visible(self.project().as_deref(), query, qv(&vector), limit, archived)),
        }
    }

    /// Recall across every allowed scope (the user's `/memory search`).
    pub fn recall_all(&self, query: &str, limit: usize) -> Result<Vec<Recalled>, String> {
        let vector = self.embed_query(query);
        self.run(self.manager.recall(None, query, qv(&vector), limit, true))
    }

    /// M3/M4: relate a newly saved memory to similar ones: the model says
    /// whether it updates (supersedes), contradicts (flagged), supports or is
    /// related to each. Returns notes.
    pub fn relate(&self, url: &str, model: &str, id: Uuid) -> Review<Vec<String>> {
        let Ok(Some(new)) = self.run(self.manager.get(id)) else {
            return Review { outcome: Ok(Vec::new()), usage: None };
        };
        let vector = self.embed_text(&new.content);
        let near = match self.run(self.manager.neighbours(&new, qv(&vector), 5)) {
            Ok(near) if !near.is_empty() => near,
            Ok(_) => return Review { outcome: Ok(Vec::new()), usage: None },
            Err(e) => return Review { outcome: Err(e), usage: None },
        };
        let (reply, usage) = match complete(url, model, lyra_memory::relate::SYSTEM_PROMPT, &lyra_memory::relate::prompt(&new, &near)) {
            Ok(r) => r,
            Err(e) => return Review { outcome: Err(e), usage: None },
        };
        let outcome = lyra_memory::relate::parse(&reply, &near).map(|links| {
            let mut notes = Vec::new();
            for (old, link, reason) in links {
                let (result, what) = match link {
                    lyra_memory::relate::Link::Replaces => (self.run(self.manager.replace(old.id, new.id, &reason)), "supersedes"),
                    lyra_memory::relate::Link::Relate(rel) => (self.run(self.manager.link(new.id, old.id, rel, &reason)), rel.as_str()),
                };
                match result {
                    Ok(()) => notes.push(format!("[{}] {what} [{}]: {reason}", new.short_id(), old.short_id())),
                    Err(e) => notes.push(format!("linking [{}] failed: {e}", old.short_id())),
                }
            }
            notes
        });
        Review { outcome, usage }
    }

    /// The user's reaction to the last run's memories.
    pub fn feedback(&self, run: Uuid, helpful: bool) -> Result<usize, String> {
        self.run(self.manager.feedback(run, helpful))
    }

    /// Give vectors to memories that don't have one yet. Returns notes.
    pub fn backfill(&self) -> Vec<String> {
        let Some(endpoint) = self.endpoint() else { return Vec::new() };
        let endpoint = &endpoint;
        let mut done = 0;
        for _ in 0..20 {
            let batch = match self.run(self.manager.needs_embedding(&endpoint.model, 32)) {
                Ok(b) if !b.is_empty() => b,
                Ok(_) => break,
                Err(e) => return vec![format!("vector backfill failed: {e}")],
            };
            let texts: Vec<&str> = batch.iter().map(|m| m.content.as_str()).collect();
            let vectors = match retrieval::embed(endpoint, &texts) {
                Ok(e) => e.vectors,
                Err(e) => return vec![format!("vector backfill failed: {e}")],
            };
            for (m, v) in batch.iter().zip(&vectors) {
                if let Err(e) = self.run(self.manager.set_embedding(m.id, &endpoint.model, v)) {
                    return vec![format!("vector backfill failed: {e}")];
                }
            }
            done += batch.len();
        }
        if done == 0 { Vec::new() } else { vec![format!("added vectors to {done} memories")] }
    }

    /// Startup upkeep: archive expired memories, add missing vectors.
    pub fn upkeep(&self) -> Vec<String> {
        let mut notes = self.run(self.manager.expire_due()).unwrap_or_else(|e| vec![format!("expiry failed: {e}")]);
        notes.extend(self.backfill());
        notes
    }

    /// Ask the chat model what the turn taught that's worth remembering (M2,
    /// M3, M8), and store it.
    /// `started` is when the conversation (or plan) began, for episodes.
    #[allow(clippy::too_many_arguments)]
    pub fn capture(
        &self,
        url: &str,
        model: &str,
        reason: &str,
        transcript: &str,
        query: &str,
        run: Option<Uuid>,
        started: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Review<Vec<String>> {
        let similar: Vec<Memory> = match self.recall(None, query, 6, false) {
            Ok(r) => r.into_iter().map(|r| r.memory).collect(),
            Err(e) => return Review { outcome: Err(e), usage: None },
        };
        let prompt = capture::prompt(reason, transcript, &similar, &self.manager.settings().default_scope);
        let (reply, usage) = match complete(url, model, capture::SYSTEM_PROMPT, &prompt) {
            Ok(r) => r,
            Err(e) => return Review { outcome: Err(e), usage: None },
        };
        let outcome = capture::parse(&reply).and_then(|plan| {
            let mut notes = self.run(self.manager.apply_capture(plan, &similar, run, started))?;
            notes.extend(self.backfill());
            Ok(notes)
        });
        Review { outcome, usage }
    }

    /// Review the collection (M9, M18): local checks, then the model's
    /// consolidations and contradictions. Returns notes.
    pub fn curate(&self, url: &str, model: &str) -> Review<Vec<String>> {
        let mut notes = self.run(self.manager.expire_due()).unwrap_or_default();
        let report = match self.run(self.manager.review_collection(self.model().as_deref())) {
            Ok(r) => r,
            Err(e) => return Review { outcome: Err(e), usage: None },
        };
        let active = match self.run(self.manager.list(&Filter::active(), 150)) {
            Ok(a) => a,
            Err(e) => return Review { outcome: Err(e), usage: None },
        };
        let (plan, usage) = if active.len() < 2 {
            (curator::Plan::default(), None)
        } else {
            let prompt = curator::prompt(&active, &report.duplicates);
            match complete(url, model, curator::SYSTEM_PROMPT, &prompt) {
                Ok((reply, usage)) => match curator::parse(&reply) {
                    Ok(plan) => (plan, usage),
                    Err(e) => return Review { outcome: Err(e), usage },
                },
                Err(e) => return Review { outcome: Err(e), usage: None },
            }
        };
        let outcome = self.run(self.manager.apply_curation(plan, &report.stale, None)).map(|applied| {
            notes.extend(applied);
            notes.extend(self.backfill());
            notes
        });
        Review { outcome, usage }
    }

    pub fn curation_due(&self, every_days: Option<i64>) -> bool {
        let Some(days) = every_days else { return false };
        match self.run(self.manager.last_curated()) {
            Ok(Some(last)) => chrono::Utc::now() - last >= chrono::Duration::days(days),
            Ok(None) => self.run(self.manager.list(&Filter::active(), 2)).is_ok_and(|m| m.len() >= 2),
            Err(_) => false,
        }
    }

    pub fn snapshot(&self) -> Result<MemorySnapshot, String> {
        let stats = self.run(self.manager.stats(self.model().as_deref()))?;
        let recent = self.run(self.manager.list(&Filter::active(), 30))?;
        let proposals = self.run(self.manager.proposals())?.iter().map(|p| describe_proposal(&p.change, &p.id, &recent)).collect();
        Ok(MemorySnapshot { stats, recent, working: self.working().clone(), proposals, vectors: self.endpoint().is_some(), project: self.project() })
    }

    /// `/memory …` commands that don't need the model. (`curate` and
    /// `episode` are started by the app, which runs them in the background.)
    pub fn command(&self, args: &str) -> Result<String, String> {
        let args = args.trim();
        let (sub, rest) = args.split_once(char::is_whitespace).unwrap_or((args, ""));
        let rest = rest.trim();
        let find = |key: &str| self.run(self.manager.find(key));
        match sub {
            "" | "stats" => self.stats_text(),
            "search" => self.search_text(rest),
            "list" => self.list_text(rest),
            "inspect" => self.inspect_text(&find(rest)?),
            "forget" => self.run(self.manager.forget(find(rest)?.id, "forgotten by the user", None)).map(|m| format!("forgot [{}] (/memory restore {} undoes it)", m.short_id(), m.short_id())),
            "archive" => self.run(self.manager.archive(find(rest)?.id, "archived by the user", None)).map(|m| format!("archived [{}]", m.short_id())),
            "restore" => self.run(self.manager.restore(find(rest)?.id)).map(|m| format!("restored [{}]", m.short_id())),
            "purge" => {
                let m = find(rest)?;
                self.run(self.manager.purge(m.id)).map(|_| format!("deleted [{}] for good", m.short_id()))
            }
            "correct" => {
                let (key, text) = rest.split_once(char::is_whitespace).ok_or("usage: /memory correct <id> <new text>")?;
                let m = find(key)?;
                let v = self.run(self.manager.correct(m.id, text, "corrected by the user", None))?;
                if let Some(e) = self.embed_text(text) {
                    let _ = self.run(self.manager.set_embedding(m.id, &e.model, &e.vector));
                }
                Ok(format!("corrected [{}], now v{v}", m.short_id()))
            }
            "approve" => self.run(self.manager.approve(rest)),
            "reject" => self.run(self.manager.reject(rest)),
            "working" if rest == "clear" => {
                self.working().clear();
                Ok("working memory cleared".into())
            }
            "working" => Ok(self.working().render().unwrap_or_else(|| "working memory is empty".into())),
            _ => Err(format!("unknown /memory command {sub:?}; try /help")),
        }
    }

    fn stats_text(&self) -> Result<String, String> {
        let s = self.run(self.manager.stats(self.model().as_deref()))?;
        let pairs = |v: Vec<String>| if v.is_empty() { "—".into() } else { v.join(" · ") };
        let mut out = vec![format!(
            "Memory: {} ({} active) · {}",
            s.total,
            s.active,
            self.path
        )];
        out.push(format!("  kinds: {}", pairs(s.by_kind.iter().map(|(k, n)| format!("{k} {n}")).collect())));
        out.push(format!("  status: {}", pairs(s.by_status.iter().map(|(k, n)| format!("{k} {n}")).collect())));
        out.push(format!("  scopes: {}", pairs(s.by_scope.iter().map(|(k, n)| format!("{k} {n}")).collect())));
        out.push(format!(
            "  never used {} · expired {} · contradictions {} · duplicate pairs {} · episodes {}",
            s.unused, s.expired, s.contradictions, s.duplicate_candidates, s.episodes
        ));
        let vectors = match self.model() {
            Some(model) => format!("{}/{} have vectors ({model})", s.embedded, s.active),
            None => "keyword search only (no [embedding] model)".into(),
        };
        let confidence = s.average_confidence.map_or("—".into(), |c| format!("{c:.2}"));
        out.push(format!("  average confidence {confidence} · {vectors}"));
        let proposals = self.run(self.manager.proposals())?;
        if !proposals.is_empty() {
            let recent = self.run(self.manager.list(&Filter::default(), 500))?;
            out.push(format!("  waiting for approval ({}):", proposals.len()));
            for p in &proposals {
                out.push(format!("    {} — {}", describe_proposal(&p.change, &p.id, &recent), p.reason));
            }
        }
        out.push("/memory search <q> · list · inspect <id> · correct <id> <text> · forget|archive|restore|purge <id> · approve|reject <id> · working [clear] · curate · episode".into());
        Ok(out.join("\n"))
    }

    fn search_text(&self, query: &str) -> Result<String, String> {
        if query.is_empty() {
            return Err("usage: /memory search <query>".into());
        }
        let found = self.recall_all(query, 10)?;
        if found.is_empty() {
            return Ok("nothing found".into());
        }
        Ok(found
            .iter()
            .map(|r| {
                let s = &r.score;
                let semantic = s.semantic.map_or(String::new(), |x| format!("meaning {x:.2} · "));
                format!(
                    "[{}] {}\n      {} · {semantic}keywords {:.2} · importance {:.2} · confidence {:.2} · recency {:.2} → {:.2}",
                    r.memory.short_id(),
                    r.memory.content,
                    r.memory.scope,
                    s.lexical,
                    s.importance,
                    s.confidence,
                    s.recency,
                    s.total
                )
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }

    fn list_text(&self, scope: &str) -> Result<String, String> {
        let filter = Filter {
            scope: (!scope.is_empty()).then(|| scope.to_string()),
            statuses: vec![MemoryStatus::Active],
            kind: None,
        };
        let list = self.run(self.manager.list(&filter, 25))?;
        if list.is_empty() {
            return Ok("no memories".into());
        }
        Ok(list.iter().map(|m| format!("[{}] {} — {}", m.short_id(), m.scope, m.content)).collect::<Vec<_>>().join("\n"))
    }

    fn inspect_text(&self, m: &Memory) -> Result<String, String> {
        let i = self.run(self.manager.inspect(m.id))?;
        let m = &i.memory;
        let fmt = |t: chrono::DateTime<chrono::Utc>| t.format("%Y-%m-%d %H:%M").to_string();
        let mut out = vec![
            format!("[{}] {}", m.short_id(), m.content),
            format!(
                "  {} · {} · {} · from {} · importance {:.2} · confidence {:.2}",
                m.scope, m.kind, m.status, m.source, m.importance, m.confidence
            ),
            format!(
                "  created {} · updated {} · last used {} · expires {}",
                fmt(m.created_at),
                fmt(m.updated_at),
                m.last_accessed_at.map_or("never".into(), fmt),
                m.expires_at.map_or("never".into(), fmt)
            ),
            format!(
                "  used {} times · helpful {} · not helpful {}{}",
                m.usage.injected,
                m.usage.helpful,
                m.usage.unhelpful,
                m.provenance.run_id.map_or(String::new(), |r| format!(" · from run {}", &r.to_string()[..8]))
            ),
        ];
        if let Some(e) = &i.episode {
            out.push(format!("  episode: {} → {} · entities: {}", e.summary, e.outcome, e.entities.join(", ")));
        }
        if !i.relationships.is_empty() {
            out.push("relationships:".into());
            out.extend(i.relationships.iter().map(|r| format!("  {r}")));
        }
        if !i.versions.is_empty() {
            out.push("versions:".into());
            out.extend(i.versions.iter().map(|v| format!("  v{} {} — {}: {}", v.version, fmt(v.created_at), v.reason, v.content)));
        }
        out.push("history:".into());
        for e in &i.events {
            let states = match (&e.from_state, &e.to_state) {
                (Some(a), Some(b)) => format!(" ({a} → {b})"),
                _ => String::new(),
            };
            out.push(format!("  {} {}{states}: {}", fmt(e.created_at), e.kind, e.reason));
        }
        Ok(out.join("\n"))
    }
}

fn describe_proposal(change: &MemoryChange, id: &Uuid, known: &[Memory]) -> String {
    let short = |m: &Uuid| known.iter().find(|k| k.id == *m).map_or(m.to_string()[..8].to_string(), |k| k.short_id());
    let id = &id.to_string()[..8];
    match change {
        MemoryChange::Merge { memories, content, .. } => format!(
            "{id} merge {} → {}",
            memories.iter().map(|m| format!("[{}]", short(m))).collect::<Vec<_>>().join(" + "),
            content
        ),
        MemoryChange::Archive { memory } => format!("{id} archive [{}]", short(memory)),
    }
}

/// Kind names the model may use for `memory_remember`.
pub fn parse_kind(kind: Option<&str>) -> Result<MemoryKind, String> {
    match kind.unwrap_or("semantic") {
        "semantic" | "fact" => Ok(MemoryKind::Semantic),
        "episodic" | "episode" => Ok(MemoryKind::Episodic),
        "working" => Ok(MemoryKind::Working),
        other => Err(format!("unknown kind {other:?} (semantic, episodic or working)")),
    }
}

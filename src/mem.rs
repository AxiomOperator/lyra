//! Lyra's side of memory: wraps the `MemoryManager` with what it can't do on
//! its own (embedding text with the configured model, asking the chat model
//! to capture or curate) and backs the context compiler, the memory panel and
//! the `/memory` commands. Policy stays in the manager.

use std::sync::{Arc, Mutex, RwLock};

use lyra_memory::curator::Stats;
use lyra_memory::store::{Filter, MemoryChange};
use lyra_memory::{
    Compiled, Memory, MemoryKind, MemoryManager, MemoryStatus, Recalled, Uuid, WorkingMemory, capture,
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
    /// The project being worked on: its memories (`project:<name>`) are
    /// recalled, other projects' aren't unless asked for by scope.
    project: RwLock<Option<String>>,
    pub working: Mutex<WorkingMemory>,
    /// Where the store is, for display.
    pub path: String,
    /// Where `/memory backup` puts backups.
    pub backups: Option<std::path::PathBuf>,
    /// Collections this large get a vector index (`[memory] vector_index_threshold`).
    vector_index_threshold: std::sync::atomic::AtomicUsize,
}

impl Mem {
    pub fn new(manager: MemoryManager, runtime: Handle, path: String, project: Option<String>) -> Self {
        Self {
            manager,
            runtime,
            project: RwLock::new(project),
            working: Mutex::new(WorkingMemory::default()),
            path,
            backups: None,
            vector_index_threshold: std::sync::atomic::AtomicUsize::new(10_000),
        }
    }

    pub fn set_vector_index_threshold(&self, n: usize) {
        self.vector_index_threshold.store(n, std::sync::atomic::Ordering::Relaxed);
    }

    /// Use this embedding endpoint (or none) from now on. Asks it for its
    /// vector size, so call it outside the async runtime. Returns notes.
    pub fn set_embedding(&self, endpoint: Option<Endpoint>) -> Vec<String> {
        let mut notes = Vec::new();
        let provider: Option<Arc<dyn lyra_memory::EmbeddingProvider>> = match endpoint {
            Some(e) => match retrieval::EndpointEmbedder::connect(e) {
                Ok(p) => Some(Arc::new(p)),
                Err(e) => {
                    notes.push(format!("embedding model unavailable, memory uses keywords only: {e}"));
                    None
                }
            },
            None => None,
        };
        match self.run(self.manager.set_embedder(provider)) {
            Ok(note) => notes.extend(note),
            Err(e) => notes.push(format!("embedding setup failed: {e}")),
        }
        notes
    }

    pub fn run<T, E: std::fmt::Display>(&self, f: impl Future<Output = Result<T, E>>) -> Result<T, String> {
        self.runtime.block_on(f).map_err(|e| format!("{e:#}"))
    }

    /// New settings and embedding endpoint (a config reload). Returns notes.
    pub fn reconfigure(&self, settings: lyra_memory::Settings, embedding: Option<Endpoint>) -> Vec<String> {
        self.manager.set_settings(settings);
        let changed = embedding.as_ref().map(|e| e.model.clone()) != self.manager.embedding_model();
        if changed { self.set_embedding(embedding) } else { Vec::new() }
    }

    pub fn project(&self) -> Option<String> {
        self.project.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_project(&self, project: Option<String>) {
        *self.project.write().unwrap_or_else(|e| e.into_inner()) = project;
    }

    pub fn model(&self) -> Option<String> {
        self.manager.embedding_model()
    }

    pub fn working(&self) -> std::sync::MutexGuard<'_, WorkingMemory> {
        self.working.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The memories worth adding to the prompt for `message` (M12).
    pub fn compile(&self, message: &str, run: Uuid) -> Result<Compiled, String> {
        self.run(self.manager.compile(message, run, self.project().as_deref()))
    }

    /// Recall in one scope, or (`None`) in everything visible from the
    /// current project.
    pub fn recall(&self, scope: Option<&str>, query: &str, limit: usize, archived: bool) -> Result<Vec<Recalled>, String> {
        match scope {
            Some(_) => self.run(self.manager.recall(scope, query, limit, archived)),
            None => self.run(self.manager.recall_visible(self.project().as_deref(), query, limit, archived)),
        }
    }

    /// Recall across every allowed scope (the user's `/memory search`).
    pub fn recall_all(&self, query: &str, limit: usize) -> Result<Vec<Recalled>, String> {
        self.run(self.manager.recall(None, query, limit, true))
    }

    /// M3/M4: relate a newly saved memory to similar ones: the model says
    /// whether it updates (supersedes), contradicts (flagged), supports or is
    /// related to each. Returns notes.
    pub fn relate(&self, url: &str, model: &str, id: Uuid) -> Review<Vec<String>> {
        let Ok(Some(new)) = self.run(self.manager.get(id)) else {
            return Review { outcome: Ok(Vec::new()), usage: None };
        };
        let near = match self.run(self.manager.neighbours(&new, 5)) {
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

    /// Give vectors to memories that need one: new ones, ones from an older
    /// embedding model, ones whose content changed (L22). Returns notes.
    pub fn backfill(&self) -> Vec<String> {
        if self.model().is_none() {
            return Vec::new();
        }
        let (mut done, mut failed) = (0, 0);
        for _ in 0..50 {
            match self.run(self.manager.reembed(32)) {
                Ok(p) => {
                    done += p.done;
                    failed += p.failed;
                    if p.remaining == 0 || p.done == 0 {
                        break;
                    }
                }
                Err(e) => return vec![format!("re-embedding failed: {e}")],
            }
        }
        let mut notes = Vec::new();
        if done > 0 {
            notes.push(format!("added vectors to {done} memories"));
        }
        if failed > 0 {
            notes.push(format!("{failed} memories couldn't be embedded; they're found by keywords until /memory reembed"));
        }
        notes
    }

    /// Startup upkeep: archive expired memories, add missing vectors.
    pub fn upkeep(&self) -> Vec<String> {
        let mut notes = self.run(self.manager.expire_due()).unwrap_or_else(|e| vec![format!("expiry failed: {e}")]);
        notes.extend(self.backfill());
        // L15: compact storage, and index vectors once the collection is large.
        let threshold = self.vector_index_threshold.load(std::sync::atomic::Ordering::Relaxed);
        notes.extend(self.run(self.manager.maintain(threshold)).unwrap_or_else(|e| vec![format!("memory maintenance failed: {e}")]));
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
        let report = match self.run(self.manager.review_collection()) {
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
        let stats = self.run(self.manager.stats())?;
        let recent = self.run(self.manager.list(&Filter::active(), 30))?;
        let proposals = self.run(self.manager.proposals())?.iter().map(|p| describe_proposal(&p.change, &p.id, &recent)).collect();
        Ok(MemorySnapshot { stats, recent, working: self.working().clone(), proposals, vectors: self.model().is_some(), project: self.project() })
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
                Ok(format!("corrected [{}], now v{v}", m.short_id()))
            }
            "events" => self.events_text(),
            "reembed" => {
                let notes = self.backfill();
                Ok(if notes.is_empty() { "every memory already has a current vector".into() } else { notes.join("\n") })
            }
            "backup" => {
                let dir = self.backups.clone().ok_or("no backup folder (no home directory)")?;
                let dest = dir.join(format!("memory-{}", chrono::Local::now().format("%Y%m%d-%H%M%S")));
                self.run(self.manager.backup(&dest))?;
                Ok(format!(
                    "backed up memory to {}\nto restore it: quit lyra, then run `lyra --restore-memory {}`",
                    crate::context::show(&dest),
                    dest.display()
                ))
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
        let s = self.run(self.manager.stats())?;
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
        out.push(format!(
            "  storage: {}{}",
            s.backend,
            s.size_bytes.map_or(String::new(), |b| format!(" · {}", human_bytes(b)))
        ));
        for op in &s.operations {
            let failed = if op.failures > 0 { format!(" · {} failed", op.failures) } else { String::new() };
            out.push(format!("  {}: {} calls · p50 {:.0} ms · p95 {:.0} ms{failed}", op.name, op.count, op.p50_ms, op.p95_ms));
        }
        let proposals = self.run(self.manager.proposals())?;
        if !proposals.is_empty() {
            let recent = self.run(self.manager.list(&Filter::default(), 500))?;
            out.push(format!("  waiting for approval ({}):", proposals.len()));
            for p in &proposals {
                out.push(format!("    {} — {}", describe_proposal(&p.change, &p.id, &recent), p.reason));
            }
        }
        out.push("/memory search <q> · list · inspect <id> · correct <id> <text> · forget|archive|restore|purge <id> · approve|reject <id> · working [clear] · curate · episode · events · reembed · backup".into());
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

    /// `/memory events`: what happened to memories lately, newest last.
    fn events_text(&self) -> Result<String, String> {
        let events = self.run(self.manager.recent_events(30))?;
        if events.is_empty() {
            return Ok("nothing has happened yet".into());
        }
        Ok(events
            .iter()
            .rev()
            .map(|e| {
                let id = e.memory_id.map_or(String::new(), |id| format!(" [{}]", &id.to_string()[..8]));
                let states = match (&e.from_state, &e.to_state) {
                    (Some(a), Some(b)) => format!(" ({a} → {b})"),
                    _ => String::new(),
                };
                format!("{} {}{id}{states}: {}", e.created_at.with_timezone(&chrono::Local).format("%m-%d %H:%M"), e.kind, e.reason)
            })
            .collect::<Vec<_>>()
            .join("\n"))
    }

    pub fn inspect_text(&self, m: &Memory) -> Result<String, String> {
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

/// `12.3 MB`.
pub fn human_bytes(b: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut size = b as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit + 1 < units.len() {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{b} B") } else { format!("{size:.1} {}", units[unit]) }
}

//! lyra's Status: everything lyra depends on, checked every minute by `lyra
//! serve` — the models (chat, embedding, reranker, decision), web search,
//! OpenAPI and MCP providers, the public address, notifications, storage,
//! backups, routines and the machines. Checks come from the config; each
//! answers up / degraded / down / off with a latency and a detail. Results go
//! to `~/.lyra/status/status.db` (uptime over 24 h and 7 d, recent latencies,
//! state changes); something that goes down twice in a row is pushed once,
//! and again when it's back.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::retrieval::Endpoint;

/// `[status]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    /// How often `lyra serve` checks everything.
    pub every_seconds: u64,
    /// Push when something goes down (and comes back).
    pub notify: bool,
    /// Checks never pushed about (their names, any case), e.g. ["web search"].
    pub mute: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self { enabled: true, every_seconds: 60, notify: true, mute: Vec::new() }
    }
}

/// Longest a check may take.
const TIMEOUT: Duration = Duration::from_secs(10);
/// How long samples are kept.
const KEEP_DAYS: i64 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Up,
    Degraded,
    Down,
    Off,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Up => "up",
            State::Degraded => "degraded",
            State::Down => "down",
            State::Off => "off",
        }
    }

    fn parse(s: &str) -> State {
        match s {
            "up" => State::Up,
            "degraded" => State::Degraded,
            "down" => State::Down,
            _ => State::Off,
        }
    }

    fn mark(self) -> &'static str {
        match self {
            State::Up => "●",
            State::Degraded => "◐",
            State::Down => "✗",
            State::Off => "○",
        }
    }
}

/// One check's result.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Probe {
    /// Stable id: "chat", "embedding", "mcp:fs", "machine:desktop", …
    pub id: String,
    pub group: String,
    pub name: String,
    /// What it talks to (host:port, URL, path), shown small.
    pub target: String,
    pub state: State,
    pub latency_ms: Option<u64>,
    pub detail: String,
}

fn probe(id: &str, group: &str, name: &str, target: &str, state: State, latency_ms: Option<u64>, detail: impl Into<String>) -> Probe {
    Probe { id: id.into(), group: group.into(), name: name.into(), target: target.into(), state, latency_ms, detail: detail.into() }
}

/// `http://172.99.99.11:8181/v1` → `172.99.99.11:8181`.
pub fn host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split('/').next().unwrap_or(rest).to_string()
}

fn ago(t: DateTime<Utc>) -> String {
    let s = (Utc::now() - t).num_seconds().max(0);
    match s {
        s if s < 90 => format!("{s}s ago"),
        s if s < 5400 => format!("{} min ago", s / 60),
        s if s < 172_800 => format!("{} h ago", s / 3600),
        s => format!("{} days ago", s / 86400),
    }
}

// ---- what a check pass needs, taken from the app (cheap), probed elsewhere

pub struct Inputs {
    pub chat_url: String,
    pub chat_model: String,
    pub embedding: Option<Endpoint>,
    pub reranker: Option<Endpoint>,
    pub search: Option<crate::websearch::Settings>,
    pub caps: Option<Arc<crate::caps::Caps>>,
    pub public_url: String,
    /// Notifications (serving only): devices with push on, sent, failed, last error.
    pub push: Option<(usize, u64, u64, String)>,
    /// lyra's stores and whether they opened.
    pub stores: Vec<(String, Result<String, String>)>,
    pub mem: Option<Arc<crate::mem::Mem>>,
    pub home: Option<PathBuf>,
    pub backup: crate::backup::Settings,
    /// `lyra serve` itself, when serving: what to say about it.
    pub serving: Option<String>,
    /// `machines_detail` rows and the server's health report (serving only).
    pub machines: Vec<Value>,
    pub server_health: Value,
}

impl crate::App {
    /// What a status pass needs from this app (the primary one).
    pub(crate) fn status_inputs(&self) -> Inputs {
        let stores = vec![
            ("memory".to_string(), self.memory_status.clone()),
            ("skills".to_string(), self.learning_status.clone()),
            ("plans".to_string(), self.planning_status.clone()),
            ("evolution".to_string(), self.evolution_status.clone()),
            ("agents".to_string(), self.agents_status.clone()),
        ];
        Inputs {
            chat_url: self.base_url.clone(),
            chat_model: self.model.clone(),
            embedding: self.embedding.clone(),
            reranker: self.reranker.clone(),
            search: self.caps.as_ref().and_then(|c| c.search.clone()),
            caps: self.caps.clone(),
            public_url: self.hub.as_ref().map(|h| h.public_url()).unwrap_or_default(),
            push: None,
            stores,
            mem: self.mem(),
            home: crate::config::home(),
            backup: self.backup.clone(),
            serving: None,
            machines: Vec::new(),
            server_health: Value::Null,
        }
    }
}

// ---- probes

/// Run `f` with a time limit; its own latency when it answers.
fn timed<F>(f: F) -> (Result<(State, String), String>, Option<u64>)
where
    F: FnOnce() -> Result<(State, String), String> + Send + 'static,
{
    let (tx, rx) = mpsc::channel();
    let started = Instant::now();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(TIMEOUT) {
        Ok(r) => (r, Some(started.elapsed().as_millis() as u64)),
        Err(_) => (Err(format!("no answer in {} s", TIMEOUT.as_secs())), None),
    }
}

fn check<F>(id: &str, group: &str, name: &str, target: &str, f: F) -> Probe
where
    F: FnOnce() -> Result<(State, String), String> + Send + 'static,
{
    match timed(f) {
        (Ok((state, detail)), ms) => probe(id, group, name, target, state, ms, detail),
        (Err(e), _) => probe(id, group, name, target, State::Down, None, e),
    }
}

fn http() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder().timeout(TIMEOUT).connect_timeout(Duration::from_secs(5)).build().map_err(|e| e.to_string())
}

/// The chat model's endpoint lists the model in use.
pub fn chat_state(model: &str, offered: &[String]) -> (State, String) {
    if offered.is_empty() || offered.iter().any(|m| m == model) {
        (State::Up, format!("{model} · {} model{} offered", offered.len(), if offered.len() == 1 { "" } else { "s" }))
    } else {
        (State::Degraded, format!("{model} isn't offered (it has {})", offered.join(", ")))
    }
}

/// How old the last backup may be.
pub fn backup_state(last: Option<DateTime<Utc>>, now: DateTime<Utc>) -> (State, String) {
    match last {
        None => (State::Down, "no backup yet (/backup now)".into()),
        Some(t) => {
            let hours = (now - t).num_hours();
            let when = ago(t);
            if hours > 72 {
                (State::Down, format!("the last backup is {when}"))
            } else if hours > 26 {
                (State::Degraded, format!("the last backup is {when}: the nightly one didn't run"))
            } else {
                (State::Up, format!("last {when}"))
            }
        }
    }
}

/// Every check, at once.
pub fn pass(i: Inputs) -> Vec<Probe> {
    let mut jobs: Vec<std::thread::JoinHandle<Vec<Probe>>> = Vec::new();
    let mut out: Vec<Probe> = Vec::new();
    let spawn = |jobs: &mut Vec<std::thread::JoinHandle<Vec<Probe>>>, f: Box<dyn FnOnce() -> Vec<Probe> + Send>| jobs.push(std::thread::spawn(f));

    // Models
    let (url, model) = (i.chat_url.clone(), i.chat_model.clone());
    spawn(
        &mut jobs,
        Box::new(move || {
            let target = host(&url);
            vec![check("chat", "Models", "Chat model", &target, move || crate::models(&url).map(|offered| chat_state(&model, &offered)))]
        }),
    );
    // The fallback, so it's known to work before it's needed.
    if let Some(fb) = crate::fallback::settings() {
        spawn(
            &mut jobs,
            Box::new(move || {
                let target = host(&fb.url);
                let (url, model) = (fb.url.clone(), fb.model.clone());
                vec![check("fallback", "Models", "Fallback chat model", &target, move || crate::models(&url).map(|offered| chat_state(&model, &offered)))]
            }),
        );
    }
    match i.embedding.clone() {
        Some(ep) => spawn(
            &mut jobs,
            Box::new(move || {
                let target = host(&ep.url);
                vec![check("embedding", "Models", "Embedding", &target, move || {
                    crate::retrieval::embed(&ep, &["status check"]).map(|e| (State::Up, format!("{} · {} dims", ep.model, e.vectors.first().map_or(0, Vec::len))))
                })]
            }),
        ),
        None => out.push(probe("embedding", "Models", "Embedding", "", State::Off, None, "not set up ([embedding]): memory uses keywords only")),
    }
    match i.reranker.clone() {
        Some(ep) => spawn(
            &mut jobs,
            Box::new(move || {
                let target = host(&ep.url);
                vec![check("reranker", "Models", "Reranker", &target, move || {
                    let docs = ["Bananas are yellow.", "Rust is a systems programming language."];
                    crate::retrieval::rerank(&ep, "What is Rust?", &docs).map(|r| {
                        if r.first().is_some_and(|top| top.index == 1) {
                            (State::Up, ep.model.clone())
                        } else {
                            (State::Degraded, "answers, but ranked a test query wrongly".into())
                        }
                    })
                })]
            }),
        ),
        None => out.push(probe("reranker", "Models", "Reranker", "", State::Off, None, "not set up ([reranker])")),
    }
    match crate::decide::url() {
        Some(url) => spawn(
            &mut jobs,
            Box::new(move || {
                vec![check("decide", "Models", "Decision model", &host(&url), || match crate::decide::probe() {
                    Some(Ok((_, model))) => Ok((State::Up, model)),
                    Some(Err(e)) => Err(e),
                    None => Ok((State::Off, "not set up".into())),
                })]
            }),
        ),
        None => out.push(probe("decide", "Models", "Decision model", "", State::Off, None, "not set up ([decide]): the chat model decides")),
    }

    // Tools & APIs
    match i.search.clone().filter(|s| s.enabled) {
        Some(s) => spawn(
            &mut jobs,
            Box::new(move || {
                let target = host(&s.searxng_url);
                vec![check("search", "Tools & APIs", "Web search", &target, move || {
                    crate::websearch::call(&s, "web_search", &json!({ "query": "weather", "count": 3 })).map(|v| match v["note"].as_str() {
                        Some(note) => (State::Degraded, note.to_string()),
                        None => (State::Up, format!("SearXNG · {} results", v["results"].as_array().map_or(0, Vec::len))),
                    })
                })]
            }),
        ),
        None => out.push(probe("search", "Tools & APIs", "Web search", "", State::Off, None, "off ([search] enabled)")),
    }
    if let Some(caps) = i.caps.clone() {
        spawn(
            &mut jobs,
            Box::new(move || {
                let started = Instant::now();
                let providers = caps.providers();
                let ms = Some(started.elapsed().as_millis() as u64 / providers.len().max(1) as u64);
                providers
                    .into_iter()
                    .map(|(kind, name, target, health, count)| {
                        use lyra_capabilities::CapabilityHealth as H;
                        let state = match health {
                            H::Healthy => State::Up,
                            H::Degraded => State::Degraded,
                            _ => State::Down,
                        };
                        probe(&format!("{}:{name}", kind.to_lowercase()), "Tools & APIs", &format!("{name} ({kind})"), &target, state, ms, format!("{count} capabilities · {}", health.as_str()))
                    })
                    .collect()
            }),
        );
    }

    // PMI, the project-management app: is it there, and does it take the token?
    let pmi = crate::pmi::settings();
    if !pmi.enabled {
        out.push(probe("pmi", "Tools & APIs", "PMI", "", State::Off, None, "off ([pmi] enabled)"));
    } else if crate::secrets::token("pmi").is_none() {
        out.push(probe("pmi", "Tools & APIs", "PMI", &host(&pmi.url), State::Off, None, "no token yet (/pmi token <token>)"));
    } else {
        spawn(
            &mut jobs,
            Box::new(move || {
                vec![check("pmi", "Tools & APIs", "PMI", &host(&pmi.url), || {
                    let me = crate::pmi::request(reqwest::Method::GET, "/v1/auth/me", None)?;
                    Ok((State::Up, format!("signed in as {}", me["user"]["name"].as_str().unwrap_or("?"))))
                })]
            }),
        );
    }

    // lyra
    if let Some(detail) = &i.serving {
        out.push(probe("lyra", "lyra", "lyra serve", "", State::Up, None, detail.clone()));
    }
    if i.serving.is_some() {
        let url = i.public_url.clone();
        if url.is_empty() {
            out.push(probe("public", "lyra", "Public address", "", State::Off, None, "[web] public_url isn't set: phones can't reach lyra from outside"));
        } else {
            spawn(
                &mut jobs,
                Box::new(move || {
                    let target = host(&url);
                    vec![check("public", "lyra", "Public address", &target, move || {
                        let resp = http()?.get(format!("{url}/health")).send().map_err(|e| format!("can't reach {url}: {e}"))?;
                        match resp.status().as_u16() {
                            200 => Ok((State::Up, format!("{url} answers"))),
                            503 => Ok((State::Degraded, "the proxy reaches lyra, but its main loop is stuck".into())),
                            code => Err(format!("{url}/health answered {code} (is the proxy pointing at lyra?)")),
                        }
                    })]
                }),
            );
        }
    }
    if let Some((devices, sent, failed, error)) = &i.push {
        out.push(match (devices, failed) {
            (0, _) => probe("push", "lyra", "Notifications", "", State::Off, None, "no device has notifications on"),
            (_, 0) => probe("push", "lyra", "Notifications", "", State::Up, None, format!("{devices} device{} · {sent} sent lately", if *devices == 1 { "" } else { "s" })),
            (_, f) => probe("push", "lyra", "Notifications", "", State::Degraded, None, format!("{f} of {} failed lately: {error}", sent + f)),
        });
    }
    {
        let (stores, mem, home) = (i.stores.clone(), i.mem.clone(), i.home.clone());
        spawn(
            &mut jobs,
            Box::new(move || {
                let target = home.as_ref().map(|h| crate::context::show(h)).unwrap_or_default();
                vec![check("storage", "lyra", "Storage", &target, move || {
                    let broken: Vec<String> = stores.iter().filter_map(|(n, r)| r.as_ref().err().map(|e| format!("{n}: {e}"))).collect();
                    let memories = mem.as_ref().and_then(|m| m.run(m.manager.stats()).ok()).map(|s| s.active);
                    let disk = home.as_ref().and_then(|h| {
                        let out = std::process::Command::new("df").args(["-P", "-k"]).arg(h).output().ok()?;
                        lyra_node::health::disks(&String::from_utf8_lossy(&out.stdout)).into_iter().next()
                    });
                    let used = disk.as_ref().and_then(|d| d["used_pct"].as_u64()).unwrap_or(0);
                    let mut parts = Vec::new();
                    if let Some(n) = memories {
                        parts.push(format!("{n} memor{}", if n == 1 { "y" } else { "ies" }));
                    }
                    if disk.is_some() {
                        parts.push(format!("disk {used}% used"));
                    }
                    let state = if !broken.is_empty() || used >= 95 {
                        State::Down
                    } else if used >= 90 {
                        State::Degraded
                    } else {
                        State::Up
                    };
                    parts.extend(broken);
                    Ok((state, parts.join(" · ")))
                })]
            }),
        );
    }
    if i.backup.enabled {
        let (state, detail) = backup_state(crate::backup::last().map(|b| b.made.with_timezone(&Utc)), Utc::now());
        out.push(probe("backup", "lyra", "Backups", &format!("nightly at {}", i.backup.at), state, None, detail));
    } else {
        out.push(probe("backup", "lyra", "Backups", "", State::Off, None, "nightly backups are off ([backup] enabled)"));
    }
    {
        let routines = crate::routines::list();
        let runs = crate::routines::runs();
        let failed: Vec<String> = routines
            .iter()
            .filter(|r| r.enabled && runs.get(&r.name).and_then(|x| x.first()).is_some_and(|x| x.outcome != "ok"))
            .map(|r| r.name.clone())
            .collect();
        let enabled = routines.iter().filter(|r| r.enabled).count();
        out.push(match (enabled, failed.is_empty()) {
            (0, _) => probe("routines", "lyra", "Routines", "", State::Off, None, "none running (/routine)"),
            (n, true) => probe("routines", "lyra", "Routines", "", State::Up, None, format!("{n} scheduled")),
            (_, false) => probe("routines", "lyra", "Routines", "", State::Degraded, None, format!("couldn't finish: {}", failed.join(", "))),
        });
    }

    // Machines (from their reports: nothing to call)
    if i.server_health.is_object() {
        let problems = crate::health::problems(&i.server_health, &crate::health::settings());
        let summary = crate::health::summary(&i.server_health);
        out.push(probe(
            "machine:server",
            "Machines",
            "server",
            "",
            if problems.is_empty() { State::Up } else { State::Degraded },
            None,
            if problems.is_empty() { summary } else { problems.iter().map(|p| p.text.clone()).collect::<Vec<_>>().join("; ") },
        ));
    }
    for m in &i.machines {
        let name = m["name"].as_str().unwrap_or("?");
        let id = format!("machine:{}", name.to_lowercase());
        let p = if m["online"] != true {
            let seen = m["last_seen"].as_str().and_then(|t| DateTime::parse_from_rfc3339(t).ok()).map_or(String::new(), |t| format!(" · last seen {}", ago(t.with_timezone(&Utc))));
            probe(&id, "Machines", name, "", State::Down, None, format!("offline{seen}"))
        } else {
            let problems: Vec<&str> = m["health"]["problems"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            let mut detail = if problems.is_empty() { m["health"]["summary"].as_str().unwrap_or("online").to_string() } else { problems.join("; ") };
            if m["update_available"] == true {
                detail += " · lyra-node update available";
            }
            probe(&id, "Machines", name, m["hostname"].as_str().unwrap_or(""), if problems.is_empty() { State::Up } else { State::Degraded }, None, detail)
        };
        out.push(p);
    }

    for j in jobs {
        if let Ok(probes) = j.join() {
            out.extend(probes);
        }
    }
    let order = ["Models", "Tools & APIs", "lyra", "Machines"];
    // Groups in order; the fixed checks in this order of the fixed checks within a group.
    let fixed = ["chat", "fallback", "embedding", "reranker", "decide", "search", "lyra", "public", "push", "storage", "backup", "routines"];
    // Marked known down: say so; and say when one answers again (only a person clears it).
    let marks = crate::known_down::all();
    for p in out.iter_mut() {
        if let Some(m) = marks.get(&p.id) {
            p.detail = if p.state == State::Up { format!("answers again: {} (clear the mark once it's fixed)", crate::known_down::describe(m)) } else { format!("{} · {}", crate::known_down::describe(m), p.detail) };
        }
    }
    out.sort_by_key(|p| (order.iter().position(|g| *g == p.group).unwrap_or(9), fixed.iter().position(|f| *f == p.id).unwrap_or(fixed.len()), p.id != "machine:server", p.id.clone()));
    out
}

// ---- the board: results with their history

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Change {
    pub at: DateTime<Utc>,
    pub from: State,
    pub to: State,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Row {
    #[serde(flatten)]
    pub probe: Probe,
    /// Share of checks it answered (up or degraded), when there were any.
    pub uptime_24h: Option<f64>,
    pub uptime_7d: Option<f64>,
    /// Latest latencies, oldest first (none: no answer).
    pub spark: Vec<Option<u64>>,
    /// In its current state since.
    pub since: Option<DateTime<Utc>>,
    /// Recent state changes, newest first.
    pub changes: Vec<Change>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Board {
    pub at: DateTime<Utc>,
    pub overall: State,
    pub rows: Vec<Row>,
}

/// Down if anything is down, degraded if anything is, else up.
pub fn overall(probes: &[Probe]) -> State {
    if probes.iter().any(|p| p.state == State::Down) {
        State::Down
    } else if probes.iter().any(|p| p.state == State::Degraded) {
        State::Degraded
    } else {
        State::Up
    }
}

impl Board {
    /// A board without history (the terminal's one-off check).
    pub fn plain(probes: Vec<Probe>) -> Board {
        Board {
            at: Utc::now(),
            overall: overall(&probes),
            rows: probes.into_iter().map(|probe| Row { probe, uptime_24h: None, uptime_7d: None, spark: Vec::new(), since: None, changes: Vec::new() }).collect(),
        }
    }
}

// ---- history (SQLite)

pub struct Store {
    rt: tokio::runtime::Runtime,
    pool: sqlx::SqlitePool,
    /// Each check's last state, to notice changes.
    last: HashMap<String, State>,
    pruned: Option<Instant>,
}

impl Store {
    pub fn open(dir: &Path) -> Result<Store, String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
        let options = sqlx::sqlite::SqliteConnectOptions::new().filename(dir.join("status.db")).create_if_missing(true).journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
        let pool = rt.block_on(sqlx::sqlite::SqlitePoolOptions::new().max_connections(1).connect_with(options)).map_err(|e| e.to_string())?;
        rt.block_on(async {
            for q in [
                "CREATE TABLE IF NOT EXISTS samples (check_id TEXT NOT NULL, at INTEGER NOT NULL, state TEXT NOT NULL, latency_ms INTEGER)",
                "CREATE INDEX IF NOT EXISTS samples_by_check ON samples (check_id, at)",
                "CREATE TABLE IF NOT EXISTS changes (check_id TEXT NOT NULL, at INTEGER NOT NULL, from_state TEXT NOT NULL, to_state TEXT NOT NULL, detail TEXT NOT NULL)",
                "CREATE INDEX IF NOT EXISTS changes_by_check ON changes (check_id, at)",
            ] {
                sqlx::query(q).execute(&pool).await?;
            }
            Ok::<(), sqlx::Error>(())
        })
        .map_err(|e| e.to_string())?;
        let rows: Vec<(String, String)> = rt
            .block_on(sqlx::query_as("SELECT check_id, to_state FROM changes c WHERE at = (SELECT MAX(at) FROM changes WHERE check_id = c.check_id)").fetch_all(&pool))
            .map_err(|e| e.to_string())?;
        let last = rows.into_iter().map(|(id, s)| (id, State::parse(&s))).collect();
        Ok(Store { rt, pool, last, pruned: None })
    }

    /// Keep this pass and return the board with history.
    pub fn record(&mut self, probes: Vec<Probe>, now: DateTime<Utc>) -> Result<Board, String> {
        let at = now.timestamp();
        let pool = self.pool.clone();
        let mut changed: Vec<(String, State, State, String)> = Vec::new();
        for p in &probes {
            let before = self.last.get(&p.id).copied();
            if before != Some(p.state) {
                changed.push((p.id.clone(), before.unwrap_or(State::Off), p.state, p.detail.clone()));
                self.last.insert(p.id.clone(), p.state);
            }
        }
        let prune = self.pruned.is_none_or(|t| t.elapsed() > Duration::from_secs(3600));
        if prune {
            self.pruned = Some(Instant::now());
        }
        let probes2 = probes.clone();
        self.rt
            .block_on(async move {
                let mut tx = pool.begin().await?;
                for p in &probes2 {
                    sqlx::query("INSERT INTO samples (check_id, at, state, latency_ms) VALUES (?, ?, ?, ?)")
                        .bind(&p.id)
                        .bind(at)
                        .bind(p.state.as_str())
                        .bind(p.latency_ms.map(|m| m as i64))
                        .execute(&mut *tx)
                        .await?;
                }
                for (id, from, to, detail) in &changed {
                    sqlx::query("INSERT INTO changes (check_id, at, from_state, to_state, detail) VALUES (?, ?, ?, ?, ?)")
                        .bind(id)
                        .bind(at)
                        .bind(from.as_str())
                        .bind(to.as_str())
                        .bind(detail)
                        .execute(&mut *tx)
                        .await?;
                }
                if prune {
                    let old = at - KEEP_DAYS * 86400;
                    sqlx::query("DELETE FROM samples WHERE at < ?").bind(old).execute(&mut *tx).await?;
                    sqlx::query("DELETE FROM changes WHERE at < ?").bind(old).execute(&mut *tx).await?;
                }
                tx.commit().await
            })
            .map_err(|e| e.to_string())?;
        let mut rows = Vec::new();
        for p in probes.iter().cloned() {
            rows.push(self.row(p, at)?);
        }
        Ok(Board { at: now, overall: overall(&probes), rows })
    }

    fn row(&self, probe: Probe, now: i64) -> Result<Row, String> {
        let pool = self.pool.clone();
        let id = probe.id.clone();
        let (u24, u7, spark, changes) = self
            .rt
            .block_on(async move {
                let uptime = |since: i64| {
                    let (pool, id) = (pool.clone(), id.clone());
                    async move {
                        let (ok, all): (Option<i64>, Option<i64>) = sqlx::query_as(
                            "SELECT SUM(state IN ('up', 'degraded')), SUM(state != 'off') FROM samples WHERE check_id = ? AND at >= ?",
                        )
                        .bind(&id)
                        .bind(since)
                        .fetch_one(&pool)
                        .await?;
                        Ok::<Option<f64>, sqlx::Error>(match (ok, all) {
                            (Some(ok), Some(all)) if all > 0 => Some(ok as f64 / all as f64 * 100.0),
                            _ => None,
                        })
                    }
                };
                let u24 = uptime(now - 86400).await?;
                let u7 = uptime(now - 7 * 86400).await?;
                let mut spark: Vec<Option<i64>> = sqlx::query_scalar("SELECT latency_ms FROM samples WHERE check_id = ? ORDER BY at DESC LIMIT 60").bind(&id).fetch_all(&pool).await?;
                spark.reverse();
                let changes: Vec<(i64, String, String, String)> =
                    sqlx::query_as("SELECT at, from_state, to_state, detail FROM changes WHERE check_id = ? ORDER BY at DESC LIMIT 10").bind(&id).fetch_all(&pool).await?;
                Ok::<_, sqlx::Error>((u24, u7, spark, changes))
            })
            .map_err(|e| e.to_string())?;
        let changes: Vec<Change> = changes
            .into_iter()
            .filter_map(|(at, from, to, detail)| Some(Change { at: DateTime::from_timestamp(at, 0)?, from: State::parse(&from), to: State::parse(&to), detail }))
            .collect();
        Ok(Row {
            since: changes.first().map(|c| c.at),
            probe,
            uptime_24h: u24,
            uptime_7d: u7,
            spark: spark.into_iter().map(|m| m.map(|m| m.max(0) as u64)).collect(),
            changes,
        })
    }
}

// ---- the worker (lyra serve) and the latest board

static LATEST: Mutex<Option<Board>> = Mutex::new(None);
static WANTED: AtomicBool = AtomicBool::new(false);

pub fn latest() -> Option<Board> {
    LATEST.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

pub fn set_latest(board: &Board) {
    *LATEST.lock().unwrap_or_else(|e| e.into_inner()) = Some(board.clone());
}

/// Ask for a check now (`/status now`, the app's Check now).
pub fn request() {
    WANTED.store(true, Ordering::SeqCst);
}

pub fn take_request() -> bool {
    WANTED.swap(false, Ordering::SeqCst)
}

/// A thread that checks whatever it's handed and keeps the history; boards come back.
pub fn worker(dir: PathBuf) -> (mpsc::Sender<Inputs>, mpsc::Receiver<Result<Board, String>>) {
    let (tx, inputs) = mpsc::channel::<Inputs>();
    let (boards, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut store = Store::open(&dir);
        for i in inputs {
            let probes = pass(i);
            let board = match &mut store {
                Ok(s) => s.record(probes.clone(), Utc::now()).or_else(|e| {
                    eprintln!("status history: {e}");
                    Ok::<Board, String>(Board::plain(probes))
                }),
                Err(_) => Ok(Board::plain(probes)),
            };
            if let Ok(b) = &board {
                set_latest(b);
            }
            if boards.send(board).is_err() {
                break;
            }
        }
    });
    (tx, rx)
}

// ---- alerts

/// Down twice in a row: told once; told again when it's back.
#[derive(Default)]
pub struct Alerts {
    /// Checks down in a row (not kept: a restart starts over).
    streak: HashMap<String, u32>,
    /// What's been told, by check id (`alerts/status.json`).
    told: crate::alerts::Ledger,
}

/// The file as it was before `alerts::Ledger`: check id → since when.
fn legacy(v: serde_json::Value) -> HashMap<String, crate::alerts::Told> {
    serde_json::from_value::<HashMap<String, DateTime<Utc>>>(v).unwrap_or_default().into_iter().map(|(id, since)| (id, crate::alerts::Told { text: "down".into(), since })).collect()
}

impl Alerts {
    /// What was told before a restart (`~/.lyra/alerts/status.json`).
    pub fn load() -> Alerts {
        Alerts { streak: HashMap::new(), told: crate::alerts::Ledger::load("status", legacy) }
    }
}

/// What to tell: (check id, problem?, text).
pub fn alerts(a: &mut Alerts, board: &Board, s: &Settings) -> Vec<(String, bool, String)> {
    let mut out = Vec::new();
    for r in &board.rows {
        let p = &r.probe;
        // Machines are reported by their health alerts already.
        // Known down (marked on the Status page): nobody needs telling again.
        if p.group == "Machines" || s.mute.iter().any(|m| m.eq_ignore_ascii_case(&p.name) || m.eq_ignore_ascii_case(&p.id)) || crate::known_down::is_down(&p.id) {
            continue;
        }
        if p.state == State::Down {
            let n = a.streak.entry(p.id.clone()).or_insert(0);
            *n += 1;
            if *n >= 2 && !a.told.is_told(&p.id) {
                let text = format!("{} is down: {}", p.name, p.detail);
                a.told.raise(&p.id, &text, board.at);
                out.push((p.id.clone(), true, text));
            }
        } else {
            a.streak.remove(&p.id);
            if let Some(t) = a.told.clear(&p.id) {
                let mins = (board.at - t.since).num_minutes().max(1);
                out.push((p.id.clone(), false, format!("{} is back after {mins} min", p.name)));
            }
        }
    }
    a.told.save();
    out
}

// ---- text

/// "all normal" or what isn't.
pub fn line(board: &Board) -> String {
    let bad: Vec<String> = board.rows.iter().filter(|r| matches!(r.probe.state, State::Down | State::Degraded)).map(|r| format!("{} {}", r.probe.name, r.probe.state.as_str())).collect();
    if bad.is_empty() { "all normal".into() } else { bad.join(" · ") }
}

/// `/status`.
pub fn describe(board: &Board) -> String {
    let mut out = vec![format!(
        "status: {} · checked {}",
        match board.overall {
            State::Up => "all normal".to_string(),
            _ => line(board),
        },
        ago(board.at)
    )];
    let mut group = "";
    for r in &board.rows {
        let p = &r.probe;
        if p.group != group {
            group = &p.group;
            out.push(format!("{group}:"));
        }
        let ms = p.latency_ms.map_or(String::new(), |m| format!(" · {m} ms"));
        let up = r.uptime_24h.map_or(String::new(), |u| format!(" · {u:.1}% 24h"));
        let target = if p.target.is_empty() { String::new() } else { format!(" ({})", p.target) };
        out.push(format!("  {} {}{target}{ms}{up} — {}", p.state.mark(), p.name, p.detail));
    }
    out.push("/status now checks again".into());
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(id: &str, state: State) -> Probe {
        probe(id, "Models", id, "", state, Some(10), "x")
    }

    #[test]
    fn states_come_from_what_the_endpoints_say() {
        assert_eq!(chat_state("qwen", &["qwen".into(), "llama".into()]).0, State::Up);
        assert_eq!(chat_state("qwen", &[]).0, State::Up, "an endpoint that lists nothing serves what it's given");
        let (s, d) = chat_state("qwen", &["llama".into()]);
        assert_eq!((s, d.contains("isn't offered")), (State::Degraded, true));
        let now = Utc::now();
        assert_eq!(backup_state(Some(now - chrono::Duration::hours(5)), now).0, State::Up);
        assert_eq!(backup_state(Some(now - chrono::Duration::hours(30)), now).0, State::Degraded);
        assert_eq!(backup_state(Some(now - chrono::Duration::days(4)), now).0, State::Down);
        assert_eq!(backup_state(None, now).0, State::Down);
        assert_eq!(host("http://172.99.99.11:8181/v1"), "172.99.99.11:8181");
        assert_eq!(overall(&[p("a", State::Up), p("b", State::Degraded), p("c", State::Off)]), State::Degraded);
    }

    #[test]
    fn a_dead_endpoint_is_down_and_timing_is_kept() {
        let (r, ms) = timed(|| Ok((State::Up, "fine".into())));
        assert_eq!(r.unwrap().0, State::Up);
        assert!(ms.is_some());
        let chat = check("chat", "Models", "Chat", "x", || crate::models("http://127.0.0.1:9/v1").map(|o| chat_state("m", &o)));
        assert_eq!(chat.state, State::Down);
        assert!(chat.latency_ms.is_none());
    }

    #[test]
    fn history_gives_uptime_changes_and_a_sparkline() {
        let dir = std::env::temp_dir().join(format!("lyra-status-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut store = Store::open(&dir).unwrap();
        let t0 = Utc::now() - chrono::Duration::minutes(10);
        for (i, state) in [State::Up, State::Up, State::Down, State::Up].into_iter().enumerate() {
            let board = store.record(vec![p("chat", state)], t0 + chrono::Duration::minutes(i as i64)).unwrap();
            assert_eq!(board.rows[0].probe.state, state);
        }
        let last = store.record(vec![p("chat", State::Up)], t0 + chrono::Duration::minutes(4)).unwrap();
        let row = &last.rows[0];
        assert_eq!(row.uptime_24h, Some(80.0), "4 of 5 answered");
        assert_eq!(row.spark.len(), 5);
        let moves: Vec<(State, State)> = row.changes.iter().map(|c| (c.from, c.to)).collect();
        assert_eq!(moves, vec![(State::Down, State::Up), (State::Up, State::Down), (State::Off, State::Up)], "newest first");
        assert_eq!(row.since, Some(row.changes[0].at));
        // It remembers across a restart.
        drop(store);
        let mut again = Store::open(&dir).unwrap();
        let b = again.record(vec![p("chat", State::Up)], Utc::now()).unwrap();
        assert_eq!(b.rows[0].changes.len(), 3, "no new change: still up");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn down_twice_is_told_once_and_back_once() {
        let mut a = Alerts::default();
        let s = Settings::default();
        let board = |state: State| Board::plain(vec![p("chat", state), probe("machine:nas", "Machines", "nas", "", State::Down, None, "offline")]);
        assert!(alerts(&mut a, &board(State::Down), &s).is_empty(), "once could be a blip");
        let told = alerts(&mut a, &board(State::Down), &s);
        assert_eq!(told.len(), 1, "machines are the health alerts' job");
        assert!(told[0].1 && told[0].2.starts_with("chat is down"));
        assert!(alerts(&mut a, &board(State::Down), &s).is_empty());
        let back = alerts(&mut a, &board(State::Up), &s);
        assert!(!back[0].1 && back[0].2.contains("is back"));
        let muted = Settings { mute: vec!["CHAT".into()], ..Settings::default() };
        let mut b = Alerts::default();
        alerts(&mut b, &board(State::Down), &muted);
        assert!(alerts(&mut b, &board(State::Down), &muted).is_empty());
        assert!(describe(&board(State::Down)).contains("✗ chat"));
    }
}

//! The capability manager: one registry of everything the agent can do (C2),
//! discovery that finds the few capabilities a goal needs (C3, C9), scored by
//! relevance and track record (C6), the permission policy (C4), usage
//! tracking (C5) and health (C8).

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

use anyhow::Result;
use lyra_memory::EmbeddingProvider;
use serde::Deserialize;

use crate::index::CapabilityIndex;
use crate::model::{Capability, CapabilityHealth, CapabilityKind, CapabilityStats, CapabilityUsage};
use crate::policy::{Policy, Rule};
use crate::store::UsageStore;

/// How much each signal counts when choosing capabilities (C6). `[capabilities.scoring]`.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct Weights {
    pub relevance: f32,
    pub reliability: f32,
    pub permission: f32,
    pub efficiency: f32,
    pub history: f32,
}

impl Default for Weights {
    fn default() -> Self {
        Self { relevance: 0.55, reliability: 0.2, permission: 0.1, efficiency: 0.1, history: 0.05 }
    }
}

/// `[capabilities]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Offer every callable capability to the model when there are at most
    /// this many; beyond that, only the ones discovery finds (plus search).
    pub max_tools: usize,
    /// Capabilities discovery returns for a message or goal.
    pub discovery_limit: usize,
    pub policy: Policy,
    pub scoring: Weights,
}

impl Default for Settings {
    fn default() -> Self {
        Self { max_tools: 16, discovery_limit: 8, policy: Policy::default(), scoring: Weights::default() }
    }
}

/// Why a capability ranked where it did.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Score {
    pub relevance: f32,
    pub reliability: f32,
    pub permission: f32,
    pub efficiency: f32,
    pub history: f32,
    pub total: f32,
}

#[derive(Debug, Clone)]
pub struct Scored {
    pub capability: Capability,
    pub score: Score,
    pub rule: Rule,
    pub health: CapabilityHealth,
}

pub struct CapabilityManager {
    caps: RwLock<Vec<Capability>>,
    usage: UsageStore,
    index: CapabilityIndex,
    stats: RwLock<HashMap<String, CapabilityStats>>,
    /// Health from checks, by capability id or `source.*`.
    checked: RwLock<HashMap<String, CapabilityHealth>>,
    settings: RwLock<Settings>,
    embedder: RwLock<Option<Arc<dyn EmbeddingProvider>>>,
    /// Capabilities the user allowed for this session (`/caps allow`).
    allowed: Mutex<HashSet<String>>,
}

impl CapabilityManager {
    /// Usage history in `<dir>/capabilities.db`, the discovery index in `<dir>/index`.
    pub async fn open(dir: &Path, settings: Settings) -> Result<Self> {
        let usage = UsageStore::open(&dir.join("capabilities.db")).await?;
        Self::with(usage, CapabilityIndex::open(&dir.join("index")).await?, settings).await
    }

    pub async fn with(usage: UsageStore, index: CapabilityIndex, settings: Settings) -> Result<Self> {
        let stats = usage.stats().await?;
        Ok(Self {
            caps: RwLock::new(Vec::new()),
            usage,
            index,
            stats: RwLock::new(stats),
            checked: RwLock::new(HashMap::new()),
            settings: RwLock::new(settings),
            embedder: RwLock::new(None),
            allowed: Mutex::new(HashSet::new()),
        })
    }

    pub fn settings(&self) -> Settings {
        self.settings.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_settings(&self, settings: Settings) {
        *self.settings.write().unwrap_or_else(|e| e.into_inner()) = settings;
    }

    pub fn set_embedder(&self, embedder: Option<Arc<dyn EmbeddingProvider>>) {
        *self.embedder.write().unwrap_or_else(|e| e.into_inner()) = embedder;
    }

    fn embedder(&self) -> Option<Arc<dyn EmbeddingProvider>> {
        self.embedder.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Replace the registry (C2, C11: loaded from every provider at startup
    /// and whenever one changes) and bring the index up to date, embedding
    /// what's new. Returns notes.
    pub async fn set_capabilities(&self, mut caps: Vec<Capability>) -> Result<Vec<String>> {
        caps.sort_by(|a, b| a.id.cmp(&b.id));
        caps.dedup_by(|a, b| a.id == b.id);
        let texts: Vec<(String, String)> = caps.iter().map(|c| (c.id.clone(), c.search_text())).collect();
        *self.caps.write().unwrap_or_else(|e| e.into_inner()) = caps;
        let embedder = self.embedder();
        let model = embedder.as_ref().map(|e| (e.model().to_string(), e.dimensions()));
        let missing = self.index.sync(&texts, model.as_ref().map(|(m, d)| (m.as_str(), *d))).await?;
        let mut notes = Vec::new();
        if let (Some(e), false) = (embedder, missing.is_empty()) {
            let mut done = 0;
            for chunk in missing.chunks(32) {
                let texts: Vec<String> = chunk.iter().map(|(_, t)| t.clone()).collect();
                match e.embed(&texts).await {
                    Ok(vectors) if vectors.len() == chunk.len() => {
                        let pairs: Vec<(String, Vec<f32>)> = chunk.iter().map(|(id, _)| id.clone()).zip(vectors).collect();
                        self.index.set_vectors(e.model(), &pairs).await?;
                        done += chunk.len();
                    }
                    Ok(_) | Err(_) => {
                        notes.push("some capabilities couldn't be embedded; discovery uses keywords for them".into());
                        break;
                    }
                }
            }
            if done > 0 {
                notes.push(format!("indexed {done} capabilit{} for semantic discovery", if done == 1 { "y" } else { "ies" }));
            }
        }
        Ok(notes)
    }

    /// Every capability, with its metadata's numbers filled in from usage (C3).
    pub fn all(&self) -> Vec<Capability> {
        let stats = self.stats.read().unwrap_or_else(|e| e.into_inner());
        self.caps
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .map(|mut c| {
                if let Some(s) = stats.get(&c.id) {
                    c.metadata.success_rate = s.success_rate();
                    c.metadata.average_latency_ms = s.average_latency_ms;
                }
                c
            })
            .collect()
    }

    /// By id or function name.
    pub fn get(&self, key: &str) -> Option<Capability> {
        self.all().into_iter().find(|c| c.id == key || c.name == key)
    }

    pub fn stats(&self, id: &str) -> CapabilityStats {
        self.stats.read().unwrap_or_else(|e| e.into_inner()).get(id).cloned().unwrap_or_default()
    }

    // ---- policy (C4)

    /// The rule for calling this capability now: the policy, unless the user
    /// allowed it for this session.
    pub fn rule(&self, c: &Capability) -> Rule {
        let rule = self.settings().policy.decide(c);
        if rule == Rule::Approval && self.allowed.lock().unwrap_or_else(|e| e.into_inner()).contains(&c.id) {
            Rule::Auto
        } else {
            rule
        }
    }

    /// Allow a capability that needs approval for the rest of the session.
    pub fn allow(&self, key: &str) -> Result<Capability, String> {
        let c = self.get(key).ok_or_else(|| format!("no capability {key:?}"))?;
        if self.settings().policy.decide(&c) == Rule::Deny {
            return Err(format!("{} is denied by policy; change [capabilities.policy] to use it", c.id));
        }
        self.allowed.lock().unwrap_or_else(|e| e.into_inner()).insert(c.id.clone());
        Ok(c)
    }

    // ---- health (C8)

    /// From checks (a source or capability that can't be reached), else from
    /// recent outcomes: mostly failing lately means degraded.
    pub fn health(&self, c: &Capability) -> CapabilityHealth {
        let checked = self.checked.read().unwrap_or_else(|e| e.into_inner());
        if let Some(h) = checked.get(&c.id).or_else(|| checked.get(&format!("{}.*", c.source)))
            && *h != CapabilityHealth::Healthy
        {
            return *h;
        }
        let s = self.stats(&c.id);
        match s.recent_success_rate {
            Some(r) if r < 0.5 && s.uses >= 4 => CapabilityHealth::Degraded,
            _ => CapabilityHealth::Healthy,
        }
    }

    /// Record a health check for a capability id or a whole source (`source.*`).
    pub fn set_health(&self, key: &str, health: CapabilityHealth) {
        self.checked.write().unwrap_or_else(|e| e.into_inner()).insert(key.to_string(), health);
    }

    /// Capabilities that can be used now: enabled, not denied, reachable.
    pub fn usable(&self) -> Vec<Capability> {
        self.all()
            .into_iter()
            .filter(|c| self.rule(c) != Rule::Deny && self.health(c) != CapabilityHealth::Unavailable)
            .collect()
    }

    // ---- discovery and scoring (C3, C6, C9)

    /// Every usable capability scored against `query` (relevance only from
    /// discovery's matches), best first.
    pub async fn discover(&self, query: &str, limit: usize, kinds: &[CapabilityKind]) -> Result<Vec<Scored>> {
        let usable: Vec<Capability> = self.usable().into_iter().filter(|c| kinds.is_empty() || kinds.contains(&c.kind)).collect();
        let mut relevance: HashMap<String, f32> = HashMap::new();
        let text = self.index.search_text(query, 50).await?;
        let top = text.iter().map(|t| t.1).fold(f32::EPSILON, f32::max);
        for (id, s) in text {
            relevance.insert(id, s / top);
        }
        if let Some(e) = self.embedder()
            && let Ok(v) = e.embed_query(query).await
        {
            let near = self.index.search_vector(e.model(), &v, 50).await?;
            // Embedding models differ in where similarities fall, so scale
            // them against the best match (as keyword scores are), ignoring
            // anything under the floor, which is noise for most models.
            const FLOOR: f32 = 0.2;
            let top = near.iter().map(|n| n.1).fold(0.0, f32::max);
            if top > FLOOR {
                for (id, sim) in near {
                    let r = relevance.entry(id).or_default();
                    *r = r.max(((sim - FLOOR) / (top - FLOOR)).clamp(0.0, 1.0));
                }
            }
        }
        let w = self.settings().scoring;
        let mut scored: Vec<Scored> = usable
            .into_iter()
            .filter_map(|c| {
                let r = *relevance.get(&c.id)?;
                (r > 0.0).then(|| self.score(c, r, &w))
            })
            .collect();
        scored.sort_by(|a, b| b.score.total.total_cmp(&a.score.total));
        scored.truncate(limit);
        Ok(scored)
    }

    /// Score a capability whose relevance is known.
    pub fn score(&self, c: Capability, relevance: f32, w: &Weights) -> Scored {
        let s = self.stats(&c.id);
        let rule = self.rule(&c);
        let health = self.health(&c);
        let score = {
            let reliability = s.reliability();
            let permission = match rule {
                Rule::Auto => 1.0,
                Rule::Approval => 0.5,
                Rule::Deny => 0.0,
            };
            let efficiency = s.average_latency_ms.map_or(0.5, |ms| 1.0 / (1.0 + ms as f32 / 2000.0));
            let history = ((1.0 + s.uses as f32).ln() / 50f32.ln()).min(1.0);
            let mut total = relevance * w.relevance
                + reliability * w.reliability
                + permission * w.permission
                + efficiency * w.efficiency
                + history * w.history;
            if health == CapabilityHealth::Degraded {
                total *= 0.7;
            }
            Score { relevance, reliability, permission, efficiency, history, total }
        };
        Scored { capability: c, score, rule, health }
    }

    // ---- usage (C5)

    /// Record an invocation; counts it as a retry when the same capability
    /// failed just before in the same run.
    pub async fn record(&self, mut u: CapabilityUsage) -> Result<()> {
        if let Some(run) = u.run_id
            && let Some((false, retries)) = self.usage.last_in_run(&u.capability_id, run).await?
        {
            u.retries = retries + 1;
        }
        self.usage.record(&u).await?;
        let stats = self.usage.stats().await?;
        *self.stats.write().unwrap_or_else(|e| e.into_inner()) = stats;
        Ok(())
    }

    /// Statistics for every capability with history, for evolution (C12).
    pub fn all_stats(&self) -> HashMap<String, CapabilityStats> {
        self.stats.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

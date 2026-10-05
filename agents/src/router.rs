//! Routing (A6, A7): which agent should handle a request. Rules first (an
//! explicit mention, keywords, intents, examples), then meaning (the
//! agents' routing texts in LanceDB), and the model only when it's close.

use std::sync::Arc;

use anyhow::Result;
use lyra_capabilities::index::CapabilityIndex;
use lyra_memory::EmbeddingProvider;
use serde::{Deserialize, Serialize};

use crate::model::{AgentProfile, MAIN};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteMethod {
    /// The user named the agent.
    Explicit,
    Rule,
    Semantic,
    Model,
}

impl RouteMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Rule => "rule",
            Self::Semantic => "semantic",
            Self::Model => "model",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RoutingDecision {
    /// An agent's name, or `main` to keep it.
    pub agent: String,
    pub confidence: f32,
    pub reason: String,
    pub method: RouteMethod,
}

/// `[agents.routing]`.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct RouterSettings {
    /// A rule score this high routes without asking anyone.
    pub rule_threshold: f32,
    /// A semantic similarity this high routes without asking the model.
    pub semantic_threshold: f32,
    /// Below this similarity no agent is considered.
    pub semantic_floor: f32,
    /// Ask the model when meaning suggests an agent but isn't sure.
    pub model_fallback: bool,
}

impl Default for RouterSettings {
    fn default() -> Self {
        Self { rule_threshold: 0.75, semantic_threshold: 0.85, semantic_floor: 0.5, model_fallback: true }
    }
}

fn words(text: &str) -> Vec<String> {
    lyra_memory::text::content_words(text)
}

/// "@writer", "ask the writer", "writer agent", "have the writer …".
pub fn explicit(agents: &[AgentProfile], message: &str) -> Option<RoutingDecision> {
    let m = message.to_lowercase();
    agents.iter().find_map(|a| {
        let names = [a.name.clone(), a.title.to_lowercase()];
        let hit = names.iter().any(|n| {
            m.contains(&format!("@{n}"))
                || m.contains(&format!("{n} agent"))
                || ["ask the ", "have the ", "use the ", "get the ", "let the "].iter().any(|p| m.contains(&format!("{p}{n}")))
        });
        hit.then(|| RoutingDecision { agent: a.name.clone(), confidence: 1.0, reason: format!("you asked for the {}", a.title), method: RouteMethod::Explicit })
    })
}

/// The message is something this agent shouldn't get.
pub fn excluded(a: &AgentProfile, message: &str) -> bool {
    let m = message.to_lowercase();
    a.delegation.exclusions.iter().any(|x| {
        let xw = words(x);
        !xw.is_empty() && xw.iter().all(|w| m.contains(w.as_str()))
    })
}

/// How well the rules match: examples (share of an example's words in the
/// message), keywords and intents. Returns the score and why.
pub fn rule_score(a: &AgentProfile, message: &str) -> (f32, String) {
    let m = message.to_lowercase();
    let mw = words(message);
    let mut best = (0.0f32, String::new());
    for ex in &a.delegation.examples {
        let ew = words(ex);
        if ew.is_empty() {
            continue;
        }
        let share = ew.iter().filter(|w| mw.contains(w)).count() as f32 / ew.len() as f32;
        if share > best.0 {
            best = (share * 0.95, format!("like \"{ex}\""));
        }
    }
    let hits: Vec<&String> = a.delegation.keywords.iter().filter(|k| m.contains(&k.to_lowercase())).collect();
    let kw = (hits.len() as f32 * 0.4).min(0.9);
    if kw > best.0 {
        best = (kw, format!("mentions {}", hits.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(", ")));
    }
    for intent in &a.delegation.intents {
        let iw: Vec<String> = intent.split('_').map(str::to_string).collect();
        if iw.iter().all(|w| m.contains(w.as_str())) && 0.8 > best.0 {
            best = (0.8, format!("intent {intent}"));
        }
    }
    best
}

/// The best rule match among agents that take work on their own.
pub fn by_rules(agents: &[AgentProfile], message: &str, threshold: f32) -> Option<RoutingDecision> {
    agents
        .iter()
        .filter(|a| a.delegation.auto_delegate && !excluded(a, message))
        .map(|a| (a, rule_score(a, message)))
        .filter(|(_, (s, _))| *s >= threshold)
        .max_by(|x, y| x.1.0.total_cmp(&y.1.0).then(x.0.delegation.priority.cmp(&y.0.delegation.priority)))
        .map(|(a, (score, why))| RoutingDecision { agent: a.name.clone(), confidence: score.min(0.99), reason: why, method: RouteMethod::Rule })
}

pub const ROUTE_PROMPT: &str = "\
You route a user's message inside an AI assistant. The main agent handles everything \
by default; specialists take requests that clearly fall in their area. Pick the \
specialist only when the message is clearly its kind of work; otherwise pick \"main\".

Respond with only a JSON object: {\"agent\": \"name or main\", \"confidence\": 0.0-1.0, \"reason\": \"few words\"}";

pub fn route_prompt(candidates: &[&AgentProfile], message: &str) -> String {
    let mut out = String::from("Specialists:\n");
    for a in candidates {
        out += &format!(
            "- {}: {} Handles: {}. Not for: {}.\n",
            a.name,
            a.description,
            a.delegation.intents.join(", "),
            if a.delegation.exclusions.is_empty() { "-".into() } else { a.delegation.exclusions.join("; ") }
        );
    }
    out += &format!("\nMessage:\n{}", message.chars().take(2000).collect::<String>());
    out
}

#[derive(Deserialize)]
struct ModelRoute {
    agent: String,
    #[serde(default)]
    confidence: f32,
    #[serde(default)]
    reason: String,
}

/// The model's choice, if it names a candidate (or main).
pub fn parse_route(reply: &str, candidates: &[&AgentProfile]) -> Result<RoutingDecision, String> {
    let reply = reply.rsplit_once("</think>").map_or(reply, |(_, a)| a);
    let (Some(s), Some(e)) = (reply.find('{'), reply.rfind('}')) else { return Err("no JSON in the routing reply".into()) };
    let r: ModelRoute = serde_json::from_str(&reply[s..=e.max(s)]).map_err(|e| format!("bad routing JSON: {e}"))?;
    let agent = AgentProfile::slug(&r.agent);
    if agent != MAIN && !candidates.iter().any(|c| c.name == agent) {
        return Err(format!("the router picked an unknown agent {:?}", r.agent));
    }
    Ok(RoutingDecision { agent, confidence: r.confidence.clamp(0.0, 1.0), reason: r.reason, method: RouteMethod::Model })
}

/// The semantic side: every agent's routing texts, embedded and kept in LanceDB.
pub struct SemanticRouter {
    index: CapabilityIndex,
    embedder: std::sync::RwLock<Option<Arc<dyn EmbeddingProvider>>>,
}

impl SemanticRouter {
    pub async fn open(dir: &std::path::Path) -> Result<Self> {
        Ok(Self { index: CapabilityIndex::open(dir).await?, embedder: Default::default() })
    }

    pub fn set_embedder(&self, e: Option<Arc<dyn EmbeddingProvider>>) {
        *self.embedder.write().unwrap_or_else(|e| e.into_inner()) = e;
    }

    fn embedder(&self) -> Option<Arc<dyn EmbeddingProvider>> {
        self.embedder.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Index the agents' routing texts (only new or changed ones are embedded).
    pub async fn sync(&self, agents: &[AgentProfile]) -> Result<usize> {
        let rows: Vec<(String, String)> = agents
            .iter()
            .flat_map(|a| a.routing_texts().into_iter().enumerate().map(move |(i, t)| (format!("{}#{i}", a.name), t)))
            .collect();
        let Some(e) = self.embedder() else {
            self.index.sync(&rows, None).await?;
            return Ok(0);
        };
        let missing = self.index.sync(&rows, Some((e.model(), e.dimensions()))).await?;
        for chunk in missing.chunks(32) {
            let texts: Vec<String> = chunk.iter().map(|(_, t)| t.clone()).collect();
            let vectors = e.embed(&texts).await?;
            let pairs: Vec<(String, Vec<f32>)> = chunk.iter().map(|(id, _)| id.clone()).zip(vectors).collect();
            self.index.set_vectors(e.model(), &pairs).await?;
        }
        Ok(missing.len())
    }

    /// Each agent's best similarity to the message, best first.
    pub async fn rank(&self, message: &str) -> Result<Vec<(String, f32)>> {
        let Some(e) = self.embedder() else { return Ok(Vec::new()) };
        let v = e.embed_query(message).await?;
        let mut best: Vec<(String, f32)> = Vec::new();
        for (id, sim) in self.index.search_vector(e.model(), &v, 50).await? {
            let agent = id.split('#').next().unwrap_or("").to_string();
            match best.iter_mut().find(|(a, _)| *a == agent) {
                Some((_, s)) => *s = s.max(sim),
                None => best.push((agent, sim)),
            }
        }
        best.sort_by(|a, b| b.1.total_cmp(&a.1));
        Ok(best)
    }
}

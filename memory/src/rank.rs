//! Ranking memories for a query (M5, M6, M10, M13): semantic and lexical
//! match, importance, confidence, recency and relationships, with
//! configurable weights. Old memories lose priority instead of disappearing.

use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::{Memory, MemoryKind};

/// How much each signal counts. `[memory.ranking]` in the config.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct Weights {
    pub semantic: f32,
    pub lexical: f32,
    pub importance: f32,
    pub confidence: f32,
    pub recency: f32,
    pub relationship: f32,
}

impl Default for Weights {
    fn default() -> Self {
        Self { semantic: 0.40, lexical: 0.20, importance: 0.15, confidence: 0.10, recency: 0.10, relationship: 0.05 }
    }
}

/// Days for a memory's recency to fall to 1/e, by kind. `[memory.half_life_days]`.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(default)]
pub struct HalfLives {
    pub working: f32,
    pub episodic: f32,
    pub semantic: f32,
}

impl Default for HalfLives {
    fn default() -> Self {
        Self { working: 1.0, episodic: 30.0, semantic: 365.0 }
    }
}

/// Why a memory ranked where it did.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Score {
    /// Cosine similarity to the query, when both have vectors.
    pub semantic: Option<f32>,
    /// Keyword match: the share of the query's words the memory contains.
    pub lexical: f32,
    pub importance: f32,
    pub confidence: f32,
    pub recency: f32,
    /// 1 when supported by another memory, 0 when contradicted, else 0.5.
    pub relationship: f32,
    pub total: f32,
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 { 0.0 } else { dot / (na.sqrt() * nb.sqrt()) }
}

/// Importance nudged by experience: memories that helped rise, ones that
/// didn't sink (M13).
pub fn effective_importance(m: &Memory) -> f32 {
    let helped = m.usage.helpful.min(4) as f32 * 0.05;
    let hurt = m.usage.unhelpful.min(4) as f32 * 0.05;
    (m.importance + helped - hurt).clamp(0.0, 1.0)
}

/// `e^(-age / half_life)`, where age runs from the last update or use, and
/// important or often-helpful memories decay more slowly (M10).
pub fn recency(m: &Memory, h: &HalfLives, now: DateTime<Utc>) -> f32 {
    let base = match m.kind {
        MemoryKind::Working => h.working,
        MemoryKind::Episodic => h.episodic,
        MemoryKind::Semantic => h.semantic,
    };
    let half_life = base * (0.5 + effective_importance(m)) * (1.0 + 0.1 * m.usage.helpful.min(10) as f32);
    let last = m.last_accessed_at.map_or(m.updated_at, |t| t.max(m.updated_at));
    let age_days = (now - last).num_seconds().max(0) as f32 / 86_400.0;
    if half_life <= 0.0 { 1.0 } else { (-age_days / half_life).exp() }
}

/// Combine the signals. Without a vector for this memory or the query, the
/// semantic weight goes to the lexical match instead.
pub fn score(
    m: &Memory,
    lexical: f32,
    semantic: Option<f32>,
    relationship: f32,
    w: &Weights,
    h: &HalfLives,
    now: DateTime<Utc>,
) -> Score {
    let mut s = Score {
        semantic: semantic.map(|x| x.max(0.0)),
        lexical,
        importance: effective_importance(m),
        confidence: m.confidence.clamp(0.0, 1.0),
        recency: recency(m, h, now),
        relationship,
        total: 0.0,
    };
    let (semantic_part, lexical_weight) = match s.semantic {
        Some(sim) => (sim * w.semantic, w.lexical),
        None => (0.0, w.lexical + w.semantic),
    };
    s.total = semantic_part
        + s.lexical * lexical_weight
        + s.importance * w.importance
        + s.confidence * w.confidence
        + s.recency * w.recency
        + s.relationship * w.relationship;
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sqlite::tests::memory;
    use crate::Usage;
    use chrono::Duration;

    #[test]
    fn cosine_basics() {
        assert!((cosine(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    #[test]
    fn decay_depends_on_kind_and_importance() {
        let now = Utc::now();
        let h = HalfLives::default();
        let mut fact = memory("user", "fact");
        fact.updated_at = now - Duration::days(30);
        let mut note = fact.clone();
        note.kind = MemoryKind::Working;
        assert!(recency(&fact, &h, now) > 0.9, "facts last");
        assert!(recency(&note, &h, now) < 0.01, "working notes fade fast");

        let mut important = fact.clone();
        important.importance = 0.9;
        let mut minor = fact.clone();
        minor.importance = 0.1;
        assert!(recency(&important, &h, now) > recency(&minor, &h, now));
    }

    #[test]
    fn helpful_memories_rise_and_unhelpful_sink() {
        let mut m = memory("user", "x");
        m.usage = Usage { injected: 5, helpful: 4, unhelpful: 0 };
        assert!(effective_importance(&m) > m.importance);
        m.usage = Usage { injected: 5, helpful: 0, unhelpful: 4 };
        assert!(effective_importance(&m) < m.importance);
    }

    #[test]
    fn semantic_weight_moves_to_lexical_without_vectors() {
        let now = Utc::now();
        let (w, h) = (Weights::default(), HalfLives::default());
        let m = memory("user", "x");
        let with = score(&m, 0.5, Some(0.5), 0.5, &w, &h, now);
        let without = score(&m, 0.5, None, 0.5, &w, &h, now);
        assert!((with.total - without.total).abs() < 1e-6, "same total when both signals agree");
        let strong_semantic = score(&m, 0.0, Some(0.9), 0.5, &w, &h, now);
        assert!(strong_semantic.total > without.total - 0.5 * w.lexical);
    }

    #[test]
    fn lower_confidence_ranks_lower() {
        let now = Utc::now();
        let (w, h) = (Weights::default(), HalfLives::default());
        let sure = memory("user", "x");
        let mut guess = sure.clone();
        guess.confidence = 0.5;
        assert!(score(&sure, 1.0, None, 0.5, &w, &h, now).total > score(&guess, 1.0, None, 0.5, &w, &h, now).total);
    }
}

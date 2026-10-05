//! The retrieval regression benchmark: known memories, queries phrased
//! differently from them, and the rank each expected memory must reach.
//! Run whenever ranking or the embedding model changes. Hybrid retrieval
//! (vectors + full-text search) must beat keywords alone.

use std::sync::Arc;

use lyra_memory::embedding::FakeEmbedder;
use lyra_memory::{MemoryManager, MemorySource, NewMemory, Settings, Uuid};

/// Topics as directions in an 8-dimensional space; a "model" that knows
/// what things are about, whatever the words.
fn topic(t: usize, nudge: f32) -> Vec<f32> {
    let mut v = vec![0.05f32; 8];
    v[t] = 1.0;
    v[(t + 1) % 8] += nudge;
    v
}

/// `(memory, its topic)`.
const MEMORIES: &[(&str, usize)] = &[
    ("The agent runtime is written in Rust.", 0),
    ("Releases ship every Friday afternoon.", 1),
    ("The production database runs PostgreSQL 16 on db1.", 2),
    ("Continuous integration runs on Forgejo Actions.", 3),
    ("The user is vegetarian and dislikes mushrooms.", 4),
    ("Backups are copied to the NAS every night.", 5),
    ("The user prefers short, direct answers.", 6),
    ("The API listens on port 8080 behind nginx.", 7),
];

/// `(query, expected memory, topic, the rank it must reach)`.
const QUERIES: &[(&str, usize, usize, usize)] = &[
    ("What language is the runtime implemented in?", 0, 0, 1),
    ("When do we deploy?", 1, 1, 1),
    ("Which postgres version is in production?", 2, 2, 1),
    ("Where do our builds and tests run?", 3, 3, 2),
    ("What should I cook for them?", 4, 4, 2),
    ("How is data protected against loss?", 5, 5, 2),
    ("How should replies be written?", 6, 6, 2),
    ("Which port does the API use?", 7, 7, 1),
];

async fn hits(m: &MemoryManager, ids: &[Uuid]) -> usize {
    let mut hits = 0;
    for (query, expected, _, rank) in QUERIES {
        let found = m.recall(None, query, 5, false).await.unwrap();
        let position = found.iter().position(|r| r.memory.id == ids[*expected]);
        if position.is_some_and(|p| p < *rank) {
            hits += 1;
        }
    }
    hits
}

async fn load(with_vectors: bool) -> (MemoryManager, Vec<Uuid>) {
    let dir = std::env::temp_dir().join(format!("lyra-retrieval-{}", Uuid::new_v4()));
    let m = MemoryManager::open_lance(&dir, "memories", Settings::default()).await.unwrap();
    if with_vectors {
        let mut e = FakeEmbedder::new("topics", 8);
        for (text, t) in MEMORIES {
            e = e.with(text, &topic(*t, 0.0));
        }
        for (q, _, t, _) in QUERIES {
            e = e.with(q, &topic(*t, 0.1));
        }
        m.set_embedder(Some(Arc::new(e))).await.unwrap();
    }
    let mut ids = Vec::new();
    for (text, _) in MEMORIES {
        ids.push(m.remember(NewMemory::fact("user", text, MemorySource::User)).await.unwrap().memory().id);
    }
    (m, ids)
}

#[tokio::test]
async fn hybrid_retrieval_meets_the_benchmark_and_beats_keywords() {
    let (hybrid, ids) = load(true).await;
    let hybrid_hits = hits(&hybrid, &ids).await;
    assert_eq!(hybrid_hits, QUERIES.len(), "every query finds its memory within its rank");

    let (keywords, ids) = load(false).await;
    let keyword_hits = hits(&keywords, &ids).await;
    assert!(hybrid_hits > keyword_hits, "hybrid {hybrid_hits} vs keywords {keyword_hits}");
}

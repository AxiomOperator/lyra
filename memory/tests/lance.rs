//! LanceDB specifics: persistence, schema versioning (L20), embedding model
//! changes and re-embedding (L21, L22), normalized errors (L24), maintenance
//! (L15) and backup/restore (L19).

use std::sync::Arc;

use lyra_memory::embedding::FakeEmbedder;
use lyra_memory::store::Filter;
use lyra_memory::{LanceStore, MemoryManager, MemorySource, MemoryStore, MemoryStoreError, NewMemory, Settings, Uuid};

fn dir(tag: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("lyra-lance-{tag}-{}", Uuid::new_v4()))
}

async fn open(path: &std::path::Path) -> MemoryManager {
    MemoryManager::open_lance(path, "memories", Settings::default()).await.unwrap()
}

fn fact(text: &str) -> NewMemory {
    NewMemory::fact("user", text, MemorySource::User)
}

#[tokio::test]
async fn memories_persist_across_reopening() {
    let path = dir("persist");
    let id = {
        let m = open(&path).await;
        m.set_embedder(Some(Arc::new(FakeEmbedder::new("emb", 16)))).await.unwrap();
        m.remember(fact("The build server is called forge")).await.unwrap().memory().id
    };
    let m = open(&path).await;
    assert_eq!(m.get(id).await.unwrap().unwrap().content, "The build server is called forge");
    m.set_embedder(Some(Arc::new(FakeEmbedder::new("emb", 16)))).await.unwrap();
    assert_eq!(m.reembed(10).await.unwrap().remaining, 0, "same model: vectors still current");
    assert_eq!(m.recall(None, "build server name", 3, false).await.unwrap()[0].memory.id, id);
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn a_newer_schema_is_refused() {
    let path = dir("schema");
    open(&path).await;
    // Pretend a future lyra wrote this store.
    let db = lancedb::connect(path.to_str().unwrap()).execute().await.unwrap();
    let meta = db.open_table("memory_meta").execute().await.unwrap();
    meta.update().only_if("id = 'schema_version'").column("doc", "'99'").execute().await.unwrap();
    let err = MemoryManager::open_lance(&path, "memories", Settings::default()).await.err().unwrap();
    let typed = err.downcast_ref::<MemoryStoreError>().expect("a normalized error");
    assert!(matches!(typed, MemoryStoreError::SchemaMismatch(_)), "{typed}");
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn a_new_embedding_model_means_re_embedding_not_mixing() {
    let path = dir("reembed");
    let m = open(&path).await;
    assert_eq!(m.set_embedder(Some(Arc::new(FakeEmbedder::new("small", 8)))).await.unwrap(), None, "first model: nothing to redo");
    for text in ["The web server runs nginx", "Backups go to the NAS", "CI uses Forgejo Actions", "The wiki lives on Gitea", "Deploys happen on Fridays"] {
        m.remember(fact(text)).await.unwrap();
    }
    assert_eq!(m.stats().await.unwrap().embedded, 5);

    // Different model and size: the old vectors stop counting, everything is redone.
    let note = m.set_embedder(Some(Arc::new(FakeEmbedder::new("large", 32)))).await.unwrap();
    assert!(note.unwrap().contains("re-embedded"));
    assert_eq!(m.stats().await.unwrap().embedded, 0, "vectors from the old model aren't mixed in");
    let first = m.reembed(2).await.unwrap();
    assert_eq!((first.done, first.remaining), (2, 3), "batches, resumable");
    let rest = m.reembed(10).await.unwrap();
    assert_eq!((rest.done, rest.remaining), (3, 0));
    assert_eq!(m.stats().await.unwrap().embedded, 5);
    assert_eq!(m.list(&Filter::active(), 10).await.unwrap().len(), 5, "nothing lost in the rebuild");

    // Same size, new model name: a new generation, vectors redone.
    m.set_embedder(Some(Arc::new(FakeEmbedder::new("large-v2", 32)))).await.unwrap();
    assert_eq!(m.reembed(0).await.unwrap().remaining, 4, "one done, four to go");
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn wrong_sized_vectors_are_refused_with_a_clear_error() {
    let path = dir("dims");
    let store = LanceStore::open(&path, "memories").await.unwrap();
    store.prepare_embeddings("emb", 4).await.unwrap();
    let m = MemoryManager::new(store, Settings::default());
    let id = m.remember(fact("x marks the spot")).await.unwrap().memory().id;
    let store = LanceStore::open(&path, "memories").await.unwrap();
    let err = store.set_embedding(id, "emb", &[1.0, 2.0]).await.unwrap_err();
    assert!(matches!(err.downcast_ref::<MemoryStoreError>(), Some(MemoryStoreError::EmbeddingDimensionMismatch { expected: 4, got: 2 })), "{err}");
    let _ = std::fs::remove_dir_all(path);
}

/// Slow (~40s: an index needs 256+ vectors to train on). Run with
/// `cargo test -p lyra-memory --test lance -- --ignored`.
#[tokio::test]
#[ignore]
async fn maintenance_compacts_and_indexes_large_collections() {
    let path = dir("index");
    let e = FakeEmbedder::new("emb", 16);
    {
        // Straight into the store: 300 distinct memories with vectors.
        let store = LanceStore::open(&path, "memories").await.unwrap();
        store.prepare_embeddings("emb", 16).await.unwrap();
        let m = MemoryManager::new(store, Settings::default());
        m.maintain(0).await.unwrap();
    }
    let store = LanceStore::open(&path, "memories").await.unwrap();
    for i in 0..300 {
        let mut memory = lyra_memory::Memory {
            id: Uuid::new_v4(),
            scope: "user".into(),
            kind: lyra_memory::MemoryKind::Semantic,
            content: format!("service s{i} listens on port {}", 8000 + i),
            tags: vec![],
            source: MemorySource::User,
            provenance: Default::default(),
            importance: 0.5,
            confidence: 1.0,
            status: lyra_memory::MemoryStatus::Active,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            last_accessed_at: None,
            expires_at: None,
            usage: Default::default(),
        };
        memory.content = memory.content.replace("  ", " ");
        store.create(&memory).await.unwrap();
        let v = lyra_memory::EmbeddingProvider::embed_query(&e, &memory.content).await.unwrap();
        store.set_embedding(memory.id, "emb", &v).await.unwrap();
        if i % 100 == 99 {
            store.maintain(0).await.unwrap();
        }
    }
    let m = MemoryManager::new(store, Settings::default());
    m.set_embedder(Some(Arc::new(e))).await.unwrap();
    assert!(m.maintain(0).await.unwrap().is_empty(), "threshold 0 means exact search only");
    let notes = m.maintain(256).await.unwrap();
    assert!(notes.iter().any(|n| n.contains("vector index")), "{notes:?}");
    // Still finds things, through the index, and by keywords after compaction.
    let found = m.recall(None, "which port does service s42 use", 5, false).await.unwrap();
    assert!(found.iter().any(|r| r.memory.content.contains("s42 ")), "{:?}", found.iter().map(|r| &r.memory.content).collect::<Vec<_>>());
    assert_eq!(m.stats().await.unwrap().embedded, 300);
    assert!(m.maintain(256).await.unwrap().is_empty(), "the index is built once");
    let _ = std::fs::remove_dir_all(path);
}

#[tokio::test]
async fn backup_and_restore() {
    let path = dir("live");
    let backup = dir("backup");
    let m = open(&path).await;
    let kept = m.remember(fact("Backups are taken nightly")).await.unwrap().memory().id;
    m.backup(&backup).await.unwrap();
    assert!(m.backup(&backup).await.is_err(), "never overwrites a backup");
    let lost = m.remember(fact("This comes after the backup")).await.unwrap().memory().id;
    drop(m);

    let replaced = lyra_memory::restore(&backup, &path).unwrap();
    let m = open(&path).await;
    assert!(m.get(kept).await.unwrap().is_some());
    assert!(m.get(lost).await.unwrap().is_none(), "back to the backup's state");
    assert!(replaced.exists(), "the replaced store is kept aside");
    assert!(lyra_memory::restore(&path.join("nope"), &path).is_err(), "only real backups restore");
    for p in [path, backup, replaced] {
        let _ = std::fs::remove_dir_all(p);
    }
}

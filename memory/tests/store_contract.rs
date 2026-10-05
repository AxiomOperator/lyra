//! L6: one suite every memory store must pass, run against each backend, so
//! LanceDB and SQLite can't drift apart in behavior.

use chrono::{Duration, Utc};
use lyra_memory::store::{Event, Filter, MemoryChange, Proposal, ProposalStatus};
use lyra_memory::{
    Episode, LanceStore, Memory, MemoryKind, MemorySource, MemoryStatus, MemoryStore, Provenance, Relationship, SqliteStore,
    Uuid,
};

fn memory(scope: &str, content: &str) -> Memory {
    let now = Utc::now();
    Memory {
        id: Uuid::new_v4(),
        scope: scope.into(),
        kind: MemoryKind::Semantic,
        content: content.into(),
        tags: vec!["one".into(), "двa".into(), "three".into()],
        source: MemorySource::User,
        provenance: Provenance { run_id: Some(Uuid::new_v4()), tool_call_id: Some("call_1".into()), conversation_id: Some(Uuid::new_v4()) },
        importance: 0.7,
        confidence: 0.9,
        status: MemoryStatus::Active,
        created_at: now - Duration::days(3),
        updated_at: now - Duration::days(1),
        last_accessed_at: None,
        expires_at: Some(now + Duration::days(30)),
        usage: Default::default(),
    }
}

/// Timestamps are kept to the microsecond.
fn same_time(a: chrono::DateTime<Utc>, b: chrono::DateTime<Utc>) -> bool {
    (a - b).num_microseconds().unwrap_or(i64::MAX).abs() < 1
}

async fn contract(store: &dyn MemoryStore) {
    // create / get: every field round-trips (L5).
    let a = memory("user", "The agent runtime is written in Rust 🦀");
    store.create(&a).await.unwrap();
    assert!(store.create(&a).await.is_err(), "ids are unique");
    let back = store.get(a.id).await.unwrap().unwrap();
    assert_eq!((back.id, &back.scope, back.kind, &back.content, &back.tags), (a.id, &a.scope, a.kind, &a.content, &a.tags));
    assert_eq!((back.source, &back.provenance, back.importance, back.confidence, back.status), (a.source, &a.provenance, a.importance, a.confidence, a.status));
    assert!(same_time(back.created_at, a.created_at) && same_time(back.updated_at, a.updated_at));
    assert!(back.last_accessed_at.is_none() && same_time(back.expires_at.unwrap(), a.expires_at.unwrap()));
    assert!(store.get(Uuid::new_v4()).await.unwrap().is_none());

    // update keeps the vector; list filters and orders newest first.
    store.set_embedding(a.id, "emb", &[1.0, 0.0, 0.0]).await.unwrap();
    let mut changed = back.clone();
    changed.status = MemoryStatus::Archived;
    changed.tags = vec![];
    changed.expires_at = None;
    store.update(&changed).await.unwrap();
    let back = store.get(a.id).await.unwrap().unwrap();
    assert_eq!((back.status, back.tags.len(), back.expires_at), (MemoryStatus::Archived, 0, None));
    assert_eq!(store.embeddings("emb").await.unwrap().len(), 1, "update keeps the vector");
    let mut b = memory("project:x", "Deploys happen on Fridays");
    b.created_at = Utc::now();
    store.create(&b).await.unwrap();
    let listed = store.list(&Filter::default(), 10).await.unwrap();
    assert_eq!(listed.iter().map(|m| m.id).collect::<Vec<_>>(), [b.id, a.id]);
    assert_eq!(store.list(&Filter::active(), 10).await.unwrap().len(), 1);
    let scoped = Filter { scope: Some("user".into()), ..Filter::default() };
    assert_eq!(store.list(&scoped, 10).await.unwrap()[0].id, a.id);
    assert_eq!(store.scopes().await.unwrap(), [("project:x".to_string(), 1)]);

    // search: keywords, scoped.
    let found = store.search("which day are deploys", None, 5).await.unwrap();
    assert_eq!(found[0].0.id, b.id);
    assert!(found[0].1 > 0.0);
    assert!(store.search("deploys", Some("user"), 5).await.unwrap().is_empty());
    assert!(store.search("the a of", None, 5).await.unwrap().is_empty(), "no content words, no results");

    // vectors: nearest, missing.
    store.set_embedding(b.id, "emb", &[0.0, 1.0, 0.0]).await.unwrap();
    let near = store.nearest("emb", &[0.1, 0.9, 0.0], 5).await.unwrap();
    assert_eq!(near[0].0, b.id);
    assert!(near[0].1 > 0.9);
    assert_eq!(store.embeddings_of("emb", &[a.id]).await.unwrap().len(), 1);
    assert!(store.missing_embeddings("emb", 10).await.unwrap().is_empty());

    // history.
    assert_eq!(store.add_version(a.id, "v1", 0.9, "created").await.unwrap(), 1);
    assert_eq!(store.add_version(a.id, "v2", 0.9, "fixed").await.unwrap(), 2);
    assert_eq!(store.versions(a.id).await.unwrap()[0].content, "v2");
    store.relate(b.id, a.id, Relationship::Supports, "backs it up").await.unwrap();
    store.relate(b.id, a.id, Relationship::Supports, "again").await.unwrap();
    let rels = store.relationships(Some(a.id)).await.unwrap();
    assert_eq!(rels, [(b.id, a.id, Relationship::Supports, "again".to_string())], "one link, the latest reason");
    store.record(&Event::new("created", Some(a.id), "first")).await.unwrap();
    store.record(&Event::new("archived", Some(a.id), "second")).await.unwrap();
    let events = store.events(Some(a.id), 10).await.unwrap();
    assert_eq!(events.iter().map(|e| e.reason.as_str()).collect::<Vec<_>>(), ["second", "first"]);
    assert_eq!(store.last_event("created").await.unwrap().unwrap().reason, "first");

    // usage.
    let run = Uuid::new_v4();
    store.record_usage(run, &[a.id, b.id], true).await.unwrap();
    store.record_usage(Uuid::new_v4(), &[a.id], false).await.unwrap();
    assert!(store.get(a.id).await.unwrap().unwrap().last_accessed_at.is_some());
    assert_eq!(store.set_helpful(run, true).await.unwrap().len(), 2);
    let usage = store.usage().await.unwrap();
    assert_eq!((usage[&a.id].injected, usage[&a.id].helpful), (1, 1));

    // episodes and proposals.
    let ep = Episode {
        id: b.id,
        scope: "project:x".into(),
        summary: "Moved deploys".into(),
        outcome: "done".into(),
        entities: vec!["db1".into()],
        started_at: Utc::now() - Duration::hours(1),
        ended_at: Utc::now(),
        source_run_id: Some(run),
    };
    store.add_episode(&ep).await.unwrap();
    assert_eq!(store.episodes(5).await.unwrap()[0].entities, ["db1"]);
    let p = Proposal::new(MemoryChange::Archive { memory: a.id }, "stale".into());
    store.add_proposal(&p).await.unwrap();
    assert_eq!(store.pending_proposals().await.unwrap().len(), 1);
    store.set_proposal_status(p.id, ProposalStatus::Applied).await.unwrap();
    assert!(store.pending_proposals().await.unwrap().is_empty());
    assert!(store.set_proposal_status(Uuid::new_v4(), ProposalStatus::Applied).await.is_err());

    // delete takes everything about the memory with it, except the audit log.
    store.delete(b.id).await.unwrap();
    assert!(store.get(b.id).await.unwrap().is_none());
    assert!(store.relationships(Some(a.id)).await.unwrap().is_empty());
    assert!(store.episodes(5).await.unwrap().is_empty());
    assert!(store.delete(b.id).await.is_err());
    assert_eq!(store.events(Some(a.id), 10).await.unwrap().len(), 2);
}

#[tokio::test]
async fn sqlite_store_meets_the_contract() {
    contract(&SqliteStore::in_memory().await.unwrap()).await;
}

#[tokio::test]
async fn lance_store_meets_the_contract() {
    let dir = std::env::temp_dir().join(format!("lyra-contract-{}", Uuid::new_v4()));
    let store = LanceStore::open(&dir, "memories").await.unwrap();
    store.prepare_embeddings("emb", 3).await.unwrap();
    contract(&store).await;
    let _ = std::fs::remove_dir_all(dir);
}

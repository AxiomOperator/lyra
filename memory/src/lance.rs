//! LanceDB storage for memories (docs/lancedb_migration.md, L2–L5, L12–L15,
//! L19–L22): one local directory, no server.
//!
//! - `memories` holds each memory with typed columns (scope, kind, status,
//!   trust, dates, tags, provenance) and its embedding: a fixed-size vector
//!   column sized for the embedding model, plus the model and generation that
//!   made it, so vectors from different models are never mixed (L21).
//!   Full-text search (an FTS index on `content`) and vector search (exact,
//!   or an index once the collection is large) both run here; filtering is
//!   applied in the store, ranking stays in the manager (L14).
//! - The history around memories (versions, relationships, events, usage,
//!   episodes, proposals) lives in small document tables: an id, the columns
//!   they're looked up by, and the record as JSON.
//! - `meta` holds the schema version (L20) and the embedding model,
//!   dimensions and generation.
//!
//! Lance errors never leave this module raw: they become
//! [`MemoryStoreError`]s (L24).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use anyhow::{Result, anyhow, bail};
use arrow_array::builder::{ListBuilder, StringBuilder};
use arrow_array::types::Float32Type;
use arrow_array::{
    Array, ArrayRef, FixedSizeListArray, Float32Array, Int64Array, ListArray, RecordBatch, RecordBatchIterator,
    StringArray, TimestampMicrosecondArray, UInt32Array,
};
use arrow_schema::{DataType, Field, Schema, SchemaRef, TimeUnit};
use chrono::{DateTime, TimeZone, Utc};
use futures::TryStreamExt;
use lancedb::index::Index;
use lancedb::index::scalar::{FtsIndexBuilder, FullTextSearchQuery};
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use lancedb::table::OptimizeAction;
use lancedb::{Connection, DistanceType, Table};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

use crate::error::MemoryStoreError;
use crate::memory::{Episode, Memory, Provenance, Relationship, Usage};
use crate::store::{Event, Filter, MemoryStore, Proposal, ProposalStatus, Version};

/// The schema this code reads and writes (L20). Bump it, with a migration,
/// whenever a table's columns change.
pub const MEMORY_SCHEMA_VERSION: u32 = 1;

/// Vector size until an embedding model says otherwise.
const DEFAULT_DIMENSIONS: usize = 1024;

const VERSIONS: &str = "memory_versions";
const RELATIONSHIPS: &str = "memory_relationships";
const EVENTS: &str = "memory_events";
const USAGE: &str = "memory_usage";
const EPISODES: &str = "memory_episodes";
const PROPOSALS: &str = "memory_proposals";
const META: &str = "memory_meta";

/// A stored vector and what made it.
#[derive(Debug, Clone)]
struct Embedding {
    vector: Vec<f32>,
    model: String,
    generation: u32,
}

/// The embedding column's current shape and model.
#[derive(Debug, Clone)]
struct VectorSpace {
    dimensions: usize,
    model: Option<String>,
    generation: u32,
}

pub struct LanceStore {
    db: Connection,
    path: PathBuf,
    name: String,
    /// Replaced when the embedding dimensions change.
    memories: RwLock<Table>,
    docs: HashMap<&'static str, Table>,
    space: RwLock<VectorSpace>,
    /// Read-modify-write sequences (version numbers, row updates) run one at a time.
    write: Mutex<()>,
    fts_ready: AtomicBool,
    /// Insertion order for document rows (ties in time).
    seq: AtomicI64,
}

fn err(e: impl Into<MemoryStoreError>) -> anyhow::Error {
    anyhow::Error::new(e.into())
}

/// A SQL string literal.
fn lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn micros(t: DateTime<Utc>) -> i64 {
    t.timestamp_micros()
}

fn from_micros(us: i64) -> Result<DateTime<Utc>> {
    Utc.timestamp_micros(us).single().ok_or_else(|| err(MemoryStoreError::CorruptData(format!("bad timestamp {us}"))))
}

fn ts_field(name: &str, nullable: bool) -> Field {
    Field::new(name, DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())), nullable)
}

fn memory_schema(dimensions: usize) -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("scope", DataType::Utf8, false),
        Field::new("kind", DataType::Utf8, false),
        Field::new("content", DataType::Utf8, false),
        Field::new("tags", DataType::List(Arc::new(Field::new("item", DataType::Utf8, true))), false),
        Field::new("source", DataType::Utf8, false),
        Field::new("importance", DataType::Float32, false),
        Field::new("confidence", DataType::Float32, false),
        Field::new("status", DataType::Utf8, false),
        ts_field("created_at", false),
        ts_field("updated_at", false),
        ts_field("last_accessed_at", true),
        ts_field("expires_at", true),
        Field::new("run_id", DataType::Utf8, true),
        Field::new("tool_call_id", DataType::Utf8, true),
        Field::new("conversation_id", DataType::Utf8, true),
        Field::new(
            "embedding",
            DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float32, true)), dimensions as i32),
            true,
        ),
        Field::new("embedding_model", DataType::Utf8, true),
        Field::new("embedding_generation", DataType::UInt32, true),
    ]))
}

/// History tables: looked up by `memory_id`, `run_id` or `kind`, ordered by `seq`.
fn doc_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("memory_id", DataType::Utf8, true),
        Field::new("other_id", DataType::Utf8, true),
        Field::new("run_id", DataType::Utf8, true),
        Field::new("kind", DataType::Utf8, true),
        Field::new("seq", DataType::Int64, false),
        Field::new("doc", DataType::Utf8, false),
    ]))
}

/// One row of a document table.
struct Doc {
    id: String,
    memory_id: Option<String>,
    other_id: Option<String>,
    run_id: Option<String>,
    kind: Option<String>,
    seq: i64,
    doc: String,
}

impl Doc {
    fn new(id: impl ToString, record: &impl Serialize) -> Result<Self> {
        Ok(Self {
            id: id.to_string(),
            memory_id: None,
            other_id: None,
            run_id: None,
            kind: None,
            seq: 0,
            doc: serde_json::to_string(record)?,
        })
    }

    fn parse<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_str(&self.doc).map_err(|e| err(MemoryStoreError::CorruptData(e.to_string())))
    }
}

fn doc_batch(rows: &[Doc]) -> Result<RecordBatch> {
    let s = |f: &dyn Fn(&Doc) -> Option<String>| Arc::new(StringArray::from(rows.iter().map(f).collect::<Vec<_>>())) as ArrayRef;
    RecordBatch::try_new(
        doc_schema(),
        vec![
            Arc::new(StringArray::from(rows.iter().map(|r| r.id.clone()).collect::<Vec<_>>())),
            s(&|r| r.memory_id.clone()),
            s(&|r| r.other_id.clone()),
            s(&|r| r.run_id.clone()),
            s(&|r| r.kind.clone()),
            Arc::new(Int64Array::from(rows.iter().map(|r| r.seq).collect::<Vec<_>>())),
            Arc::new(StringArray::from(rows.iter().map(|r| r.doc.clone()).collect::<Vec<_>>())),
        ],
    )
    .map_err(err)
}

fn column<'a, T: 'static>(b: &'a RecordBatch, name: &str) -> Result<&'a T> {
    b.column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<T>())
        .ok_or_else(|| err(MemoryStoreError::SchemaMismatch(format!("column {name} is missing or has the wrong type"))))
}

fn opt_str(a: &StringArray, i: usize) -> Option<String> {
    (!a.is_null(i)).then(|| a.value(i).to_string())
}

fn docs_from(batches: &[RecordBatch]) -> Result<Vec<Doc>> {
    let mut out = Vec::new();
    for b in batches {
        let (id, mid, oid, run, kind) = (
            column::<StringArray>(b, "id")?,
            column::<StringArray>(b, "memory_id")?,
            column::<StringArray>(b, "other_id")?,
            column::<StringArray>(b, "run_id")?,
            column::<StringArray>(b, "kind")?,
        );
        let (seq, doc) = (column::<Int64Array>(b, "seq")?, column::<StringArray>(b, "doc")?);
        for i in 0..b.num_rows() {
            out.push(Doc {
                id: id.value(i).to_string(),
                memory_id: opt_str(mid, i),
                other_id: opt_str(oid, i),
                run_id: opt_str(run, i),
                kind: opt_str(kind, i),
                seq: seq.value(i),
                doc: doc.value(i).to_string(),
            });
        }
    }
    Ok(out)
}

fn memory_batch(schema: &SchemaRef, rows: &[(Memory, Option<Embedding>)]) -> Result<RecordBatch> {
    let dims = match schema.field_with_name("embedding").map_err(err)?.data_type() {
        DataType::FixedSizeList(_, n) => *n,
        _ => return Err(err(MemoryStoreError::SchemaMismatch("embedding column isn't a vector".into()))),
    };
    let strs = |f: &dyn Fn(&Memory) -> String| Arc::new(StringArray::from(rows.iter().map(|(m, _)| f(m)).collect::<Vec<_>>())) as ArrayRef;
    let opt = |f: &dyn Fn(&Memory) -> Option<String>| Arc::new(StringArray::from(rows.iter().map(|(m, _)| f(m)).collect::<Vec<_>>())) as ArrayRef;
    let time = |f: &dyn Fn(&Memory) -> Option<DateTime<Utc>>| {
        Arc::new(TimestampMicrosecondArray::from(rows.iter().map(|(m, _)| f(m).map(micros)).collect::<Vec<_>>()).with_timezone("UTC"))
            as ArrayRef
    };
    let mut tags = ListBuilder::new(StringBuilder::new());
    for (m, _) in rows {
        for t in &m.tags {
            tags.values().append_value(t);
        }
        tags.append(true);
    }
    let vectors = FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
        rows.iter().map(|(_, e)| e.as_ref().map(|e| e.vector.iter().copied().map(Some).collect::<Vec<_>>())),
        dims,
    );
    RecordBatch::try_new(
        schema.clone(),
        vec![
            strs(&|m| m.id.to_string()),
            strs(&|m| m.scope.clone()),
            strs(&|m| m.kind.as_str().to_string()),
            strs(&|m| m.content.clone()),
            Arc::new(tags.finish()),
            strs(&|m| m.source.as_str().to_string()),
            Arc::new(Float32Array::from(rows.iter().map(|(m, _)| m.importance).collect::<Vec<_>>())),
            Arc::new(Float32Array::from(rows.iter().map(|(m, _)| m.confidence).collect::<Vec<_>>())),
            strs(&|m| m.status.as_str().to_string()),
            time(&|m| Some(m.created_at)),
            time(&|m| Some(m.updated_at)),
            time(&|m| m.last_accessed_at),
            time(&|m| m.expires_at),
            opt(&|m| m.provenance.run_id.map(|r| r.to_string())),
            opt(&|m| m.provenance.tool_call_id.clone()),
            opt(&|m| m.provenance.conversation_id.map(|r| r.to_string())),
            Arc::new(vectors),
            Arc::new(StringArray::from(rows.iter().map(|(_, e)| e.as_ref().map(|e| e.model.clone())).collect::<Vec<_>>())),
            Arc::new(UInt32Array::from(rows.iter().map(|(_, e)| e.as_ref().map(|e| e.generation)).collect::<Vec<_>>())),
        ],
    )
    .map_err(err)
}

fn memories_from(batches: &[RecordBatch]) -> Result<Vec<(Memory, Option<Embedding>)>> {
    let mut out = Vec::new();
    for b in batches {
        let s = |n: &str| column::<StringArray>(b, n);
        let (id, scope, kind, content, source, status) = (s("id")?, s("scope")?, s("kind")?, s("content")?, s("source")?, s("status")?);
        let (run, call, conv) = (s("run_id")?, s("tool_call_id")?, s("conversation_id")?);
        let tags = column::<ListArray>(b, "tags")?;
        let (imp, conf) = (column::<Float32Array>(b, "importance")?, column::<Float32Array>(b, "confidence")?);
        let t = |n: &str| column::<TimestampMicrosecondArray>(b, n);
        let (created, updated, accessed, expires) = (t("created_at")?, t("updated_at")?, t("last_accessed_at")?, t("expires_at")?);
        let opt_time = |a: &TimestampMicrosecondArray, i: usize| -> Result<Option<DateTime<Utc>>> {
            if a.is_null(i) { Ok(None) } else { from_micros(a.value(i)).map(Some) }
        };
        let uuid = |a: &StringArray, i: usize| -> Result<Option<Uuid>> {
            opt_str(a, i).map(|v| Uuid::parse_str(&v)).transpose().map_err(|e| err(MemoryStoreError::CorruptData(e.to_string())))
        };
        // Search results may leave the vector columns out.
        let vectors = b.column_by_name("embedding").and_then(|c| c.as_any().downcast_ref::<FixedSizeListArray>());
        let models = b.column_by_name("embedding_model").and_then(|c| c.as_any().downcast_ref::<StringArray>());
        let generations = b.column_by_name("embedding_generation").and_then(|c| c.as_any().downcast_ref::<UInt32Array>());
        for i in 0..b.num_rows() {
            let tag_values = tags.value(i);
            let tag_values = tag_values.as_any().downcast_ref::<StringArray>().ok_or_else(|| err(MemoryStoreError::CorruptData("tags".into())))?;
            let corrupt = |what: &str, e: String| err(MemoryStoreError::CorruptData(format!("{what}: {e}")));
            let memory = Memory {
                id: Uuid::parse_str(id.value(i)).map_err(|e| corrupt("id", e.to_string()))?,
                scope: scope.value(i).to_string(),
                kind: kind.value(i).parse().map_err(|e: anyhow::Error| corrupt("kind", e.to_string()))?,
                content: content.value(i).to_string(),
                tags: (0..tag_values.len()).map(|j| tag_values.value(j).to_string()).collect(),
                source: source.value(i).parse().unwrap_or(crate::MemorySource::Conversation),
                provenance: Provenance { run_id: uuid(run, i)?, tool_call_id: opt_str(call, i), conversation_id: uuid(conv, i)? },
                importance: imp.value(i),
                confidence: conf.value(i),
                status: status.value(i).parse().map_err(|e: anyhow::Error| corrupt("status", e.to_string()))?,
                created_at: from_micros(created.value(i))?,
                updated_at: from_micros(updated.value(i))?,
                last_accessed_at: opt_time(accessed, i)?,
                expires_at: opt_time(expires, i)?,
                usage: Usage::default(),
            };
            let embedding = match (vectors, models) {
                (Some(v), Some(m)) if !v.is_null(i) && !m.is_null(i) => {
                    let values = v.value(i);
                    let values = values.as_any().downcast_ref::<Float32Array>().ok_or_else(|| corrupt("embedding", "not f32".into()))?;
                    Some(Embedding {
                        vector: values.values().to_vec(),
                        model: m.value(i).to_string(),
                        generation: generations.map_or(0, |g| if g.is_null(i) { 0 } else { g.value(i) }),
                    })
                }
                _ => None,
            };
            out.push((memory, embedding));
        }
    }
    Ok(out)
}

async fn collect(stream: impl std::future::Future<Output = lancedb::Result<lancedb::arrow::SendableRecordBatchStream>>) -> Result<Vec<RecordBatch>> {
    stream.await.map_err(err)?.try_collect::<Vec<_>>().await.map_err(err)
}

fn reader(batch: RecordBatch) -> Box<dyn arrow_array::RecordBatchReader + Send> {
    let schema = batch.schema();
    Box::new(RecordBatchIterator::new(vec![Ok(batch)], schema))
}

/// Total size of the files under `dir`.
fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            _ => e.metadata().map(|m| m.len()).unwrap_or(0),
        })
        .sum()
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

impl LanceStore {
    /// Open (or create) the store in directory `path`, memories in table `name`.
    pub async fn open(path: &Path, name: &str) -> Result<Self> {
        std::fs::create_dir_all(path).map_err(|e| err(MemoryStoreError::Unavailable(format!("{}: {e}", path.display()))))?;
        let uri = path.to_str().ok_or_else(|| err(MemoryStoreError::Unavailable("path isn't UTF-8".into())))?;
        let db = lancedb::connect(uri).execute().await.map_err(err)?;
        let names = db.table_names().execute().await.map_err(err)?;
        let open_or_create = async |table: &str, schema: SchemaRef| -> Result<Table> {
            if names.iter().any(|n| n == table) {
                db.open_table(table).execute().await.map_err(err)
            } else {
                db.create_empty_table(table, schema).execute().await.map_err(err)
            }
        };
        let meta = open_or_create(META, doc_schema()).await?;
        let mut docs = HashMap::new();
        for t in [VERSIONS, RELATIONSHIPS, EVENTS, USAGE, EPISODES, PROPOSALS] {
            docs.insert(t, open_or_create(t, doc_schema()).await?);
        }
        docs.insert(META, meta);
        let memories = open_or_create(name, memory_schema(DEFAULT_DIMENSIONS)).await?;
        let mut store = Self {
            db,
            path: path.to_path_buf(),
            name: name.to_string(),
            memories: RwLock::new(memories),
            docs,
            space: RwLock::new(VectorSpace { dimensions: DEFAULT_DIMENSIONS, model: None, generation: 0 }),
            write: Mutex::new(()),
            fts_ready: AtomicBool::new(false),
            seq: AtomicI64::new(0),
        };
        store.check_schema().await?;
        store.load_space().await?;
        let has_fts = store.memories.read().await.list_indices().await.map_err(err)?.iter().any(|i| i.columns.iter().any(|c| c == "content"));
        store.fts_ready = AtomicBool::new(has_fts);
        Ok(store)
    }

    fn doc_table(&self, name: &str) -> &Table {
        &self.docs[name]
    }

    async fn meta(&self, key: &str) -> Result<Option<String>> {
        let rows = collect(self.doc_table(META).query().only_if(format!("id = {}", lit(key))).execute()).await?;
        Ok(docs_from(&rows)?.into_iter().next().map(|d| d.doc))
    }

    async fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        let doc = Doc { id: key.into(), memory_id: None, other_id: None, run_id: None, kind: None, seq: 0, doc: value.into() };
        self.upsert_docs(META, vec![doc]).await
    }

    /// L20: a store written by a newer schema is refused rather than misread.
    async fn check_schema(&self) -> Result<()> {
        match self.meta("schema_version").await? {
            None => self.set_meta("schema_version", &MEMORY_SCHEMA_VERSION.to_string()).await,
            Some(v) => {
                let v: u32 = v.parse().map_err(|_| err(MemoryStoreError::CorruptData(format!("schema_version {v:?}"))))?;
                if v > MEMORY_SCHEMA_VERSION {
                    return Err(err(MemoryStoreError::SchemaMismatch(format!(
                        "the memory store is at schema v{v}, this lyra understands v{MEMORY_SCHEMA_VERSION}"
                    ))));
                }
                Ok(())
            }
        }
    }

    async fn load_space(&self) -> Result<()> {
        let schema = self.memories.read().await.schema().await.map_err(err)?;
        let dimensions = match schema.field_with_name("embedding").map_err(err)?.data_type() {
            DataType::FixedSizeList(_, n) => *n as usize,
            _ => return Err(err(MemoryStoreError::SchemaMismatch("embedding column isn't a vector".into()))),
        };
        let model = self.meta("embedding_model").await?;
        let generation = self.meta("embedding_generation").await?.and_then(|g| g.parse().ok()).unwrap_or(0);
        *self.space.write().await = VectorSpace { dimensions, model, generation };
        Ok(())
    }

    fn next_seq(&self) -> i64 {
        let now = Utc::now().timestamp_micros();
        let mut prev = self.seq.load(Ordering::SeqCst);
        loop {
            let next = now.max(prev + 1);
            match self.seq.compare_exchange(prev, next, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => return next,
                Err(p) => prev = p,
            }
        }
    }

    async fn add_docs(&self, table: &str, mut rows: Vec<Doc>) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        for r in &mut rows {
            r.seq = self.next_seq();
        }
        self.doc_table(table).add(reader(doc_batch(&rows)?)).execute().await.map_err(err)?;
        Ok(())
    }

    async fn upsert_docs(&self, table: &str, mut rows: Vec<Doc>) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        for r in rows.iter_mut().filter(|r| r.seq == 0) {
            r.seq = self.next_seq();
        }
        let mut m = self.doc_table(table).merge_insert(&["id"]);
        m.when_matched_update_all(None).when_not_matched_insert_all();
        m.execute(reader(doc_batch(&rows)?)).await.map_err(err)?;
        Ok(())
    }

    async fn docs(&self, table: &str, filter: Option<String>) -> Result<Vec<Doc>> {
        let mut q = self.doc_table(table).query();
        if let Some(f) = filter {
            q = q.only_if(f);
        }
        let mut rows = docs_from(&collect(q.execute()).await?)?;
        rows.sort_by_key(|d| d.seq);
        Ok(rows)
    }

    async fn rows(&self, filter: Option<String>) -> Result<Vec<(Memory, Option<Embedding>)>> {
        let table = self.memories.read().await;
        let mut q = table.query();
        if let Some(f) = filter {
            q = q.only_if(f);
        }
        memories_from(&collect(q.execute()).await?)
    }

    async fn row(&self, id: Uuid) -> Result<Option<(Memory, Option<Embedding>)>> {
        Ok(self.rows(Some(format!("id = {}", lit(&id.to_string())))).await?.into_iter().next())
    }

    async fn upsert_rows(&self, rows: &[(Memory, Option<Embedding>)]) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        let table = self.memories.read().await;
        let schema = table.schema().await.map_err(err)?;
        let mut m = table.merge_insert(&["id"]);
        m.when_matched_update_all(None).when_not_matched_insert_all();
        m.execute(reader(memory_batch(&schema, rows)?)).await.map_err(err)?;
        drop(table);
        self.ensure_fts().await
    }

    /// The FTS index is built once there's text to index; rows added later
    /// are searched too until the index is optimized.
    async fn ensure_fts(&self) -> Result<()> {
        if self.fts_ready.load(Ordering::SeqCst) {
            return Ok(());
        }
        let table = self.memories.read().await;
        if table.count_rows(None).await.map_err(err)? == 0 {
            return Ok(());
        }
        table.create_index(&["content"], Index::FTS(FtsIndexBuilder::default())).execute().await.map_err(err)?;
        self.fts_ready.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Rebuild the memories table with a new vector size; every vector is
    /// dropped (they're re-embedded), everything else is kept.
    async fn rebuild(&self, dimensions: usize) -> Result<()> {
        let rows: Vec<(Memory, Option<Embedding>)> = self.rows(None).await?.into_iter().map(|(m, _)| (m, None)).collect();
        let mut table = self.memories.write().await;
        self.db.drop_table(&self.name, &[]).await.map_err(err)?;
        let schema = memory_schema(dimensions);
        *table = self.db.create_empty_table(&self.name, schema.clone()).execute().await.map_err(err)?;
        if !rows.is_empty() {
            table.add(reader(memory_batch(&schema, &rows)?)).execute().await.map_err(err)?;
        }
        drop(table);
        self.fts_ready.store(false, Ordering::SeqCst);
        self.ensure_fts().await
    }

    fn id_list(ids: &[Uuid]) -> String {
        ids.iter().map(|i| lit(&i.to_string())).collect::<Vec<_>>().join(", ")
    }
}

#[async_trait::async_trait]
impl MemoryStore for LanceStore {
    fn backend(&self) -> &'static str {
        "lance"
    }

    async fn create(&self, memory: &Memory) -> Result<()> {
        let _w = self.write.lock().await;
        // Insert only: an existing id is left alone and reported.
        let table = self.memories.read().await;
        let schema = table.schema().await.map_err(err)?;
        let mut m = table.merge_insert(&["id"]);
        m.when_not_matched_insert_all();
        let result = m.execute(reader(memory_batch(&schema, &[(memory.clone(), None)])?)).await.map_err(err)?;
        drop(table);
        if result.num_inserted_rows == 0 {
            bail!("memory {} already exists", memory.id);
        }
        self.ensure_fts().await
    }

    async fn get(&self, id: Uuid) -> Result<Option<Memory>> {
        Ok(self.row(id).await?.map(|(m, _)| m))
    }

    async fn update(&self, memory: &Memory) -> Result<()> {
        let _w = self.write.lock().await;
        // Keep the vector: a content change is re-embedded by the manager.
        let Some((_, embedding)) = self.row(memory.id).await? else { bail!("no memory with id {}", memory.id) };
        self.upsert_rows(&[(memory.clone(), embedding)]).await
    }

    async fn delete(&self, id: Uuid) -> Result<()> {
        let _w = self.write.lock().await;
        if self.row(id).await?.is_none() {
            bail!("no memory with id {id}");
        }
        let id = lit(&id.to_string());
        self.memories.read().await.delete(&format!("id = {id}")).await.map_err(err)?;
        for t in [VERSIONS, USAGE, EPISODES] {
            self.doc_table(t).delete(&format!("memory_id = {id}")).await.map_err(err)?;
        }
        self.doc_table(RELATIONSHIPS).delete(&format!("memory_id = {id} OR other_id = {id}")).await.map_err(err)?;
        Ok(())
    }

    async fn search(&self, query: &str, scope: Option<&str>, limit: usize) -> Result<Vec<(Memory, f32)>> {
        let words = crate::text::content_words(query);
        if words.is_empty() || !self.fts_ready.load(Ordering::SeqCst) {
            return Ok(Vec::new());
        }
        let table = self.memories.read().await;
        let mut q = table.query().full_text_search(FullTextSearchQuery::new(words.join(" "))).limit(limit);
        if let Some(scope) = scope {
            q = q.only_if(format!("scope = {}", lit(scope)));
        }
        let batches = collect(q.execute()).await?;
        let mut scores = Vec::new();
        for b in &batches {
            let s = column::<Float32Array>(b, "_score")?;
            scores.extend((0..b.num_rows()).map(|i| s.value(i)));
        }
        Ok(memories_from(&batches)?.into_iter().map(|(m, _)| m).zip(scores).collect())
    }

    async fn list(&self, filter: &Filter, limit: usize) -> Result<Vec<Memory>> {
        let mut conditions = Vec::new();
        if let Some(scope) = &filter.scope {
            conditions.push(format!("scope = {}", lit(scope)));
        }
        if !filter.statuses.is_empty() {
            let statuses: Vec<String> = filter.statuses.iter().map(|s| lit(s.as_str())).collect();
            conditions.push(format!("status IN ({})", statuses.join(", ")));
        }
        if let Some(kind) = filter.kind {
            conditions.push(format!("kind = {}", lit(kind.as_str())));
        }
        let mut rows: Vec<Memory> =
            self.rows((!conditions.is_empty()).then(|| conditions.join(" AND "))).await?.into_iter().map(|(m, _)| m).collect();
        rows.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
        rows.truncate(limit);
        Ok(rows)
    }

    async fn scopes(&self) -> Result<Vec<(String, u64)>> {
        let mut counts: HashMap<String, u64> = HashMap::new();
        for (m, _) in self.rows(Some("status = 'active'".into())).await? {
            *counts.entry(m.scope).or_default() += 1;
        }
        let mut out: Vec<(String, u64)> = counts.into_iter().collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        Ok(out)
    }

    async fn set_embedding(&self, id: Uuid, model: &str, vector: &[f32]) -> Result<()> {
        let space = self.space.read().await.clone();
        crate::embedding::check(vector, space.dimensions).map_err(err)?;
        if space.model.as_deref().is_some_and(|m| m != model) {
            return Err(err(MemoryStoreError::InvalidQuery(format!(
                "vectors here come from {}, not {model}",
                space.model.unwrap_or_default()
            ))));
        }
        let _w = self.write.lock().await;
        let Some((memory, _)) = self.row(id).await? else { bail!("no memory with id {id}") };
        let embedding = Embedding { vector: vector.to_vec(), model: model.to_string(), generation: space.generation };
        self.upsert_rows(&[(memory, Some(embedding))]).await
    }

    async fn embeddings(&self, model: &str) -> Result<Vec<(Uuid, Vec<f32>)>> {
        let generation = self.space.read().await.generation;
        let filter = format!("embedding_model = {} AND embedding_generation = {generation}", lit(model));
        Ok(self.rows(Some(filter)).await?.into_iter().filter_map(|(m, e)| Some((m.id, e?.vector))).collect())
    }

    async fn embeddings_of(&self, model: &str, ids: &[Uuid]) -> Result<Vec<(Uuid, Vec<f32>)>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let generation = self.space.read().await.generation;
        let filter = format!(
            "id IN ({}) AND embedding_model = {} AND embedding_generation = {generation}",
            Self::id_list(ids),
            lit(model)
        );
        Ok(self.rows(Some(filter)).await?.into_iter().filter_map(|(m, e)| Some((m.id, e?.vector))).collect())
    }

    async fn missing_embeddings(&self, model: &str, limit: usize) -> Result<Vec<Memory>> {
        let generation = self.space.read().await.generation;
        let filter = format!(
            "status = 'active' AND (embedding_model IS NULL OR embedding_model != {} OR embedding_generation IS NULL OR embedding_generation != {generation})",
            lit(model)
        );
        let mut rows: Vec<Memory> = self.rows(Some(filter)).await?.into_iter().map(|(m, _)| m).collect();
        rows.sort_by_key(|m| std::cmp::Reverse(m.created_at));
        rows.truncate(limit);
        Ok(rows)
    }

    async fn nearest(&self, model: &str, vector: &[f32], limit: usize) -> Result<Vec<(Uuid, f32)>> {
        let generation = self.space.read().await.generation;
        if vector.len() != self.space.read().await.dimensions {
            return Ok(Vec::new());
        }
        let table = self.memories.read().await;
        let q = table
            .query()
            .nearest_to(vector)
            .map_err(err)?
            .distance_type(DistanceType::Cosine)
            .only_if(format!("embedding_model = {} AND embedding_generation = {generation}", lit(model)))
            .select(Select::columns(&["id"]))
            .limit(limit);
        let batches = collect(q.execute()).await?;
        let mut out = Vec::new();
        for b in &batches {
            let (ids, dist) = (column::<StringArray>(b, "id")?, column::<Float32Array>(b, "_distance")?);
            for i in 0..b.num_rows() {
                let id = Uuid::parse_str(ids.value(i)).map_err(|e| err(MemoryStoreError::CorruptData(e.to_string())))?;
                // Cosine distance is 1 - similarity.
                out.push((id, 1.0 - dist.value(i)));
            }
        }
        Ok(out)
    }

    async fn prepare_embeddings(&self, model: &str, dimensions: usize) -> Result<Option<String>> {
        let space = self.space.read().await.clone();
        if space.model.as_deref() == Some(model) && space.dimensions == dimensions {
            return Ok(None);
        }
        let _w = self.write.lock().await;
        let had_vectors = space.model.is_some();
        if space.dimensions != dimensions {
            self.rebuild(dimensions).await?;
        }
        // A new generation: older vectors are no longer used and get redone.
        let generation = space.generation + 1;
        self.set_meta("embedding_model", model).await?;
        self.set_meta("embedding_dimensions", &dimensions.to_string()).await?;
        self.set_meta("embedding_generation", &generation.to_string()).await?;
        *self.space.write().await = VectorSpace { dimensions, model: Some(model.to_string()), generation };
        Ok(had_vectors.then(|| {
            format!(
                "embedding model changed ({} → {model}, {dimensions} dimensions): memories are being re-embedded",
                space.model.unwrap_or_default()
            )
        }))
    }

    async fn add_version(&self, id: Uuid, content: &str, confidence: f32, reason: &str) -> Result<i64> {
        let _w = self.write.lock().await;
        let next = self.versions(id).await?.first().map_or(1, |v| v.version + 1);
        let v = Version { memory_id: id, version: next, content: content.into(), confidence, reason: reason.into(), created_at: Utc::now() };
        let mut doc = Doc::new(Uuid::new_v4(), &v)?;
        doc.memory_id = Some(id.to_string());
        self.add_docs(VERSIONS, vec![doc]).await?;
        Ok(next)
    }

    async fn versions(&self, id: Uuid) -> Result<Vec<Version>> {
        let docs = self.docs(VERSIONS, Some(format!("memory_id = {}", lit(&id.to_string())))).await?;
        let mut out: Vec<Version> = docs.iter().map(Doc::parse).collect::<Result<_>>()?;
        out.sort_by_key(|v| std::cmp::Reverse(v.version));
        Ok(out)
    }

    async fn relate(&self, from: Uuid, to: Uuid, relationship: Relationship, reason: &str) -> Result<()> {
        #[derive(Serialize)]
        struct Link<'a> {
            reason: &'a str,
            created_at: DateTime<Utc>,
        }
        let mut doc = Doc::new(format!("{from}:{to}:{}", relationship.as_str()), &Link { reason, created_at: Utc::now() })?;
        doc.memory_id = Some(from.to_string());
        doc.other_id = Some(to.to_string());
        doc.kind = Some(relationship.as_str().into());
        self.upsert_docs(RELATIONSHIPS, vec![doc]).await
    }

    async fn relationships(&self, id: Option<Uuid>) -> Result<Vec<(Uuid, Uuid, Relationship, String)>> {
        let filter = id.map(|id| {
            let id = lit(&id.to_string());
            format!("memory_id = {id} OR other_id = {id}")
        });
        let corrupt = |e: String| err(MemoryStoreError::CorruptData(e));
        self.docs(RELATIONSHIPS, filter)
            .await?
            .into_iter()
            .map(|d| {
                let reason = serde_json::from_str::<serde_json::Value>(&d.doc).map_err(|e| corrupt(e.to_string()))?["reason"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                Ok((
                    Uuid::parse_str(d.memory_id.as_deref().unwrap_or("")).map_err(|e| corrupt(e.to_string()))?,
                    Uuid::parse_str(d.other_id.as_deref().unwrap_or("")).map_err(|e| corrupt(e.to_string()))?,
                    d.kind.as_deref().unwrap_or("").parse().map_err(|e: anyhow::Error| corrupt(e.to_string()))?,
                    reason,
                ))
            })
            .collect()
    }

    async fn record(&self, event: &Event) -> Result<()> {
        let mut doc = Doc::new(event.id, event)?;
        doc.memory_id = event.memory_id.map(|m| m.to_string());
        doc.run_id = event.run_id.map(|r| r.to_string());
        doc.kind = Some(event.kind.clone());
        self.add_docs(EVENTS, vec![doc]).await
    }

    async fn events(&self, id: Option<Uuid>, limit: usize) -> Result<Vec<Event>> {
        let filter = id.map(|id| format!("memory_id = {}", lit(&id.to_string())));
        let docs = self.docs(EVENTS, filter).await?;
        docs.iter().rev().take(limit).map(Doc::parse).collect()
    }

    async fn last_event(&self, kind: &str) -> Result<Option<Event>> {
        let docs = self.docs(EVENTS, Some(format!("kind = {}", lit(kind)))).await?;
        docs.last().map(Doc::parse).transpose()
    }

    async fn record_usage(&self, run: Uuid, ids: &[Uuid], injected: bool) -> Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let now = Utc::now();
        let mut docs = Vec::new();
        for id in ids {
            let mut doc = Doc::new(Uuid::new_v4(), &serde_json::json!({ "retrieved_at": now, "helpful": null }))?;
            doc.memory_id = Some(id.to_string());
            doc.run_id = Some(run.to_string());
            doc.kind = Some(if injected { "injected" } else { "retrieved" }.into());
            docs.push(doc);
        }
        self.add_docs(USAGE, docs).await?;
        // Last access moves forward.
        let _w = self.write.lock().await;
        let table = self.memories.read().await;
        table
            .update()
            .only_if(format!("id IN ({})", Self::id_list(ids)))
            .column("last_accessed_at", format!("arrow_cast({}, 'Timestamp(Microsecond, Some(\"UTC\"))')", micros(now)))
            .execute()
            .await
            .map_err(err)?;
        Ok(())
    }

    async fn set_helpful(&self, run: Uuid, helpful: bool) -> Result<Vec<Uuid>> {
        let _w = self.write.lock().await;
        let mut docs = self.docs(USAGE, Some(format!("run_id = {} AND kind = 'injected'", lit(&run.to_string())))).await?;
        let mut ids = Vec::new();
        for d in &mut docs {
            let mut v: serde_json::Value = d.parse()?;
            v["helpful"] = serde_json::Value::Bool(helpful);
            d.doc = v.to_string();
            if let Some(id) = d.memory_id.as_deref().and_then(|m| Uuid::parse_str(m).ok()) {
                ids.push(id);
            }
        }
        self.upsert_docs(USAGE, docs).await?;
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    async fn usage(&self) -> Result<HashMap<Uuid, Usage>> {
        let mut out: HashMap<Uuid, Usage> = HashMap::new();
        for d in self.docs(USAGE, None).await? {
            let Some(id) = d.memory_id.as_deref().and_then(|m| Uuid::parse_str(m).ok()) else { continue };
            let u = out.entry(id).or_default();
            if d.kind.as_deref() == Some("injected") {
                u.injected += 1;
            }
            match d.parse::<serde_json::Value>()?["helpful"].as_bool() {
                Some(true) => u.helpful += 1,
                Some(false) => u.unhelpful += 1,
                None => {}
            }
        }
        Ok(out)
    }

    async fn add_episode(&self, episode: &Episode) -> Result<()> {
        let mut doc = Doc::new(episode.id, episode)?;
        doc.memory_id = Some(episode.id.to_string());
        doc.run_id = episode.source_run_id.map(|r| r.to_string());
        self.add_docs(EPISODES, vec![doc]).await
    }

    async fn episodes(&self, limit: usize) -> Result<Vec<Episode>> {
        let mut out: Vec<Episode> = self.docs(EPISODES, None).await?.iter().map(Doc::parse).collect::<Result<_>>()?;
        out.sort_by_key(|e| std::cmp::Reverse(e.ended_at));
        out.truncate(limit);
        Ok(out)
    }

    async fn add_proposal(&self, proposal: &Proposal) -> Result<()> {
        let mut doc = Doc::new(proposal.id, proposal)?;
        doc.kind = Some(proposal.status.as_str().into());
        self.add_docs(PROPOSALS, vec![doc]).await
    }

    async fn pending_proposals(&self) -> Result<Vec<Proposal>> {
        let mut out: Vec<Proposal> =
            self.docs(PROPOSALS, Some("kind = 'pending'".into())).await?.iter().map(Doc::parse).collect::<Result<_>>()?;
        out.sort_by_key(|p| std::cmp::Reverse(p.created_at));
        Ok(out)
    }

    async fn set_proposal_status(&self, id: Uuid, status: ProposalStatus) -> Result<()> {
        let _w = self.write.lock().await;
        let mut docs = self.docs(PROPOSALS, Some(format!("id = {}", lit(&id.to_string())))).await?;
        let Some(doc) = docs.first_mut() else { bail!("no proposal with id {id}") };
        let mut p: Proposal = doc.parse()?;
        p.status = status;
        doc.doc = serde_json::to_string(&p)?;
        doc.kind = Some(status.as_str().into());
        self.upsert_docs(PROPOSALS, docs).await
    }

    async fn maintain(&self, vector_index_threshold: usize) -> Result<Vec<String>> {
        let _w = self.write.lock().await;
        let mut notes = Vec::new();
        let table = self.memories.read().await;
        // Compaction folds new rows into the FTS index and merges small files.
        table.optimize(OptimizeAction::All).await.map_err(err)?;
        for t in self.docs.values() {
            t.optimize(OptimizeAction::All).await.map_err(err)?;
        }
        // L15: exact search until the collection is large enough to need an index.
        let with_vectors = table.count_rows(Some("embedding IS NOT NULL".into())).await.map_err(err)?;
        let has_vector_index = table.list_indices().await.map_err(err)?.iter().any(|i| i.columns.iter().any(|c| c == "embedding"));
        if vector_index_threshold > 0 && with_vectors >= vector_index_threshold.max(256) && !has_vector_index {
            table.create_index(&["embedding"], Index::Auto).execute().await.map_err(err)?;
            notes.push(format!("built a vector index over {with_vectors} memories"));
        }
        Ok(notes)
    }

    async fn size_bytes(&self) -> Result<Option<u64>> {
        Ok(Some(dir_size(&self.path)))
    }

    async fn backup(&self, dest: &Path) -> Result<()> {
        if dest.exists() {
            bail!("{} already exists", dest.display());
        }
        // Writes wait while the directory is copied, so the copy is consistent.
        let _w = self.write.lock().await;
        let _m = self.memories.write().await;
        copy_dir(&self.path, dest).map_err(|e| anyhow!("backing up to {}: {e}", dest.display()))
    }
}

/// Put a backup in place of the store at `path` (lyra must not have it open).
/// The current directory is kept next to it, renamed, in case it's needed.
pub fn restore(backup: &Path, path: &Path) -> Result<PathBuf> {
    if !backup.join(format!("{META}.lance")).exists() {
        bail!("{} isn't a memory backup", backup.display());
    }
    let kept = path.with_file_name(format!(
        "{}.before-restore-{}",
        path.file_name().map_or("lance".into(), |n| n.to_string_lossy().to_string()),
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    ));
    if path.exists() {
        std::fs::rename(path, &kept)?;
    }
    copy_dir(backup, path)?;
    Ok(kept)
}

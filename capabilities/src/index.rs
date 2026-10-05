//! The discovery index (C3, C9): every capability's searchable text in
//! LanceDB, with a full-text index and, when an embedding model is set, its
//! vector. Kept between runs, so only new or changed capabilities are
//! embedded again.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use arrow_array::types::Float32Type;
use arrow_array::{Array, FixedSizeListArray, Float32Array, RecordBatch, RecordBatchIterator, StringArray};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use futures::TryStreamExt;
use lancedb::index::Index;
use lancedb::index::scalar::{FtsIndexBuilder, FullTextSearchQuery};
use lancedb::query::{ExecutableQuery, QueryBase, Select};
use lancedb::{Connection, DistanceType, Table};
use tokio::sync::Mutex;

const TABLE: &str = "capabilities";

fn schema(dimensions: usize) -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("text", DataType::Utf8, false),
        Field::new("hash", DataType::Utf8, false),
        Field::new("model", DataType::Utf8, true),
        Field::new(
            "embedding",
            DataType::FixedSizeList(Arc::new(Field::new("item", DataType::Float32, true)), dimensions.max(1) as i32),
            true,
        ),
    ]))
}

/// A stable hash of a capability's text (to notice changes).
fn hash(text: &str) -> String {
    format!("{:016x}", text.bytes().fold(1469598103934665603u64, |h, b| (h ^ b as u64).wrapping_mul(1099511628211)))
}

fn lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

pub struct CapabilityIndex {
    db: Connection,
    table: Mutex<Table>,
    dimensions: Mutex<usize>,
}

/// One row: id, text, hash, model and vector.
type Row = (String, String, String, Option<String>, Option<Vec<f32>>);

async fn collect(q: impl std::future::Future<Output = lancedb::Result<lancedb::arrow::SendableRecordBatchStream>>) -> Result<Vec<RecordBatch>> {
    Ok(q.await?.try_collect::<Vec<_>>().await?)
}

fn rows_from(batches: &[RecordBatch]) -> Vec<Row> {
    let mut out = Vec::new();
    for b in batches {
        let s = |n: &str| b.column_by_name(n).and_then(|c| c.as_any().downcast_ref::<StringArray>().cloned());
        let (Some(id), Some(text), Some(hash), Some(model)) = (s("id"), s("text"), s("hash"), s("model")) else { continue };
        let vectors = b.column_by_name("embedding").and_then(|c| c.as_any().downcast_ref::<FixedSizeListArray>().cloned());
        for i in 0..b.num_rows() {
            let vector = vectors.as_ref().filter(|v| !v.is_null(i)).and_then(|v| {
                v.value(i).as_any().downcast_ref::<Float32Array>().map(|f| f.values().to_vec())
            });
            out.push((
                id.value(i).to_string(),
                text.value(i).to_string(),
                hash.value(i).to_string(),
                (!model.is_null(i)).then(|| model.value(i).to_string()),
                vector,
            ));
        }
    }
    out
}

fn batch(schema: &SchemaRef, rows: &[Row], dimensions: usize) -> Result<RecordBatch> {
    let vectors = FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
        rows.iter().map(|r| r.4.clone().filter(|v| v.len() == dimensions).map(|v| v.into_iter().map(Some).collect::<Vec<_>>())),
        dimensions.max(1) as i32,
    );
    let model: Vec<Option<String>> = rows.iter().map(|r| r.4.as_ref().filter(|v| v.len() == dimensions).and(r.3.clone())).collect();
    Ok(RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from(rows.iter().map(|r| r.0.clone()).collect::<Vec<_>>())),
            Arc::new(StringArray::from(rows.iter().map(|r| r.1.clone()).collect::<Vec<_>>())),
            Arc::new(StringArray::from(rows.iter().map(|r| r.2.clone()).collect::<Vec<_>>())),
            Arc::new(StringArray::from(model)),
            Arc::new(vectors),
        ],
    )?)
}

impl CapabilityIndex {
    pub async fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)?;
        let db = lancedb::connect(dir.to_str().unwrap_or(".")).execute().await?;
        let names = db.table_names().execute().await?;
        let table = if names.iter().any(|n| n == TABLE) {
            db.open_table(TABLE).execute().await?
        } else {
            db.create_empty_table(TABLE, schema(1)).execute().await?
        };
        let dimensions = match table.schema().await?.field_with_name("embedding")?.data_type() {
            DataType::FixedSizeList(_, n) => *n as usize,
            _ => 1,
        };
        Ok(Self { db, table: Mutex::new(table), dimensions: Mutex::new(dimensions) })
    }

    /// Make the index match `capabilities` (`(id, text)`): add new ones,
    /// refresh changed ones, drop removed ones. Returns the ids whose vectors
    /// are missing for `model` (to embed).
    pub async fn sync(&self, capabilities: &[(String, String)], model: Option<(&str, usize)>) -> Result<Vec<(String, String)>> {
        let mut table = self.table.lock().await;
        let mut dims = self.dimensions.lock().await;
        // A different vector size: start over (the index is only a cache).
        if let Some((_, d)) = model
            && d != *dims
        {
            self.db.drop_table(TABLE, &[]).await?;
            *table = self.db.create_empty_table(TABLE, schema(d)).execute().await?;
            *dims = d;
        }
        let existing = rows_from(&collect(table.query().execute()).await?);
        let schema = table.schema().await?;
        let wanted: std::collections::HashMap<&str, &str> = capabilities.iter().map(|(i, t)| (i.as_str(), t.as_str())).collect();
        let stale: Vec<String> = existing.iter().filter(|r| !wanted.contains_key(r.0.as_str())).map(|r| lit(&r.0)).collect();
        if !stale.is_empty() {
            table.delete(&format!("id IN ({})", stale.join(", "))).await?;
        }
        let mut changed: Vec<Row> = Vec::new();
        for (id, text) in capabilities {
            let h = hash(text);
            match existing.iter().find(|r| &r.0 == id) {
                Some(r) if r.2 == h => {}
                _ => changed.push((id.clone(), text.clone(), h, None, None)),
            }
        }
        if !changed.is_empty() {
            let mut m = table.merge_insert(&["id"]);
            m.when_matched_update_all(None).when_not_matched_insert_all();
            let b = batch(&schema, &changed, *dims)?;
            m.execute(Box::new(RecordBatchIterator::new(vec![Ok(b)], schema.clone()))).await?;
        }
        let rows = rows_from(&collect(table.query().execute()).await?);
        if !rows.is_empty() && !table.list_indices().await?.iter().any(|i| i.columns.iter().any(|c| c == "text")) {
            table.create_index(&["text"], Index::FTS(FtsIndexBuilder::default())).execute().await?;
        }
        Ok(match model {
            Some((name, _)) => rows.into_iter().filter(|r| r.3.as_deref() != Some(name) || r.4.is_none()).map(|r| (r.0, r.1)).collect(),
            None => Vec::new(),
        })
    }

    /// Store vectors made by `model`.
    pub async fn set_vectors(&self, model: &str, vectors: &[(String, Vec<f32>)]) -> Result<()> {
        if vectors.is_empty() {
            return Ok(());
        }
        let table = self.table.lock().await;
        let dims = *self.dimensions.lock().await;
        let ids: Vec<String> = vectors.iter().map(|(i, _)| lit(i)).collect();
        let current = rows_from(&collect(table.query().only_if(format!("id IN ({})", ids.join(", "))).execute()).await?);
        let rows: Vec<Row> = current
            .into_iter()
            .filter_map(|r| {
                let v = vectors.iter().find(|(i, _)| *i == r.0)?.1.clone();
                Some((r.0, r.1, r.2, Some(model.to_string()), Some(v)))
            })
            .collect();
        let schema = table.schema().await?;
        let mut m = table.merge_insert(&["id"]);
        m.when_matched_update_all(None).when_not_matched_insert_all();
        let b = batch(&schema, &rows, dims)?;
        m.execute(Box::new(RecordBatchIterator::new(vec![Ok(b)], schema.clone()))).await?;
        Ok(())
    }

    /// Full-text matches, `(id, score)`, best first.
    pub async fn search_text(&self, query: &str, limit: usize) -> Result<Vec<(String, f32)>> {
        let words = lyra_memory::text::content_words(query);
        let table = self.table.lock().await;
        if words.is_empty() || !table.list_indices().await?.iter().any(|i| i.columns.iter().any(|c| c == "text")) {
            return Ok(Vec::new());
        }
        let q = table.query().full_text_search(FullTextSearchQuery::new(words.join(" "))).select(Select::columns(&["id"])).limit(limit);
        let mut out = Vec::new();
        for b in collect(q.execute()).await? {
            let ids = b.column_by_name("id").and_then(|c| c.as_any().downcast_ref::<StringArray>().cloned());
            let scores = b.column_by_name("_score").and_then(|c| c.as_any().downcast_ref::<Float32Array>().cloned());
            if let (Some(ids), Some(scores)) = (ids, scores) {
                out.extend((0..b.num_rows()).map(|i| (ids.value(i).to_string(), scores.value(i))));
            }
        }
        Ok(out)
    }

    /// Nearest by meaning, `(id, cosine similarity)`, best first.
    pub async fn search_vector(&self, model: &str, vector: &[f32], limit: usize) -> Result<Vec<(String, f32)>> {
        if vector.len() != *self.dimensions.lock().await {
            return Ok(Vec::new());
        }
        let table = self.table.lock().await;
        let q = table
            .query()
            .nearest_to(vector)?
            .distance_type(DistanceType::Cosine)
            .only_if(format!("model = {}", lit(model)))
            .select(Select::columns(&["id"]))
            .limit(limit);
        let mut out = Vec::new();
        for b in collect(q.execute()).await? {
            let ids = b.column_by_name("id").and_then(|c| c.as_any().downcast_ref::<StringArray>().cloned());
            let dist = b.column_by_name("_distance").and_then(|c| c.as_any().downcast_ref::<Float32Array>().cloned());
            if let (Some(ids), Some(dist)) = (ids, dist) {
                out.extend((0..b.num_rows()).map(|i| (ids.value(i).to_string(), 1.0 - dist.value(i))));
            }
        }
        Ok(out)
    }
}

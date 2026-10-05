//! Embedding generation (L4) lives outside storage: the manager asks an
//! [`EmbeddingProvider`] for vectors and hands them to the store. Any
//! OpenAI-compatible server (llama.cpp, vLLM, Ollama, …) can sit behind it.

use anyhow::Result;

#[async_trait::async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// The model's name; vectors from different models are never mixed (L21).
    fn model(&self) -> &str;
    /// The length of every vector it returns.
    fn dimensions(&self) -> usize;
    /// Vectors for stored text, one per input.
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>>;
    /// A vector for a search query (models may embed queries differently).
    async fn embed_query(&self, text: &str) -> Result<Vec<f32>>;
}

/// A vector is usable: the right length and all values finite.
pub fn check(vector: &[f32], dimensions: usize) -> Result<(), crate::MemoryStoreError> {
    if vector.len() != dimensions {
        return Err(crate::MemoryStoreError::EmbeddingDimensionMismatch { expected: dimensions, got: vector.len() });
    }
    if vector.is_empty() || vector.iter().any(|x| !x.is_finite()) {
        return Err(crate::MemoryStoreError::InvalidQuery("embedding is empty or not finite".into()));
    }
    Ok(())
}

/// A deterministic embedding model for tests and benchmarks: chosen texts get
/// fixed vectors; anything else becomes a hashed bag of its content words, so
/// texts sharing words are close.
pub struct FakeEmbedder {
    pub model: String,
    pub dimensions: usize,
    pub fixed: std::collections::HashMap<String, Vec<f32>>,
}

impl FakeEmbedder {
    pub fn new(model: &str, dimensions: usize) -> Self {
        Self { model: model.into(), dimensions, fixed: Default::default() }
    }

    /// `text` embeds to `vector` (padded or cut to the dimensions).
    pub fn with(mut self, text: &str, vector: &[f32]) -> Self {
        let mut v = vector.to_vec();
        v.resize(self.dimensions, 0.0);
        self.fixed.insert(text.to_string(), v);
        self
    }

    fn vector(&self, text: &str) -> Vec<f32> {
        if let Some(v) = self.fixed.get(text) {
            return v.clone();
        }
        let mut v = vec![0.0f32; self.dimensions];
        for word in crate::text::content_words(text) {
            let h = word.bytes().fold(1469598103934665603u64, |h, b| (h ^ b as u64).wrapping_mul(1099511628211));
            v[(h % self.dimensions as u64) as usize] += 1.0;
        }
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm == 0.0 {
            v[0] = 1.0;
        } else {
            v.iter_mut().for_each(|x| *x /= norm);
        }
        v
    }
}

#[async_trait::async_trait]
impl EmbeddingProvider for FakeEmbedder {
    fn model(&self) -> &str {
        &self.model
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|t| self.vector(t)).collect())
    }

    async fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        Ok(self.vector(text))
    }
}

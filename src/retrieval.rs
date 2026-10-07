//! Clients for the embedding and reranker models (OpenAI-style `/embeddings`,
//! Jina/Cohere-style `/rerank`, as served by vLLM and llama.cpp). The
//! embedding endpoint is memory's [`EmbeddingProvider`]; `check` confirms
//! both are reachable and behaving.

use std::time::Duration;

use serde::Deserialize;

/// An OpenAI-compatible endpoint, as configured under `[embedding]` / `[reranker]`.
#[derive(Clone, Deserialize)]
pub struct Endpoint {
    pub url: String,
    pub model: String,
    /// The embedding model's vector size; asked of the model when not set.
    #[serde(default)]
    pub dimensions: Option<usize>,
}

impl Endpoint {
    fn post<T: for<'de> Deserialize<'de>>(&self, path: &str, body: serde_json::Value) -> Result<T, String> {
        let url = format!("{}/{path}", self.url.trim_end_matches('/'));
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| e.to_string())?;
        let started = std::time::Instant::now();
        let resp = client.post(&url).json(&body).send().map_err(|e| e.to_string())?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("{url}: {status}: {}", resp.text().unwrap_or_default()));
        }
        let reply: serde_json::Value = resp.json().map_err(|e| format!("{url}: {e}"))?;
        // Counted for whoever this thread works for, like the chat model's calls.
        let kind = if path == "rerank" { "reranker" } else { "embedding" };
        crate::usage::record_usage(kind, &self.model, &reply["usage"], started.elapsed().as_millis() as u64);
        serde_json::from_value(reply).map_err(|e| format!("{url}: {e}"))
    }
}

pub struct Embeddings {
    /// One vector per input, in input order.
    pub vectors: Vec<Vec<f32>>,
    #[allow(dead_code)] // for cost tracking once retrieval is used in chat
    pub tokens: u64,
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingData>,
    usage: Option<TokenUsage>,
}

#[derive(Deserialize)]
struct EmbeddingData {
    index: usize,
    embedding: Vec<f32>,
}

#[derive(Deserialize)]
struct TokenUsage {
    prompt_tokens: u64,
}

impl EmbeddingResponse {
    fn into_embeddings(mut self) -> Embeddings {
        self.data.sort_by_key(|d| d.index);
        Embeddings {
            vectors: self.data.into_iter().map(|d| d.embedding).collect(),
            tokens: self.usage.map_or(0, |u| u.prompt_tokens),
        }
    }
}

/// Embed `texts` in one request.
pub fn embed(endpoint: &Endpoint, texts: &[&str]) -> Result<Embeddings, String> {
    let body = serde_json::json!({ "model": endpoint.model, "input": texts });
    let resp: EmbeddingResponse = endpoint.post("embeddings", body)?;
    if resp.data.len() != texts.len() {
        return Err(format!("asked for {} embeddings, got {}", texts.len(), resp.data.len()));
    }
    Ok(resp.into_embeddings())
}

/// Qwen3-Embedding (and similar instruction-tuned embedders) match better when
/// a search query, but not the stored text, says what it's looking for.
const QUERY_INSTRUCTION: &str = "Instruct: Given a user message, retrieve stored memories that are relevant to it\nQuery: ";

/// Instruction for finding capabilities (tools, workflows) for a task.
/// Matching a request to the specialist agent that handles that kind of work.
pub const ROUTING_INSTRUCTION: &str = "Instruct: Given a user request, retrieve examples of requests handled by the same specialist\nQuery: ";

pub const CAPABILITY_INSTRUCTION: &str = "Instruct: Given a task to do, retrieve the tools and procedures that can do it\nQuery: ";

/// Embed a search query with a given retrieval instruction.
pub fn embed_query_with(endpoint: &Endpoint, instruction: &str, query: &str) -> Result<Vec<f32>, String> {
    let text = format!("{instruction}{query}");
    let mut e = embed(endpoint, &[&text])?;
    e.vectors.pop().ok_or_else(|| "no embedding returned".into())
}

/// The embedding endpoint as memory's embedding provider (L4). Calls are
/// blocking HTTP, so they run on tokio's blocking pool.
pub struct EndpointEmbedder {
    endpoint: Endpoint,
    dimensions: usize,
    /// What queries are looking for (memories by default).
    instruction: &'static str,
}

impl EndpointEmbedder {
    /// Ready an endpoint, asking it for its vector size unless configured.
    /// Blocking: call it outside the async runtime.
    pub fn connect(endpoint: Endpoint) -> Result<Self, String> {
        let dimensions = match endpoint.dimensions {
            Some(d) => d,
            None => embed(&endpoint, &["dimension probe"])?.vectors.pop().map_or(0, |v| v.len()),
        };
        if dimensions == 0 {
            return Err(format!("{} returned an empty vector", endpoint.model));
        }
        Ok(Self { endpoint, dimensions, instruction: QUERY_INSTRUCTION })
    }

    /// Queries look for something else (e.g. capabilities).
    pub fn with_instruction(mut self, instruction: &'static str) -> Self {
        self.instruction = instruction;
        self
    }
}

#[async_trait::async_trait]
impl lyra_memory::EmbeddingProvider for EndpointEmbedder {
    fn model(&self) -> &str {
        &self.endpoint.model
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }

    async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        let (endpoint, texts) = (self.endpoint.clone(), texts.to_vec());
        // The blocking pool's threads don't know whose memory this is.
        let user = crate::acting::current();
        tokio::task::spawn_blocking(move || {
            let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
            crate::acting::run(&user, || embed(&endpoint, &refs)).map(|e| e.vectors).map_err(anyhow::Error::msg)
        })
        .await?
    }

    async fn embed_query(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let (endpoint, text, instruction) = (self.endpoint.clone(), text.to_string(), self.instruction);
        let user = crate::acting::current();
        tokio::task::spawn_blocking(move || crate::acting::run(&user, || embed_query_with(&endpoint, instruction, &text)).map_err(anyhow::Error::msg)).await?
    }
}

/// A document's position in the input and its relevance to the query.
pub struct Ranked {
    pub index: usize,
    pub score: f64,
}

#[derive(Deserialize)]
struct RerankResponse {
    results: Vec<RerankResult>,
}

#[derive(Deserialize)]
struct RerankResult {
    index: usize,
    relevance_score: f64,
}

/// Score `documents` against `query`, most relevant first.
pub fn rerank(endpoint: &Endpoint, query: &str, documents: &[&str]) -> Result<Vec<Ranked>, String> {
    let body = serde_json::json!({ "model": endpoint.model, "query": query, "documents": documents });
    let resp: RerankResponse = endpoint.post("rerank", body)?;
    let mut ranked: Vec<Ranked> = resp
        .results
        .into_iter()
        .map(|r| Ranked { index: r.index, score: r.relevance_score })
        .collect();
    ranked.sort_by(|a, b| b.score.total_cmp(&a.score));
    Ok(ranked)
}

/// Exercise both models with a tiny request and describe the result for the UI.
/// Returns one line per configured model, `Err` for one that failed.
pub fn check(embedding: Option<&Endpoint>, reranker: Option<&Endpoint>) -> Vec<Result<String, String>> {
    let mut lines = Vec::new();
    if let Some(endpoint) = embedding {
        lines.push(match embed(endpoint, &["ping"]) {
            Ok(e) => Ok(format!("embedding ready · {} · {} dims", endpoint.model, e.vectors[0].len())),
            Err(e) => Err(format!("embedding unavailable: {e}")),
        });
    }
    if let Some(endpoint) = reranker {
        let documents = ["Bananas are yellow.", "Rust is a systems programming language."];
        lines.push(match rerank(endpoint, "What is Rust?", &documents) {
            Ok(r) if r.first().is_some_and(|top| top.index == 1) => {
                Ok(format!("reranker ready · {}", endpoint.model))
            }
            Ok(_) => Err("reranker responded but ranked a test query wrongly".into()),
            Err(e) => Err(format!("reranker unavailable: {e}")),
        });
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embeddings_are_returned_in_input_order() {
        // Shape of a vLLM response, with `data` deliberately out of order.
        let resp: EmbeddingResponse = serde_json::from_str(
            r#"{"object":"list","model":"embedding",
                "data":[{"index":1,"object":"embedding","embedding":[0.5,0.6]},
                        {"index":0,"object":"embedding","embedding":[0.1,0.2]}],
                "usage":{"prompt_tokens":5,"total_tokens":5,"completion_tokens":0,"prompt_tokens_details":null}}"#,
        )
        .unwrap();
        let e = resp.into_embeddings();
        assert_eq!(e.vectors, [vec![0.1, 0.2], vec![0.5, 0.6]]);
        assert_eq!(e.tokens, 5);
    }

    #[test]
    fn rerank_response_parses() {
        // Trimmed from a real vLLM `/v1/rerank` response.
        let resp: RerankResponse = serde_json::from_str(
            r#"{"id":"score-1","model":"reranker","usage":{"prompt_tokens":244,"total_tokens":244},
                "results":[{"index":0,"document":{"text":"Rust is...","multi_modal":null},"relevance_score":0.93},
                           {"index":1,"document":{"text":"Bananas...","multi_modal":null},"relevance_score":1.6e-6}]}"#,
        )
        .unwrap();
        assert_eq!(resp.results.len(), 2);
        assert_eq!(resp.results[0].index, 0);
        assert!(resp.results[0].relevance_score > resp.results[1].relevance_score);
    }
}

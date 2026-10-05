//! Clients for the embedding and reranker models (OpenAI-style `/embeddings`,
//! Jina/Cohere-style `/rerank`, as served by vLLM and llama.cpp). Not used by
//! the chat yet; `check` confirms they're reachable and behaving.

use std::time::Duration;

use serde::Deserialize;

/// An OpenAI-compatible endpoint, as configured under `[embedding]` / `[reranker]`.
#[derive(Clone, Deserialize)]
pub struct Endpoint {
    pub url: String,
    pub model: String,
}

impl Endpoint {
    fn post<T: for<'de> Deserialize<'de>>(&self, path: &str, body: serde_json::Value) -> Result<T, String> {
        let url = format!("{}/{path}", self.url.trim_end_matches('/'));
        let client = reqwest::blocking::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| e.to_string())?;
        let resp = client.post(&url).json(&body).send().map_err(|e| e.to_string())?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("{url}: {status}: {}", resp.text().unwrap_or_default()));
        }
        resp.json().map_err(|e| format!("{url}: {e}"))
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

/// Embed a search query (with the retrieval instruction).
pub fn embed_query(endpoint: &Endpoint, query: &str) -> Result<Vec<f32>, String> {
    let text = format!("{QUERY_INSTRUCTION}{query}");
    let mut e = embed(endpoint, &[&text])?;
    e.vectors.pop().ok_or_else(|| "no embedding returned".into())
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

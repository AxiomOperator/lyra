//! Web search and reading pages: `web_search` asks a SearXNG instance
//! (`[search] searxng_url`), `web_fetch` reads a page as plain text. Both
//! only read, so the main agent may use them; replies cite the pages used.

use std::time::Duration;

use lyra_capabilities::{Capability, CapabilityKind, RiskLevel};
use serde::Deserialize;
use serde_json::{Value, json};

/// `[search]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    /// A SearXNG instance with the JSON format turned on.
    pub searxng_url: String,
    pub max_results: usize,
    /// Most characters of a page handed back.
    pub fetch_max_chars: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self { enabled: true, searxng_url: "http://127.0.0.1:8080".into(), max_results: 8, fetch_max_chars: 20_000 }
    }
}

pub fn capabilities() -> Vec<Capability> {
    let tool = |name: &str, description: &str, parameters: Value| {
        let mut c = Capability::new(name, CapabilityKind::NativeTool, description, RiskLevel::ReadOnly);
        c.input_schema = parameters;
        c.source = "web".into();
        c.tags = vec!["web".into(), "search".into(), "internet".into()];
        c.permissions = vec!["web.read".into()];
        c
    };
    vec![
        tool(
            "web_search",
            "Search the web for current information. Returns titles, URLs and snippets; read a page with web_fetch. Cite the pages you rely on as Markdown links.",
            json!({ "type": "object", "properties": {
                "query": { "type": "string", "description": "What to search for." },
                "count": { "type": "integer", "description": "How many results (default 8)." },
            }, "required": ["query"] }),
        ),
        tool(
            "web_fetch",
            "Read a web page (http/https) as plain text, to answer from it. Cite it as a Markdown link.",
            json!({ "type": "object", "properties": {
                "url": { "type": "string", "description": "The page's URL." },
            }, "required": ["url"] }),
        ),
    ]
}

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) lyra")
        .build()
        .map_err(|e| e.to_string())
}

/// Turn SearXNG's JSON into a short list.
pub fn results(body: &Value, count: usize) -> Value {
    let list: Vec<Value> = body["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["url"].as_str().is_some_and(|u| u.starts_with("http")))
        .take(count)
        .map(|r| {
            json!({
                "title": r["title"].as_str().unwrap_or("").trim(),
                "url": r["url"],
                "snippet": r["content"].as_str().unwrap_or("").chars().take(300).collect::<String>(),
            })
        })
        .collect();
    let mut out = json!({ "results": list });
    if list.is_empty() {
        let blocked: Vec<String> = body["unresponsive_engines"].as_array().into_iter().flatten().filter_map(|e| e[0].as_str().map(str::to_string)).collect();
        out["note"] = json!(if blocked.is_empty() { "nothing found".to_string() } else { format!("nothing found (engines not answering: {})", blocked.join(", ")) });
    }
    out
}

fn search(s: &Settings, args: &Value) -> Result<Value, String> {
    let query = args["query"].as_str().filter(|q| !q.trim().is_empty()).ok_or("query is required")?;
    let count = args["count"].as_u64().map_or(s.max_results, |c| c as usize).clamp(1, 20);
    let url = format!("{}/search", s.searxng_url.trim_end_matches('/'));
    let target = reqwest::Url::parse_with_params(&url, &[("q", query), ("format", "json")]).map_err(|e| format!("bad [search] searxng_url: {e}"))?;
    let resp = client()?
        .get(target)
        .send()
        .map_err(|e| format!("search isn't reachable ({url}): {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("search failed: {} (is the JSON format on in SearXNG?)", resp.status()));
    }
    let body: Value = resp.json().map_err(|e| format!("search answered with something other than JSON: {e}"))?;
    Ok(results(&body, count))
}

fn fetch(s: &Settings, args: &Value) -> Result<Value, String> {
    let url = args["url"].as_str().ok_or("url is required")?.trim();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("only http:// and https:// pages".into());
    }
    let resp = client()?.get(url).send().map_err(|e| format!("couldn't load {url}: {e}"))?;
    let status = resp.status();
    let final_url = resp.url().to_string();
    let kind = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    if !status.is_success() {
        return Err(format!("{url} answered {status}"));
    }
    // A few MB at most: pages, not downloads.
    let bytes = resp.bytes().map_err(|e| e.to_string())?;
    if bytes.len() > 5_000_000 {
        return Err(format!("{url} is too big to read ({} bytes)", bytes.len()));
    }
    let body = String::from_utf8_lossy(&bytes);
    let text = if kind.contains("html") || body.trim_start().starts_with('<') {
        crate::text::page_text(&body, s.fetch_max_chars)
    } else if kind.starts_with("text/") || kind.contains("json") || kind.contains("xml") {
        body.chars().take(s.fetch_max_chars).collect()
    } else {
        return Err(format!("{url} isn't a page lyra can read ({kind})"));
    };
    let title = body
        .find("<title")
        .and_then(|i| body[i..].find('>').map(|j| i + j + 1))
        .and_then(|i| body[i..].find("</title>").map(|j| body[i..i + j].trim().to_string()))
        .unwrap_or_default();
    Ok(json!({ "url": final_url, "title": title, "text": text }))
}

pub fn call(s: &Settings, name: &str, args: &Value) -> Result<Value, String> {
    if !s.enabled {
        return Err("web search is off ([search] enabled)".into());
    }
    match name {
        "web_search" => search(s, args),
        "web_fetch" => fetch(s, args),
        other => Err(format!("{other} isn't a web tool")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searxng_results_become_a_short_list() {
        let body = json!({
            "results": [
                { "title": " Fedora Linux ", "url": "https://fedoraproject.org/", "content": "The Fedora Project" },
                { "title": "ad", "url": "javascript:void(0)", "content": "" },
                { "title": "Wiki", "url": "https://en.wikipedia.org/wiki/Fedora_Linux", "content": "Fedora Linux is…" }
            ],
            "unresponsive_engines": []
        });
        let r = results(&body, 5);
        assert_eq!(r["results"].as_array().unwrap().len(), 2, "only real links");
        assert_eq!(r["results"][0]["title"], "Fedora Linux");
        let none = results(&json!({ "results": [], "unresponsive_engines": [["brave", "too many requests"]] }), 5);
        assert!(none["note"].as_str().unwrap().contains("brave"), "says why nothing came back");
    }
}

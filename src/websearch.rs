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
    /// Long pages are condensed (the facts and links that matter) by the
    /// background model before they reach the conversation.
    pub reader: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { enabled: true, searxng_url: "http://127.0.0.1:8080".into(), max_results: 8, fetch_max_chars: 20_000, reader: true }
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
            "Read a web page (http/https), to answer from it. A long page comes back condensed to what matters for `focus` (facts, dates, numbers, links); ask for `full` only when you need the page word for word. Cite it as a Markdown link.",
            json!({ "type": "object", "properties": {
                "url": { "type": "string", "description": "The page's URL." },
                "focus": { "type": "string", "description": "What you're looking for on it, e.g. \"release date, parameter count, license\"." },
                "full": { "type": "boolean", "description": "The whole page as text, not condensed (default false)." },
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
    // A long page: what matters, not the whole thing (the conversation stays small).
    if s.reader && args["full"] != true && condense(text.chars().count(), conversation_size())
        && let Some(short) = read(&final_url, &title, &text, args["focus"].as_str().unwrap_or(""))
    {
        return Ok(json!({ "url": final_url, "title": title, "condensed": short, "note": format!("condensed from {} characters; web_fetch with full: true for the whole page", text.chars().count()) }));
    }
    Ok(json!({ "url": final_url, "title": title, "text": text }))
}

thread_local! {
    /// How big the conversation asking is (characters): small, a page goes as
    /// it is (quick); big, it's condensed (the context stays small).
    static CONVERSATION: std::cell::Cell<usize> = const { std::cell::Cell::new(usize::MAX) };
}

/// This thread's tool calls are for a conversation this big.
pub fn set_conversation_size(chars: usize) {
    CONVERSATION.with(|c| c.set(chars));
}

pub fn conversation_size() -> usize {
    CONVERSATION.with(|c| c.get())
}

/// Below this, pages aren't condensed: a model call per page costs more time
/// than the context it saves (about 30k tokens).
const CONDENSE_FROM: usize = 100_000;

/// Whether a page this long, for a conversation this big, is condensed.
fn condense(page: usize, conversation: usize) -> bool {
    page > READ_OVER && conversation >= CONDENSE_FROM
}

/// Pages longer than this are condensed (shorter ones cost less as they are
/// than a model call to shorten them).
const READ_OVER: usize = 6_000;
/// The longest summary (about 350 words): the fallback writes until it's told to stop.
const READER_TOKENS: u64 = 600;

/// What the reader is told.
const READER: &str = "You condense a web page for a researcher. Keep what it is and who published it, every date (published, announced, released), the facts, \
     numbers, versions, names and short quotes that matter, and the links worth following as [text](url) (take URLs from the page's link list). \
     Drop navigation, ads, cookie notices and boilerplate. Plain Markdown, at most about 300 words: be brief, bullet points are fine. If the page has nothing on what they're looking for, \
     say so in one line. Never add anything that isn't on the page.";

/// The page condensed by the background model (the fallback when it takes
/// background jobs); `None` when that didn't work (the page goes as it is).
fn read(url: &str, title: &str, text: &str, focus: &str) -> Option<String> {
    let (chat_url, model) = crate::learn::chat()?;
    let input = format!("Looking for: {}\nPage: {title}\nURL: {url}\n\n{text}", if focus.trim().is_empty() { "the main points" } else { focus.trim() });
    let (out, _) = crate::learn::complete_light_short(&chat_url, &model, READER, &input, READER_TOKENS).ok()?;
    let out = out.rsplit_once("</think>").map_or(out.as_str(), |(_, a)| a).trim().to_string();
    (!out.is_empty()).then_some(out)
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
    fn pages_are_condensed_only_once_the_conversation_is_big() {
        assert!(!condense(20_000, 30_000), "early on: as it is (quick)");
        assert!(condense(20_000, 150_000), "later: condensed (the context stays small)");
        assert!(!condense(3_000, 150_000), "a short page goes as it is");
        assert!(condense(20_000, usize::MAX), "outside a conversation: condensed");
    }

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

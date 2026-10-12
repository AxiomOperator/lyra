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
    if s.reader && args["full"] != true && condense(text.chars().count(), conversation_size()) {
        match read_within(&final_url, &title, &text, args["focus"].as_str().unwrap_or(""), READ_WITHIN) {
            Some(Some(short)) => {
                return Ok(json!({ "url": final_url, "title": title, "condensed": short, "note": format!("condensed from {} characters; web_fetch with full: true for the whole page", text.chars().count()) }));
            }
            // The models are busy: the start of the page, rather than waiting minutes.
            None => {
                let start: String = text.chars().take(READ_OVER).collect();
                return Ok(json!({ "url": final_url, "title": title, "text": start, "note": format!("the first {READ_OVER} of {} characters (condensing it took too long); web_fetch with full: true for the whole page", text.chars().count()) }));
            }
            Some(None) => {}
        }
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

/// Condensing a page may take this long; past it the start of the page goes
/// instead (the models are busy, and a reply shouldn't wait minutes per page).
const READ_WITHIN: std::time::Duration = std::time::Duration::from_secs(45);

/// `read`, within `limit`: `None` when it took longer (it's left to finish on
/// its own), `Some(None)` when it didn't work.
fn read_within(url: &str, title: &str, text: &str, focus: &str, limit: std::time::Duration) -> Option<Option<String>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let (url, title, text, focus) = (url.to_string(), title.to_string(), text.to_string(), focus.to_string());
    crate::acting::spawn(move || {
        let _ = tx.send(read(&url, &title, &text, &focus));
    });
    rx.recv_timeout(limit).ok()
}

/// The page condensed by the background model (the fallback when it takes
/// background jobs); `None` when that didn't work (the page goes as it is).
fn read(url: &str, title: &str, text: &str, focus: &str) -> Option<String> {
    let (chat_url, model) = crate::learn::chat()?;
    let input = format!("Looking for: {}\nPage: {title}\nURL: {url}\n\n{text}", if focus.trim().is_empty() { "the main points" } else { focus.trim() });
    let (out, _) = crate::learn::complete_light_short(&chat_url, &model, READER, &input, READER_TOKENS).ok()?;
    let out = out.rsplit_once("</think>").map_or(out.as_str(), |(_, a)| a).trim().to_string();
    (!out.is_empty()).then_some(out)
}

static CONFIGURED: std::sync::RwLock<Option<Settings>> = std::sync::RwLock::new(None);

/// `[search]` as configured, for work outside a chat (web watches).
pub fn configure(s: Settings) {
    *CONFIGURED.write().unwrap_or_else(|e| e.into_inner()) = Some(s);
}

pub fn configured() -> Settings {
    CONFIGURED.read().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}

/// A page's whole text and title, as it is (never condensed: watches compare it).
pub fn page(url: &str) -> Result<(String, String), String> {
    let v = call(&configured(), "web_fetch", &json!({ "url": url, "full": true }))?;
    Ok((v["title"].as_str().unwrap_or("").to_string(), v["text"].as_str().unwrap_or("").to_string()))
}

/// A search's results: (title, url).
pub fn found(query: &str) -> Result<Vec<(String, String)>, String> {
    let v = call(&configured(), "web_search", &json!({ "query": query, "count": 10 }))?;
    Ok(v["results"].as_array().into_iter().flatten().filter_map(|r| Some((r["title"].as_str()?.to_string(), r["url"].as_str()?.to_string()))).collect())
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

/// Searches in one reply (the tool loop's), so it doesn't search the same
/// thing over and over: a search nearly the same as one made already isn't
/// run again (the earlier results come back, with a nudge to open one), and
/// after `MAX_PER_REPLY` searches the reply works from what it found.
#[derive(Default)]
pub struct Searches {
    /// Each search's words and its results (empty while it runs).
    done: Vec<(Vec<String>, String, String)>,
}

/// Searches one reply may make.
pub const MAX_PER_REPLY: usize = 8;

fn query_words(arguments: &str) -> (String, Vec<String>) {
    let query = serde_json::from_str::<Value>(arguments).ok().and_then(|v| v["query"].as_str().map(str::to_string)).unwrap_or_default();
    let mut words: Vec<String> = query.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() > 2).map(str::to_string).collect();
    words.sort();
    words.dedup();
    (query, words)
}

impl Searches {
    /// The result to give instead of searching (a near-repeat, or past the
    /// budget); `None`: go ahead (it's counted from now).
    pub fn check(&mut self, arguments: &str) -> Option<String> {
        let (query, words) = query_words(arguments);
        let like = |w: &Vec<String>| {
            let shared = words.iter().filter(|x| w.contains(x)).count();
            let union = words.len() + w.len() - shared;
            union > 0 && shared as f32 / union as f32 >= 0.6
        };
        if let Some((_, earlier, results)) = self.done.iter().find(|(w, ..)| like(w)) {
            let results: Value = serde_json::from_str(results).unwrap_or(Value::Null);
            return Some(
                json!({
                    "note": format!("You searched for nearly this already (\"{earlier}\"), so it wasn't searched again: these are those results. Open the most useful with web_fetch, or search for something different."),
                    "results": results.get("results").cloned().unwrap_or(Value::Array(vec![])),
                })
                .to_string(),
            );
        }
        if self.done.len() >= MAX_PER_REPLY {
            return Some(json!({ "note": format!("That's {MAX_PER_REPLY} searches in this reply, the most for one: work from what you found. Open the best results with web_fetch, then answer (and say what you couldn't find).") }).to_string());
        }
        self.done.push((words, query, String::new()));
        None
    }

    /// A search's results, once it ran.
    pub fn record(&mut self, arguments: &str, results: &str) {
        let (_, words) = query_words(arguments);
        if let Some(d) = self.done.iter_mut().find(|(w, ..)| *w == words) {
            d.2 = results.to_string();
        }
    }
}

#[cfg(test)]
mod searches_tests {
    use super::*;

    #[test]
    fn a_page_waits_only_so_long_for_its_summary() {
        // No chat model set up: condensing doesn't work, and says so at once.
        let started = std::time::Instant::now();
        let got = read_within("https://x", "X", &"words ".repeat(2000), "", std::time::Duration::from_millis(200));
        assert!(matches!(got, Some(None) | None) && started.elapsed() < std::time::Duration::from_secs(2));
        // A model that takes the request and says nothing: it's not waited for.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let held: Vec<_> = listener.incoming().take(3).collect();
            std::thread::sleep(std::time::Duration::from_secs(5));
            drop(held);
        });
        crate::learn::set_chat(&url, "slow");
        let started = std::time::Instant::now();
        assert_eq!(read_within("https://x", "X", &"words ".repeat(2000), "", std::time::Duration::from_millis(300)), None, "past the limit");
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn a_reply_doesnt_search_the_same_thing_twice_or_endlessly() {
        let mut s = Searches::default();
        let q = |t: &str| json!({ "query": t }).to_string();
        assert!(s.check(&q("Claude 4 release news Oct 2026 Anthropic blog news")).is_none());
        s.record(&q("Claude 4 release news Oct 2026 Anthropic blog news"), r#"{"results":[{"title":"Claude","url":"https://claude.com/"}]}"#);
        // The same, reworded: not searched again; the earlier results come back.
        let again = s.check(&q("Claude 4 news release Oct 10 2026 Anthropic announcement blog")).expect("a near-repeat");
        let v: Value = serde_json::from_str(&again).unwrap();
        assert!(v["note"].as_str().unwrap().contains("web_fetch"));
        assert_eq!(v["results"][0]["url"], "https://claude.com/");
        assert!(s.check(&q("OpenAI GPT-5 release date news Oct 2026 OpenAI blog news")).is_none(), "something different");
        assert!(s.check(&q("OpenAI GPT-5 announcement news Oct 2026 OpenAI newsroom blog")).is_some(), "and its rewording");
        // The budget.
        for t in ["vLLM release", "SGLang release", "MCP SDK release", "arXiv reasoning papers", "CrewAI framework", "DeepSeek model card"] {
            assert!(s.check(&q(t)).is_none(), "{t}");
        }
        let over = s.check(&q("Gemini 3 news")).expect("past the budget");
        assert!(over.contains("8 searches in this reply"));
    }
}

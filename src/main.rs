mod config;
mod context;
mod learn;
mod retrieval;
mod stats;
mod tools;
mod ui;

use std::io::{BufRead, BufReader};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use config::Config;
use context::Context;
use learn::{Learning, Outcome, Review, SkillsSnapshot};
use lyra_learning::{Mode, SkillStatus, evaluator};
use retrieval::Endpoint;
use stats::{Pricing, Stats, Totals, Usage, secs};
use tools::{MemorySnapshot, Tools};

/// Cap on model → tools → model round trips in one turn.
const MAX_TOOL_ROUNDS: usize = 8;

#[derive(Serialize)]
struct Message {
    role: String,
    content: String,
    /// Tools an assistant message asked to run.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<ToolCall>,
    /// For `role: "tool"`: the call this is the result of.
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    /// The model's thinking; shown in the UI but never sent back to the model.
    #[serde(skip)]
    reasoning: String,
    /// Timing and token counts for an assistant reply.
    #[serde(skip)]
    stats: Option<Stats>,
    /// Learned skills that were in the prompt for this reply.
    #[serde(skip)]
    skills: Vec<String>,
}

impl Message {
    fn new(role: &str, content: String) -> Self {
        Self {
            role: role.into(),
            content,
            tool_calls: Vec::new(),
            tool_call_id: None,
            reasoning: String::new(),
            stats: None,
            skills: Vec::new(),
        }
    }

    /// Whether the model sees this message (info and error lines are UI-only).
    fn is_history(&self) -> bool {
        matches!(self.role.as_str(), "user" | "assistant" | "tool")
    }
}

/// A complete tool call, as sent back to the model in the conversation history.
#[derive(Clone, Default, Serialize)]
struct ToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: FunctionCall,
}

#[derive(Clone, Default, Serialize)]
struct FunctionCall {
    name: String,
    /// JSON text, streamed in pieces.
    arguments: String,
}

/// A streamed fragment of a tool call; fragments with the same `index` are joined.
#[derive(Deserialize)]
struct ToolCallDelta {
    index: usize,
    id: Option<String>,
    function: Option<FunctionDelta>,
}

#[derive(Deserialize)]
struct FunctionDelta {
    name: Option<String>,
    arguments: Option<String>,
}

/// One SSE chunk from a streaming `/chat/completions` response.
#[derive(Deserialize)]
struct Chunk {
    #[serde(default)]
    choices: Vec<ChunkChoice>,
    /// Sent in the final chunk when `stream_options.include_usage` is set.
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct ChunkChoice {
    #[serde(default)]
    delta: Delta,
}

#[derive(Default, Deserialize)]
struct Delta {
    content: Option<String>,
    /// llama.cpp/DeepSeek use `reasoning_content`; some servers use `reasoning`.
    #[serde(alias = "reasoning")]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolCallDelta>,
}

/// What one streamed response produced.
struct Round {
    stats: Stats,
    content: String,
    tool_calls: Vec<ToolCall>,
}

enum StreamEvent {
    Token(String),
    Reasoning(String),
    /// The model asked to run these tools.
    ToolCalls(Vec<ToolCall>),
    /// A tool finished; `content` is what the model will see.
    ToolResult { id: String, name: String, content: String },
    Done(Stats),
    Error(String),
    /// Progress note from the worker for the activity log.
    Log(String),
    /// Health check results for the embedding/reranker models.
    Models(Vec<Result<String, String>>),
    /// Fresh numbers for the memory panel.
    Memory(Result<MemorySnapshot, String>),
    /// A finished learning review.
    Reviewed(Review),
    /// Skills added to the system prompt for the message being answered,
    /// with their approximate size in tokens.
    SkillsApplied { names: Vec<String>, tokens: u64 },
    /// Fresh contents for the skills panel.
    Skills(Result<SkillsSnapshot, String>),
}

/// What `main` opened before the UI starts.
struct Services {
    tools: Option<Arc<Tools>>,
    /// Where memory lives, or why it's off.
    memory_status: Result<String, String>,
    learning: Option<Arc<Learning>>,
    /// Where skills live, or why learning is off.
    learning_status: Result<String, String>,
}

/// What the assistant is doing right now, for the session panel.
#[derive(Clone, PartialEq)]
enum Phase {
    Idle,
    /// Request sent, nothing back yet.
    Waiting,
    Thinking,
    Streaming,
    /// Running these tools.
    Tools(String),
}

#[derive(Clone, Copy, PartialEq)]
enum Level {
    Info,
    Tool,
    Learn,
    Error,
}

/// One line in the activity log.
struct Activity {
    time: String,
    level: Level,
    text: String,
}

const MAX_ACTIVITY: usize = 500;

struct App {
    base_url: String,
    model: String,
    /// Built from SOUL.md / AGENT.md / USER.md; sent first with every request.
    system_prompt: Option<String>,
    pricing: Pricing,
    /// Not used by the chat yet; checked at startup and on reload.
    embedding: Option<Endpoint>,
    reranker: Option<Endpoint>,
    /// Memory tools; `None` when disabled or the database couldn't be opened.
    tools: Option<Arc<Tools>>,
    totals: Totals,
    /// When the in-flight request was sent.
    started: Option<Instant>,
    /// When the current phase began, for live timers.
    phase_since: Instant,
    phase: Phase,
    messages: Vec<Message>,
    input: String,
    waiting: bool,
    show_reasoning: bool,
    /// Side panels (Ctrl-B); hidden automatically on narrow terminals.
    show_panels: bool,
    activity: Vec<Activity>,
    /// Context files loaded, as `(name, path)`.
    context_files: Vec<(&'static str, String)>,
    /// `None` while a health check is running.
    model_status: Option<Vec<Result<String, String>>>,
    /// Where memory lives, or why it's off.
    memory_status: Result<String, String>,
    memory: Option<Result<MemorySnapshot, String>>,
    learning: Option<Arc<Learning>>,
    learning_status: Result<String, String>,
    skills: Option<Result<SkillsSnapshot, String>>,
    /// A learning review is running in the background.
    reviewing: bool,
    /// Skills applied to the message being answered (or last answered).
    applied_skills: Vec<String>,
    /// Their approximate size in the prompt.
    applied_skills_tokens: u64,
    /// Approximate size of the tool definitions sent with every request.
    tools_tokens: u64,
    /// Top line of the chat view when scrolled up; `None` follows the bottom.
    scroll: Option<u16>,
    /// Chat view size from the last frame, used for scroll bounds.
    max_scroll: u16,
    page: u16,
    tx: Sender<StreamEvent>,
    rx: Receiver<StreamEvent>,
}

impl App {
    fn new(config: Config, context: Context, services: Services) -> Self {
        let Services { tools, memory_status, learning, learning_status } = services;
        let tools_tokens =
            tools.as_ref().map_or(0, |t| learn::approx_tokens(&t.definitions().to_string()));
        let (tx, rx) = mpsc::channel();
        Self {
            base_url: config.url.clone(),
            model: config.model.clone(),
            system_prompt: system_prompt(&context, tools.is_some()),
            pricing: pricing(&config),
            embedding: config.embedding,
            reranker: config.reranker,
            tools,
            totals: Totals::default(),
            started: None,
            phase_since: Instant::now(),
            phase: Phase::Idle,
            messages: Vec::new(),
            input: String::new(),
            waiting: false,
            show_reasoning: true,
            show_panels: true,
            activity: Vec::new(),
            context_files: context.files(),
            model_status: None,
            memory_status,
            memory: None,
            learning,
            learning_status,
            skills: None,
            reviewing: false,
            applied_skills: Vec::new(),
            applied_skills_tokens: 0,
            tools_tokens,
            scroll: None,
            max_scroll: 0,
            page: 1,
            tx,
            rx,
        }
    }

    /// Push the user's input and start streaming a reply on a background thread.
    fn send(&mut self) {
        let content = self.input.trim().to_string();
        if content.is_empty() || self.waiting {
            return;
        }
        self.input.clear();
        self.scroll = None;
        if content.starts_with('/') {
            self.command(&content);
            return;
        }
        self.messages.push(Message::new("user", content.clone()));
        self.applied_skills.clear();
        self.applied_skills_tokens = 0;
        self.waiting = true;
        self.started = Some(Instant::now());
        self.set_phase(Phase::Waiting);

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let system = self.system_prompt.clone().map(|p| Message::new("system", p));
        let history: Vec<Value> = system
            .iter()
            .chain(self.messages.iter().filter(|m| m.is_history()))
            .map(|m| serde_json::to_value(m).expect("message serializes"))
            .collect();
        let (model, tools, tx) = (self.model.clone(), self.tools.clone(), self.tx.clone());
        let learning = self.learning.clone();
        thread::spawn(move || {
            let mut history = history;
            if let Some(learning) = learning {
                apply_skills(&learning, &content, &mut history, &tx);
            }
            let event = match converse(&url, &model, history, tools.as_deref(), &tx) {
                Ok(stats) => StreamEvent::Done(stats),
                Err(e) => StreamEvent::Error(e),
            };
            let _ = tx.send(event);
        });
    }

    fn handle(&mut self, event: StreamEvent) {
        match event {
            StreamEvent::Token(t) => {
                self.set_phase(Phase::Streaming);
                self.reply().content.push_str(&t);
            }
            StreamEvent::Reasoning(t) => {
                self.set_phase(Phase::Thinking);
                self.reply().reasoning.push_str(&t);
            }
            StreamEvent::ToolCalls(calls) => {
                for call in &calls {
                    let text = format!("{} {}", call.function.name, call.function.arguments);
                    self.log(Level::Tool, text);
                }
                let names: Vec<_> = calls.iter().map(|c| c.function.name.as_str()).collect();
                self.set_phase(Phase::Tools(names.join(", ")));
                self.reply().tool_calls = calls;
            }
            StreamEvent::ToolResult { id, name, content } => {
                self.log(Level::Tool, format!("↳ {content}"));
                self.set_phase(Phase::Waiting);
                if name.starts_with("memory_") {
                    self.refresh_memory();
                }
                let mut result = Message::new("tool", content);
                result.tool_call_id = Some(id);
                self.messages.push(result);
            }
            StreamEvent::Done(stats) => {
                self.waiting = false;
                self.started = None;
                self.set_phase(Phase::Idle);
                self.log(
                    Level::Info,
                    format!("done · {} out tokens · {}", stats.output, secs(stats.elapsed)),
                );
                self.totals.add(&stats);
                let skills = self.applied_skills.clone();
                let reply = self.reply();
                reply.stats = Some(stats);
                reply.skills = skills;
                self.review(false);
            }
            StreamEvent::Error(e) => {
                self.waiting = false;
                self.started = None;
                self.set_phase(Phase::Idle);
                self.log(Level::Error, e.clone());
                self.messages.push(Message::new("error", e));
            }
            StreamEvent::Log(text) => self.log(Level::Info, text),
            StreamEvent::Models(results) => {
                for result in &results {
                    match result {
                        Ok(line) => self.log(Level::Info, line.clone()),
                        Err(line) => self.log(Level::Error, line.clone()),
                    }
                }
                self.model_status = Some(results);
            }
            StreamEvent::SkillsApplied { names, tokens } => {
                self.log(Level::Learn, format!("applying skills: {}", names.join(", ")));
                self.applied_skills = names;
                self.applied_skills_tokens = tokens;
            }
            StreamEvent::Reviewed(Review { outcome, usage }) => {
                self.reviewing = false;
                self.totals.add_review(usage.as_ref());
                let tokens = usage.map_or("usage unknown".into(), |u| {
                    format!("{} tokens", u.prompt_tokens + u.completion_tokens)
                });
                match outcome {
                    Ok(Outcome::Learned(skill)) => {
                        let id = learn::short(&skill);
                        self.log(Level::Learn, format!("learned {} ({}) · {tokens}", skill.name, skill.status));
                        let next = if skill.status == SkillStatus::Proposed {
                            format!("\n/approve {id} to start using it · /reject {id} to discard it")
                        } else {
                            String::new()
                        };
                        let text = format!(
                            "💡 learned a skill ({}): {} — {}\n{}{next}",
                            skill.status, skill.name, skill.description, skill.instructions
                        );
                        self.messages.push(Message::new("info", text));
                        self.refresh_skills();
                    }
                    Ok(Outcome::Nothing(why)) => {
                        self.log(Level::Learn, format!("no lesson: {why} · {tokens}"));
                    }
                    Err(e) => self.log(Level::Error, format!("learning review failed: {e} · {tokens}")),
                }
            }
            StreamEvent::Skills(snapshot) => {
                if let Err(e) = &snapshot {
                    self.log(Level::Error, format!("skills: {e}"));
                }
                self.skills = Some(snapshot);
            }
            StreamEvent::Memory(snapshot) => {
                if let Err(e) = &snapshot {
                    self.log(Level::Error, format!("memory: {e}"));
                }
                self.memory = Some(snapshot);
            }
        }
    }

    fn log(&mut self, level: Level, text: String) {
        let time = chrono::Local::now().format("%H:%M:%S").to_string();
        self.activity.push(Activity { time, level, text });
        if self.activity.len() > MAX_ACTIVITY {
            self.activity.remove(0);
        }
    }

    fn set_phase(&mut self, phase: Phase) {
        if self.phase != phase {
            self.phase = phase;
            self.phase_since = Instant::now();
        }
    }

    /// Log what was loaded at startup and kick off background checks.
    fn start(&mut self) {
        self.log_context();
        match self.memory_status.clone() {
            Ok(path) if self.tools.is_some() => self.log(Level::Info, format!("memory · {path}")),
            Ok(off) => self.log(Level::Info, off),
            Err(line) => self.log(Level::Error, line),
        }
        match self.learning_status.clone() {
            Ok(line) => self.log(Level::Info, line),
            Err(line) => self.log(Level::Error, line),
        }
        self.check_models();
        self.refresh_memory();
        self.refresh_skills();
    }

    /// Re-read the skills panel's contents in the background.
    fn refresh_skills(&self) {
        let Some(learning) = self.learning.clone() else { return };
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(StreamEvent::Skills(learning.snapshot()));
        });
    }

    /// The model-visible conversation as (role, text) pairs, with tool calls spelled out.
    fn history_text(&self) -> Vec<(&str, String)> {
        self.messages
            .iter()
            .filter(|m| m.is_history())
            .map(|m| {
                let mut text = m.content.clone();
                for call in &m.tool_calls {
                    text += &format!("\n→ {} {}", call.function.name, call.function.arguments);
                }
                (m.role.as_str(), text)
            })
            .collect()
    }

    /// Review the conversation for a reusable lesson in the background: after
    /// every turn when a trigger fires, or on /learn (`forced`).
    fn review(&mut self, forced: bool) {
        let Some(learning) = self.learning.clone() else { return };
        if learning.mode == Mode::Off || self.reviewing {
            return;
        }
        let owned = self.history_text();
        let history: Vec<(&str, &str)> = owned.iter().map(|(r, c)| (*r, c.as_str())).collect();
        let trigger = if forced {
            Some("the user asked to review this conversation for a lesson")
        } else {
            learn::last_turn(&history).and_then(|turn| evaluator::trigger(&turn))
        };
        let Some(trigger) = trigger else { return };
        let transcript = learn::transcript(&history, 16);
        self.log(Level::Learn, format!("reviewing for a lesson: {trigger}"));
        self.reviewing = true;

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let (model, tx) = (self.model.clone(), self.tx.clone());
        thread::spawn(move || {
            let _ = tx.send(StreamEvent::Reviewed(learning.review(&url, &model, trigger, &transcript)));
        });
    }

    /// Handle a `/command` typed in the input box.
    fn command(&mut self, line: &str) {
        let (name, arg) = line.split_once(' ').unwrap_or((line, ""));
        let learning = self.learning.clone();
        let need = || -> Result<Arc<Learning>, String> {
            learning.clone().ok_or_else(|| match &self.learning_status {
                Err(why) => why.clone(),
                Ok(_) => "learning is off".into(),
            })
        };
        let result = match name {
            "/help" => Ok(COMMANDS.to_string()),
            "/skills" => need().and_then(|l| l.describe()),
            "/approve" => need().and_then(|l| l.approve(arg)),
            "/reject" => need().and_then(|l| l.reject(arg)),
            "/forget-skill" => need().and_then(|l| l.forget(arg)),
            "/learn" => need().and_then(|l| {
                if l.mode == Mode::Off {
                    Err("learning mode is off ([learning] mode in config.toml)".into())
                } else if self.reviewing {
                    Err("a review is already running".into())
                } else {
                    Ok("reviewing the conversation for a lesson…".into())
                }
            }),
            _ => Err(format!("unknown command {name} — try /help")),
        };
        let ok = result.is_ok();
        let (role, text) = match result {
            Ok(text) => ("info", text),
            Err(e) => ("error", e),
        };
        self.messages.push(Message::new(role, format!("> {line}\n{text}")));
        if ok && name == "/learn" {
            self.review(true);
        }
        if ok && matches!(name, "/approve" | "/reject" | "/forget-skill") {
            self.log(Level::Learn, line.to_string());
            self.refresh_skills();
        }
    }

    fn log_context(&mut self) {
        let text = match self.context_files.len() {
            0 => "no SOUL.md / USER.md / AGENT.md found".to_string(),
            n => format!("loaded {n} context file{}", if n == 1 { "" } else { "s" }),
        };
        self.log(Level::Info, text);
    }

    /// Re-read the memory panel's numbers in the background.
    fn refresh_memory(&self) {
        let Some(tools) = self.tools.clone() else { return };
        let tx = self.tx.clone();
        thread::spawn(move || {
            let _ = tx.send(StreamEvent::Memory(tools.snapshot(50)));
        });
    }

    /// Re-read the context files and config.toml; takes effect on the next request.
    fn reload(&mut self) {
        let context = Context::load();
        self.system_prompt = system_prompt(&context, self.tools.is_some());
        self.context_files = context.files();
        self.log(Level::Info, "reloaded config and context files".into());
        self.log_context();
        match Config::load() {
            Ok(config) => {
                self.base_url = config.url.clone();
                self.model = config.model.clone();
                self.pricing = pricing(&config);
                self.embedding = config.embedding;
                self.reranker = config.reranker;
            }
            // Keep the current settings rather than dropping to defaults.
            Err(e) => {
                let e = format!("config not reloaded: {e}");
                self.log(Level::Error, e.clone());
                self.messages.push(Message::new("error", e));
            }
        }
        self.scroll = None;
        self.check_models();
        self.refresh_memory();
    }

    /// Ping the embedding and reranker models in the background.
    fn check_models(&mut self) {
        self.model_status = None;
        let (embedding, reranker) = (self.embedding.clone(), self.reranker.clone());
        let tx = self.tx.clone();
        thread::spawn(move || {
            let results = retrieval::check(embedding.as_ref(), reranker.as_ref());
            let _ = tx.send(StreamEvent::Models(results));
        });
    }

    fn scroll_up(&mut self, n: u16) {
        let top = self.scroll.unwrap_or(self.max_scroll);
        self.scroll = Some(top.saturating_sub(n));
    }

    fn scroll_down(&mut self, n: u16) {
        if let Some(top) = self.scroll {
            let top = top.saturating_add(n);
            self.scroll = (top < self.max_scroll).then_some(top);
        }
    }

    /// The assistant message being streamed into, created on the first token.
    fn reply(&mut self) -> &mut Message {
        if self.messages.last().is_none_or(|m| m.role != "assistant") {
            self.messages.push(Message::new("assistant", String::new()));
        }
        self.messages.last_mut().unwrap()
    }
}

const COMMANDS: &str = "\
/skills              list learned skills (proposals in full)
/approve <id>        start using a proposed skill
/reject <id>         discard a proposed skill for good
/forget-skill <id>   delete a skill
/learn               review the conversation for a lesson now
/help                this list";

/// Add the active skills that match the user's message to the system prompt.
fn apply_skills(learning: &Learning, message: &str, history: &mut Vec<Value>, tx: &Sender<StreamEvent>) {
    match learning.relevant(message) {
        Ok(skills) if !skills.is_empty() => {
            let section = learn::prompt_section(&skills);
            let names = skills.into_iter().map(|s| s.name).collect();
            let tokens = learn::approx_tokens(&section);
            let _ = tx.send(StreamEvent::SkillsApplied { names, tokens });
            match history.first_mut() {
                Some(first) if first["role"] == "system" => {
                    let prompt = format!("{}\n\n{section}", first["content"].as_str().unwrap_or(""));
                    first["content"] = Value::String(prompt);
                }
                _ => history.insert(0, json!({ "role": "system", "content": section })),
            }
        }
        Ok(_) => {}
        Err(e) => {
            let _ = tx.send(StreamEvent::Log(format!("skill search failed: {e}")));
        }
    }
}

/// Context files plus, when memory is on, instructions for the memory tools.
fn system_prompt(context: &Context, memory: bool) -> Option<String> {
    let parts: Vec<String> = context
        .system_prompt()
        .into_iter()
        .chain(memory.then(|| tools::MEMORY_PROMPT.to_string()))
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

fn pricing(config: &Config) -> Pricing {
    Pricing {
        input_per_mtok: config.input_cost_per_mtok,
        cached_per_mtok: config.cached_input_cost_per_mtok.unwrap_or(config.input_cost_per_mtok),
        output_per_mtok: config.output_cost_per_mtok,
        currency: config.currency.clone(),
    }
}

/// One user turn: stream a reply; if the model calls tools, run them, add the
/// results to the history and stream again. Stats cover the whole turn.
fn converse(
    url: &str,
    model: &str,
    mut history: Vec<Value>,
    tools: Option<&Tools>,
    tx: &Sender<StreamEvent>,
) -> Result<Stats, String> {
    let start = Instant::now();
    let mut total: Option<Stats> = None;
    for round in 1..=MAX_TOOL_ROUNDS {
        let n = history.len();
        let note = format!("round {round} · sending {n} message{}", if n == 1 { "" } else { "s" });
        tx.send(StreamEvent::Log(note)).map_err(|e| e.to_string())?;
        let mut body = json!({
            "model": model,
            "messages": history,
            "stream": true,
            "stream_options": { "include_usage": true },
        });
        if let Some(tools) = tools {
            body["tools"] = tools.definitions();
        }
        let round = stream(url, &body, tx)?;
        match &mut total {
            Some(total) => total.absorb(round.stats),
            None => total = Some(round.stats),
        }
        let Some(tools) = tools.filter(|_| !round.tool_calls.is_empty()) else {
            let mut stats = total.expect("at least one round");
            stats.elapsed = start.elapsed();
            return Ok(stats);
        };

        tx.send(StreamEvent::ToolCalls(round.tool_calls.clone())).map_err(|e| e.to_string())?;
        history.push(json!({
            "role": "assistant",
            "content": round.content,
            "tool_calls": round.tool_calls,
        }));
        for call in &round.tool_calls {
            let content = tools.run(&call.function.name, &call.function.arguments);
            history.push(json!({ "role": "tool", "tool_call_id": call.id, "content": content }));
            let (id, name) = (call.id.clone(), call.function.name.clone());
            tx.send(StreamEvent::ToolResult { id, name, content }).map_err(|e| e.to_string())?;
        }
    }
    Err(format!("stopped after {MAX_TOOL_ROUNDS} rounds of tool calls"))
}

/// POST the request, forward each delta as it arrives, and measure the reply.
fn stream(url: &str, body: &Value, tx: &Sender<StreamEvent>) -> Result<Round, String> {
    // No overall timeout: a long generation is fine as long as tokens keep coming.
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(None)
        .build()
        .map_err(|e| e.to_string())?;
    let start = Instant::now();
    let mut ttft = None;
    let mut usage = None;
    let mut chunks = 0;
    let mut content = String::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let resp = client.post(url).json(body).send().map_err(|e| e.to_string())?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("{status}: {}", resp.text().unwrap_or_default()));
    }
    let mut logged_first = false;
    for line in BufReader::new(resp).lines() {
        if !logged_first && let Some(ttft) = ttft {
            logged_first = true;
            tx.send(StreamEvent::Log(format!("first token after {}", secs(ttft))))
                .map_err(|e| e.to_string())?;
        }
        let line = line.map_err(|e| e.to_string())?;
        let Some(data) = line.strip_prefix("data:") else { continue };
        let data = data.trim();
        if data == "[DONE]" {
            break;
        }
        let chunk: Chunk = serde_json::from_str(data).map_err(|e| format!("{e}: {data}"))?;
        usage = chunk.usage.or(usage);
        let Some(choice) = chunk.choices.into_iter().next() else { continue };
        let delta = choice.delta;
        for part in delta.tool_calls {
            ttft.get_or_insert_with(|| start.elapsed());
            if tool_calls.len() <= part.index {
                tool_calls.resize_with(part.index + 1, ToolCall::default);
            }
            let call = &mut tool_calls[part.index];
            call.kind = "function".into();
            if let Some(id) = part.id {
                call.id = id;
            }
            if let Some(f) = part.function {
                call.function.name += f.name.as_deref().unwrap_or("");
                call.function.arguments += f.arguments.as_deref().unwrap_or("");
            }
        }
        if let Some(t) = &delta.content {
            content.push_str(t);
        }
        let events = [
            delta.reasoning_content.map(StreamEvent::Reasoning),
            delta.content.map(StreamEvent::Token),
        ];
        for event in events.into_iter().flatten() {
            if matches!(&event, StreamEvent::Token(t) | StreamEvent::Reasoning(t) if t.is_empty()) {
                continue;
            }
            ttft.get_or_insert_with(|| start.elapsed());
            chunks += 1;
            tx.send(event).map_err(|e| e.to_string())?;
        }
    }
    let elapsed = start.elapsed();
    let stats = match usage {
        Some(u) => Stats {
            ttft,
            elapsed,
            input: u.prompt_tokens,
            cached: u.cached(),
            output: u.completion_tokens,
            estimated: false,
        },
        // Most servers send one token per chunk, so the chunk count is a fair guess.
        None => Stats { ttft, elapsed, input: 0, cached: 0, output: chunks, estimated: true },
    };
    Ok(Round { stats, content, tool_calls })
}

fn main() {
    let config = match Config::load() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("lyra: bad config: {e}");
            std::process::exit(1);
        }
    };
    // Memory is async (sqlx); a small runtime lets lyra's threads call into it.
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let (tools, memory_status) = open_memory(&config, runtime.handle());
    let (learning, learning_status) = open_learning(&config, runtime.handle());
    let services = Services { tools, memory_status, learning, learning_status };
    let mut app = App::new(config, Context::load(), services);
    app.start();
    ratatui::run(|terminal| run(terminal, &mut app)).expect("terminal error");
}

/// Open the skills database, unless learning is off.
fn open_learning(
    config: &Config,
    runtime: &tokio::runtime::Handle,
) -> (Option<Arc<Learning>>, Result<String, String>) {
    let c = &config.learning;
    // Config::load already checked the mode parses.
    let mode: Mode = c.mode.parse().unwrap_or(Mode::Propose);
    let Some(path) = c.path() else {
        return (None, Err("learning off: no data directory (set [learning] path)".into()));
    };
    match runtime.block_on(lyra_learning::LearningManager::open(&path)) {
        Ok(manager) => {
            let learning =
                Learning::new(Arc::new(manager), runtime.clone(), mode, c.min_confidence, c.max_skills);
            let status = format!("skills · {} · mode {}", context::show(&path), c.mode);
            (Some(Arc::new(learning)), Ok(status))
        }
        Err(e) => (None, Err(format!("learning off: {e:#}"))),
    }
}

/// Open the memory database and build the memory tools, if enabled.
fn open_memory(
    config: &Config,
    runtime: &tokio::runtime::Handle,
) -> (Option<Arc<Tools>>, Result<String, String>) {
    if !config.memory.enabled {
        return (None, Ok("memory off".into()));
    }
    let Some(path) = config.memory.path() else {
        return (None, Err("memory off: no data directory (set [memory] path)".into()));
    };
    match runtime.block_on(lyra_memory::MemoryManager::open(&path)) {
        Ok(manager) => {
            let tools = Tools::new(
                Arc::new(manager),
                runtime.clone(),
                config.memory.default_scope.clone(),
            );
            (Some(Arc::new(tools)), Ok(context::show(&path)))
        }
        Err(e) => (None, Err(format!("memory off: {e:#}"))),
    }
}

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> std::io::Result<()> {
    loop {
        // Drain everything the stream has produced since the last frame.
        while let Ok(event) = app.rx.try_recv() {
            app.handle(event);
        }

        terminal.draw(|f| ui::draw(f, app))?;

        if !event::poll(Duration::from_millis(30))? {
            continue;
        }
        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        match key.code {
            KeyCode::Esc => return Ok(()),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.show_reasoning = !app.show_reasoning;
            }
            KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::CONTROL) => app.reload(),
            KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.show_panels = !app.show_panels;
            }
            KeyCode::Up => app.scroll_up(1),
            KeyCode::Down => app.scroll_down(1),
            KeyCode::PageUp => app.scroll_up(app.page),
            KeyCode::PageDown => app.scroll_down(app.page),
            KeyCode::Enter => app.send(),
            KeyCode::Backspace => {
                app.input.pop();
            }
            KeyCode::Char(c) => app.input.push(c),
            _ => {}
        }
    }
}

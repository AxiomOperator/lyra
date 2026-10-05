mod config;
mod context;
mod retrieval;
mod stats;

use std::io::{BufRead, BufReader};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use serde::{Deserialize, Serialize};

use config::Config;
use context::Context;
use retrieval::Endpoint;
use stats::{Pricing, Stats, Totals, Usage, percent, secs, thousands};

#[derive(Serialize)]
struct Message {
    role: String,
    content: String,
    /// The model's thinking; shown in the UI but never sent back to the model.
    #[serde(skip)]
    reasoning: String,
    /// Timing and token counts for an assistant reply.
    #[serde(skip)]
    stats: Option<Stats>,
}

impl Message {
    fn new(role: &str, content: String) -> Self {
        Self { role: role.into(), content, reasoning: String::new(), stats: None }
    }
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
}

enum StreamEvent {
    Token(String),
    Reasoning(String),
    Done(Stats),
    Error(String),
    /// Result of a background health check of the embedding/reranker models.
    Status(Result<String, String>),
}

struct App {
    base_url: String,
    model: String,
    /// Built from SOUL.md / AGENT.md / USER.md; sent first with every request.
    system_prompt: Option<String>,
    pricing: Pricing,
    /// Not used by the chat yet; checked at startup and on reload.
    embedding: Option<Endpoint>,
    reranker: Option<Endpoint>,
    totals: Totals,
    /// When the in-flight request was sent.
    started: Option<Instant>,
    messages: Vec<Message>,
    input: String,
    waiting: bool,
    show_reasoning: bool,
    /// Top line of the chat view when scrolled up; `None` follows the bottom.
    scroll: Option<u16>,
    /// Chat view size from the last frame, used for scroll bounds.
    max_scroll: u16,
    page: u16,
    tx: Sender<StreamEvent>,
    rx: Receiver<StreamEvent>,
}

impl App {
    fn new(config: Config, context: Context) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            base_url: config.url.clone(),
            model: config.model.clone(),
            system_prompt: context.system_prompt(),
            pricing: pricing(&config),
            embedding: config.embedding,
            reranker: config.reranker,
            totals: Totals::default(),
            started: None,
            messages: vec![Message::new("info", context.summary())],
            input: String::new(),
            waiting: false,
            show_reasoning: true,
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
        self.messages.push(Message::new("user", content));
        self.waiting = true;
        self.started = Some(Instant::now());

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        // Error and info lines are UI-only; don't send them to the model.
        let system = self.system_prompt.clone().map(|p| Message::new("system", p));
        let history: Vec<&Message> = system
            .iter()
            .chain(self.messages.iter().filter(|m| m.role == "user" || m.role == "assistant"))
            .collect();
        let body = serde_json::json!({
            "model": self.model,
            "messages": history,
            "stream": true,
            "stream_options": { "include_usage": true },
        });
        let tx = self.tx.clone();
        thread::spawn(move || {
            let event = match stream(&url, &body, &tx) {
                Ok(stats) => StreamEvent::Done(stats),
                Err(e) => StreamEvent::Error(e),
            };
            let _ = tx.send(event);
        });
    }

    fn handle(&mut self, event: StreamEvent) {
        match event {
            StreamEvent::Token(t) => self.reply().content.push_str(&t),
            StreamEvent::Reasoning(t) => self.reply().reasoning.push_str(&t),
            StreamEvent::Done(stats) => {
                self.waiting = false;
                self.started = None;
                self.totals.add(&stats);
                self.reply().stats = Some(stats);
            }
            StreamEvent::Status(Ok(line)) => self.messages.push(Message::new("info", line)),
            StreamEvent::Status(Err(line)) => self.messages.push(Message::new("error", line)),
            StreamEvent::Error(e) => {
                self.waiting = false;
                self.started = None;
                self.messages.push(Message::new("error", e));
            }
        }
    }

    /// Re-read the context files and config.toml; takes effect on the next request.
    fn reload(&mut self) {
        let context = Context::load();
        self.system_prompt = context.system_prompt();
        self.messages.push(Message::new("info", format!("reloaded · {}", context.summary())));
        match Config::load() {
            Ok(config) => {
                self.base_url = config.url.clone();
                self.model = config.model.clone();
                self.pricing = pricing(&config);
                self.embedding = config.embedding;
                self.reranker = config.reranker;
            }
            // Keep the current settings rather than dropping to defaults.
            Err(e) => self.messages.push(Message::new("error", format!("config not reloaded: {e}"))),
        }
        self.scroll = None;
        self.check_models();
    }

    /// Ping the embedding and reranker models in the background; results show as info lines.
    fn check_models(&self) {
        let (embedding, reranker) = (self.embedding.clone(), self.reranker.clone());
        let tx = self.tx.clone();
        thread::spawn(move || {
            for line in retrieval::check(embedding.as_ref(), reranker.as_ref()) {
                let _ = tx.send(StreamEvent::Status(line));
            }
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

fn pricing(config: &Config) -> Pricing {
    Pricing {
        input_per_mtok: config.input_cost_per_mtok,
        cached_per_mtok: config.cached_input_cost_per_mtok.unwrap_or(config.input_cost_per_mtok),
        output_per_mtok: config.output_cost_per_mtok,
        currency: config.currency.clone(),
    }
}

/// POST the request, forward each delta as it arrives, and measure the reply.
fn stream(url: &str, body: &serde_json::Value, tx: &Sender<StreamEvent>) -> Result<Stats, String> {
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
    let resp = client.post(url).json(body).send().map_err(|e| e.to_string())?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("{status}: {}", resp.text().unwrap_or_default()));
    }
    for line in BufReader::new(resp).lines() {
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
    Ok(match usage {
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
    })
}

fn main() {
    let config = match Config::load() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("lyra: bad config: {e}");
            std::process::exit(1);
        }
    };
    let mut app = App::new(config, Context::load());
    app.check_models();
    ratatui::run(|terminal| run(terminal, &mut app)).expect("terminal error");
}

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> std::io::Result<()> {
    loop {
        // Drain everything the stream has produced since the last frame.
        while let Ok(event) = app.rx.try_recv() {
            app.handle(event);
        }

        terminal.draw(|f| draw(f, app))?;

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

fn draw(f: &mut Frame, app: &mut App) {
    let [chat_area, status_area, input_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(2),
        Constraint::Length(3),
    ])
    .areas(f.area());

    // Chat history
    let mut lines: Vec<Line> = Vec::new();
    for m in &app.messages {
        let (label, color) = match m.role.as_str() {
            "user" => ("you", Color::Cyan),
            "assistant" => ("lyra", Color::Green),
            "info" => ("context", Color::DarkGray),
            _ => ("error", Color::Red),
        };
        lines.push(Line::from(label.bold().fg(color)));
        let reasoning = m.reasoning.trim();
        if !reasoning.is_empty() {
            let dim = Style::default().fg(Color::DarkGray).italic();
            if app.show_reasoning {
                lines.extend(reasoning.lines().map(|l| Line::styled(l.to_string(), dim)));
            } else {
                let n = reasoning.lines().count();
                let plural = if n == 1 { "" } else { "s" };
                let note = format!("[reasoning hidden · {n} line{plural} · Ctrl-R]");
                lines.push(Line::styled(note, dim));
            }
            if !m.content.is_empty() {
                lines.push(Line::default());
            }
        }
        let body = if m.role == "info" { Style::default().fg(Color::DarkGray) } else { Style::default() };
        lines.extend(m.content.lines().map(|l| Line::styled(l.to_string(), body)));
        if let Some(stats) = &m.stats {
            lines.push(Line::from(reply_stats(stats, &app.pricing).dark_gray()));
        }
        lines.push(Line::default());
    }
    // Show "thinking..." until the first token arrives.
    if app.waiting && app.messages.last().is_some_and(|m| m.role == "user") {
        lines.push(Line::from("thinking...".italic().dark_gray()));
    }

    let title = format!(" lyra · {} · {} ", app.model, app.base_url);
    let mut block = Block::bordered().title(title);
    if app.scroll.is_some() {
        block = block.title_bottom(Line::from(" ↓ more below · PgDn ".dark_gray()).right_aligned());
    }
    let chat = Paragraph::new(Text::from(lines))
        .block(block)
        .wrap(Wrap { trim: false });
    // line_count includes the block's borders, as does the area height.
    let total = chat.line_count(chat_area.width);
    app.page = chat_area.height.saturating_sub(2).max(1);
    app.max_scroll = total.saturating_sub(chat_area.height as usize) as u16;
    // Clamp after resizes / reasoning toggles; reaching the bottom resumes following.
    app.scroll = app.scroll.filter(|&top| top < app.max_scroll);
    let top = app.scroll.unwrap_or(app.max_scroll);
    f.render_widget(chat.scroll((top, 0)), chat_area);

    let status = vec![
        Line::from(session_stats(app).dark_gray()),
        Line::from(cache_stats(app).dark_gray()),
    ];
    f.render_widget(Paragraph::new(status), status_area);

    // Input box
    let input = Paragraph::new(app.input.as_str())
        .style(Style::default())
        .block(Block::bordered().title(" message (Enter send · ↑↓/PgUp/PgDn scroll · Ctrl-R reasoning · Ctrl-L reload · Esc quit) "));
    f.render_widget(input, input_area);
    f.set_cursor_position((
        input_area.x + 1 + app.input.chars().count() as u16,
        input_area.y + 1,
    ));
}

/// One-line summary under an assistant reply.
fn reply_stats(stats: &Stats, pricing: &Pricing) -> String {
    let mut parts = Vec::new();
    if let Some(ttft) = stats.ttft {
        parts.push(format!("ttft {}", secs(ttft)));
    }
    if let Some(tps) = stats.tokens_per_sec() {
        parts.push(format!("{tps:.1} tok/s"));
    }
    if stats.estimated {
        parts.push(format!("in ? · out ~{}", thousands(stats.output)));
    } else {
        let mut input = format!("in {}", thousands(stats.input));
        if stats.cached > 0 {
            input += &format!(" ({} cached)", thousands(stats.cached));
        }
        parts.push(input);
        parts.push(format!("out {}", thousands(stats.output)));
        parts.push(pricing.format(pricing.cost(stats.input, stats.cached, stats.output)));
    }
    parts.push(format!("{} total", secs(stats.elapsed)));
    parts.join(" · ")
}

/// Status bar: running totals for the session, plus a live timer while streaming.
fn session_stats(app: &App) -> String {
    let t = &app.totals;
    // Mark totals that include chunk-count estimates.
    let approx = if t.estimated { "~" } else { "" };
    let mut parts = vec![
        format!(" session: {} repl{}", t.replies, if t.replies == 1 { "y" } else { "ies" }),
        format!("in {approx}{}", thousands(t.input)),
        format!("out {approx}{}", thousands(t.output)),
        format!("total {approx}{}", thousands(t.input + t.output)),
        format!("cost {approx}{}", app.pricing.format(app.pricing.cost(t.input, t.cached, t.output))),
    ];
    if let Some(avg) = t.avg_ttft() {
        parts.push(format!("avg ttft {}", secs(avg)));
    }
    if let Some(started) = app.started {
        parts.push(format!("streaming {}", secs(started.elapsed())));
    }
    parts.join(" · ")
}

/// Status bar, second line: prompt cache usage across the session.
fn cache_stats(app: &App) -> String {
    let t = &app.totals;
    let mut parts = vec![format!(
        " cache: {} of {} prompt tokens cached",
        thousands(t.cached),
        thousands(t.input)
    )];
    if let Some(rate) = percent(t.cached, t.input) {
        parts.push(format!("hit rate {rate:.1}%"));
    }
    parts.push(format!("cost {}", app.pricing.format(app.pricing.cost(t.cached, t.cached, 0))));
    parts.push(format!("saved {}", app.pricing.format(app.pricing.savings(t.cached))));
    parts.join(" · ")
}

mod config;

use std::io::{BufRead, BufReader};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use serde::{Deserialize, Serialize};

use config::Config;

#[derive(Serialize)]
struct Message {
    role: String,
    content: String,
    /// The model's thinking; shown in the UI but never sent back to the model.
    #[serde(skip)]
    reasoning: String,
}

impl Message {
    fn new(role: &str, content: String) -> Self {
        Self { role: role.into(), content, reasoning: String::new() }
    }
}

/// One SSE chunk from a streaming `/chat/completions` response.
#[derive(Deserialize)]
struct Chunk {
    #[serde(default)]
    choices: Vec<ChunkChoice>,
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
    Done,
    Error(String),
}

struct App {
    base_url: String,
    model: String,
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
    fn new(base_url: String, model: String) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            base_url,
            model,
            messages: Vec::new(),
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

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        // Error lines are UI-only; don't send them to the model.
        let history: Vec<&Message> = self
            .messages
            .iter()
            .filter(|m| m.role == "user" || m.role == "assistant")
            .collect();
        let body = serde_json::json!({ "model": self.model, "messages": history, "stream": true });
        let tx = self.tx.clone();
        thread::spawn(move || {
            let event = match stream(&url, &body, &tx) {
                Ok(()) => StreamEvent::Done,
                Err(e) => StreamEvent::Error(e),
            };
            let _ = tx.send(event);
        });
    }

    fn handle(&mut self, event: StreamEvent) {
        match event {
            StreamEvent::Token(t) => self.reply().content.push_str(&t),
            StreamEvent::Reasoning(t) => self.reply().reasoning.push_str(&t),
            StreamEvent::Done => self.waiting = false,
            StreamEvent::Error(e) => {
                self.waiting = false;
                self.messages.push(Message::new("error", e));
            }
        }
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

/// POST the request and forward each content delta as it arrives.
fn stream(url: &str, body: &serde_json::Value, tx: &Sender<StreamEvent>) -> Result<(), String> {
    // No overall timeout: a long generation is fine as long as tokens keep coming.
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(None)
        .build()
        .map_err(|e| e.to_string())?;
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
        let Some(choice) = chunk.choices.into_iter().next() else { continue };
        let delta = choice.delta;
        let events = [
            delta.reasoning_content.map(StreamEvent::Reasoning),
            delta.content.map(StreamEvent::Token),
        ];
        for event in events.into_iter().flatten() {
            tx.send(event).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn main() {
    let config = match Config::load() {
        Ok(config) => config,
        Err(e) => {
            eprintln!("lyra: bad config: {e}");
            std::process::exit(1);
        }
    };
    let mut app = App::new(config.url, config.model);
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
    let [chat_area, input_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).areas(f.area());

    // Chat history
    let mut lines: Vec<Line> = Vec::new();
    for m in &app.messages {
        let (label, color) = match m.role.as_str() {
            "user" => ("you", Color::Cyan),
            "assistant" => ("lyra", Color::Green),
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
        lines.extend(m.content.lines().map(|l| Line::from(l.to_string())));
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

    // Input box
    let input = Paragraph::new(app.input.as_str())
        .style(Style::default())
        .block(Block::bordered().title(" message (Enter send · ↑↓/PgUp/PgDn scroll · Ctrl-R reasoning · Esc quit) "));
    f.render_widget(input, input_area);
    f.set_cursor_position((
        input_area.x + 1 + app.input.chars().count() as u16,
        input_area.y + 1,
    ));
}

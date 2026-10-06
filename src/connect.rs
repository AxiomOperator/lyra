//! `lyra connect`: the terminal UI for a lyra that runs elsewhere (`lyra
//! serve`). It speaks the same protocol as the web app — a snapshot, then
//! small updates — and sends messages, commands and approval answers back.
//! Nothing is stored here but the device's token (`~/.config/lyra/remote.toml`).

use std::sync::mpsc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Clear, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

/// `~/.config/lyra/remote.toml`.
#[derive(Serialize, Deserialize)]
pub struct RemoteConfig {
    pub url: String,
    pub token: String,
    pub name: String,
}

pub fn config_path() -> Option<std::path::PathBuf> {
    Some(lyra_node::config_dir()?.join("remote.toml"))
}

pub fn configured() -> bool {
    config_path().is_some_and(|p| p.exists())
}

/// What the connection tells the screen.
enum Incoming {
    Connected,
    Lost(String),
    Message(Value),
}

/// The conversation as the server last described it.
#[derive(Default)]
struct View {
    messages: Vec<Value>,
    /// Rendered Markdown per message, with the content length it came from.
    rendered: Vec<Option<(usize, Vec<Line<'static>>)>>,
    status: Value,
    commands: Vec<(String, String, String)>,
    seq: u64,
    ready: bool,
    connected: bool,
    banner: String,
}

impl View {
    fn apply(&mut self, m: Value) {
        if m["type"] == "snapshot" {
            self.seq = m["seq"].as_u64().unwrap_or(0);
            self.messages = m["messages"].as_array().cloned().unwrap_or_default();
            self.rendered = vec![None; self.messages.len()];
            self.status = m["status"].clone();
            self.commands = m["commands"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| (str_of(&c["usage"]), str_of(&c["description"]), str_of(&c["completion"])))
                .collect();
            // Handled here: the file goes up from this machine.
            self.commands.push(("/attach <path>".into(), "send a file with your next message (/attach clear drops them)".into(), "/attach ".into()));
            self.ready = true;
            return;
        }
        if !self.ready || m["type"] == "pong" {
            return;
        }
        let seq = m["seq"].as_u64().unwrap_or(0);
        if seq != 0 && seq <= self.seq {
            return;
        }
        self.seq = seq.max(self.seq);
        let index = m["index"].as_u64().unwrap_or(0) as usize;
        match m["type"].as_str().unwrap_or("") {
            "add" | "replace" => {
                if index >= self.messages.len() {
                    self.messages.resize(index + 1, Value::Null);
                    self.rendered.resize(index + 1, None);
                }
                self.messages[index] = m["message"].clone();
                self.rendered[index] = None;
            }
            "append" => {
                if let Some(msg) = self.messages.get_mut(index) {
                    msg["content"] = json!(format!("{}{}", str_of(&msg["content"]), str_of(&m["text"])));
                    msg["reasoning"] = json!(format!("{}{}", str_of(&msg["reasoning"]), str_of(&m["reasoning"])));
                }
            }
            "truncate" => {
                let n = m["length"].as_u64().unwrap_or(0) as usize;
                self.messages.truncate(n);
                self.rendered.truncate(n);
            }
            "reset" => {
                self.messages = m["messages"].as_array().cloned().unwrap_or_default();
                self.rendered = vec![None; self.messages.len()];
            }
            "status" => self.status = m["status"].clone(),
            "resync" => self.ready = false,
            _ => {}
        }
    }
}

fn str_of(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

fn truncate(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max { flat } else { format!("{}…", flat.chars().take(max.saturating_sub(1)).collect::<String>()) }
}

/// Keep the connection up (reconnecting), feeding the screen and sending its messages.
async fn connection(config: RemoteConfig, to_screen: mpsc::Sender<Incoming>, mut from_screen: tokio::sync::mpsc::UnboundedReceiver<String>) {
    let base = lyra_node::socket_url(&config.url, "ws", &config.token);
    // The conversation this terminal shows (kept across reconnects).
    let session = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let mut pause = 1;
    loop {
        let url = format!("{base}&session={}", session.lock().map(|s| s.clone()).unwrap_or_default());
        match tokio_tungstenite::connect_async(url.as_str()).await {
            Ok((ws, _)) => {
                pause = 1;
                let _ = to_screen.send(Incoming::Connected);
                let (mut sink, mut stream) = ws.split();
                // A terminal that's open counts as watching: no phone notifications meanwhile.
                let visible = json!({ "type": "visible", "visible": true }).to_string();
                let _ = sink.send(Message::Text(visible.clone().into())).await;
                let mut heartbeat = tokio::time::interval(Duration::from_secs(30));
                let mut heard = std::time::Instant::now();
                let ping = json!({ "type": "ping" }).to_string();
                let why = loop {
                    tokio::select! {
                        _ = heartbeat.tick() => {
                            // lyra answers pings: silence means a dead connection a proxy kept open.
                            if heard.elapsed() > lyra_node::SILENCE {
                                break "no answer from lyra".to_string();
                            }
                            if sink.send(Message::Text(visible.clone().into())).await.is_err() || sink.send(Message::Text(ping.clone().into())).await.is_err() {
                                break "connection lost".to_string();
                            }
                        }
                        out = from_screen.recv() => match out {
                            Some(text) => {
                                if sink.send(Message::Text(text.into())).await.is_err() {
                                    break "connection lost".to_string();
                                }
                            }
                            None => return,
                        },
                        incoming = stream.next() => { heard = std::time::Instant::now(); match incoming {
                            Some(Ok(Message::Text(t))) => {
                                if let Ok(v) = serde_json::from_str::<Value>(t.as_str()) {
                                    let resync = v["type"] == "resync";
                                    if let Some(id) = v["session_id"].as_str()
                                        && let Ok(mut s) = session.lock()
                                    {
                                        *s = id.to_string();
                                    }
                                    let _ = to_screen.send(Incoming::Message(v));
                                    if resync {
                                        break "catching up".to_string();
                                    }
                                }
                            }
                            Some(Ok(_)) => {}
                            Some(Err(e)) => break e.to_string(),
                            None => break "lyra closed the connection".to_string(),
                        }},
                    }
                };
                let _ = to_screen.send(Incoming::Lost(why));
            }
            Err(tokio_tungstenite::tungstenite::Error::Http(r)) if r.status().as_u16() == 401 => {
                let _ = to_screen.send(Incoming::Lost("this terminal isn't paired any more: lyra connect --pair <code>".into()));
                return;
            }
            Err(e) => {
                let _ = to_screen.send(Incoming::Lost(format!("can't reach {}: {e}", config.url)));
            }
        }
        tokio::time::sleep(Duration::from_secs(pause)).await;
        pause = (pause * 2).min(30);
    }
}

struct Screen {
    view: View,
    input: String,
    /// Top line when scrolled up; `None` follows the bottom.
    scroll: Option<u16>,
    max_scroll: u16,
    page: u16,
    show_reasoning: bool,
    palette: usize,
    palette_hidden: bool,
    /// The side panel (devices and machines online); Ctrl-B toggles it.
    show_panel: bool,
    out: tokio::sync::mpsc::UnboundedSender<String>,
    url: String,
    /// The server's address and this device's token, for uploads.
    base: String,
    token: String,
    /// Files attached for the next message: (upload id, name).
    files: Vec<(String, String)>,
}

impl Screen {
    fn send(&mut self, v: Value) {
        if self.out.send(v.to_string()).is_err() || !self.view.connected {
            self.view.banner = "not connected — reconnecting…".into();
        }
    }

    fn approval(&self) -> Option<&Value> {
        self.view.status["approvals"].as_array().and_then(|a| a.first())
    }

    /// The word being typed starts with `@`: the machines to pick from.
    fn mentioning(&self) -> Option<&str> {
        let word = self.input.rsplit(' ').next().unwrap_or("");
        word.starts_with('@').then_some(word)
    }

    fn palette_entries(&self) -> Vec<(String, String, String)> {
        if self.palette_hidden {
            return Vec::new();
        }
        if let Some(word) = self.mentioning() {
            let typed = word.trim_start_matches('@').to_lowercase();
            let before = &self.input[..self.input.len() - word.len()];
            let mut names = vec![("server".to_string(), "where lyra runs".to_string())];
            for m in self.view.status["machines_detail"].as_array().into_iter().flatten().filter(|m| m["online"] == true) {
                let name = str_of(&m["name"]);
                let about = [str_of(&m["hostname"]), str_of(&m["os"])].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
                names.push((name, if about.is_empty() { "online".into() } else { format!("online · {about}") }));
            }
            if names.len() > 1 {
                names.push(("all".into(), format!("the server and {} machine(s) at once", names.len() - 1)));
            }
            for g in self.view.status["groups"].as_array().into_iter().flatten() {
                let members: Vec<String> = g["machines"].as_array().into_iter().flatten().filter_map(|m| m.as_str().map(str::to_string)).collect();
                names.push((str_of(&g["name"]), format!("group: {}", members.join(", "))));
            }
            return names
                .into_iter()
                .filter(|(n, _)| n.to_lowercase().starts_with(&typed))
                .map(|(n, d)| (format!("@{n}"), d, format!("{before}@{n} ")))
                .collect();
        }
        if !self.input.starts_with('/') {
            return Vec::new();
        }
        let q = self.input.to_lowercase();
        let word = q.split_whitespace().next().unwrap_or("").to_string();
        let found: Vec<_> = self.view.commands.iter().filter(|c| c.0.to_lowercase().starts_with(&q)).cloned().collect();
        if !found.is_empty() || !q.contains(' ') {
            return found;
        }
        self.view.commands.iter().filter(|c| c.0.split_whitespace().next() == Some(word.as_str())).cloned().collect()
    }

    /// `/attach <path>`: upload a file now; it goes with the next message.
    fn attach(&mut self, arg: &str) {
        self.view.banner = match arg {
            "" => match self.files.len() {
                0 => "nothing attached: /attach <path>".into(),
                _ => format!("attached: {}", self.files.iter().map(|f| f.1.as_str()).collect::<Vec<_>>().join(", ")),
            },
            "clear" => {
                self.files.clear();
                "attachments dropped".into()
            }
            path => match upload(&self.base, &self.token, &lyra_node::expand(path)) {
                Ok((id, name, size)) => {
                    self.files.push((id, name.clone()));
                    format!("attached {name} ({}) — it goes with your next message", size_text(size))
                }
                Err(e) => format!("couldn't attach {path}: {e}"),
            },
        };
    }

    fn submit(&mut self) {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return;
        }
        if text == "/attach" || text.starts_with("/attach ") {
            self.attach(text.trim_start_matches("/attach").trim());
            self.input.clear();
            return;
        }
        // While an approval is open, y / n / a typed out answer it too.
        if let Some(id) = self.approval().and_then(|a| a["id"].as_u64())
            && matches!(text.to_lowercase().as_str(), "y" | "yes" | "n" | "no" | "a" | "always")
        {
            self.send(json!({ "type": "approve", "id": id, "answer": text }));
        } else {
            let files: Vec<&str> = self.files.iter().map(|f| f.0.as_str()).collect();
            self.send(json!({ "type": "send", "text": text, "files": files }));
            self.files.clear();
        }
        self.input.clear();
        self.scroll = None;
        self.palette = 0;
    }
}

/// Most a file may be (the server says the same).
const MAX_UPLOAD: u64 = 25 * 1024 * 1024;

/// Upload a file to the server (`POST /api/files`): its id, name and size.
fn upload(base: &str, token: &str, path: &std::path::Path) -> Result<(String, String, u64), String> {
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > MAX_UPLOAD {
        return Err(format!("it's {}, more than 25 MB", size_text(size)));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "file".into());
    let escaped: String = name.bytes().map(|b| if b.is_ascii_alphanumeric() || b".-_ ".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
    let resp = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| e.to_string())?
        .post(format!("{}/api/files", base.trim_end_matches('/')))
        .bearer_auth(token)
        .header("X-Filename", escaped.replace(' ', "%20"))
        .header("Content-Type", "application/octet-stream")
        .body(bytes)
        .send()
        .map_err(|e| e.to_string())?;
    let ok = resp.status().is_success();
    let v: Value = resp.json().unwrap_or(json!({}));
    if !ok {
        return Err(v["error"].as_str().unwrap_or("the server refused it").to_string());
    }
    Ok((str_of(&v["id"]), str_of(&v["name"]), size))
}

fn size_text(n: u64) -> String {
    match n {
        n if n >= 1024 * 1024 => format!("{:.1} MB", n as f64 / 1048576.0),
        n if n >= 1024 => format!("{} KB", n / 1024),
        n => format!("{n} bytes"),
    }
}

fn role_label(role: &str) -> (&'static str, Color) {
    match role {
        "user" => ("you", Color::Cyan),
        "assistant" => ("lyra", Color::Green),
        "agent" => ("↪ agent", Color::LightBlue),
        "approval" => ("approval", Color::Yellow),
        "info" => ("system", Color::Magenta),
        _ => ("error", Color::Red),
    }
}

fn draw(f: &mut Frame, s: &mut Screen) {
    let approval = approval_box(s);
    let approval_height = approval.as_ref().map_or(0, |p| p.line_count(f.area().width) as u16).min(f.area().height / 2);
    let [header, middle, approval_area, input_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1), Constraint::Length(approval_height), Constraint::Length(3)]).areas(f.area());
    let (chat, panel) = if s.show_panel && middle.width >= 100 {
        let [chat, panel] = Layout::horizontal([Constraint::Min(40), Constraint::Length(36)]).areas(middle);
        (chat, Some(panel))
    } else {
        (middle, None)
    };

    // Header: connection, conversation, what lyra is doing, machines.
    let st = &s.view.status;
    let dot = if s.view.connected { Span::styled("● ", Style::default().fg(Color::Green)) } else { Span::styled("● ", Style::default().fg(Color::Red)) };
    let machines: Vec<String> = st["machines"].as_array().into_iter().flatten().filter_map(|m| m.as_str().map(str::to_string)).collect();
    let mut spans = vec![dot, Span::styled("lyra ", Style::default().bold())];
    if let Some(t) = st["title"].as_str() {
        spans.push(Span::styled(format!("· {} ", truncate(t, 40)), Style::default().fg(Color::DarkGray)));
    }
    let phase = str_of(&st["phase"]);
    let phase_style = if s.approval().is_some() {
        Style::default().fg(Color::Yellow).bold()
    } else if phase.starts_with('↪') {
        Style::default().fg(Color::LightBlue)
    } else if st["waiting"] == true {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    spans.push(Span::styled(phase, phase_style));
    if !machines.is_empty() {
        spans.push(Span::styled(format!("  · machines: {}", machines.join(", ")), Style::default().fg(Color::DarkGray)));
    }
    if !s.view.banner.is_empty() {
        spans.push(Span::styled(format!("  {}", s.view.banner), Style::default().fg(Color::Yellow)));
    }
    f.render_widget(Line::from(spans), header);

    draw_chat(f, s, chat);
    if let Some(panel) = panel {
        draw_panel(f, s, panel);
    }
    if let Some(p) = approval {
        f.render_widget(p, approval_area);
    }
    let title = if s.approval().is_some() {
        Line::from(vec![
            Span::raw(" press "),
            Span::styled("y", Style::default().fg(Color::Green).bold()),
            Span::raw(" allow · "),
            Span::styled("n", Style::default().fg(Color::Red).bold()),
            Span::raw(" deny · "),
            Span::styled("a", Style::default().fg(Color::Yellow).bold()),
            Span::raw(" allow for this session "),
        ])
    } else {
        Line::from(format!(
            " {}{} · Enter send · / commands · @ machines · ^X stop · PgUp PgDn · ^R reasoning · ^B panel · Esc quit ",
            if s.files.is_empty() { String::new() } else { format!("📎 {} · ", s.files.iter().map(|f| f.1.as_str()).collect::<Vec<_>>().join(", ")) },
            s.url
        ))
    };
    let border = if s.approval().is_some() { Style::default().fg(Color::Yellow) } else { Style::default() };
    f.render_widget(Paragraph::new(s.input.as_str()).block(Block::bordered().title(title).border_style(border)), input_area);
    f.set_cursor_position((input_area.x + 1 + s.input.chars().count() as u16, input_area.y + 1));
    draw_palette(f, s, chat);
}

fn draw_chat(f: &mut Frame, s: &mut Screen, area: Rect) {
    let dim = Style::default().fg(Color::DarkGray);
    let mut lines: Vec<Line> = Vec::new();
    if !s.view.ready {
        lines.push(Line::styled(if s.view.connected { "loading…" } else { "connecting…" }, dim.italic()));
    }
    for (i, m) in s.view.messages.iter().enumerate() {
        let role = m["role"].as_str().unwrap_or("info");
        let content = m["content"].as_str().unwrap_or("");
        if role == "tool" || role == "agent_tool" {
            match crate::ui::fleet_lines(content) {
                Some(each) => lines.extend(each.into_iter().map(|(ok, text)| Line::styled(text, if ok { dim } else { Style::default().fg(Color::Red) }))),
                None => lines.push(Line::styled(format!("  ↳ {}", truncate(content, 160)), dim)),
            }
            lines.push(Line::default());
            continue;
        }
        let (label, color) = role_label(role);
        lines.push(Line::from(label.bold().fg(color)));
        let reasoning = m["reasoning"].as_str().unwrap_or("").trim();
        if !reasoning.is_empty() {
            if s.show_reasoning {
                lines.extend(reasoning.lines().map(|l| Line::styled(l.to_string(), dim.italic())));
            } else {
                lines.push(Line::styled(format!("[reasoning hidden · {} lines · Ctrl-R]", reasoning.lines().count()), dim.italic()));
            }
        }
        if matches!(role, "assistant" | "agent") {
            let cache = &mut s.view.rendered[i];
            if cache.as_ref().is_none_or(|(len, _)| *len != content.len()) {
                let base = if role == "agent" { Style::default().fg(Color::Blue) } else { Style::default() };
                *cache = Some((content.len(), crate::markdown::render(content, base)));
            }
            lines.extend(cache.as_ref().map(|c| c.1.clone()).unwrap_or_default());
        } else {
            let style = match role {
                "info" => dim,
                "approval" => Style::default().fg(Color::Yellow),
                "error" => Style::default().fg(Color::Red),
                _ => Style::default(),
            };
            lines.extend(content.lines().map(|l| Line::styled(l.to_string(), style)));
        }
        for t in m["tools"].as_array().into_iter().flatten() {
            lines.push(Line::styled(format!("→ {}", truncate(t.as_str().unwrap_or(""), 160)), dim));
        }
        if let Some(stats) = m["stats"].as_str() {
            lines.push(Line::styled(stats.to_string(), dim));
        }
        let list = |k: &str| m[k].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default();
        if !list("skills").is_empty() {
            lines.push(Line::styled(format!("used skills: {}", list("skills")), Style::default().fg(Color::Magenta)));
        }
        if !list("agents").is_empty() {
            lines.push(Line::styled(format!("handled with: {}", list("agents")), Style::default().fg(Color::LightBlue)));
        }
        lines.push(Line::default());
    }
    let last = s.view.messages.last().and_then(|m| m["role"].as_str()).unwrap_or("");
    if s.view.status["waiting"] == true && matches!(last, "user" | "tool" | "agent" | "approval") {
        lines.push(Line::from("thinking...".italic().dark_gray()));
    }
    let mut block = Block::bordered().title(format!(" lyra · {} ", s.view.status["model"].as_str().unwrap_or("")));
    if s.scroll.is_some() {
        block = block.title_bottom(Line::from(" ↓ more below · PgDn ".dark_gray()).right_aligned());
    }
    let chat = Paragraph::new(Text::from(lines)).block(block).wrap(Wrap { trim: false });
    let total = chat.line_count(area.width);
    s.page = area.height.saturating_sub(2).max(1);
    s.max_scroll = total.saturating_sub(area.height as usize) as u16;
    s.scroll = s.scroll.filter(|&top| top < s.max_scroll);
    f.render_widget(chat.scroll((s.scroll.unwrap_or(s.max_scroll), 0)), area);
}

/// Who's here: devices online, machines (online or not), pairing requests,
/// agents at work.
fn draw_panel(f: &mut Frame, s: &Screen, area: Rect) {
    let st = &s.view.status;
    let dim = Style::default().fg(Color::DarkGray);
    let width = area.width.saturating_sub(2) as usize;
    let mut lines: Vec<Line> = vec![Line::from("Devices online".bold())];
    let online = st["online"].as_array().cloned().unwrap_or_default();
    if online.is_empty() {
        lines.push(Line::styled("  none", dim));
    }
    for d in &online {
        lines.push(Line::from(vec![Span::styled("● ", Style::default().fg(Color::Green)), Span::raw(truncate(&str_of(&d["name"]), width.saturating_sub(2)))]));
    }
    lines.push(Line::default());
    lines.push(Line::from("Machines".bold()));
    let machines = st["machines_detail"].as_array().cloned().unwrap_or_default();
    if machines.is_empty() {
        lines.push(Line::styled("  none paired", dim));
    }
    for m in &machines {
        let on = m["online"] == true;
        let mut text = str_of(&m["name"]);
        if m["update_available"] == true {
            text += " · update";
        }
        lines.push(Line::from(vec![
            Span::styled(if on { "● " } else { "○ " }, Style::default().fg(if on { Color::Green } else { Color::DarkGray })),
            Span::styled(truncate(&text, width.saturating_sub(2)), if on { Style::default() } else { dim }),
        ]));
        if on && let Some(h) = m["hostname"].as_str().filter(|h| !h.is_empty()) {
            lines.push(Line::styled(format!("  {}", truncate(h, width.saturating_sub(2))), dim));
        }
        if on {
            health_lines(&m["health"], width, &mut lines);
        }
    }
    if st["server_health"].is_object() {
        lines.push(Line::raw("● server"));
        health_lines(&st["server_health"], width, &mut lines);
    }
    let pairing = st["pairing"].as_array().cloned().unwrap_or_default();
    if !pairing.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from("Wants to pair".bold().yellow()));
        for p in &pairing {
            lines.push(Line::styled(truncate(&format!("? {} ({})", str_of(&p["name"]), str_of(&p["hostname"])), width), Style::default().fg(Color::Yellow)));
            lines.push(Line::styled(truncate(&format!("  /devices approve {}", str_of(&p["code"])), width), dim));
        }
    }
    // Conversations open on the server; others may be answering at the same time.
    let open = st["conversations"].as_array().cloned().unwrap_or_default();
    if open.len() > 1 {
        lines.push(Line::default());
        lines.push(Line::from("Conversations".bold()));
        for c in &open {
            let here = c["session"] == st["session"];
            let title = c["title"].as_str().filter(|t| !t.is_empty()).unwrap_or("(new)");
            let mark = if c["answering"] == true { "↻ " } else if here { "› " } else { "  " };
            let style = if here { Style::default().fg(Color::Cyan) } else if c["answering"] == true { Style::default().fg(Color::LightBlue) } else { dim };
            lines.push(Line::styled(truncate(&format!("{mark}{title}"), width), style));
        }
        lines.push(Line::styled(truncate("  /sessions · /resume <id>", width), dim));
    }
    // What lyra depends on: only what isn't up is listed.
    if let Some(rows) = st["status"]["rows"].as_array() {
        lines.push(Line::default());
        let bad: Vec<&Value> = rows.iter().filter(|r| matches!(r["state"].as_str(), Some("down" | "degraded"))).collect();
        lines.push(Line::from(vec!["Status ".bold(), if bad.is_empty() { Span::styled("all normal", Style::default().fg(Color::Green)) } else { Span::raw("") }]));
        for r in bad {
            let color = if r["state"] == "down" { Color::Red } else { Color::Yellow };
            lines.push(Line::styled(truncate(&format!("{} {}: {}", if r["state"] == "down" { "✗" } else { "◐" }, str_of(&r["name"]), str_of(&r["detail"])), width), Style::default().fg(color)));
        }
    }
    let routines = st["routines"].as_array().cloned().unwrap_or_default();
    if !routines.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from("Routines".bold()));
        for r in &routines {
            let last = &r["runs"][0];
            let (mark, color) = if r["running"] == true {
                ("↻", Color::LightBlue)
            } else if last.is_null() {
                ("·", Color::DarkGray)
            } else if last["needs_user"] == true {
                ("⚠", Color::Red)
            } else {
                ("✓", Color::Green)
            };
            let next = r["next"].as_str().and_then(|n| chrono::DateTime::parse_from_rfc3339(n).ok()).map_or_else(
                || if r["enabled"] == true { String::new() } else { " · paused".into() },
                |n| format!(" · {}", n.with_timezone(&chrono::Local).format("%a %H:%M")),
            );
            lines.push(Line::from(vec![Span::styled(format!("{mark} "), Style::default().fg(color)), Span::raw(truncate(&format!("{}{next}", str_of(&r["name"])), width.saturating_sub(2)))]));
        }
    }
    lines.push(Line::default());
    lines.push(Line::from("Backup".bold()));
    let backed = if st["backing_up"] == true {
        "backing up…".to_string()
    } else if let Some(made) = st["backup"]["made"].as_str().and_then(|m| chrono::DateTime::parse_from_rfc3339(m).ok()) {
        format!("{} · {}", made.with_timezone(&chrono::Local).format("%m-%d %H:%M"), crate::backup::size_text(st["backup"]["size"].as_u64().unwrap_or(0)))
    } else {
        "none yet · /backup now".into()
    };
    lines.push(Line::styled(truncate(&backed, width), dim));
    if let Some(d) = st["decide"].as_object() {
        lines.push(Line::default());
        lines.push(Line::from("Decisions".bold()));
        lines.push(Line::raw(truncate(&str_of(&d["model"]), width)));
        let to_chat = d["to_chat"].as_u64().unwrap_or(0);
        let text = format!(
            "{} decided · {} ms{}",
            d["decided"].as_u64().unwrap_or(0),
            d["ms"].as_u64().unwrap_or(0),
            if to_chat > 0 { format!(" · {to_chat} to chat") } else { String::new() }
        );
        lines.push(Line::styled(truncate(&text, width), dim));
    }
    let working: Vec<String> = st["agents"].as_array().into_iter().flatten().filter(|a| a["working"] == true).map(|a| str_of(&a["title"])).collect();
    if !working.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from("Working".bold()));
        for a in working {
            lines.push(Line::styled(format!("↪ {a}"), Style::default().fg(Color::LightBlue)));
        }
    }
    f.render_widget(Paragraph::new(lines).block(Block::bordered().title(" Online ")), area);
}

/// A machine's health under its name: the summary, then any problems in red.
fn health_lines(h: &Value, width: usize, lines: &mut Vec<Line<'static>>) {
    if let Some(summary) = h["summary"].as_str().filter(|s| !s.is_empty()) {
        lines.push(Line::styled(format!("  {}", truncate(summary, width.saturating_sub(2))), Style::default().fg(Color::DarkGray)));
    }
    for p in h["problems"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        lines.push(Line::styled(format!("  ⚠ {}", truncate(p, width.saturating_sub(4))), Style::default().fg(Color::Red)));
    }
}

/// The open approval, in full: who, what kind of thing, exactly what, why, keys.
fn approval_box(s: &Screen) -> Option<Paragraph<'static>> {
    let a = s.approval()?;
    let dangerous = a["dangerous"] == true;
    let color = if dangerous { Color::Red } else { Color::Yellow };
    let bold = Style::default().bold();
    let mut lines = vec![Line::from(vec![
        Span::styled(str_of(&a["agent"]), bold.fg(Color::LightBlue)),
        Span::styled(format!(" wants to {}:", str_of(&a["what"])), bold),
    ])];
    let detail = str_of(&a["detail"]);
    let many = detail.lines().count() > 1;
    for l in detail.lines() {
        let style = if many && l.starts_with("in ") { Style::default().fg(Color::DarkGray) } else { Style::default().fg(Color::Cyan) };
        lines.push(Line::styled(format!("  {l}"), style));
    }
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled(if dangerous { "Risk: " } else { "Why it asks: " }, bold.fg(color)),
        Span::styled(str_of(&a["why"]), Style::default().fg(color)),
    ]));
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled(" y ", Style::default().fg(Color::Black).bg(Color::Green).bold()),
        Span::raw(" allow once    "),
        Span::styled(" n ", Style::default().fg(Color::Black).bg(Color::Red).bold()),
        Span::raw(" deny    "),
        Span::styled(" a ", Style::default().fg(Color::Black).bg(Color::Yellow).bold()),
        Span::raw(" allow this exact action for the rest of the session"),
    ]));
    let n = s.view.status["approvals"].as_array().map_or(0, Vec::len);
    let title = format!(" Approval needed{} ", if n > 1 { format!(" · 1 of {n}") } else { String::new() });
    Some(Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }).block(Block::bordered().title(Line::from(Span::styled(title, bold.fg(color)))).border_style(Style::default().fg(color))))
}

fn draw_palette(f: &mut Frame, s: &Screen, area: Rect) {
    let entries = s.palette_entries();
    if entries.is_empty() || area.height < 5 {
        return;
    }
    let rows = entries.len().min(12).min(area.height as usize - 2);
    let selected = s.palette.min(entries.len() - 1);
    let first = selected.saturating_sub(rows - 1);
    let usage_width = entries.iter().map(|e| e.0.chars().count()).max().unwrap_or(0).min(44);
    let width = area.width.min(120);
    let lines: Vec<Line> = entries
        .iter()
        .enumerate()
        .skip(first)
        .take(rows)
        .map(|(i, e)| {
            let usage = format!(" {:<usage_width$}  ", truncate(&e.0, usage_width));
            let style = if i == selected { Style::default().bg(Color::DarkGray) } else { Style::default() };
            Line::from(vec![Span::styled(usage, style.fg(Color::Cyan)), Span::styled(e.1.clone(), style.fg(Color::Gray))])
        })
        .collect();
    let height = rows as u16 + 2;
    let popup = Rect { x: area.x, y: area.y + area.height - height, width, height };
    f.render_widget(Clear, popup);
    f.render_widget(Paragraph::new(lines).block(Block::bordered().title(format!(" {} · {} · ↑↓ Tab Esc ", if s.mentioning().is_some() { "machines" } else { "commands" }, entries.len())).border_style(Style::default().fg(Color::Cyan))), popup);
}

fn ui_loop(terminal: &mut DefaultTerminal, s: &mut Screen, incoming: mpsc::Receiver<Incoming>) -> std::io::Result<()> {
    loop {
        while let Ok(msg) = incoming.try_recv() {
            match msg {
                Incoming::Connected => {
                    s.view.connected = true;
                    s.view.banner.clear();
                }
                Incoming::Lost(why) => {
                    s.view.connected = false;
                    s.view.ready = false;
                    s.view.banner = why;
                }
                Incoming::Message(v) => s.view.apply(v),
            }
        }
        terminal.draw(|f| draw(f, s))?;
        if !event::poll(Duration::from_millis(30))? {
            continue;
        }
        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // An approval is open: y / n / a answer it at once.
        if let Some(id) = s.approval().and_then(|a| a["id"].as_u64())
            && s.input.is_empty()
            && !ctrl
            && let KeyCode::Char(c @ ('y' | 'Y' | 'n' | 'N' | 'a' | 'A')) = key.code
        {
            s.send(json!({ "type": "approve", "id": id, "answer": c.to_string() }));
            continue;
        }
        let palette = s.palette_entries();
        if !palette.is_empty() {
            let n = palette.len();
            let complete = |s: &mut Screen| {
                let c = &palette[s.palette.min(n - 1)];
                let send = !c.2.ends_with(' ') && c.2 == s.input.trim();
                s.input = c.2.clone();
                s.palette = 0;
                if send {
                    s.submit();
                }
            };
            match key.code {
                KeyCode::Esc => {
                    s.palette_hidden = true;
                    continue;
                }
                KeyCode::Up => {
                    s.palette = (s.palette + n - 1) % n;
                    continue;
                }
                KeyCode::Down => {
                    s.palette = (s.palette + 1) % n;
                    continue;
                }
                KeyCode::Tab => {
                    complete(s);
                    continue;
                }
                KeyCode::Enter if s.mentioning().is_some() || (!s.input.contains(' ') && !s.view.commands.iter().any(|c| c.0.split_whitespace().next() == Some(s.input.trim()))) => {
                    complete(s);
                    continue;
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Esc => return Ok(()),
            KeyCode::Char('c') if ctrl => return Ok(()),
            KeyCode::Char('r') if ctrl => s.show_reasoning = !s.show_reasoning,
            KeyCode::Char('b') if ctrl => s.show_panel = !s.show_panel,
            KeyCode::Char('x') if ctrl => s.send(json!({ "type": "stop" })),
            KeyCode::Up => s.scroll = Some(s.scroll.unwrap_or(s.max_scroll).saturating_sub(1)),
            KeyCode::Down => s.scroll = s.scroll.map(|t| t + 1).filter(|&t| t < s.max_scroll),
            KeyCode::PageUp => s.scroll = Some(s.scroll.unwrap_or(s.max_scroll).saturating_sub(s.page)),
            KeyCode::PageDown => s.scroll = s.scroll.map(|t| t + s.page).filter(|&t| t < s.max_scroll),
            KeyCode::Enter => s.submit(),
            KeyCode::Backspace => {
                s.input.pop();
                s.palette = 0;
                s.palette_hidden = false;
            }
            KeyCode::Char(c) => {
                s.input.push(c);
                s.palette = 0;
                s.palette_hidden = false;
            }
            _ => {}
        }
    }
}

pub const USAGE: &str = "\
lyra connect — the terminal UI for a lyra server (lyra serve)

  lyra connect --pair <code> --url <https://lyra…> [--name desk]   pair this terminal (code from `lyra pair`)
  lyra connect                                                     open it (also plain `lyra` once paired)

The token is kept in ~/.config/lyra/remote.toml; nothing else is stored here.";

pub fn main(args: &[String]) {
    let flag = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return;
    }
    lyra_node::tls_provider();
    let path = config_path().expect("a home directory");
    if let Some(code) = flag("--pair") {
        let existing: Option<RemoteConfig> = std::fs::read_to_string(&path).ok().and_then(|t| toml::from_str(&t).ok());
        let Some(url) = flag("--url").or(existing.map(|c| c.url)) else {
            eprintln!("lyra connect: which server? --url https://lyra.example.com");
            std::process::exit(2);
        };
        let name = flag("--name").unwrap_or_else(|| "terminal".into());
        match lyra_node::pair(&url, &code, &name, "device") {
            Ok(token) => {
                let config = RemoteConfig { url: url.trim_end_matches('/').to_string(), token, name };
                if let Err(e) = toml::to_string_pretty(&config).map_err(|e| e.to_string()).and_then(|t| lyra_node::write_private(&path, &t)) {
                    eprintln!("lyra connect: couldn't save {}: {e}", path.display());
                    std::process::exit(1);
                }
                println!("paired; run `lyra connect` (or just `lyra`)");
                return;
            }
            Err(e) => {
                eprintln!("lyra connect: {e}");
                std::process::exit(1);
            }
        }
    }
    let config: RemoteConfig = match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| toml::from_str(&t).map_err(|e| e.to_string())) {
        Ok(c) => c,
        Err(_) => {
            eprintln!("lyra connect: not paired yet.\n\n{USAGE}");
            std::process::exit(1);
        }
    };
    run(config);
}

/// On the server itself, while `lyra serve` runs: connect to it with this
/// home's own terminal device (made once, kept in `web/terminal.toml`).
pub fn local(home: &std::path::Path, listen: &str) {
    lyra_node::tls_provider();
    let path = home.join("web").join("terminal.toml");
    let addr = listen.replace("0.0.0.0", "127.0.0.1").replace("[::]", "[::1]");
    let url = format!("http://{addr}");
    let devices = match lyra_web::Devices::open(&home.join("web")) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("lyra: {e}");
            std::process::exit(1);
        }
    };
    let saved: Option<RemoteConfig> = std::fs::read_to_string(&path).ok().and_then(|t| toml::from_str(&t).ok());
    let config = match saved.filter(|c| devices.authenticate(&c.token).is_some()) {
        Some(mut c) => {
            c.url = url;
            c
        }
        None => match devices.add("server-terminal", "device") {
            Ok((_, token)) => {
                let c = RemoteConfig { url, token, name: "server-terminal".into() };
                if let Err(e) = toml::to_string_pretty(&c).map_err(|e| e.to_string()).and_then(|t| lyra_node::write_private(&path, &t)) {
                    eprintln!("lyra: couldn't save {}: {e}", path.display());
                }
                c
            }
            Err(e) => {
                eprintln!("lyra: {e}");
                std::process::exit(1);
            }
        },
    };
    run(config);
}

/// Open the terminal UI on a server.
pub fn run(config: RemoteConfig) {
    lyra_node::tls_provider();
    let url = config.url.clone();
    let token = config.token.clone();
    let (to_screen, incoming) = mpsc::channel();
    let (out, from_screen) = tokio::sync::mpsc::unbounded_channel();
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    rt.spawn(connection(config, to_screen, from_screen));
    let mut screen = Screen {
        view: View::default(),
        input: String::new(),
        scroll: None,
        max_scroll: 0,
        page: 1,
        show_reasoning: false,
        palette: 0,
        palette_hidden: false,
        show_panel: true,
        out,
        url: url.trim_start_matches("https://").trim_start_matches("http://").to_string(),
        base: url.clone(),
        token,
        files: Vec::new(),
    };
    ratatui::run(|terminal| ui_loop(terminal, &mut screen, incoming)).expect("terminal error");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_uploads_with_the_device_token() {
        let _guard = crate::decide::tests::LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let (url, server) = crate::decide::tests::fake(json!({ "id": "abc123", "name": "notes v2.txt", "size": 5 }));
        let file = std::env::temp_dir().join(format!("lyra-attach-{}", std::process::id())).join("notes v2.txt");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "hello").unwrap();
        let base = url.trim_end_matches("/v1");
        assert_eq!(upload(base, "tok", &file).unwrap(), ("abc123".into(), "notes v2.txt".into(), 5));
        let request = server.join().unwrap().to_lowercase();
        assert!(request.starts_with("post /api/files"), "{request}");
        assert!(request.contains("authorization: bearer tok") && request.contains("x-filename: notes%20v2.txt"), "{request}");
        assert!(request.ends_with("hello"));
        assert!(upload(base, "tok", &file.with_file_name("missing")).is_err());
        let _ = std::fs::remove_dir_all(file.parent().unwrap());
    }

    #[test]
    fn the_view_follows_the_servers_updates() {
        let mut v = View::default();
        v.apply(json!({ "type": "add", "seq": 1, "index": 0, "message": {} }));
        assert!(v.messages.is_empty(), "nothing before the snapshot");
        v.apply(json!({ "type": "snapshot", "seq": 5, "messages": [{ "role": "user", "content": "hi" }], "status": { "phase": "● idle" },
            "commands": [{ "usage": "/new", "description": "start over", "completion": "/new" }] }));
        v.apply(json!({ "type": "add", "seq": 4, "index": 1, "message": { "role": "assistant", "content": "old" } }));
        assert_eq!(v.messages.len(), 1, "updates older than the snapshot are skipped");
        v.apply(json!({ "type": "add", "seq": 6, "index": 1, "message": { "role": "assistant", "content": "Hel", "reasoning": "" } }));
        v.apply(json!({ "type": "append", "seq": 7, "index": 1, "text": "lo", "reasoning": "" }));
        assert_eq!(v.messages[1]["content"], "Hello");
        v.apply(json!({ "type": "status", "seq": 8, "status": { "approvals": [{ "id": 3 }] } }));
        assert_eq!(v.status["approvals"][0]["id"], 3);
        v.apply(json!({ "type": "reset", "seq": 9, "messages": [] }));
        assert!(v.messages.is_empty() && v.rendered.is_empty());
        assert_eq!(v.commands[0].2, "/new");
    }
}

//! Drawing: the chat, the side panels (session, agent, memory, activity) and
//! the input box.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Paragraph, Wrap};

use crate::stats::{Pricing, Stats, percent, secs, thousands};
use crate::{App, Level, Phase};

/// Side panels need at least this much width to be useful next to the chat.
const MIN_WIDTH_FOR_PANELS: u16 = 100;
const PANEL_WIDTH: u16 = 46;

const HELP: &str = " Enter send · /help · ↑↓ PgUp PgDn scroll · ^R reasoning · ^B panels · ^L reload · Esc quit ";

pub fn draw(f: &mut Frame, app: &mut App) {
    let [main, input_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).areas(f.area());

    if app.show_panels && main.width >= MIN_WIDTH_FOR_PANELS {
        let [chat_area, side] =
            Layout::horizontal([Constraint::Min(40), Constraint::Length(PANEL_WIDTH)]).areas(main);
        draw_chat(f, app, chat_area);
        draw_panels(f, app, side);
    } else {
        // No room for panels: keep the session numbers as a status line.
        let [chat_area, status_area] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(main);
        draw_chat(f, app, chat_area);
        let line = format!(" {} · {}", phase_text(app), session_line(app));
        f.render_widget(Line::from(line.dark_gray()), status_area);
    }

    let input = Paragraph::new(app.input.as_str()).block(Block::bordered().title(HELP));
    f.render_widget(input, input_area);
    f.set_cursor_position((input_area.x + 1 + app.input.chars().count() as u16, input_area.y + 1));
}

fn draw_chat(f: &mut Frame, app: &mut App, area: Rect) {
    let dim = Style::default().fg(Color::DarkGray);
    let mut lines: Vec<Line> = Vec::new();
    for m in &app.messages {
        if m.role == "tool" {
            // Tool results: one dim line, under the call that produced them.
            lines.push(Line::styled(format!("  ↳ {}", truncate(&m.content, 160)), dim));
            lines.push(Line::default());
            continue;
        }
        let (label, color) = match m.role.as_str() {
            "user" => ("you", Color::Cyan),
            "assistant" => ("lyra", Color::Green),
            "info" => ("system", Color::Magenta),
            _ => ("error", Color::Red),
        };
        lines.push(Line::from(label.bold().fg(color)));
        let reasoning = m.reasoning.trim();
        if !reasoning.is_empty() {
            let thinking = dim.italic();
            if app.show_reasoning {
                lines.extend(reasoning.lines().map(|l| Line::styled(l.to_string(), thinking)));
            } else {
                let n = reasoning.lines().count();
                let plural = if n == 1 { "" } else { "s" };
                let note = format!("[reasoning hidden · {n} line{plural} · Ctrl-R]");
                lines.push(Line::styled(note, thinking));
            }
            if !m.content.is_empty() {
                lines.push(Line::default());
            }
        }
        let body = if m.role == "info" { dim } else { Style::default() };
        lines.extend(m.content.lines().map(|l| Line::styled(l.to_string(), body)));
        for call in &m.tool_calls {
            let text = format!("→ {} {}", call.function.name, truncate(&call.function.arguments, 160));
            lines.push(Line::styled(text, dim));
        }
        if let Some(stats) = &m.stats {
            lines.push(Line::styled(reply_stats(stats, &app.pricing), dim));
        }
        lines.push(Line::default());
    }
    // Show "thinking..." until the first token arrives, and while waiting on a tool.
    if app.waiting && app.messages.last().is_some_and(|m| m.role == "user" || m.role == "tool") {
        lines.push(Line::from("thinking...".italic().dark_gray()));
    }

    let title = format!(" lyra · {} ", app.model);
    let mut block = Block::bordered().title(title);
    if app.scroll.is_some() {
        block = block.title_bottom(Line::from(" ↓ more below · PgDn ".dark_gray()).right_aligned());
    }
    let chat = Paragraph::new(Text::from(lines)).block(block).wrap(Wrap { trim: false });
    // line_count includes the block's borders, as does the area height.
    let total = chat.line_count(area.width);
    app.page = area.height.saturating_sub(2).max(1);
    app.max_scroll = total.saturating_sub(area.height as usize) as u16;
    // Clamp after resizes / reasoning toggles; reaching the bottom resumes following.
    app.scroll = app.scroll.filter(|&top| top < app.max_scroll);
    let top = app.scroll.unwrap_or(app.max_scroll);
    f.render_widget(chat.scroll((top, 0)), area);
}

fn draw_panels(f: &mut Frame, app: &App, area: Rect) {
    // Content width inside a bordered panel.
    let width = area.width.saturating_sub(2) as usize;
    let session = session_panel(app, width);
    let agent = agent_panel(app, width);
    let (skills_title, skills) = skills_panel(app, width);
    let [session_area, agent_area, memory_area, skills_area, activity_area] = Layout::vertical([
        Constraint::Length(session.len() as u16 + 2),
        Constraint::Length(agent.len() as u16 + 2),
        Constraint::Fill(1),
        Constraint::Length(skills.len() as u16 + 2),
        Constraint::Fill(1),
    ])
    .areas(area);

    f.render_widget(Paragraph::new(session).block(Block::bordered().title(" Session ")), session_area);
    f.render_widget(Paragraph::new(agent).block(Block::bordered().title(" Agent ")), agent_area);
    draw_memory(f, app, memory_area, width);
    let block = Block::bordered().title(skills_title);
    f.render_widget(Paragraph::new(skills).block(block), skills_area);
    draw_activity(f, app, activity_area, width);
}

fn session_panel(app: &App, width: usize) -> Vec<Line<'static>> {
    let t = &app.totals;
    let approx = if t.estimated { "~" } else { "" };
    let p = &app.pricing;
    let mut lines = vec![
        Line::from(vec![label("state"), phase_span(app)]),
        Line::from(vec![label("model"), Span::raw(truncate(&app.model, width.saturating_sub(8)))]),
        Line::from(vec![label("server"), Span::raw(truncate(&host(&app.base_url), width.saturating_sub(8)))]),
        Line::from(vec![
            label("replies"),
            Span::raw(t.replies.to_string()),
            Span::raw(t.avg_ttft().map(|d| format!(" · avg ttft {}", secs(d))).unwrap_or_default()),
        ]),
    ];
    if let Some(last) = app.messages.iter().rev().find_map(|m| m.stats.as_ref()) {
        let mut text = last.ttft.map(|d| format!("ttft {}", secs(d))).unwrap_or_default();
        if let Some(tps) = last.tokens_per_sec() {
            text += &format!(" · {tps:.1} tok/s");
        }
        lines.push(Line::from(vec![label("last"), Span::raw(text)]));
    }
    lines.push(Line::from(vec![
        label("tokens"),
        Span::raw(format!(
            "in {approx}{} · out {approx}{}",
            thousands(t.input),
            thousands(t.output)
        )),
    ]));
    let hit = percent(t.cached, t.input).map(|r| format!("{r:.0}% hit")).unwrap_or("—".into());
    lines.push(Line::from(vec![
        label("cache"),
        Span::raw(format!("{hit} · saved {}", p.format(p.savings(t.cached)))),
    ]));
    lines.push(Line::from(vec![
        label("cost"),
        Span::raw(format!("{approx}{}", p.format(p.cost(t.input, t.cached, t.output)))),
    ]));
    lines
}

fn agent_panel(app: &App, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let prompt = app.system_prompt.as_ref().map_or(0, |p| p.len() / 4);
    lines.push(Line::from(vec![label("prompt"), Span::raw(format!("~{} tokens", thousands(prompt as u64)))]));
    if app.context_files.is_empty() {
        lines.push(Line::from(vec![label("context"), "no SOUL/USER/AGENT.md".dark_gray()]));
    }
    for (name, path) in &app.context_files {
        lines.push(Line::from(vec![label(name), Span::raw(truncate_start(path, width.saturating_sub(8)))]));
    }
    let tools = if app.tools.is_some() { "memory ×4".into() } else { "none".dark_gray() };
    lines.push(Line::from(vec![label("tools"), tools]));
    match &app.model_status {
        None => lines.push(Line::from(vec![label("models"), "checking…".dark_gray()])),
        Some(results) if results.is_empty() => {
            lines.push(Line::from(vec![label("models"), "no embedding/reranker".dark_gray()]));
        }
        Some(results) => {
            for result in results {
                let (mark, text) = match result {
                    Ok(t) => ("✓ ".green(), t),
                    Err(t) => ("✗ ".red(), t),
                };
                lines.push(Line::from(vec![mark, Span::raw(truncate(text, width.saturating_sub(2)))]));
            }
        }
    }
    lines
}

fn draw_memory(f: &mut Frame, app: &App, area: Rect, width: usize) {
    let dim = Style::default().fg(Color::DarkGray);
    let mut title = " Memory ".to_string();
    let mut lines: Vec<Line> = Vec::new();
    match (&app.memory_status, &app.memory) {
        (Err(e), _) => lines.push(Line::from(truncate(e, width).red())),
        // Memory turned off in config: say so rather than waiting for a snapshot.
        (Ok(off), None) if app.tools.is_none() => lines.push(Line::styled(truncate(off, width), dim)),
        (Ok(_), None) => lines.push(Line::styled("loading…", dim)),
        (Ok(_), Some(Err(e))) => lines.push(Line::from(truncate(e, width).red())),
        (Ok(path), Some(Ok(snapshot))) => {
            title = format!(" Memory · {} ", thousands(snapshot.total));
            lines.push(Line::styled(truncate_start(path, width), dim));
            if snapshot.total == 0 {
                lines.push(Line::styled("empty — ask lyra to remember something", dim));
            } else {
                let scopes: Vec<String> =
                    snapshot.scopes.iter().map(|(s, n)| format!("{s} {n}")).collect();
                lines.push(Line::from(truncate(&scopes.join(" · "), width).cyan()));
                for m in &snapshot.recent {
                    let scope = format!("[{}] ", m.scope);
                    let rest = width.saturating_sub(scope.chars().count() + 2);
                    lines.push(Line::from(vec![
                        "• ".dark_gray(),
                        Span::styled(scope, dim),
                        Span::raw(truncate(&m.content, rest)),
                    ]));
                }
            }
        }
    }
    f.render_widget(Paragraph::new(lines).block(Block::bordered().title(title)), area);
}

/// Most skill lines shown; /skills lists everything.
const MAX_SKILL_LINES: usize = 6;

fn skills_panel(app: &App, width: usize) -> (String, Vec<Line<'static>>) {
    let dim = Style::default().fg(Color::DarkGray);
    let reviewing = if app.reviewing { " · reviewing…" } else { "" };
    let mut lines = Vec::new();
    let snapshot = match (&app.learning_status, &app.skills) {
        (Err(e), _) => return (" Skills ".into(), vec![Line::from(truncate(e, width).red())]),
        (Ok(_), None) => return (" Skills ".into(), vec![Line::styled("loading…", dim)]),
        (Ok(_), Some(Err(e))) => return (" Skills ".into(), vec![Line::from(truncate(e, width).red())]),
        (Ok(_), Some(Ok(snapshot))) => snapshot,
    };
    let title = format!(" Skills · {} active{reviewing} ", snapshot.active.len());
    let mode = format!("{:?}", snapshot.mode).to_lowercase();
    let mut summary = format!("mode {mode}");
    if !snapshot.proposed.is_empty() {
        summary += &format!(" · {} to review (/skills)", snapshot.proposed.len());
    }
    if snapshot.rejected > 0 {
        summary += &format!(" · {} rejected", snapshot.rejected);
    }
    lines.push(Line::styled(truncate(&summary, width), dim));
    for skill in &snapshot.proposed {
        let text = format!("{} {}", crate::learn::short(skill), skill.name);
        lines.push(Line::from(vec!["? ".yellow(), Span::raw(truncate(&text, width.saturating_sub(2)))]));
    }
    for skill in &snapshot.active {
        lines.push(Line::from(vec!["✓ ".green(), Span::raw(truncate(&skill.name, width.saturating_sub(2)))]));
    }
    if snapshot.proposed.is_empty() && snapshot.active.is_empty() {
        lines.push(Line::styled("none yet — corrections become proposals", dim));
    }
    if lines.len() > MAX_SKILL_LINES {
        let more = lines.len() - (MAX_SKILL_LINES - 1);
        lines.truncate(MAX_SKILL_LINES - 1);
        lines.push(Line::styled(format!("… {more} more (/skills)"), dim));
    }
    (title, lines)
}

fn draw_activity(f: &mut Frame, app: &App, area: Rect, width: usize) {
    // Newest at the bottom; show as many as fit.
    let rows = area.height.saturating_sub(2) as usize;
    let lines: Vec<Line> = app.activity[app.activity.len().saturating_sub(rows)..]
        .iter()
        .map(|a| {
            let style = match a.level {
                Level::Info => Style::default(),
                Level::Tool => Style::default().fg(Color::Yellow),
                Level::Learn => Style::default().fg(Color::Magenta),
                Level::Error => Style::default().fg(Color::Red),
            };
            Line::from(vec![
                Span::styled(format!("{} ", a.time), Style::default().fg(Color::DarkGray)),
                Span::styled(truncate(&a.text, width.saturating_sub(9)), style),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines).block(Block::bordered().title(" Activity ")), area);
}

/// Fixed-width key for the panels' `key value` rows.
fn label(text: &str) -> Span<'static> {
    Span::styled(format!("{text:<8}"), Style::default().fg(Color::DarkGray))
}

fn phase_span(app: &App) -> Span<'static> {
    let text = phase_text(app);
    match app.phase {
        Phase::Idle => text.dark_gray(),
        Phase::Tools(_) => text.yellow(),
        _ => text.green(),
    }
}

fn phase_text(app: &App) -> String {
    let since = secs(app.phase_since.elapsed());
    match &app.phase {
        Phase::Idle => "● idle".into(),
        Phase::Waiting => format!("◌ waiting {since}"),
        Phase::Thinking => format!("◐ thinking {since}"),
        Phase::Streaming => format!("▸ streaming {since}"),
        Phase::Tools(names) => format!("⚙ {names}"),
    }
}

/// Compact session totals for the status line when panels are hidden.
fn session_line(app: &App) -> String {
    let t = &app.totals;
    let p = &app.pricing;
    format!(
        "{} replies · in {} · out {} · cache {} · {}",
        t.replies,
        thousands(t.input),
        thousands(t.output),
        percent(t.cached, t.input).map(|r| format!("{r:.0}%")).unwrap_or("—".into()),
        p.format(p.cost(t.input, t.cached, t.output)),
    )
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

/// `http://host:port/v1` -> `host:port`.
fn host(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split('/').next().unwrap_or(rest).to_string()
}

/// Last `max` characters, for paths where the end matters most.
fn truncate_start(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let tail: String = text.chars().skip(count - (max - 1)).collect();
    format!("…{tail}")
}

/// First `max` characters on one line, with an ellipsis if cut.
fn truncate(text: &str, max: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if max == 0 {
        return String::new();
    }
    match flat.char_indices().nth(max - 1) {
        Some((i, _)) if flat.chars().count() > max => format!("{}…", &flat[..i]),
        _ => flat,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_fits_within_max() {
        assert_eq!(truncate("hello world", 20), "hello world");
        assert_eq!(truncate("hello world", 5), "hell…");
        assert_eq!(truncate("hello", 5), "hello");
        assert_eq!(truncate("a\nb   c", 10), "a b c");
        assert_eq!(truncate("héllo wörld", 6), "héllo…");
        assert_eq!(truncate("abc", 0), "");
    }

    #[test]
    fn truncate_start_keeps_the_end() {
        assert_eq!(truncate_start("~/a/b/memory.db", 20), "~/a/b/memory.db");
        assert_eq!(truncate_start("~/a/b/memory.db", 10), "…memory.db");
    }

    #[test]
    fn host_strips_scheme_and_path() {
        assert_eq!(host("http://172.99.99.11:8181/v1"), "172.99.99.11:8181");
        assert_eq!(host("localhost:11434"), "localhost:11434");
    }
}

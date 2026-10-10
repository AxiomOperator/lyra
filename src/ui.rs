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

const HELP: &str = " Enter send · / commands · ↑↓ PgUp PgDn scroll · ^X stop · ^R reasoning · ^B panels · ^L reload · Esc quit ";

pub fn draw(f: &mut Frame, app: &mut App) {
    // An agent waiting for approval gets its own box above the input, so the
    // whole question is always on screen (never scrolled or cut off).
    let approval = approval_box(app);
    let approval_height = approval.as_ref().map_or(0, |p| p.line_count(f.area().width) as u16).min(f.area().height / 2);
    let [main, approval_area, input_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(approval_height), Constraint::Length(3)]).areas(f.area());

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

    if let Some(approval) = approval {
        f.render_widget(approval, approval_area);
    }
    let input = if app.approvals.is_empty() {
        Paragraph::new(app.input.as_str()).block(Block::bordered().title(HELP))
    } else {
        let keys = Line::from(vec![
            Span::raw(" press "),
            Span::styled("y", Style::default().fg(Color::Green).bold()),
            Span::raw(" allow · "),
            Span::styled("n", Style::default().fg(Color::Red).bold()),
            Span::raw(" deny · "),
            Span::styled("a", Style::default().fg(Color::Yellow).bold()),
            Span::raw(" allow for this session "),
        ]);
        Paragraph::new(app.input.as_str()).block(Block::bordered().title(keys).border_style(Style::default().fg(Color::Yellow)))
    };
    f.render_widget(input, input_area);
    draw_palette(f, app, main);
    f.set_cursor_position((input_area.x + 1 + app.input.chars().count() as u16, input_area.y + 1));
}

/// The oldest pending approval: who asks, what kind of thing, exactly what,
/// why it needs a yes, and the keys.
fn approval_box(app: &App) -> Option<Paragraph<'static>> {
    let r = app.approvals.first()?;
    let color = if r.dangerous { Color::Red } else { Color::Yellow };
    let bold = Style::default().bold();
    let mut lines = vec![Line::from(vec![Span::styled(r.agent.clone(), bold.fg(Color::LightBlue)), Span::styled(format!(" wants to {}:", r.what), bold)])];
    for l in r.detail.lines() {
        let style = if l.starts_with("in ") && r.detail.lines().count() > 1 { Style::default().fg(Color::DarkGray) } else { Style::default().fg(Color::Cyan) };
        lines.push(Line::styled(format!("  {l}"), style));
    }
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled(if r.dangerous { "Risk: " } else { "Why it asks: " }, bold.fg(color)),
        Span::styled(r.why.clone(), Style::default().fg(color)),
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
    let more = if app.approvals.len() > 1 { format!(" · 1 of {}", app.approvals.len()) } else { String::new() };
    let title = Line::from(Span::styled(format!(" Approval needed{more} "), bold.fg(color)));
    Some(
        Paragraph::new(Text::from(lines))
            .wrap(Wrap { trim: false })
            .block(Block::bordered().title(title).border_style(Style::default().fg(color))),
    )
}

/// Most commands the palette shows at once (it scrolls with the selection).
const PALETTE_ROWS: usize = 12;

/// The commands matching what's typed, just above the input line.
fn draw_palette(f: &mut Frame, app: &App, area: Rect) {
    let entries = app.palette_entries();
    if entries.is_empty() || area.height < 5 {
        return;
    }
    let rows = entries.len().min(PALETTE_ROWS).min(area.height as usize - 2);
    let selected = app.palette.min(entries.len() - 1);
    let first = selected.saturating_sub(rows - 1);
    let usage_width = entries.iter().map(|e| e.usage.chars().count()).max().unwrap_or(0).min(44);
    let width = area.width.min(120);
    let inner = width.saturating_sub(2) as usize;
    let lines: Vec<Line> = entries
        .iter()
        .enumerate()
        .skip(first)
        .take(rows)
        .map(|(i, e)| {
            let usage = format!(" {:<usage_width$}  ", truncate(&e.usage, usage_width));
            let description = truncate(&e.description, inner.saturating_sub(usage.chars().count()));
            let (u, d) = if i == selected {
                let style = Style::default().bg(Color::DarkGray);
                (Span::styled(usage, style.fg(Color::Cyan).bold()), Span::styled(format!("{description:<width$}", width = inner.saturating_sub(usage_width + 3)), style))
            } else {
                (Span::styled(usage, Style::default().fg(Color::Cyan)), Span::styled(description, Style::default().fg(Color::DarkGray)))
            };
            Line::from(vec![u, d])
        })
        .collect();
    let height = rows as u16 + 2;
    let popup = Rect { x: area.x, y: area.y + area.height - height, width, height };
    let title = format!(" commands · {} · ↑↓ Tab Esc ", entries.len());
    f.render_widget(ratatui::widgets::Clear, popup);
    f.render_widget(Paragraph::new(lines).block(Block::bordered().title(title).border_style(Style::default().fg(Color::Cyan))), popup);
}

/// A tool result that reads better as several lines: a fleet run (a line per
/// machine) or a coding job (live steps, then what it changed).
pub fn tool_lines(content: &str) -> Option<Vec<(bool, String)>> {
    fleet_lines(content).or_else(|| coding_lines(content))
}

/// A coding job: its latest steps while it runs, then the harness, what changed and its summary.
pub fn coding_lines(content: &str) -> Option<Vec<(bool, String)>> {
    let v: serde_json::Value = serde_json::from_str(content).ok()?;
    if let Some(steps) = v["progress"].as_array() {
        let mut out: Vec<(bool, String)> = steps.iter().filter(|e| matches!(e["kind"].as_str(), Some("harness" | "handover"))).map(|e| (true, format!("  ↳ {}", e["text"].as_str().unwrap_or("")))).collect();
        for e in steps.iter().filter(|e| matches!(e["kind"].as_str(), Some("tool" | "error"))).rev().take(3).collect::<Vec<_>>().into_iter().rev() {
            out.push((e["kind"] != "error", format!("    · {}", truncate(e["text"].as_str().unwrap_or(""), 120))));
        }
        return Some(out);
    }
    let harness = v["harness"].as_str()?;
    v["dir"].as_str()?;
    let name = match harness {
        "claude" => "Claude Code",
        "opencode" => "OpenCode",
        h => h,
    };
    let ok = v["ok"] == true;
    let files = v["files"].as_array().map_or(0, Vec::len);
    let mut out = vec![(ok, format!(
        "  ↳ {name} {} · {} file{} · {}{}",
        if ok { "✓" } else { "✗" },
        files,
        if files == 1 { "" } else { "s" },
        v["diff_stat"].as_str().filter(|d| !d.is_empty()).unwrap_or("no diff"),
        v["handed_over_from"]["harness"].as_str().map_or(String::new(), |h| format!(" · took over from {h}"))
    ))];
    let said = v["summary"].as_str().filter(|t| !t.is_empty()).or(v["error"].as_str()).unwrap_or("");
    for line in said.lines().filter(|l| !l.trim().is_empty()).take(3) {
        out.push((ok, format!("    {}", truncate(line, 140))));
    }
    Some(out)
}

/// A `fleet_run` result as a line per machine: (worked, text).
pub fn fleet_lines(content: &str) -> Option<Vec<(bool, String)>> {
    let v: serde_json::Value = serde_json::from_str(content).ok()?;
    let results = v["results"].as_array().filter(|r| r.iter().all(|x| x["machine"].is_string()))?;
    let mut out = vec![(v["failed"] == 0, format!("  ↳ {} machines: {} ok, {} failed", v["machines"], v["ok"], v["failed"]))];
    for r in results {
        let ok = r["ok"] == true;
        let first = |k: &str| r[k].as_str().and_then(|t| t.lines().map(str::trim).find(|l| !l.is_empty())).map(str::to_string);
        let what = r["error"].as_str().map(str::to_string).or_else(|| first("stdout")).or_else(|| first("stderr")).unwrap_or_default();
        let exit = r["exit_code"].as_i64().map_or(String::new(), |c| format!(" exit {c}"));
        out.push((ok, format!("    {} {}{exit} · {}", if ok { "✓" } else { "✗" }, r["machine"].as_str().unwrap_or("?"), truncate(&what, 120))));
    }
    Some(out)
}

fn draw_chat(f: &mut Frame, app: &mut App, area: Rect) {
    let dim = Style::default().fg(Color::DarkGray);
    let mut lines: Vec<Line> = Vec::new();
    // Replies are Markdown: render each once per change, not every frame.
    for m in app.messages.iter_mut().filter(|m| m.role == "assistant") {
        if m.rendered.as_ref().is_none_or(|(len, _)| *len != m.content.len()) {
            m.rendered = Some((m.content.len(), crate::markdown::render(&m.content, Style::default())));
        }
    }
    for m in &app.messages {
        if m.role == "tool" || m.role == "agent_tool" {
            // Tool results: one dim line, under the call that produced them
            // (a line per machine for one run on several).
            match tool_lines(&m.content) {
                Some(each) => lines.extend(each.into_iter().map(|(ok, text)| Line::styled(text, if ok { dim } else { Style::default().fg(Color::Red) }))),
                None => lines.push(Line::styled(format!("  ↳ {}", truncate(&m.content, 160)), dim)),
            }
            lines.push(Line::default());
            continue;
        }
        let (label, color) = match m.role.as_str() {
            "user" => ("you", Color::Cyan),
            "assistant" => ("lyra", Color::Green),
            "info" => ("system", Color::Magenta),
            // A specialist agent working for lyra (UI only; not in the history).
            "agent" => ("↪ agent", Color::LightBlue),
            "approval" => ("approval", Color::Yellow),
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
        match &m.rendered {
            Some((_, rendered)) if m.role == "assistant" => lines.extend(rendered.iter().cloned()),
            _ => {
                let body = match m.role.as_str() {
                    "info" => dim,
                    "agent" => Style::default().fg(Color::Blue),
                    "approval" => Style::default().fg(Color::Yellow),
                    _ => Style::default(),
                };
                lines.extend(m.content.lines().map(|l| Line::styled(l.to_string(), body)));
            }
        }
        for call in &m.tool_calls {
            let text = format!("→ {} {}", call.function.name, truncate(&call.function.arguments, 160));
            lines.push(Line::styled(text, dim));
        }
        if let Some(stats) = &m.stats {
            lines.push(Line::styled(reply_stats(stats, &app.pricing), dim));
        }
        if !m.memories.is_empty() {
            let text = format!("used memories: {}", m.memories.join(" "));
            lines.push(Line::styled(text, Style::default().fg(Color::Cyan)));
        }
        if !m.skills.is_empty() {
            let text = format!("used skills: {}", m.skills.join(", "));
            lines.push(Line::styled(text, Style::default().fg(Color::Magenta)));
        }
        if !m.agents.is_empty() {
            let text = format!("handled with: {}", m.agents.join(", "));
            lines.push(Line::styled(text, Style::default().fg(Color::LightBlue)));
        }
        lines.push(Line::default());
    }
    // Show "thinking..." until the first token arrives, and while waiting on a tool.
    if app.waiting && app.messages.last().is_some_and(|m| matches!(m.role.as_str(), "user" | "tool" | "agent")) {
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
    let (agents_title, agents) = agents_panel(app, width);
    let (skills_title, skills) = skills_panel(app, width);
    let (plan_title, plan) = plan_panel(app, width);
    let (evolution_title, evolution) = evolution_panel(app, width);
    let (goals_title, goals) = goals_panel(app, width);
    let [session_area, agent_area, agents_area, memory_area, skills_area, goals_area, plan_area, evolution_area, activity_area] = Layout::vertical([
        Constraint::Length(session.len() as u16 + 2),
        Constraint::Length(agent.len() as u16 + 2),
        Constraint::Length(if agents.is_empty() { 0 } else { agents.len() as u16 + 2 }),
        Constraint::Fill(1),
        Constraint::Length(skills.len() as u16 + 2),
        Constraint::Length(if goals.is_empty() { 0 } else { goals.len() as u16 + 2 }),
        Constraint::Length(if plan.is_empty() { 0 } else { plan.len() as u16 + 2 }),
        Constraint::Length(evolution.len() as u16 + 2),
        Constraint::Fill(1),
    ])
    .areas(area);
    f.render_widget(Paragraph::new(evolution).block(Block::bordered().title(evolution_title)), evolution_area);
    if !goals.is_empty() {
        f.render_widget(Paragraph::new(goals).block(Block::bordered().title(goals_title)), goals_area);
    }
    if !plan.is_empty() {
        f.render_widget(Paragraph::new(plan).block(Block::bordered().title(plan_title)), plan_area);
    }

    f.render_widget(Paragraph::new(session).block(Block::bordered().title(format!(" Session · lyra {} ", crate::changelog::version()))), session_area);
    f.render_widget(Paragraph::new(agent).block(Block::bordered().title(" Agent ")), agent_area);
    if !agents.is_empty() {
        f.render_widget(Paragraph::new(agents).block(Block::bordered().title(agents_title)), agents_area);
    }
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
    ];
    if app.chat_only {
        lines.push(Line::from(vec![label("tools"), Span::styled("off: chat only (/chat-only)", Style::default().fg(Color::LightBlue))]));
    }
    if let Some(board) = crate::status::latest() {
        let color = match board.overall {
            crate::status::State::Down => Color::Red,
            crate::status::State::Degraded => Color::Yellow,
            _ => Color::Green,
        };
        lines.push(Line::from(vec![label("status"), Span::styled(truncate(&crate::status::line(&board), width.saturating_sub(8)), Style::default().fg(color))]));
    }
    let backed = match crate::backup::last() {
        _ if crate::backup::running() => "backing up…".to_string(),
        Some(b) => format!("{} · {}", b.made.format("%m-%d %H:%M"), crate::backup::size_text(b.size)),
        None => "none yet (/backup now)".into(),
    };
    lines.push(Line::from(vec![label("backup"), Span::raw(truncate(&backed, width.saturating_sub(8)))]));
    if let Some(model) = crate::decide::model() {
        let (calls, fallbacks, ms) = crate::decide::stats();
        let text = format!("{model} · {calls} decided{} · {ms} ms", if fallbacks > 0 { format!(", {fallbacks} to chat") } else { String::new() });
        lines.push(Line::from(vec![label("decide"), Span::raw(truncate(&text, width.saturating_sub(8)))]));
    }
    lines.extend([
        Line::from(vec![
            label("replies"),
            Span::raw(t.replies.to_string()),
            Span::raw(t.avg_ttft().map(|d| format!(" · avg ttft {}", secs(d))).unwrap_or_default()),
        ]),
    ]);
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
    // Learning reviews are extra requests; their share of the totals above.
    if t.reviews > 0 {
        let tokens = thousands(t.review_input + t.review_output);
        let cost = p.format(p.cost(t.review_input, t.review_cached, t.review_output));
        lines.push(Line::from(vec![
            label("reviews"),
            Span::raw(format!("{} · {tokens} tok · {cost}", t.reviews)),
        ]));
    }
    lines
}

fn agent_panel(app: &App, width: usize) -> Vec<Line<'static>> {
    let dim = Style::default().fg(Color::DarkGray);
    let mut lines = Vec::new();
    let home = crate::config::home().map_or("—".into(), |h| crate::context::show(&h));
    lines.push(Line::from(vec![label("home"), Span::raw(truncate_start(&home, width.saturating_sub(8)))]));
    // Everything sent ahead of the conversation: system prompt, tool definitions,
    // and the skills and memories added for the latest message.
    let system = app.system_prompt.as_deref().map_or(0, crate::learn::approx_tokens);
    let total = system + app.tools_tokens + app.applied_skills_tokens + app.applied_memories_tokens;
    lines.push(Line::from(vec![label("prompt"), Span::raw(format!("~{} tokens/request", thousands(total)))]));
    for parts in [
        format!("system {} · tools {}", thousands(system), thousands(app.tools_tokens)),
        format!("skills {} · memories {}", thousands(app.applied_skills_tokens), thousands(app.applied_memories_tokens)),
    ] {
        lines.push(Line::from(vec![label(""), Span::styled(truncate(&parts, width.saturating_sub(8)), dim)]));
    }
    if app.context_files.is_empty() {
        lines.push(Line::from(vec![label("context"), "no SOUL/USER/AGENT.md".dark_gray()]));
    }
    for (name, path) in &app.context_files {
        lines.push(Line::from(vec![label(name), Span::raw(truncate_start(path, width.saturating_sub(8)))]));
    }
    // Capabilities: how many of each kind, what's offered, what's unwell.
    if let Some(caps) = &app.caps {
        let all = caps.manager.all();
        let mut kinds: Vec<(String, usize)> = Vec::new();
        for c in &all {
            match kinds.iter_mut().find(|(k, _)| k == c.kind.as_str()) {
                Some((_, n)) => *n += 1,
                None => kinds.push((c.kind.as_str().to_string(), 1)),
            }
        }
        let summary = kinds.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(" · ");
        lines.push(Line::from(vec![label("caps"), Span::raw(truncate(&format!("{} · {summary}", all.len()), width.saturating_sub(8)))]));
        let unwell: Vec<String> = all
            .iter()
            .filter_map(|c| {
                let h = caps.manager.health(c);
                (h != lyra_capabilities::CapabilityHealth::Healthy).then(|| format!("{} {}", c.id, h.as_str()))
            })
            .collect();
        let offered = format!("{} offered per message", app.tool_count);
        lines.push(Line::from(vec![label(""), Span::styled(truncate(&offered, width.saturating_sub(8)), dim)]));
        if !unwell.is_empty() {
            lines.push(Line::from(truncate(&unwell.join(" · "), width).yellow()));
        }
    } else {
        let tools = if app.tool_count == 0 { "none".dark_gray() } else { format!("{}", app.tool_count).into() };
        lines.push(Line::from(vec![label("tools"), tools]));
    }
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
        (Ok(path), Some(Ok(snap))) => {
            let s = &snap.stats;
            let busy = match (app.capturing, app.memory_curating) {
                (true, _) => " · capturing…",
                (_, true) => " · curating…",
                _ => "",
            };
            title = format!(" Memory · {} active{busy} ", thousands(s.active as u64));
            lines.push(Line::styled(truncate_start(path, width), dim));

            let mut summary: Vec<String> = s.by_kind.iter().map(|(k, n)| format!("{k} {n}")).collect();
            summary.push(if snap.vectors { format!("vectors {}/{}", s.embedded, s.active) } else { "keywords only".into() });
            lines.push(Line::styled(truncate(&summary.join(" · "), width), dim));
            // Storage and how fast search is (L23).
            let mut storage = vec![s.backend.to_string()];
            if let Some(b) = s.size_bytes {
                storage.push(crate::mem::human_bytes(b));
            }
            if let Some(op) = s.operations.iter().find(|o| o.name == "memory.hybrid_search") {
                storage.push(format!("search p50 {:.0}ms", op.p50_ms));
            }
            let failures: u64 = s.operations.iter().map(|o| o.failures).sum();
            let line = Line::from(vec![label("store"), Span::raw(truncate(&storage.join(" · "), width.saturating_sub(8)))]);
            lines.push(if failures > 0 { line.red() } else { line });
            let project = snap.project.as_deref().map_or("none (all projects left out)".to_string(), |p| format!("project:{p}"));
            lines.push(Line::from(vec![label("project"), Span::raw(truncate(&project, width.saturating_sub(8)))]));

            let mut attention = Vec::new();
            for (n, what) in [
                (snap.proposals.len(), "to approve"),
                (s.contradictions, "contradiction"),
                (s.duplicate_candidates, "duplicate pair"),
                (s.expired, "expired"),
            ] {
                if n > 0 {
                    let plural = if n > 1 && matches!(what, "contradiction" | "duplicate pair") { "s" } else { "" };
                    attention.push(format!("{n} {what}{plural}"));
                }
            }
            if !attention.is_empty() {
                lines.push(Line::from(truncate(&format!("{} (/memory)", attention.join(" · ")), width).yellow()));
            }

            // Working memory: what the current task is about.
            let w = &snap.working;
            if let Some(goal) = &w.goal {
                lines.push(Line::from(vec![label("goal"), Span::raw(truncate(goal, width.saturating_sub(8)))]).cyan());
            }
            if !w.plan.is_empty() || !w.values.is_empty() {
                let text = format!("{} plan steps · {} notes", w.plan.len(), w.values.len());
                lines.push(Line::from(vec![label("working"), Span::raw(text)]).cyan());
            }
            if let Some(last) = w.recent_tools.front() {
                lines.push(Line::from(vec![label("tool"), Span::styled(truncate(last, width.saturating_sub(8)), dim)]));
            }
            if app.totals.replies > 0 {
                let n = app.applied_memories.len();
                let text = format!("last reply used {n} memor{}", if n == 1 { "y" } else { "ies" });
                lines.push(Line::from(truncate(&text, width).cyan()));
            }

            if s.active == 0 {
                lines.push(Line::styled("empty — tell lyra something worth remembering", dim));
            }
            for m in &snap.recent {
                let scope = format!("[{}] ", m.scope);
                let rest = width.saturating_sub(scope.chars().count() + 2);
                let unsure = m.confidence < 0.7;
                lines.push(Line::from(vec![
                    if unsure { "? ".yellow() } else { "• ".dark_gray() },
                    Span::styled(scope, dim),
                    Span::raw(truncate(&m.content, rest)),
                ]));
            }
        }
    }
    f.render_widget(Paragraph::new(lines).block(Block::bordered().title(title)), area);
}

/// Most step lines shown in the Plan panel; `/plan` shows them all.
const MAX_PLAN_STEPS: usize = 8;

/// The current plan: goal, steps with their status, budget use and any note.
/// Empty when there's no plan.
fn plan_panel(app: &App, width: usize) -> (String, Vec<Line<'static>>) {
    let Some(plan) = &app.current_plan else { return (String::new(), Vec::new()) };
    let dim = Style::default().fg(Color::DarkGray);
    let busy = if app.plan_busy { " · working…" } else { "" };
    let title = format!(" Plan {} · v{} · {}{busy} ", lyra_execution::short(plan.id), plan.version, plan.status);
    let mut lines = Vec::new();
    if let Some(goal) = &app.current_goal {
        let status = format!("goal {} · ", goal.status);
        lines.push(Line::from(vec![
            Span::styled(status.clone(), dim),
            Span::raw(truncate(&goal.description, width.saturating_sub(status.chars().count()))),
        ]));
    }
    for s in plan.steps.iter().take(MAX_PLAN_STEPS) {
        let icon = crate::plan::icon(s.status);
        let style = match s.status {
            lyra_execution::StepStatus::Completed => Style::default().fg(Color::Green),
            lyra_execution::StepStatus::Failed => Style::default().fg(Color::Red),
            lyra_execution::StepStatus::Running => Style::default().fg(Color::Blue),
            lyra_execution::StepStatus::Blocked => Style::default().fg(Color::Yellow),
            _ => dim,
        };
        let flag = if s.needs_approval() { " ⚠" } else { "" };
        let text = format!("{} {}{flag}", s.key, s.title);
        lines.push(Line::from(vec![Span::styled(format!("{icon} "), style), Span::raw(truncate(&text, width.saturating_sub(2)))]));
    }
    if plan.steps.len() > MAX_PLAN_STEPS {
        lines.push(Line::styled(format!("… {} more (/plan)", plan.steps.len() - MAX_PLAN_STEPS), dim));
    }
    lines.push(Line::styled(truncate(&lyra_execution::budget::describe(&plan.budget, &plan.usage), width), dim));
    if let Some(note) = &plan.note {
        lines.push(Line::from(truncate(note, width).yellow()));
    }
    (title, lines)
}

/// The main agent and its specialists: who's working right now (A1, and
/// "the TUI must show when the main agent is interacting with subagents").
/// An agent's colour in the terminal.
fn agent_color(name: &str) -> Color {
    match name {
        "sky" => Color::LightBlue,
        "teal" => Color::Cyan,
        "emerald" => Color::LightGreen,
        "amber" => Color::Yellow,
        "orange" => Color::LightRed,
        "rose" => Color::Red,
        "violet" => Color::LightMagenta,
        "fuchsia" => Color::Magenta,
        _ => Color::Gray,
    }
}

pub(crate) fn agents_panel(app: &App, width: usize) -> (String, Vec<Line<'static>>) {
    let dim = Style::default().fg(Color::DarkGray);
    let Some(agents) = &app.agents else { return (String::new(), Vec::new()) };
    let active = agents.active.lock().map(|a| a.clone()).unwrap_or_default();
    let title = if active.is_empty() { format!(" Agents · {} ", app.agents_panel.len()) } else { format!(" Agents · {} working ", active.len()) };
    let mut lines = vec![Line::from(vec![
        Span::styled(if active.is_empty() { "● " } else { "◆ " }, Style::default().fg(Color::Green)),
        Span::raw("main"),
        Span::styled(if active.is_empty() { "" } else { " → delegating" }, Style::default().fg(Color::LightBlue)),
    ])];
    for a in &app.agents_panel {
        let working = active.contains(&a.title);
        let (mark, style) = match (working, a.enabled) {
            (true, _) => ("↪ ", Style::default().fg(Color::LightBlue).bold()),
            (false, true) => ("· ", Style::default()),
            (false, false) => ("‖ ", dim),
        };
        let mut info = String::new();
        if a.delegations > 0 {
            info += &format!(" {}×", a.delegations);
        }
        if a.corrected > 0 {
            info += &format!(" {} corrected", a.corrected);
        }
        if !a.auto && a.enabled {
            info += " on request";
        }
        if working {
            info = " working…".into();
        }
        let name_width = width.saturating_sub(2 + info.chars().count());
        // Its own colour (as in the app), unless it's off.
        let named = if a.enabled && !working { style.fg(agent_color(&a.color)) } else { style };
        lines.push(Line::from(vec![Span::styled(mark, style), Span::styled(truncate(&a.title, name_width), named), Span::styled(info, dim)]));
    }
    // This conversation's task board.
    if let Some(board) = crate::board::line(&app.session_id) {
        lines.push(Line::from(vec![Span::styled("☰ ", Style::default().fg(Color::LightBlue)), Span::styled(truncate(&format!("board: {board}"), width.saturating_sub(2)), dim)]));
    }
    for r in &app.approvals {
        lines.push(Line::styled(truncate(&format!("⚠ {} awaits approval", r.agent), width), Style::default().fg(Color::Yellow)));
    }
    if app.wizard_busy {
        lines.push(Line::styled("building a new agent…", Style::default().fg(Color::Yellow)));
    } else if app.wizard_active() {
        lines.push(Line::styled("creating an agent (answer in chat)", Style::default().fg(Color::Yellow)));
    }
    (title, lines)
}

/// Most goals shown; /goals lists them all.
const MAX_GOALS: usize = 5;

fn goals_panel(app: &App, width: usize) -> (String, Vec<Line<'static>>) {
    let dim = Style::default().fg(Color::DarkGray);
    let Some(snapshot) = &app.goals_panel else {
        return if app.goals.is_some() { (" Goals ".into(), vec![Line::styled("loading…", dim)]) } else { (String::new(), Vec::new()) };
    };
    let s = match snapshot {
        Ok(s) => s,
        Err(e) => return (" Goals ".into(), vec![Line::from(truncate(e, width).red())]),
    };
    let title = format!(" Goals · {} open · {} ", s.open, s.mode.as_str());
    let mut lines = Vec::new();
    if s.ranked.is_empty() {
        lines.push(Line::styled("none — /goal new <title>", dim));
    }
    for (g, _, blocker) in s.ranked.iter().take(MAX_GOALS) {
        use lyra_goals::GoalStatus as S;
        let (mark, style) = match g.status {
            S::Active => ("▸ ", Style::default().fg(Color::Green)),
            S::Blocked => ("⏸ ", Style::default().fg(Color::Yellow)),
            S::Proposed => ("? ", Style::default().fg(Color::Yellow)),
            _ => ("‖ ", dim),
        };
        let filled = (g.progress.clamp(0.0, 1.0) * 6.0).round() as usize;
        let bar = format!(" {}{} {:>3.0}%", "█".repeat(filled), "░".repeat(6 - filled), g.progress * 100.0);
        let indent = if g.parent_goal_id.is_some() { " " } else { "" };
        let name_width = width.saturating_sub(2 + bar.chars().count() + indent.len());
        lines.push(Line::from(vec![
            Span::styled(format!("{indent}{mark}"), style),
            Span::raw(truncate(&g.title, name_width)),
            Span::styled(bar, dim),
        ]));
        if let Some(reason) = blocker {
            lines.push(Line::from(truncate(&format!("  {reason}"), width).yellow()));
        }
    }
    if s.ranked.len() > MAX_GOALS || s.open > s.ranked.len() {
        lines.push(Line::styled(format!("… {} more (/goals)", s.open.saturating_sub(MAX_GOALS)), dim));
    }
    if s.mode != lyra_goals::AutonomyMode::Reactive {
        let text = match &s.session.stopped {
            Some((_, why)) => format!("autonomy {why}"),
            None => format!("session: {} plans · {} tool calls", s.session.plans, s.session.tool_calls),
        };
        lines.push(Line::styled(truncate(&text, width), dim));
    }
    (title, lines)
}

fn evolution_panel(app: &App, width: usize) -> (String, Vec<Line<'static>>) {
    let dim = Style::default().fg(Color::DarkGray);
    let snapshot = match (&app.evolution_status, &app.evolution_panel) {
        (Err(e), _) => return (" Evolution ".into(), vec![Line::from(truncate(e, width).red())]),
        (Ok(off), _) if app.evolution.is_none() => return (" Evolution ".into(), vec![Line::styled(truncate(off, width), dim)]),
        (Ok(_), None) => return (" Evolution ".into(), vec![Line::styled("loading…", dim)]),
        (Ok(_), Some(Err(e))) => return (" Evolution ".into(), vec![Line::from(truncate(e, width).red())]),
        (Ok(_), Some(Ok(snapshot))) => snapshot,
    };
    let st = &snapshot.stats;
    let busy = app.evolving.map(|w| format!(" · {w}…")).unwrap_or_default();
    let title = format!(" Evolution · gen {}{busy} ", st.generation);
    let mut lines = Vec::new();
    let mode = format!("{:?}", snapshot.mode).to_lowercase();
    lines.push(Line::styled(truncate(&format!("mode {mode} · {} runs", st.runs), width), dim));
    let success = st.success_rate.map_or("—".into(), |r| format!("{:.0}%", r * 100.0));
    lines.push(Line::from(vec![
        label("success"),
        Span::raw(truncate(&format!("{success} of {} judged · {} corr.", st.judged, st.corrections), width.saturating_sub(8))),
    ]));
    lines.push(Line::from(vec![
        label("per run"),
        Span::raw(format!("{:.1} model · {:.1} tool calls", st.avg_model_calls, st.avg_tool_calls)),
    ]));
    let mut evolved = Vec::new();
    for (n, what) in [(snapshot.guidelines, "guideline"), (snapshot.workflows, "workflow"), (snapshot.tools, "tool")] {
        if n > 0 {
            evolved.push(format!("{n} {what}{}", if n == 1 { "" } else { "s" }));
        }
    }
    evolved.extend(snapshot.settings.iter().cloned());
    if !evolved.is_empty() {
        lines.push(Line::from(vec![label("evolved"), Span::raw(truncate(&evolved.join(" · "), width.saturating_sub(8)))]));
    }
    if st.pending > 0 {
        lines.push(Line::from(truncate(&format!("{} candidate(s) to review (/evolve list)", st.pending), width).yellow()));
    }
    let review = snapshot.last_review.map_or("never".into(), |t| t.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string());
    lines.push(Line::styled(truncate(&format!("last review {review}"), width), dim));
    (title, lines)
}

/// Most skill lines shown; /skills lists everything.
const MAX_SKILL_LINES: usize = 8;

fn skills_panel(app: &App, width: usize) -> (String, Vec<Line<'static>>) {
    let dim = Style::default().fg(Color::DarkGray);
    let mut lines = Vec::new();
    let snapshot = match (&app.learning_status, &app.skills) {
        (Err(e), _) => return (" Skills ".into(), vec![Line::from(truncate(e, width).red())]),
        (Ok(_), None) => return (" Skills ".into(), vec![Line::styled("loading…", dim)]),
        (Ok(_), Some(Err(e))) => return (" Skills ".into(), vec![Line::from(truncate(e, width).red())]),
        (Ok(_), Some(Ok(snapshot))) => snapshot,
    };
    let h = &snapshot.health;
    let busy = match (app.reviewing, app.curating) {
        (true, _) => " · reviewing…",
        (_, true) => " · curating…",
        _ => "",
    };
    let title = format!(" Skills · {} active{busy} ", h.active);

    let mut summary = format!("mode {}", snapshot.mode.as_str());
    if let Some(r) = h.average_reliability {
        summary += &format!(" · avg reliability {r:.2}");
    }
    if h.deprecated > 0 {
        summary += &format!(" · {} deprecated", h.deprecated);
    }
    lines.push(Line::styled(truncate(&summary, width), dim));

    // Things that need a person's attention.
    let mut attention = Vec::new();
    let to_review = h.pending_proposals + h.proposed;
    if to_review > 0 {
        attention.push(format!("{to_review} to review"));
    }
    for (n, what) in [
        (h.conflicts, "conflict"),
        (h.duplicate_candidates, "duplicate pair"),
        (h.failing, "failing"),
        (h.stale, "stale"),
    ] {
        if n > 0 {
            attention.push(format!("{n} {what}{}", if n == 1 || what == "failing" || what == "stale" { "" } else { "s" }));
        }
    }
    if !attention.is_empty() {
        lines.push(Line::from(truncate(&format!("{} (/skills)", attention.join(" · ")), width).yellow()));
    }
    if !snapshot.errors.is_empty() {
        let n = snapshot.errors.len();
        let text = format!("{n} unreadable skill file{} (/skills)", if n == 1 { "" } else { "s" });
        lines.push(Line::from(truncate(&text, width).red()));
    }
    if app.totals.replies > 0 {
        let used = if app.applied_skills.is_empty() {
            "last reply used no skills".to_string()
        } else {
            format!("last reply used {}", app.applied_skills.join(", "))
        };
        lines.push(Line::from(truncate(&used, width).magenta()));
    }
    for proposal in &snapshot.proposals {
        lines.push(Line::from(vec!["~ ".yellow(), Span::raw(truncate(proposal, width.saturating_sub(2)))]));
    }
    for skill in &snapshot.proposed {
        let text = format!("{} {}", crate::learn::short_id(skill.id), skill.name);
        lines.push(Line::from(vec!["? ".yellow(), Span::raw(truncate(&text, width.saturating_sub(2)))]));
    }
    for skill in &snapshot.active {
        // Reliability and number of uses, so weak skills stand out.
        let record = format!(" {:.2}·{}", skill.usage.reliability(), skill.usage.use_count);
        let name_width = width.saturating_sub(2 + record.chars().count());
        lines.push(Line::from(vec![
            "✓ ".green(),
            Span::raw(truncate(&skill.name, name_width)),
            Span::styled(record, dim),
        ]));
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
                Level::Memory => Style::default().fg(Color::Cyan),
                Level::Plan => Style::default().fg(Color::Blue),
                Level::Evolve => Style::default().fg(Color::Green),
                Level::Agent => Style::default().fg(Color::LightBlue),
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
        Phase::Delegating(_) => text.light_blue(),
        Phase::Approval(_) => text.yellow().bold(),
        _ => text.green(),
    }
}

pub(crate) fn phase_text(app: &App) -> String {
    let since = secs(app.phase_since.elapsed());
    match &app.phase {
        Phase::Idle => "● idle".into(),
        Phase::Waiting => format!("◌ waiting {since}"),
        Phase::Thinking => format!("◐ thinking {since}"),
        Phase::Streaming => format!("▸ streaming {since}"),
        Phase::Tools(names) => format!("⚙ {names}"),
        Phase::Delegating(agent) => format!("↪ {agent} {since}"),
        Phase::Approval(agent) => format!("⚠ {agent} awaits your y/n"),
    }
}

/// What it's doing, in words and without a clock (a routine's progress on the app's page).
pub(crate) fn doing_text(app: &App) -> String {
    match &app.phase {
        Phase::Idle => "finishing".into(),
        Phase::Waiting => "waiting for the model".into(),
        Phase::Thinking => "thinking".into(),
        Phase::Streaming => "writing".into(),
        Phase::Tools(names) => format!("using {names}"),
        Phase::Delegating(agent) => format!("{agent} is working on it"),
        Phase::Approval(agent) => format!("{agent} is waiting for a yes or no"),
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
pub(crate) fn reply_stats(stats: &Stats, pricing: &Pricing) -> String {
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

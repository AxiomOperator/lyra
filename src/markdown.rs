//! Markdown to styled terminal lines, for the model's replies: headings,
//! emphasis, inline and fenced code, lists (nested, numbered, task lists),
//! quotes, links, rules and tables. Works on partial text too, so a reply
//! renders properly while it streams.

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Render `text` as lines. `base` is the style of ordinary text.
pub fn render(text: &str, base: Style) -> Vec<Line<'static>> {
    let mut r = Renderer::new(base);
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for event in Parser::new_ext(text, options) {
        r.event(event);
    }
    r.finish()
}

fn code_style() -> Style {
    Style::default().fg(Color::Yellow)
}

/// A list being rendered: its next number (`None` for bullets).
struct List {
    next: Option<u64>,
}

struct Table {
    alignments: Vec<Alignment>,
    rows: Vec<Vec<Vec<Span<'static>>>>,
    header_rows: usize,
}

struct Renderer {
    base: Style,
    lines: Vec<Line<'static>>,
    /// The line being built.
    spans: Vec<Span<'static>>,
    /// Inline styles in effect (bold, italic, link, …), innermost last.
    styles: Vec<Style>,
    lists: Vec<List>,
    /// Nesting of block quotes.
    quotes: usize,
    /// Inside a fenced or indented code block.
    code: bool,
    /// A list item's marker waiting to start its first line.
    marker: Option<String>,
    link: Option<String>,
    table: Option<Table>,
    cell: Vec<Span<'static>>,
    /// Whether the last block left a blank line (to avoid doubling them).
    blank: bool,
}

impl Renderer {
    fn new(base: Style) -> Self {
        Self {
            base,
            lines: Vec::new(),
            spans: Vec::new(),
            styles: Vec::new(),
            lists: Vec::new(),
            quotes: 0,
            code: false,
            marker: None,
            link: None,
            table: None,
            cell: Vec::new(),
            blank: true,
        }
    }

    fn style(&self) -> Style {
        self.styles.iter().fold(self.base, |s, add| s.patch(*add))
    }

    /// What goes before each line: quote bars and list indentation.
    fn prefix(&mut self) -> Vec<Span<'static>> {
        let mut out = Vec::new();
        for _ in 0..self.quotes {
            out.push(Span::styled("▎ ", Style::default().fg(Color::DarkGray)));
        }
        let depth = self.lists.len();
        if depth > 0 {
            let indent = "  ".repeat(depth - 1);
            match self.marker.take() {
                Some(marker) => out.push(Span::styled(format!("{indent}{marker}"), Style::default().fg(Color::Cyan))),
                // Continuation lines line up with the item's text.
                None => out.push(Span::raw(format!("{indent}  "))),
            }
        }
        out
    }

    fn push_text(&mut self, text: &str) {
        let style = self.style();
        if self.table.is_some() {
            self.cell.push(Span::styled(text.to_string(), style));
            return;
        }
        if self.spans.is_empty() {
            self.spans = self.prefix();
        }
        self.spans.push(Span::styled(text.to_string(), style));
    }

    /// End the current line (if anything is on it).
    fn flush(&mut self) {
        if !self.spans.is_empty() {
            self.lines.push(Line::from(std::mem::take(&mut self.spans)));
            self.blank = false;
        }
    }

    /// A blank line between blocks (top level only, and never two in a row).
    fn gap(&mut self) {
        self.flush();
        if !self.blank && self.lists.is_empty() {
            self.lines.push(Line::default());
            self.blank = true;
        }
    }

    fn event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) if self.code => {
                // Code keeps its lines exactly, indented under a gutter.
                for line in text.split_inclusive('\n') {
                    let mut spans = self.prefix();
                    spans.push(Span::styled("│ ", Style::default().fg(Color::DarkGray)));
                    spans.push(Span::styled(line.trim_end_matches('\n').to_string(), code_style()));
                    self.lines.push(Line::from(spans));
                }
                self.blank = false;
            }
            Event::Text(text) => self.push_text(&text),
            Event::Code(code) => {
                let style = self.style().patch(code_style());
                let span = Span::styled(code.to_string(), style);
                if self.table.is_some() {
                    self.cell.push(span);
                } else {
                    if self.spans.is_empty() {
                        self.spans = self.prefix();
                    }
                    self.spans.push(span);
                }
            }
            Event::SoftBreak => self.push_text(" "),
            Event::HardBreak => self.flush(),
            Event::Rule => {
                self.gap();
                self.lines.push(Line::styled("─".repeat(40), Style::default().fg(Color::DarkGray)));
                self.blank = false;
                self.gap();
            }
            Event::TaskListMarker(done) => {
                let mark = if done { "☑ " } else { "☐ " };
                self.push_text(mark);
            }
            Event::Html(html) | Event::InlineHtml(html) => self.push_text(&html),
            Event::FootnoteReference(name) => self.push_text(&format!("[{name}]")),
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => {
                if self.lists.is_empty() {
                    self.gap();
                } else {
                    self.flush();
                }
            }
            Tag::Heading { level, .. } => {
                self.gap();
                let color = match level {
                    HeadingLevel::H1 => Color::Magenta,
                    HeadingLevel::H2 => Color::Cyan,
                    _ => Color::Blue,
                };
                let mut style = Style::default().fg(color).add_modifier(Modifier::BOLD);
                if level == HeadingLevel::H1 {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                self.styles.push(style);
            }
            Tag::BlockQuote(_) => {
                self.gap();
                self.quotes += 1;
                self.styles.push(Style::default().add_modifier(Modifier::ITALIC));
            }
            Tag::CodeBlock(kind) => {
                self.gap();
                self.code = true;
                if let CodeBlockKind::Fenced(lang) = kind
                    && !lang.is_empty()
                {
                    let mut spans = self.prefix();
                    spans.push(Span::styled(format!("╭ {lang}"), Style::default().fg(Color::DarkGray)));
                    self.lines.push(Line::from(spans));
                }
            }
            Tag::List(start) => {
                if self.lists.is_empty() {
                    self.gap();
                } else {
                    self.flush();
                }
                self.lists.push(List { next: start });
            }
            Tag::Item => {
                self.flush();
                let marker = match self.lists.last_mut() {
                    Some(List { next: Some(n) }) => {
                        let m = format!("{n}. ");
                        *n += 1;
                        m
                    }
                    _ => "• ".to_string(),
                };
                self.marker = Some(marker);
            }
            Tag::Emphasis => self.styles.push(Style::default().add_modifier(Modifier::ITALIC)),
            Tag::Strong => self.styles.push(Style::default().add_modifier(Modifier::BOLD)),
            Tag::Strikethrough => self.styles.push(Style::default().add_modifier(Modifier::CROSSED_OUT)),
            Tag::Link { dest_url, .. } => {
                self.link = Some(dest_url.to_string());
                self.styles.push(Style::default().fg(Color::Blue).add_modifier(Modifier::UNDERLINED));
            }
            Tag::Image { dest_url, .. } => {
                self.push_text("[image: ");
                self.link = Some(dest_url.to_string());
            }
            Tag::Table(alignments) => {
                self.gap();
                self.table = Some(Table { alignments, rows: Vec::new(), header_rows: 0 });
            }
            Tag::TableHead => {
                self.styles.push(Style::default().add_modifier(Modifier::BOLD));
                if let Some(t) = &mut self.table {
                    t.rows.push(Vec::new());
                }
            }
            Tag::TableRow => {
                if let Some(t) = &mut self.table {
                    t.rows.push(Vec::new());
                }
            }
            Tag::TableCell => self.cell.clear(),
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.flush(),
            TagEnd::Heading(_) => {
                self.styles.pop();
                self.flush();
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.styles.pop();
                self.quotes = self.quotes.saturating_sub(1);
            }
            TagEnd::CodeBlock => {
                self.code = false;
                self.blank = false;
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
                if self.lists.is_empty() {
                    self.blank = false;
                }
            }
            TagEnd::Item => self.flush(),
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                self.styles.pop();
            }
            TagEnd::Link => {
                self.styles.pop();
                if let Some(url) = self.link.take() {
                    // Show where it goes unless the text already is the address.
                    let shown: String = self.spans.iter().rev().take(1).map(|s| s.content.to_string()).collect();
                    if shown != url && !url.starts_with('#') {
                        self.push_text_styled(format!(" ({url})"), Style::default().fg(Color::DarkGray));
                    }
                }
            }
            TagEnd::Image => {
                if let Some(url) = self.link.take() {
                    self.push_text(&format!(" {url}]"));
                }
            }
            TagEnd::TableHead => {
                self.styles.pop();
                if let Some(t) = &mut self.table {
                    t.header_rows = t.rows.len();
                }
            }
            TagEnd::TableCell => {
                let cell = std::mem::take(&mut self.cell);
                if let Some(row) = self.table.as_mut().and_then(|t| t.rows.last_mut()) {
                    row.push(cell);
                }
            }
            TagEnd::Table => {
                if let Some(t) = self.table.take() {
                    self.table_lines(t);
                }
            }
            _ => {}
        }
    }

    fn push_text_styled(&mut self, text: String, style: Style) {
        if self.table.is_some() {
            self.cell.push(Span::styled(text, style));
            return;
        }
        if self.spans.is_empty() {
            self.spans = self.prefix();
        }
        self.spans.push(Span::styled(text, style));
    }

    /// Columns padded to their widest cell, `│` between them, a rule under the header.
    fn table_lines(&mut self, t: Table) {
        let width = |cell: &Vec<Span>| cell.iter().map(|s| s.content.chars().count()).sum::<usize>();
        let columns = t.rows.iter().map(Vec::len).max().unwrap_or(0);
        let widths: Vec<usize> =
            (0..columns).map(|c| t.rows.iter().filter_map(|r| r.get(c)).map(width).max().unwrap_or(0)).collect();
        let border = Style::default().fg(Color::DarkGray);
        for (i, row) in t.rows.iter().enumerate() {
            let mut spans = self.prefix();
            for (c, w) in widths.iter().enumerate() {
                if c > 0 {
                    spans.push(Span::styled(" │ ", border));
                }
                let cell = row.get(c).cloned().unwrap_or_default();
                let pad = w.saturating_sub(width(&cell));
                let (left, right) = match t.alignments.get(c) {
                    Some(Alignment::Right) => (pad, 0),
                    Some(Alignment::Center) => (pad / 2, pad - pad / 2),
                    _ => (0, pad),
                };
                spans.push(Span::raw(" ".repeat(left)));
                spans.extend(cell);
                spans.push(Span::raw(" ".repeat(right)));
            }
            self.lines.push(Line::from(spans));
            if i + 1 == t.header_rows {
                let rule: Vec<String> = widths.iter().map(|w| "─".repeat(*w)).collect();
                let mut spans = self.prefix();
                spans.push(Span::styled(rule.join("─┼─"), border));
                self.lines.push(Line::from(spans));
            }
        }
        self.blank = false;
    }

    fn finish(mut self) -> Vec<Line<'static>> {
        self.flush();
        // A partial table (still streaming) shows what's there so far.
        if let Some(t) = self.table.take() {
            self.table_lines(t);
        }
        while self.lines.last().is_some_and(|l| l.spans.iter().all(|s| s.content.trim().is_empty())) {
            self.lines.pop();
        }
        while self.lines.first().is_some_and(|l| l.spans.is_empty()) {
            self.lines.remove(0);
        }
        self.lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[Line]) -> Vec<String> {
        lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect()
    }

    #[test]
    fn renders_the_usual_reply_shapes() {
        let md = "# Title\n\nSome **bold** and *italic* with `code`.\n\n- one\n- two\n  1. nested\n\n```rust\nfn main() {}\n```\n\n> quoted\n\n[docs](https://example.com)";
        let lines = render(md, Style::default());
        let text = plain(&lines);
        assert_eq!(text[0], "Title");
        assert!(lines[0].spans[0].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(text[2], "Some bold and italic with code.");
        let bold = lines[2].spans.iter().find(|s| s.content == "bold").unwrap();
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
        assert!(text.contains(&"• one".to_string()) && text.contains(&"  1. nested".to_string()));
        assert!(text.contains(&"╭ rust".to_string()) && text.contains(&"│ fn main() {}".to_string()));
        assert!(text.contains(&"▎ quoted".to_string()));
        assert!(text.contains(&"docs (https://example.com)".to_string()));
    }

    #[test]
    fn tables_line_up() {
        let md = "| name | port |\n|------|-----:|\n| api | 8080 |\n| db | 5432 |";
        let text = plain(&render(md, Style::default()));
        assert_eq!(text, ["name │ port", "─────┼─────", "api  │ 8080", "db   │ 5432"]);
    }

    #[test]
    fn partial_text_renders_while_streaming() {
        let text = plain(&render("Here is a list:\n\n- first\n- sec", Style::default()));
        assert_eq!(text, ["Here is a list:", "", "• first", "• sec"]);
        assert!(!plain(&render("```py\nprint(1", Style::default())).is_empty());
    }
}

//! Markup as plain text, for the model and for pushes: a short HTML snippet
//! (a Teams message), Office XML (inside a Word, Excel or PowerPoint file) and
//! a whole web page (html2text, after dropping scripts, styles and menus).

/// Tags out (whatever is between `<` and `>`), the text between kept as is.
fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut tag = false;
    for c in s.chars() {
        match c {
            '<' => tag = true,
            '>' => tag = false,
            c if !tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// The usual named and numeric entities read; `&amp;` last, so `&amp;lt;` stays `&lt;`.
fn entities(s: &str) -> String {
    s.replace("&nbsp;", " ").replace("&#160;", " ").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&apos;", "'").replace("&amp;", "&")
}

/// A short piece of HTML (a Teams message): line breaks and paragraphs become
/// lines, tags go, entities are read.
pub fn html_text(html: &str) -> String {
    let lined = html.replace("<br>", "\n").replace("<br/>", "\n").replace("<br />", "\n").replace("</p>", "\n").replace("</div>", "\n");
    let text = entities(&strip_tags(&lined));
    text.lines().map(str::trim_end).collect::<Vec<_>>().join("\n").trim().to_string()
}

/// Text out of Office XML: paragraphs become lines, tabs stay, tags go, empty lines too.
pub fn xml_text(xml: &str) -> String {
    let xml = xml.replace("</w:p>", "\n").replace("</a:p>", "\n").replace("</si>", "\n").replace("<w:tab/>", "\t");
    entities(&strip_tags(&xml)).lines().map(str::trim_end).filter(|l| !l.trim().is_empty()).collect::<Vec<_>>().join("\n")
}

/// A page as text: scripts, styles and navigation dropped, links kept short.
pub fn page_text(html: &str, max: usize) -> String {
    // Drop what is never content before converting.
    let mut cleaned = html.to_string();
    for tag in ["script", "style", "noscript", "svg", "nav", "footer", "header", "form", "iframe"] {
        loop {
            let lower = cleaned.to_lowercase();
            let Some(start) = lower.find(&format!("<{tag}")) else { break };
            let close = format!("</{tag}>");
            let end = lower[start..].find(&close).map_or(cleaned.len(), |e| start + e + close.len());
            cleaned.replace_range(start..end, " ");
        }
    }
    let text = html2text::from_read(cleaned.as_bytes(), 100).unwrap_or_default();
    let mut out = String::new();
    let mut blank = 0;
    for line in text.lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    if out.chars().count() > max {
        let kept: String = out.chars().take(max).collect();
        return format!("{kept}\n… (page cut at {max} characters)");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn teams_html_reads_as_text() {
        assert_eq!(html_text("<p>Can you <b>check</b> the firewall&nbsp;rules?</p><p>Thanks</p>"), "Can you check the firewall rules?\nThanks");
        assert_eq!(html_text("<at id=\"0\">Garrett</at> ping"), "Garrett ping");
        assert_eq!(html_text("a<br>b &amp;lt;c&amp;gt;"), "a\nb &lt;c&gt;", "an escaped entity stays escaped");
    }

    #[test]
    fn office_xml_reads_as_lines() {
        assert_eq!(xml_text("<w:p><w:t>Phase 1 &amp; 2</w:t></w:p><w:p></w:p><w:p><w:t>A</w:t><w:tab/><w:t>B</w:t></w:p>"), "Phase 1 & 2\nA\tB");
    }

    #[test]
    fn pages_read_as_text_without_scripts_or_menus() {
        let html = r#"<html><head><title>T</title><style>.x{color:red}</style><script>alert(1)</script></head>
            <body><nav><a href="/">Home</a> <a href="/about">About</a></nav>
            <h1>Release notes</h1><p>Fedora 43 ships <b>GNOME 49</b>.</p><footer>© 2026</footer></body></html>"#;
        let text = page_text(html, 10_000);
        assert!(text.contains("Release notes") && text.contains("GNOME 49"), "{text}");
        assert!(!text.contains("alert(1)") && !text.contains("color:red") && !text.contains("About") && !text.contains("2026"), "{text}");
        assert!(page_text(&"<p>word </p>".repeat(5000), 100).contains("page cut at 100"));
    }
}

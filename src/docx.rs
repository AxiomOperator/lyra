//! A Markdown document as a Word file (.docx): headings, paragraphs, bullet
//! and numbered lists, **bold** and *italic*. Enough for a letter, memo or
//! one-pager; Word does the rest. A .docx is a zip of a few XML parts.

use std::io::Write;

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Runs of text with **bold** and *italic* (or _italic_) read.
fn runs(text: &str) -> String {
    let mut out = String::new();
    let (mut bold, mut italic) = (false, false);
    let mut buf = String::new();
    let flush = |buf: &mut String, out: &mut String, bold: bool, italic: bool| {
        if buf.is_empty() {
            return;
        }
        let props = match (bold, italic) {
            (false, false) => String::new(),
            (b, i) => format!("<w:rPr>{}{}</w:rPr>", if b { "<w:b/>" } else { "" }, if i { "<w:i/>" } else { "" }),
        };
        out.push_str(&format!("<w:r>{props}<w:t xml:space=\"preserve\">{}</w:t></w:r>", esc(buf)));
        buf.clear();
    };
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '*' && chars.get(i + 1) == Some(&'*') {
            flush(&mut buf, &mut out, bold, italic);
            bold = !bold;
            i += 2;
            continue;
        }
        if (chars[i] == '*' || chars[i] == '_') && (italic || chars.get(i + 1).is_some_and(|c| !c.is_whitespace())) {
            flush(&mut buf, &mut out, bold, italic);
            italic = !italic;
            i += 1;
            continue;
        }
        buf.push(chars[i]);
        i += 1;
    }
    flush(&mut buf, &mut out, bold, italic);
    out
}

fn para(style: Option<&str>, text: &str, indent: bool) -> String {
    let mut props = String::new();
    if let Some(s) = style {
        props.push_str(&format!("<w:pStyle w:val=\"{s}\"/>"));
    }
    if indent {
        props.push_str("<w:ind w:left=\"360\" w:hanging=\"360\"/>");
    }
    let props = if props.is_empty() { String::new() } else { format!("<w:pPr>{props}</w:pPr>") };
    // Lines within a paragraph stay lines (a letter's sign-off, an address).
    let body = text.split('\n').map(runs).collect::<Vec<_>>().join("<w:r><w:br/></w:r>");
    format!("<w:p>{props}{body}</w:p>")
}

/// The body XML for some Markdown.
fn body(markdown: &str) -> String {
    let mut out = String::new();
    let mut paragraph: Vec<String> = Vec::new();
    let end = |paragraph: &mut Vec<String>, out: &mut String| {
        if !paragraph.is_empty() {
            out.push_str(&para(None, &paragraph.join("\n"), false));
            paragraph.clear();
        }
    };
    for line in markdown.lines() {
        let t = line.trim();
        if t.is_empty() {
            end(&mut paragraph, &mut out);
            continue;
        }
        let heading = t.chars().take_while(|c| *c == '#').count();
        if (1..=3).contains(&heading) && t[heading..].starts_with(' ') {
            end(&mut paragraph, &mut out);
            out.push_str(&para(Some(&format!("Heading{heading}")), t[heading..].trim(), false));
        } else if let Some(item) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")).or_else(|| t.strip_prefix("• ")) {
            end(&mut paragraph, &mut out);
            out.push_str(&para(None, &format!("•\t{item}"), true));
        } else if let Some((n, item)) = t.split_once(". ").filter(|(n, _)| !n.is_empty() && n.len() <= 3 && n.chars().all(|c| c.is_ascii_digit())) {
            end(&mut paragraph, &mut out);
            out.push_str(&para(None, &format!("{n}.\t{item}"), true));
        } else {
            paragraph.push(t.to_string());
        }
    }
    end(&mut paragraph, &mut out);
    out
}

const STYLES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii="Calibri" w:hAnsi="Calibri" w:cs="Calibri"/><w:sz w:val="22"/></w:rPr></w:rPrDefault><w:pPrDefault><w:pPr><w:spacing w:after="160" w:line="276" w:lineRule="auto"/></w:pPr></w:pPrDefault></w:docDefaults>
<w:style w:type="paragraph" w:default="1" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:pPr><w:keepNext/><w:spacing w:before="240" w:after="120"/><w:outlineLvl w:val="0"/></w:pPr><w:rPr><w:b/><w:sz w:val="32"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Heading2"><w:name w:val="heading 2"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:pPr><w:keepNext/><w:spacing w:before="200" w:after="80"/><w:outlineLvl w:val="1"/></w:pPr><w:rPr><w:b/><w:sz w:val="26"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Heading3"><w:name w:val="heading 3"/><w:basedOn w:val="Normal"/><w:next w:val="Normal"/><w:pPr><w:keepNext/><w:outlineLvl w:val="2"/></w:pPr><w:rPr><w:b/><w:sz w:val="24"/></w:rPr></w:style>
</w:styles>"#;

/// The .docx file for some Markdown.
pub fn from_markdown(markdown: &str) -> Result<Vec<u8>, String> {
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{}<w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440" w:header="720" w:footer="720" w:gutter="0"/></w:sectPr></w:body></w:document>"#,
        body(markdown)
    );
    let parts: [(&str, &str); 5] = [
        (
            "[Content_Types].xml",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/><Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/></Types>"#,
        ),
        (
            "_rels/.rels",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#,
        ),
        (
            "word/_rels/document.xml.rels",
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#,
        ),
        ("word/styles.xml", STYLES),
        ("word/document.xml", &document),
    ];
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, text) in parts {
            z.start_file(name, opts).map_err(|e| e.to_string())?;
            z.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        }
        z.finish().map_err(|e| e.to_string())?;
    }
    Ok(buf.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_becomes_a_word_file_that_reads_back() {
        let md = "# Memo\n\nTo the **board**: the *new* policy\nstarts Monday.\n\n## Steps\n- Read it\n- Sign it\n1. First & foremost\n";
        let bytes = from_markdown(md).unwrap();
        // lyra's own reader (files.rs) gets the text back out.
        let text = crate::files::office_text(&bytes, "memo.docx").unwrap();
        assert!(text.contains("Memo") && text.contains("To the board: the new policy") && text.contains("starts Monday.") && text.contains("Read it") && text.contains("First & foremost"), "{text}");
        assert!(body("Thank you,\nGarrett").contains("<w:br/>"), "a sign-off keeps its line");
        assert!(body(md).contains("<w:pStyle w:val=\"Heading1\"/>") && body(md).contains("<w:b/>") && body(md).contains("<w:i/>"));
    }
}

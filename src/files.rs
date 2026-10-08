//! Each person's OneDrive and SharePoint files (read-only: Files.Read.All),
//! through their Microsoft 365 connection: search, read the text of a file
//! (Word, Excel, PowerPoint, text; PDFs give their link), and attach one to a
//! mail draft.

use std::io::Read;

use base64::Engine;
use lyra_capabilities::{Capability, CapabilityKind, RiskLevel};
use serde_json::{Value, json};

use crate::graph::graph;

/// Attached as a file up to this size; bigger ones go in as a link.
const ATTACH_UP_TO: u64 = 3 * 1024 * 1024;

fn ready() -> Result<(), String> {
    let user = crate::acting::current();
    if !crate::graph::connected_for(&user) || !crate::graph::has(&user, "Files.Read.All") {
        return Err("lyra can't see your files yet: connect again (More → Outlook → Add Teams & files)".into());
    }
    Ok(())
}

fn item(v: &Value) -> Value {
    json!({
        "id": v["id"],
        "drive": v["parentReference"]["driveId"],
        "name": v["name"],
        "where": v["parentReference"]["path"].as_str().map(|p| p.rsplit_once("root:").map_or(p, |(_, x)| x).to_string()).filter(|p| !p.is_empty()).or_else(|| v["parentReference"]["siteId"].as_str().map(|_| "SharePoint".to_string())),
        "modified": v["lastModifiedDateTime"],
        "by": v["lastModifiedBy"]["user"]["displayName"],
        "size": v["size"],
        "link": v["webUrl"],
    })
}

/// Search the person's files: OneDrive and the SharePoint sites they can open.
pub fn search(query: &str, count: usize) -> Result<Vec<Value>, String> {
    ready()?;
    let body = json!({ "requests": [{ "entityTypes": ["driveItem"], "query": { "queryString": query }, "from": 0, "size": count.clamp(1, 25) }] });
    let v = graph(reqwest::Method::POST, "/search/query", Some(&body))?;
    Ok(v["value"][0]["hitsContainers"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|h| h["hits"].as_array().cloned().unwrap_or_default())
        .map(|h| item(&h["resource"]))
        .collect())
}

/// The text of a Word, Excel or PowerPoint file (its zip of XML).
pub fn office_text(bytes: &[u8], name: &str) -> Result<String, String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| format!("can't open {name}: {e}"))?;
    let lower = name.to_lowercase();
    let mut parts: Vec<String> = (0..zip.len()).filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string())).collect();
    parts.sort_by_key(|p| (p.len(), p.clone()));
    let wanted = |p: &str| {
        if lower.ends_with(".docx") {
            p == "word/document.xml"
        } else if lower.ends_with(".pptx") {
            p.starts_with("ppt/slides/slide") && p.ends_with(".xml")
        } else {
            p == "xl/sharedStrings.xml"
        }
    };
    let mut out = Vec::new();
    for p in parts.iter().filter(|p| wanted(p)) {
        let mut s = String::new();
        if let Ok(mut f) = zip.by_name(p) {
            let _ = f.read_to_string(&mut s);
        }
        let t = crate::text::xml_text(&s);
        if !t.is_empty() {
            out.push(if lower.ends_with(".pptx") { format!("[{}]\n{t}", p.trim_start_matches("ppt/slides/").trim_end_matches(".xml")) } else { t });
        }
    }
    Ok(out.join("\n\n"))
}

fn content(drive: &str, id: &str) -> Result<Vec<u8>, String> {
    crate::graph::graph_bytes(&format!("/drives/{}/items/{}/content", lyra_web::oidc::encode(drive), lyra_web::oidc::encode(id)))
}

pub fn capabilities() -> Vec<Capability> {
    let tool = |name: &str, description: &str, risk: RiskLevel, properties: Value, required: &[&str]| {
        let mut c = Capability::new(name, CapabilityKind::NativeTool, description, risk);
        c.input_schema = json!({ "type": "object", "properties": properties, "required": required });
        c.source = "files".into();
        c.tags = ["file", "files", "document", "onedrive", "sharepoint", "doc", "spreadsheet", "attach", "find"].iter().map(|t| t.to_string()).collect();
        c
    };
    let which = json!({
        "id": { "type": "string", "description": "The file's id (from files_search)." },
        "drive": { "type": "string", "description": "Its drive (from files_search)." },
    });
    let mut read_props = which.clone();
    read_props["max_chars"] = json!({ "type": "integer" });
    read_props["question"] = json!({ "type": "string", "description": "For a picture or a scan: what to look for in it." });
    let mut attach_props = which.clone();
    attach_props["draft"] = json!({ "type": "string", "description": "The draft's id (from mail_draft)." });
    vec![
        tool("files_search", "Search the user's OneDrive and SharePoint files by name or content.", RiskLevel::ReadOnly, json!({ "query": { "type": "string" }, "count": { "type": "integer" } }), &["query"]),
        tool("files_read", "What one file says (Word, Excel, PowerPoint, text, CSV, Markdown, PDFs including scans, pictures); others give their link.", RiskLevel::ReadOnly, read_props, &["id", "drive"]),
        tool("files_attach", "Attach a file to a mail draft (as the file up to 3 MB, else a link in the text). Sending still waits for the user's yes.", RiskLevel::LowWrite, attach_props, &["id", "drive", "draft"]),
    ]
}

pub fn call(name: &str, args: &Value) -> Result<Value, String> {
    ready()?;
    let (id, drive) = (args["id"].as_str().unwrap_or(""), args["drive"].as_str().unwrap_or(""));
    let meta = || graph(reqwest::Method::GET, &format!("/drives/{}/items/{}?$select=id,name,size,webUrl,file,parentReference,lastModifiedDateTime", lyra_web::oidc::encode(drive), lyra_web::oidc::encode(id)), None);
    match name {
        "files_search" => Ok(json!({ "files": search(args["query"].as_str().unwrap_or(""), args["count"].as_u64().unwrap_or(10) as usize)? })),
        "files_read" => {
            let m = meta()?;
            let fname = m["name"].as_str().unwrap_or("").to_string();
            let lower = fname.to_lowercase();
            let max = args["max_chars"].as_u64().unwrap_or(12_000).clamp(500, 40_000) as usize;
            let size = m["size"].as_u64().unwrap_or(0);
            if size > 25 * 1024 * 1024 {
                return Ok(json!({ "name": fname, "link": m["webUrl"], "note": "too big to read here: open the link" }));
            }
            let text = if [".docx", ".pptx", ".xlsx"].iter().any(|x| lower.ends_with(x)) {
                office_text(&content(drive, id)?, &fname)?
            } else if crate::vision::handles(&fname) {
                crate::vision::read(&fname, &content(drive, id)?, args["question"].as_str())?
            } else if [".txt", ".md", ".csv", ".json", ".log", ".xml", ".html", ".yaml", ".yml", ".ini", ".conf", ".ps1", ".sh", ".py"].iter().any(|x| lower.ends_with(x)) {
                String::from_utf8_lossy(&content(drive, id)?).to_string()
            } else {
                return Ok(json!({ "name": fname, "link": m["webUrl"], "note": "lyra can't read this kind of file yet: open the link" }));
            };
            let cut = text.chars().count() > max;
            Ok(json!({ "name": fname, "link": m["webUrl"], "text": text.chars().take(max).collect::<String>(), "cut": cut }))
        }
        "files_attach" => {
            let draft = args["draft"].as_str().unwrap_or("");
            let d = graph(reqwest::Method::GET, &format!("/me/messages/{}?$select=isDraft,body", lyra_web::oidc::encode(draft)), None)?;
            if d["isDraft"] != true {
                return Err("that isn't a draft (mail_draft first)".into());
            }
            let m = meta()?;
            let fname = m["name"].as_str().unwrap_or("file").to_string();
            if m["size"].as_u64().unwrap_or(u64::MAX) <= ATTACH_UP_TO {
                let bytes = content(drive, id)?;
                let att = json!({ "@odata.type": "#microsoft.graph.fileAttachment", "name": fname, "contentBytes": base64::engine::general_purpose::STANDARD.encode(&bytes) });
                graph(reqwest::Method::POST, &format!("/me/messages/{}/attachments", lyra_web::oidc::encode(draft)), Some(&att))?;
                Ok(json!({ "attached": fname }))
            } else {
                // Too big to attach: a link at the top of the draft.
                let link = m["webUrl"].as_str().unwrap_or("");
                let body = d["body"]["content"].as_str().unwrap_or("");
                let line = format!("<p>📎 <a href=\"{link}\">{fname}</a></p>");
                let content = match body.to_lowercase().find("<body").and_then(|i| body[i..].find('>').map(|j| i + j + 1)) {
                    Some(at) => format!("{}{line}{}", &body[..at], &body[at..]),
                    None => format!("{line}{body}"),
                };
                graph(reqwest::Method::PATCH, &format!("/me/messages/{}", lyra_web::oidc::encode(draft)), Some(&json!({ "body": { "contentType": "html", "content": content } })))?;
                Ok(json!({ "linked": fname, "note": "too big to attach: a link is in the draft" }))
            }
        }
        other => Err(format!("{other} isn't a files tool")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_documents_read_as_text() {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default();
            z.start_file("word/document.xml", opts).unwrap();
            use std::io::Write;
            z.write_all(br#"<w:document><w:body><w:p><w:r><w:t>Firewall plan</w:t></w:r></w:p><w:p><w:r><w:t>Phase 1 &amp; 2</w:t></w:r></w:p></w:body></w:document>"#).unwrap();
            z.finish().unwrap();
        }
        assert_eq!(office_text(buf.get_ref(), "Plan.docx").unwrap(), "Firewall plan\nPhase 1 & 2");
    }
}

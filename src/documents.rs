//! The document workspace: letters, memos and one-pagers written side by side
//! with lyra. Each person's documents are Markdown files in their own folder
//! (`documents/<id>.md`); lyra drafts or revises one on request (the whole
//! text back, in the person's writing style), and a finished one goes to their
//! OneDrive as a Word file, onto a mail draft as an attachment, or downloads.
//! Nothing is sent: a mail stays a draft until they send it.

use std::path::PathBuf;

use base64::Engine;
use serde_json::{Value, json};

fn dir(user: &str) -> Option<PathBuf> {
    if user == lyra_web::users::OWNER { Some(crate::config::home()?.join("documents")) } else { Some(crate::context::user_dir(user)?.join("documents")) }
}

fn safe_id(id: &str) -> Result<String, String> {
    let id = id.trim();
    if id.is_empty() || id.len() > 80 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err("that isn't a document".into());
    }
    Ok(id.to_string())
}

fn path(user: &str, id: &str) -> Result<PathBuf, String> {
    Ok(dir(user).ok_or("no lyra home")?.join(format!("{}.md", safe_id(id)?)))
}

/// A document's title: its first heading, else its first line.
fn title_of(text: &str) -> String {
    let first = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("Untitled");
    first.trim_start_matches('#').trim().chars().take(80).collect::<String>()
}

fn new_id(title: &str) -> String {
    let s: String = title.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let s = s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    format!("{}-{}", s.chars().take(40).collect::<String>().trim_end_matches('-'), &lyra_learning::Uuid::new_v4().simple().to_string()[..4]).trim_start_matches('-').to_string()
}

/// Their documents, newest first.
pub fn list(user: &str) -> Value {
    let mut rows: Vec<(std::time::SystemTime, Value)> = std::fs::read_dir(match dir(user) {
        Some(d) => d,
        None => return json!([]),
    })
    .into_iter()
    .flatten()
    .flatten()
    .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
    .filter_map(|e| {
        let text = std::fs::read_to_string(e.path()).ok()?;
        let modified = e.metadata().ok()?.modified().ok()?;
        let id = e.path().file_stem()?.to_string_lossy().into_owned();
        Some((modified, json!({ "id": id, "title": title_of(&text), "words": text.split_whitespace().count(), "updated": chrono::DateTime::<chrono::Utc>::from(modified).to_rfc3339() })))
    })
    .collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.0));
    json!(rows.into_iter().map(|r| r.1).collect::<Vec<_>>())
}

pub fn read(user: &str, id: &str) -> Result<Value, String> {
    let text = std::fs::read_to_string(path(user, id)?).map_err(|_| "no such document".to_string())?;
    Ok(json!({ "id": id, "title": title_of(&text), "text": text }))
}

/// Keep it (`id` empty: a new one); its id back.
pub fn save(user: &str, id: &str, text: &str) -> Result<String, String> {
    if text.chars().count() > 200_000 {
        return Err("that document is too long (200,000 characters at most)".into());
    }
    let id = if id.trim().is_empty() { new_id(&title_of(text)) } else { safe_id(id)? };
    crate::store::write_text(&path(user, &id)?, text)?;
    Ok(id)
}

pub fn remove(user: &str, id: &str) -> Result<(), String> {
    std::fs::remove_file(path(user, id)?).map_err(|_| "no such document".to_string())
}

const EDIT: &str = "You write and revise documents (letters, memos, one-pagers, notices) with the person you work for. You get the document as it is now (it may be empty) and what they want. Answer with the whole document after the change, in Markdown (# for the title, ## for sections, - for lists, **bold**), and nothing else: no comments before or after, no code fences. Keep what they didn't ask to change. Write as they would.";

/// What lyra makes of `text` after `instruction`: the whole new text (not saved).
pub fn ask(user: &str, text: &str, instruction: &str, url: &str, model: &str) -> Result<String, String> {
    let instruction = instruction.trim();
    if instruction.is_empty() {
        return Err("tell lyra what to write or change".into());
    }
    let system = match crate::style::section(user) {
        Some(st) => format!("{EDIT}\n\n{st}"),
        None => EDIT.to_string(),
    };
    let input = format!("The document now:\n<<<\n{}\n>>>\n\nWhat they want: {instruction}", text.chars().take(60_000).collect::<String>());
    let (reply, _) = crate::learn::complete(url, model, &system, &input)?;
    let reply = reply.rsplit("</think>").next().unwrap_or(&reply).trim();
    // A fenced answer, despite the ask: the inside.
    let reply = reply.strip_prefix("```markdown").or_else(|| reply.strip_prefix("```md")).or_else(|| reply.strip_prefix("```")).map_or(reply, |r| r.trim_end().trim_end_matches("```")).trim();
    if reply.is_empty() {
        return Err("the model sent nothing back: try again".into());
    }
    Ok(reply.to_string())
}

fn file_name(title: &str) -> String {
    let name: String = title.chars().map(|c| if c.is_alphanumeric() || " -_.,()".contains(c) { c } else { ' ' }).collect();
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("{}.docx", if name.is_empty() { "Document".into() } else { name.chars().take(80).collect::<String>() })
}

const DOCX: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

/// The Word file, for downloading: its name and bytes (base64).
pub fn download(user: &str, id: &str) -> Result<Value, String> {
    let text = std::fs::read_to_string(path(user, id)?).map_err(|_| "no such document".to_string())?;
    let bytes = crate::docx::from_markdown(&text)?;
    Ok(json!({ "name": file_name(&title_of(&text)), "mime": DOCX, "base64": base64::engine::general_purpose::STANDARD.encode(bytes) }))
}

/// Save it to their OneDrive as a Word file (in "lyra", replacing one of the same name).
pub fn to_onedrive(user: &str, id: &str) -> Result<Value, String> {
    if !crate::graph::connected_for(user) {
        return Err("your Outlook isn't connected: More → Outlook → Connect".into());
    }
    let text = std::fs::read_to_string(path(user, id)?).map_err(|_| "no such document".to_string())?;
    let name = file_name(&title_of(&text));
    let bytes = crate::docx::from_markdown(&text)?;
    let item = crate::graph::graph_put(&format!("/me/drive/root:/lyra/{}:/content", lyra_web::oidc::encode(&name)), bytes, DOCX)?;
    Ok(json!({ "name": item["name"], "url": item["webUrl"] }))
}

/// A new mail draft with the Word file attached (and any recipients); nothing is sent.
pub fn attach_to_mail(user: &str, id: &str, to: &[String]) -> Result<Value, String> {
    if !crate::mail::connected_for(user) {
        return Err("your Outlook mail isn't connected: More → Outlook → Connect".into());
    }
    let text = std::fs::read_to_string(path(user, id)?).map_err(|_| "no such document".to_string())?;
    let title = title_of(&text);
    let bytes = crate::docx::from_markdown(&text)?;
    let recipients: Vec<Value> = to.iter().map(|a| a.trim()).filter(|a| a.contains('@')).map(|a| json!({ "emailAddress": { "address": a } })).collect();
    let draft = crate::graph::graph(reqwest::Method::POST, "/me/messages", Some(&json!({ "subject": title, "body": { "contentType": "text", "content": "" }, "toRecipients": recipients })))?;
    let draft_id = draft["id"].as_str().ok_or("Outlook made no draft")?;
    crate::graph::graph(
        reqwest::Method::POST,
        &format!("/me/messages/{}/attachments", lyra_web::oidc::encode(draft_id)),
        Some(&json!({ "@odata.type": "#microsoft.graph.fileAttachment", "name": file_name(&title), "contentType": DOCX, "contentBytes": base64::engine::general_purpose::STANDARD.encode(bytes) })),
    )?;
    Ok(json!({ "draft": draft_id, "subject": title, "link": draft["webLink"] }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_ids_and_file_names() {
        assert_eq!(title_of("\n# Office closed Friday\n\nDear all,"), "Office closed Friday");
        assert_eq!(title_of(""), "Untitled");
        assert!(new_id("Office closed: Friday!").starts_with("office-closed-friday-"));
        assert!(safe_id("../etc/passwd").is_err() && safe_id("memo-1a2b").is_ok());
        assert_eq!(file_name("Q4 / budget: draft?"), "Q4 budget draft.docx");
    }
}

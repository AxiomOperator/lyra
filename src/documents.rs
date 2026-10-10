//! The note editor: a note (or a letter, memo or one-pager) written side by
//! side with lyra. Notes are Markdown files in each person's notes folder
//! (`notes.rs`); here one opens by its file name, is saved as it's typed (a
//! new title renames its file), lyra drafts or revises it on request (the
//! whole text back, in the person's writing style), and a finished one goes
//! to their OneDrive as a Word file, onto a mail draft as an attachment, or
//! downloads. Nothing is sent: a mail stays a draft until they send it.

use std::path::PathBuf;

use base64::Engine;
use serde_json::{Value, json};

fn dir(user: &str) -> Option<PathBuf> {
    crate::notes::dir_for(user)
}

fn safe_id(id: &str) -> Result<String, String> {
    let id = id.trim();
    if id.is_empty() || id.len() > 120 || !id.chars().all(|c| c.is_alphanumeric() || c == '-') {
        return Err("that isn't a note".into());
    }
    Ok(id.to_string())
}

fn path(user: &str, id: &str) -> Result<PathBuf, String> {
    Ok(dir(user).ok_or("no lyra home")?.join(format!("{}.md", safe_id(id)?)))
}

/// A document's title: its first heading, else its first line.
pub fn title_of(text: &str) -> String {
    let first = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("Untitled");
    first.trim_start_matches('#').trim().chars().take(80).collect::<String>()
}

/// A file name for `title` in `dir` that isn't another note's (`keep`: this note's own).
pub fn free_id(dir: &std::path::Path, title: &str, keep: Option<&str>) -> String {
    let base = Some(crate::notes::slug(title)).filter(|s| !s.is_empty()).unwrap_or_else(|| "untitled".into());
    let taken = |id: &str| Some(id) != keep && dir.join(format!("{id}.md")).exists();
    (1..).map(|n| if n == 1 { base.clone() } else { format!("{base}-{n}") }).find(|id| !taken(id)).unwrap_or(base)
}

pub fn read(user: &str, id: &str) -> Result<Value, String> {
    let text = std::fs::read_to_string(path(user, id)?).map_err(|_| "no such note".to_string())?;
    Ok(json!({ "id": id, "title": title_of(&text), "text": text }))
}

/// Keep it (`id` empty: a new one); its id back, which follows its title
/// (a new title renames the file, unless another note has that name).
pub fn save(user: &str, id: &str, text: &str) -> Result<String, String> {
    if text.chars().count() > 200_000 {
        return Err("that note is too long (200,000 characters at most)".into());
    }
    let d = dir(user).ok_or("no lyra home")?;
    let old = (!id.trim().is_empty()).then(|| safe_id(id)).transpose()?;
    let new = free_id(&d, &title_of(text), old.as_deref());
    crate::store::write_text(&path(user, &new)?, text)?;
    if let Some(old) = old.filter(|o| *o != new) {
        let _ = std::fs::remove_file(path(user, &old)?);
    }
    Ok(new)
}

pub fn remove(user: &str, id: &str) -> Result<(), String> {
    std::fs::remove_file(path(user, id)?).map_err(|_| "no such note".to_string())
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
    let text = std::fs::read_to_string(path(user, id)?).map_err(|_| "no such note".to_string())?;
    let bytes = crate::docx::from_markdown(&text)?;
    Ok(json!({ "name": file_name(&title_of(&text)), "mime": DOCX, "base64": base64::engine::general_purpose::STANDARD.encode(bytes) }))
}

/// Save it to their OneDrive as a Word file (in "lyra", replacing one of the same name).
pub fn to_onedrive(user: &str, id: &str) -> Result<Value, String> {
    if !crate::graph::connected_for(user) {
        return Err("your Outlook isn't connected: Profile → Connections → Outlook → Connect".into());
    }
    let text = std::fs::read_to_string(path(user, id)?).map_err(|_| "no such note".to_string())?;
    let name = file_name(&title_of(&text));
    let bytes = crate::docx::from_markdown(&text)?;
    let item = crate::graph::graph_put(&format!("/me/drive/root:/lyra/{}:/content", lyra_web::oidc::encode(&name)), bytes, DOCX)?;
    Ok(json!({ "name": item["name"], "url": item["webUrl"] }))
}

/// A new mail draft with the Word file attached (and any recipients); nothing is sent.
pub fn attach_to_mail(user: &str, id: &str, to: &[String]) -> Result<Value, String> {
    if !crate::mail::connected_for(user) {
        return Err("your Outlook mail isn't connected: Profile → Connections → Outlook → Connect".into());
    }
    let text = std::fs::read_to_string(path(user, id)?).map_err(|_| "no such note".to_string())?;
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
        assert!(safe_id("../etc/passwd").is_err() && safe_id("memo-1a2b").is_ok());
        assert_eq!(file_name("Q4 / budget: draft?"), "Q4 budget draft.docx");
    }

    #[test]
    fn a_note_follows_its_title_and_never_takes_anothers_name() {
        crate::config::with_test_home(|home| {
            let d = home.join("notes");
            let id = save("owner", "", "# Untitled\n\n").unwrap();
            assert_eq!(id, "untitled");
            assert_eq!(save("owner", "", "# Untitled\n").unwrap(), "untitled-2", "a second untitled one");
            // Retitled: the file follows, and the old name is gone.
            let id = save("owner", &id, "# Office closed Friday\n\nDear all,").unwrap();
            assert_eq!(id, "office-closed-friday");
            assert!(d.join("office-closed-friday.md").exists() && !d.join("untitled.md").exists());
            // It reads as a note (title and text).
            let n = crate::acting::run("owner", || crate::notes::find("Office closed Friday")).unwrap();
            assert_eq!(n.text, "Dear all,");
            // Another note's title: a name of its own.
            assert_eq!(save("owner", "untitled-2", "# Office closed Friday\n").unwrap(), "office-closed-friday-2");
        });
    }

}

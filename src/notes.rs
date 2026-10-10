//! Each person's notes, lists and documents: one Markdown file each (the
//! owner's in `~/.lyra/notes`, anyone else's in `~/.lyra/users/<id>/notes`),
//! its `# Title` first. A list is a note of checklist lines (`- [ ] milk`); a
//! document is a longer note written with lyra in the editor (`documents.rs`:
//! drafts, Word, OneDrive, mail), marked as one in `.documents.json` (the
//! file names of the notes that are documents: started as one, moved from
//! the old Documents page, or switched in the editor). Said or typed in chat, kept here,
//! searchable; all theirs alone.

use std::path::PathBuf;

use chrono::Local;
use lyra_capabilities::{Capability, CapabilityKind, RiskLevel};
use serde_json::{Value, json};

fn dir() -> Option<PathBuf> {
    dir_for(&crate::acting::current())
}

/// Someone's notes folder.
pub fn dir_for(user: &str) -> Option<PathBuf> {
    if crate::acting::is_owner(user) { Some(crate::config::home()?.join("notes")) } else { Some(crate::context::user_dir(user)?.join("notes")) }
}

fn documents_file(dir: &std::path::Path) -> PathBuf {
    dir.join(".documents.json")
}

/// The file names of the notes in `dir` that are documents.
pub fn documents(dir: &std::path::Path) -> Vec<String> {
    crate::store::read_json(&documents_file(dir))
}

/// Mark a note as a document, or as a plain note again.
pub fn set_document(dir: &std::path::Path, slug: &str, on: bool) -> Result<(), String> {
    crate::store::JsonStore::<Vec<String>>::new(documents_file(dir)).update(|v| {
        v.retain(|x| x != slug);
        if on {
            v.push(slug.to_string());
            v.sort();
        }
    })
}

/// A note's file was renamed (a new title): it stays a document if it was one.
pub fn renamed(dir: &std::path::Path, old: &str, new: &str) -> Result<(), String> {
    crate::store::JsonStore::<Vec<String>>::new(documents_file(dir)).update(|v| {
        if v.iter().any(|x| x == old) {
            v.retain(|x| x != old && x != new);
            v.push(new.to_string());
            v.sort();
        }
    })
}

/// What a note is: "document", "list" or "note".
fn kind(n: &Note, documents: &[String]) -> &'static str {
    if documents.contains(&n.slug) {
        "document"
    } else if n.is_list() {
        "list"
    } else {
        "note"
    }
}

/// A title's file name.
pub fn slug(title: &str) -> String {
    let s: String = title.trim().to_lowercase().chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect();
    s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-").chars().take(60).collect()
}

#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub slug: String,
    pub title: String,
    pub text: String,
    pub updated: String,
}

impl Note {
    /// Checklist items: (done, text).
    pub fn items(&self) -> Vec<(bool, String)> {
        self.text
            .lines()
            .filter_map(|l| {
                let l = l.trim_start();
                l.strip_prefix("- [ ] ").map(|t| (false, t.trim().to_string())).or_else(|| l.strip_prefix("- [x] ").or_else(|| l.strip_prefix("- [X] ")).map(|t| (true, t.trim().to_string())))
            })
            .collect()
    }

    pub fn is_list(&self) -> bool {
        !self.items().is_empty()
    }
}

fn read_file(p: &std::path::Path) -> Option<Note> {
    let text = std::fs::read_to_string(p).ok()?;
    let slug = p.file_stem()?.to_string_lossy().to_string();
    let (title, body) = match text.split_once('\n') {
        Some((first, rest)) if first.starts_with("# ") => (first[2..].trim().to_string(), rest.trim_start_matches('\n').to_string()),
        _ => (slug.replace('-', " "), text.clone()),
    };
    let updated = p.metadata().ok().and_then(|m| m.modified().ok()).map(|t| chrono::DateTime::<Local>::from(t).format("%Y-%m-%d %H:%M").to_string()).unwrap_or_default();
    Some(Note { slug, title, text: body.trim_end().to_string(), updated })
}

fn write_note(n: &Note) -> Result<(), String> {
    let d = dir().ok_or("no lyra home")?;
    crate::store::write_text(&d.join(format!("{}.md", n.slug)), &format!("# {}\n\n{}\n", n.title.trim(), n.text.trim_end()))
}

/// Every note, newest first.
pub fn all() -> Vec<Note> {
    let Some(d) = dir() else { return vec![] };
    let mut v: Vec<Note> = std::fs::read_dir(d).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "md")).filter_map(|p| read_file(&p)).collect();
    v.sort_by(|a, b| b.updated.cmp(&a.updated));
    v
}

/// A note by its title (exact, then the only one containing it).
pub fn find(title: &str) -> Result<Note, String> {
    let key = slug(title);
    let all = all();
    if let Some(n) = all.iter().find(|n| n.slug == key) {
        return Ok(n.clone());
    }
    let found: Vec<&Note> = all.iter().filter(|n| n.slug.contains(&key) || n.title.to_lowercase().contains(&title.trim().to_lowercase())).collect();
    match found.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(format!("no note called {title:?}")),
        many => Err(format!("{} notes match {title:?}: {}", many.len(), many.iter().map(|n| n.title.as_str()).collect::<Vec<_>>().join(", "))),
    }
}

/// Save a note (new, replaced, or added to).
pub fn save(title: &str, text: &str, append: bool) -> Result<Note, String> {
    let title = if title.trim().is_empty() { format!("Note {}", Local::now().format("%Y-%m-%d %H:%M")) } else { title.trim().to_string() };
    let key = slug(&title);
    if key.is_empty() {
        return Err("a note needs a title".into());
    }
    let existing = all().into_iter().find(|n| n.slug == key);
    let text = match (existing, append) {
        (Some(old), true) => format!("{}\n\n{}", old.text, text.trim()),
        _ => text.trim().to_string(),
    };
    let n = Note { slug: key, title, text, updated: String::new() };
    write_note(&n)?;
    Ok(n)
}

/// Add items to a list (made when it doesn't exist).
pub fn list_add(list: &str, items: &[String]) -> Result<Note, String> {
    let mut n = find(list).or_else(|_| save(list, "", false))?;
    let have: Vec<String> = n.items().iter().map(|(_, t)| t.to_lowercase()).collect();
    for it in items.iter().map(|i| i.trim()).filter(|i| !i.is_empty()) {
        if !have.contains(&it.to_lowercase()) {
            n.text = format!("{}\n- [ ] {it}", n.text).trim_start().to_string();
        }
    }
    write_note(&n)?;
    Ok(n)
}

/// Tick (or untick, or remove) an item, matched by its words.
pub fn list_mark(list: &str, item: &str, mark: &str) -> Result<(Note, String), String> {
    let mut n = find(list)?;
    let key = item.trim().to_lowercase();
    let mut hit = None;
    let lines: Vec<String> = n
        .text
        .lines()
        .filter_map(|l| {
            let t = l.trim_start();
            let body = t.strip_prefix("- [ ] ").or_else(|| t.strip_prefix("- [x] ")).or_else(|| t.strip_prefix("- [X] "));
            match body {
                Some(b) if hit.is_none() && b.to_lowercase().contains(&key) => {
                    hit = Some(b.trim().to_string());
                    match mark {
                        "remove" => None,
                        "undone" => Some(format!("- [ ] {}", b.trim())),
                        _ => Some(format!("- [x] {}", b.trim())),
                    }
                }
                _ => Some(l.to_string()),
            }
        })
        .collect();
    let hit = hit.ok_or_else(|| format!("nothing like {item:?} on {}", n.title))?;
    n.text = lines.join("\n");
    write_note(&n)?;
    Ok((n, hit))
}

pub fn delete(title: &str) -> Result<Note, String> {
    let n = find(title)?;
    let d = dir().ok_or("no lyra home")?;
    std::fs::remove_file(d.join(format!("{}.md", n.slug))).map_err(|e| e.to_string())?;
    let _ = set_document(&d, &n.slug, false);
    Ok(n)
}

/// Notes with every word of `query` (title or text), newest first.
pub fn search(query: &str) -> Vec<Note> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    all().into_iter().filter(|n| {
        let hay = format!("{} {}", n.title, n.text).to_lowercase();
        words.iter().all(|w| hay.contains(w))
    }).collect()
}

fn view(n: &Note, documents: &[String]) -> Value {
    let items = n.items();
    json!({
        "kind": kind(n, documents),
        "slug": n.slug, "title": n.title, "updated": n.updated, "list": !items.is_empty(), "words": n.text.split_whitespace().count(),
        "items": items.iter().map(|(d, t)| json!({ "done": d, "text": t })).collect::<Vec<_>>(),
        "text": if items.is_empty() { n.text.chars().take(4000).collect::<String>() } else { String::new() },
    })
}

/// The Notes page.
pub fn page() -> Value {
    let documents = dir().map(|d| documents(&d)).unwrap_or_default();
    json!(all().iter().map(|n| view(n, &documents)).collect::<Vec<_>>())
}

// ---- tools

pub fn capabilities() -> Vec<Capability> {
    let tool = |name: &str, description: &str, risk: RiskLevel, properties: Value, required: &[&str]| {
        let mut c = Capability::new(name, CapabilityKind::NativeTool, description, risk);
        c.input_schema = json!({ "type": "object", "properties": properties, "required": required });
        c.source = "notes".into();
        c.tags = ["note", "notes", "list", "lists", "shopping", "idea", "jot", "write down", "checklist"].iter().map(|t| t.to_string()).collect();
        c
    };
    let title = json!({ "type": "string", "description": "The note's or list's name (e.g. \"groceries\", \"ideas\", \"meeting with Dana\")." });
    vec![
        tool("note_save", "Save a note for the user (\"note that the gate code is 4411\", \"jot down: …\"). append adds to the note of that name.", RiskLevel::LowWrite, json!({ "title": title, "text": { "type": "string" }, "append": { "type": "boolean" } }), &["text"]),
        tool("note_find", "Search the user's notes and lists by words; without words, list them all.", RiskLevel::ReadOnly, json!({ "query": { "type": "string" } }), &[]),
        tool("note_read", "One note or list in full.", RiskLevel::ReadOnly, json!({ "title": title }), &["title"]),
        tool("note_delete", "Delete one of the user's notes or lists.", RiskLevel::LowWrite, json!({ "title": title }), &["title"]),
        tool(
            "list_add",
            "Add items to one of the user's lists (made if it doesn't exist): \"add milk and eggs to groceries\".",
            RiskLevel::LowWrite,
            json!({ "list": title, "items": { "type": "array", "items": { "type": "string" } } }),
            &["list", "items"],
        ),
        tool(
            "list_mark",
            "Tick off (done), untick (undone) or remove an item on a list, matched by its words.",
            RiskLevel::LowWrite,
            json!({ "list": title, "item": { "type": "string" }, "mark": { "type": "string", "enum": ["done", "undone", "remove"] } }),
            &["list", "item"],
        ),
    ]
}

pub fn call(name: &str, args: &Value) -> Result<Value, String> {
    let s = |k: &str| args[k].as_str().unwrap_or("").to_string();
    match name {
        "note_save" => {
            let n = save(&s("title"), &s("text"), args["append"] == true)?;
            Ok(json!({ "saved": n.title }))
        }
        "note_find" => {
            let q = s("query");
            let found = if q.trim().is_empty() { all() } else { search(&q) };
            Ok(json!({ "notes": found.iter().take(30).map(|n| json!({ "title": n.title, "updated": n.updated, "list": n.is_list(), "preview": n.text.chars().take(160).collect::<String>() })).collect::<Vec<_>>() }))
        }
        "note_read" => Ok(view(&find(&s("title"))?, &dir().map(|d| documents(&d)).unwrap_or_default())),
        "note_delete" => Ok(json!({ "deleted": delete(&s("title"))?.title })),
        "list_add" => {
            let items: Vec<String> = args["items"].as_array().into_iter().flatten().filter_map(|i| i.as_str().map(str::to_string)).collect();
            let n = list_add(&s("list"), &items)?;
            Ok(json!({ "list": n.title, "open": n.items().iter().filter(|(d, _)| !d).count() }))
        }
        "list_mark" => {
            let mark = args["mark"].as_str().unwrap_or("done");
            let (n, item) = list_mark(&s("list"), &s("item"), mark)?;
            Ok(json!({ "list": n.title, "item": item, "marked": mark }))
        }
        other => Err(format!("{other} isn't a notes tool")),
    }
}

/// `/notes [words]`, `/note <title>: <text>`, `/list <name> [add <items, …>|done <item>|remove <item>]`.
pub fn command(name: &str, arg: &str) -> Result<String, String> {
    let a = arg.trim();
    match name {
        "/notes" => {
            let found = if a.is_empty() { all() } else { search(a) };
            if found.is_empty() {
                return Ok("no notes yet: /note <title>: <text>, /list <name> add <items>".into());
            }
            Ok(found.iter().map(|n| format!("{}{} · {}", n.title, if n.is_list() { format!(" ({} open)", n.items().iter().filter(|(d, _)| !d).count()) } else { String::new() }, n.updated)).collect::<Vec<_>>().join("\n"))
        }
        "/note" => {
            if let Some(t) = a.strip_prefix("delete ") {
                return delete(t).map(|n| format!("deleted {}", n.title));
            }
            let (title, text) = a.split_once(':').map_or(("", a), |(t, x)| (t.trim(), x.trim()));
            if text.is_empty() {
                return find(title).map(|n| format!("# {}\n{}", n.title, n.text));
            }
            save(title, text, true).map(|n| format!("noted in {}", n.title))
        }
        "/list" => {
            let (list, rest) = a.split_once(' ').map_or((a, ""), |(l, r)| (l, r.trim()));
            let (verb, what) = rest.split_once(' ').map_or((rest, ""), |(v, w)| (v, w.trim()));
            match verb {
                "" => find(list).map(|n| n.items().iter().map(|(d, t)| format!("{} {t}", if *d { "☑" } else { "☐" })).collect::<Vec<_>>().join("\n")).map(|t| if t.is_empty() { format!("{list} is empty") } else { t }),
                "add" => {
                    let items: Vec<String> = what.split([',', ';']).map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
                    list_add(list, &items).map(|n| format!("{}: {} open", n.title, n.items().iter().filter(|(d, _)| !d).count()))
                }
                "done" | "undone" | "remove" => list_mark(list, what, verb).map(|(n, item)| format!("{}: {item} {verb}", n.title)),
                _ => Err("usage: /list <name> [add <items, …> | done <item> | undone <item> | remove <item>]".into()),
            }
        }
        _ => Err("unknown".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checklists_read_and_titles_become_names() {
        let n = Note { slug: "groceries".into(), title: "Groceries".into(), text: "For the weekend\n- [ ] milk\n- [x] eggs\n  - [ ] bread".into(), updated: String::new() };
        assert_eq!(n.items(), vec![(false, "milk".into()), (true, "eggs".into()), (false, "bread".into())]);
        assert!(n.is_list());
        assert_eq!(slug("Meeting with Dana!"), "meeting-with-dana");
        assert_eq!(slug("  Ideas / Q4  "), "ideas-q4");
    }
}

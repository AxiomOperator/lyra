//! Projects: folders on a person's own PC, lent by their open lyra page
//! (the browser's File System Access). Requests only ever go to that
//! person's pages (`Hub::call_folder`); reading is theirs to do, a change
//! waits for their yes. No shell, nothing else on the PC.

use std::time::Duration;

use base64::Engine;
use lyra_capabilities::{Capability, CapabilityKind, RiskLevel};
use serde_json::{Value, json};

use crate::caps::{Ask, Remote};

/// A path inside the folder: relative, no `..`, no drive or root.
pub fn safe_path(path: &str) -> Result<String, String> {
    let p = path.trim().replace('\\', "/");
    if p.starts_with('/') || p.contains(':') || p.starts_with('~') {
        return Err(format!("{path:?}: a path inside the folder, like docs/plan.md"));
    }
    let parts: Vec<&str> = p.split('/').filter(|x| !x.is_empty() && *x != ".").collect();
    if parts.contains(&"..") {
        return Err(format!("{path:?}: can't go outside the folder"));
    }
    Ok(parts.join("/"))
}

pub fn capabilities() -> Vec<Capability> {
    let tool = |name: &str, description: &str, risk: RiskLevel, properties: Value, required: &[&str]| {
        let mut c = Capability::new(name, CapabilityKind::NativeTool, description, risk);
        c.input_schema = json!({ "type": "object", "properties": properties, "required": required });
        c.source = "projects".into();
        c.tags = ["project", "projects", "folder", "local", "file", "files", "pc", "computer", "code"].iter().map(|t| t.to_string()).collect();
        c
    };
    let folder = json!({ "type": "string", "description": "The folder's name (from project_folders)." });
    vec![
        tool("project_folders", "The project folders on the user's own PC that they've lent lyra (from their open lyra page): names, whether lyra may change them.", RiskLevel::ReadOnly, json!({}), &[]),
        tool(
            "project_list",
            "What's in a project folder (or a folder inside it).",
            RiskLevel::ReadOnly,
            json!({ "folder": folder, "path": { "type": "string", "description": "Inside the folder; empty for its top." }, "depth": { "type": "integer", "description": "How many levels down (1–4)." } }),
            &["folder"],
        ),
        tool(
            "project_read",
            "The text of a file in a project folder (text, code, Markdown, CSV, Word, Excel, PowerPoint).",
            RiskLevel::ReadOnly,
            json!({ "folder": folder, "path": { "type": "string" }, "max_chars": { "type": "integer" } }),
            &["folder", "path"],
        ),
        tool("project_search", "Find files in a project folder by name or by the text in them.", RiskLevel::ReadOnly, json!({ "folder": folder, "query": { "type": "string" } }), &["folder", "query"]),
        tool(
            "project_write",
            "Write a text file in a project folder (a new file, or replace or append to one). The user approves first, seeing the change.",
            RiskLevel::LowWrite,
            json!({ "folder": folder, "path": { "type": "string" }, "content": { "type": "string" }, "append": { "type": "boolean" } }),
            &["folder", "path", "content"],
        ),
    ]
}

/// A write needs the person's yes, showing what changes.
pub fn approval(name: &str, args: &Value) -> Option<Ask> {
    if name != "project_write" {
        return None;
    }
    let (folder, path) = (args["folder"].as_str().unwrap_or("?"), args["path"].as_str().unwrap_or("?"));
    let append = args["append"] == true;
    Some(Ask {
        what: format!("{} {path} in {folder}", if append { "add to" } else { "write" }),
        detail: args["content"].as_str().unwrap_or("").chars().take(800).collect(),
        why: if append { "it changes a file on your PC".into() } else { "it creates the file, or replaces it if it's there".into() },
        dangerous: false,
    })
}

const OFFICE: &[&str] = &[".docx", ".xlsx", ".pptx"];

pub fn call(remote: Option<&dyn Remote>, name: &str, args: &Value) -> Result<Value, String> {
    let remote = remote.ok_or("project folders come through lyra's app (lyra serve)")?;
    let user = crate::acting::current();
    if name == "project_folders" {
        let folders = remote.folders(&user);
        if folders.is_empty() {
            return Ok(json!({ "folders": [], "note": "none lent right now: in lyra's app, Projects → Open folder (lyra must be open on that PC)" }));
        }
        return Ok(json!({ "folders": folders.iter().map(|(pc, f)| json!({ "folder": f.name, "on": pc, "writable": f.writable, "needs_ok": !f.allowed })).collect::<Vec<_>>() }));
    }
    let folder = args["folder"].as_str().unwrap_or("").trim();
    if folder.is_empty() {
        return Err("which folder? (project_folders lists them)".into());
    }
    let path = safe_path(args["path"].as_str().unwrap_or(""))?;
    let ask = |req: Value, secs: u64| remote.call_folder(&user, folder, req, Duration::from_secs(secs));
    match name {
        "project_list" => ask(json!({ "op": "list", "path": path, "depth": args["depth"].as_u64().unwrap_or(1).clamp(1, 4) }), 30),
        "project_search" => {
            let query = args["query"].as_str().unwrap_or("").trim();
            if query.len() < 2 {
                return Err("search for what?".into());
            }
            ask(json!({ "op": "search", "path": path, "query": query }), 120)
        }
        "project_read" => {
            if path.is_empty() {
                return Err("which file?".into());
            }
            let max = args["max_chars"].as_u64().unwrap_or(12_000).clamp(500, 40_000) as usize;
            let office = OFFICE.iter().any(|x| path.to_lowercase().ends_with(x));
            let v = ask(json!({ "op": "read", "path": path, "binary": office, "max_bytes": if office { 15_000_000 } else { 400_000 } }), 60)?;
            let text = match v["base64"].as_str() {
                Some(b) => crate::files::office_text(&base64::engine::general_purpose::STANDARD.decode(b).map_err(|e| e.to_string())?, &path)?,
                None => match v["text"].as_str() {
                    Some(t) => t.to_string(),
                    None => return Ok(v),
                },
            };
            let cut = text.chars().count() > max || v["cut"] == true;
            Ok(json!({ "path": path, "text": text.chars().take(max).collect::<String>(), "cut": cut }))
        }
        "project_write" => {
            if path.is_empty() {
                return Err("which file?".into());
            }
            let content = args["content"].as_str().unwrap_or("");
            ask(json!({ "op": "write", "path": path, "content": content, "append": args["append"] == true }), 30)
        }
        other => Err(format!("{other} isn't a projects tool")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_inside_the_folder() {
        assert_eq!(safe_path("docs/./plan.md").unwrap(), "docs/plan.md");
        assert_eq!(safe_path("docs\\plan.md").unwrap(), "docs/plan.md");
        assert_eq!(safe_path("").unwrap(), "");
        for bad in ["../secret", "docs/../../x", "/etc/passwd", "C:/Windows", "~/x"] {
            assert!(safe_path(bad).is_err(), "{bad}");
        }
    }
}

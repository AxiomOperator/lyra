//! Each person's Microsoft Teams chats (read-only: Chat.Read), through their
//! Microsoft 365 connection: which chats have new messages, and what was said.

use chrono::{DateTime, Local, Utc};
use lyra_capabilities::{Capability, CapabilityKind, RiskLevel};
use serde_json::{Value, json};

use crate::calendar::graph;

pub fn connected_for(user: &str) -> bool {
    crate::calendar::connected_for(user) && crate::calendar::has(user, "Chat.Read")
}

fn ready() -> Result<(), String> {
    let user = crate::acting::current();
    if !crate::calendar::connected_for(&user) {
        return Err("your Microsoft 365 isn't connected: in the app, More → Outlook → Connect".into());
    }
    if !crate::calendar::has(&user, "Chat.Read") {
        return Err("lyra can't see your Teams chats yet: connect again (More → Outlook → Add Teams & files)".into());
    }
    Ok(())
}

/// A Teams message's text: HTML tags out, entities read.
pub fn text_of(html: &str) -> String {
    let mut out = String::new();
    let mut tag = false;
    for c in html.replace("<br>", "\n").replace("</p>", "\n").chars() {
        match c {
            '<' => tag = true,
            '>' => tag = false,
            c if !tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&nbsp;", " ").replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").trim().to_string()
}

fn when(v: &Value) -> String {
    v.as_str().and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map(|t| t.with_timezone(&Local).format("%a %b %-d %H:%M").to_string()).unwrap_or_default()
}

/// Recent chats: name, the last message, whether there's something unread.
pub fn chats(count: usize) -> Result<Vec<Value>, String> {
    ready()?;
    let v = graph(reqwest::Method::GET, &format!("/me/chats?$expand=lastMessagePreview,members&$top={}&$orderby=lastMessagePreview/createdDateTime desc", count.clamp(1, 50)), None)?;
    let me = crate::acting::current();
    let my_name = lyra_web::Users::open(&crate::config::home().unwrap_or_default().join("web")).get(&me).map(|u| u.name).unwrap_or_default();
    Ok(v["value"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            let last = &c["lastMessagePreview"];
            let others: Vec<String> = c["members"].as_array().into_iter().flatten().filter_map(|m| m["displayName"].as_str()).filter(|n| *n != my_name).map(str::to_string).collect();
            let name = c["topic"].as_str().filter(|t| !t.is_empty()).map(str::to_string).unwrap_or_else(|| others.join(", "));
            let at = last["createdDateTime"].as_str().and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map(|t| t.with_timezone(&Utc));
            let read = c["viewpoint"]["lastMessageReadDateTime"].as_str().and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map(|t| t.with_timezone(&Utc));
            let from = last["from"]["user"]["displayName"].as_str().unwrap_or("");
            json!({
                "id": c["id"],
                "chat": name,
                "kind": c["chatType"],
                "last_from": from,
                "last": text_of(last["body"]["content"].as_str().unwrap_or("")).chars().take(200).collect::<String>(),
                "at": when(&last["createdDateTime"]),
                "unread": at.zip(read).is_some_and(|(a, r)| a > r) && from != my_name,
            })
        })
        .collect())
}

pub fn capabilities() -> Vec<Capability> {
    let tool = |name: &str, description: &str, properties: Value, required: &[&str]| {
        let mut c = Capability::new(name, CapabilityKind::NativeTool, description, RiskLevel::ReadOnly);
        c.input_schema = json!({ "type": "object", "properties": properties, "required": required });
        c.source = "teams".into();
        c.tags = ["teams", "chat", "chats", "message", "messages", "unread", "microsoft"].iter().map(|t| t.to_string()).collect();
        c
    };
    vec![
        tool("teams_chats", "The user's recent Microsoft Teams chats: who, the last message, and which have something unread.", json!({ "unread_only": { "type": "boolean" }, "count": { "type": "integer" } }), &[]),
        tool("teams_read", "The latest messages in one Teams chat (by its id from teams_chats, or a person's name or the chat's topic).", json!({ "chat": { "type": "string" }, "count": { "type": "integer" } }), &["chat"]),
    ]
}

pub fn call(name: &str, args: &Value) -> Result<Value, String> {
    match name {
        "teams_chats" => {
            let mut list = chats(args["count"].as_u64().unwrap_or(20) as usize)?;
            if args["unread_only"] == true {
                list.retain(|c| c["unread"] == true);
            }
            Ok(json!({ "chats": list }))
        }
        "teams_read" => {
            let key = args["chat"].as_str().unwrap_or("").trim().to_string();
            // An id, or the chat whose name has these words.
            let id = if key.contains(':') && key.contains('@') {
                key.clone()
            } else {
                let k = key.to_lowercase();
                let list = chats(50)?;
                list.iter().find(|c| c["chat"].as_str().is_some_and(|n| n.to_lowercase().contains(&k))).and_then(|c| c["id"].as_str().map(str::to_string)).ok_or_else(|| format!("no Teams chat with {key:?}"))?
            };
            let n = args["count"].as_u64().unwrap_or(15).clamp(1, 50);
            let v = graph(reqwest::Method::GET, &format!("/me/chats/{}/messages?$top={n}", lyra_web::oidc::encode(&id)), None)?;
            let mut msgs: Vec<Value> = v["value"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|m| m["messageType"] == "message")
                .map(|m| json!({ "from": m["from"]["user"]["displayName"], "at": when(&m["createdDateTime"]), "text": text_of(m["body"]["content"].as_str().unwrap_or("")).chars().take(1500).collect::<String>() }))
                .collect();
            msgs.reverse();
            Ok(json!({ "messages": msgs }))
        }
        other => Err(format!("{other} isn't a Teams tool")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn teams_html_reads_as_text() {
        assert_eq!(text_of("<p>Can you <b>check</b> the firewall&nbsp;rules?</p><p>Thanks</p>"), "Can you check the firewall rules?\nThanks");
        assert_eq!(text_of("<at id=\"0\">Garrett</at> ping"), "Garrett ping");
    }
}

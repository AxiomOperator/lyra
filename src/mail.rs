//! Each person's Outlook mail (Microsoft Graph, through the same connection
//! as their calendar). Reading, searching, drafting and tidying their own
//! mailbox happen at once (only they see drafts, flags and folders); sending
//! waits for their yes, showing who it goes to and what it says.

use chrono::{DateTime, Local, Utc};
use lyra_capabilities::{Capability, CapabilityKind, RiskLevel};
use serde_json::{Value, json};

use crate::calendar::{graph, graph_with};
use crate::caps::Ask;

/// Bodies as plain text, times in UTC.
const TEXT: &str = "outlook.body-content-type=\"text\", outlook.timezone=\"UTC\"";
const SELECT: &str = "subject,from,toRecipients,ccRecipients,receivedDateTime,bodyPreview,isRead,importance,hasAttachments,flag,inferenceClassification,conversationId,isDraft,webLink";

/// Mail is connected for this person (their Outlook connection includes it).
pub fn connected_for(user: &str) -> bool {
    crate::calendar::connected_for(user) && crate::calendar::has(user, "Mail.ReadWrite")
}

fn ready() -> Result<(), String> {
    let user = crate::acting::current();
    if !crate::calendar::connected_for(&user) {
        return Err("your Outlook isn't connected: in the app, More → Outlook → Connect".into());
    }
    if !crate::calendar::has(&user, "Mail.ReadWrite") {
        return Err("lyra can't see your mail yet: connect Outlook again (More → Outlook) to add mail".into());
    }
    Ok(())
}

fn enc(s: &str) -> String {
    lyra_web::oidc::encode(s)
}

fn who(v: &Value) -> String {
    let a = &v["emailAddress"];
    match (a["name"].as_str().filter(|n| !n.is_empty()), a["address"].as_str()) {
        (Some(n), Some(addr)) if n != addr => format!("{n} <{addr}>"),
        (_, Some(addr)) => addr.to_string(),
        (Some(n), None) => n.to_string(),
        _ => "?".into(),
    }
}

fn when(v: &Value) -> String {
    v.as_str().and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map(|t| t.with_timezone(&Local).format("%a %b %-d %H:%M").to_string()).unwrap_or_default()
}

/// A message in a few fields.
fn brief(m: &Value) -> Value {
    let mut v = json!({
        "id": m["id"],
        "from": who(&m["from"]),
        "subject": m["subject"],
        "received": when(&m["receivedDateTime"]),
        "preview": m["bodyPreview"].as_str().unwrap_or("").chars().take(220).collect::<String>(),
    });
    if let Some(l) = m["webLink"].as_str() {
        v["link"] = json!(l);
    }
    if m["isRead"] == false {
        v["unread"] = json!(true);
    }
    if m["importance"] == "high" {
        v["important"] = json!(true);
    }
    if m["hasAttachments"] == true {
        v["attachments"] = json!(true);
    }
    if m["flag"]["flagStatus"] == "flagged" {
        v["flagged"] = json!(true);
    }
    if m["inferenceClassification"] == "other" {
        v["other"] = json!(true);
    }
    v
}

fn list(path: &str) -> Result<Vec<Value>, String> {
    Ok(graph_with(reqwest::Method::GET, path, None, TEXT)?["value"].as_array().cloned().unwrap_or_default())
}

/// The newest messages in the inbox (`unread`: only those; `focused`: not newsletters).
pub fn inbox(unread: bool, focused: bool, count: usize) -> Result<Vec<Value>, String> {
    ready()?;
    // Outlook wants the sort field first in the filter when there is one.
    let mut filters = vec!["receivedDateTime ge 2000-01-01T00:00:00Z"];
    if unread {
        filters.push("isRead eq false");
    }
    if focused {
        filters.push("inferenceClassification eq 'focused'");
    }
    let filter = format!("&$filter={}", enc(&filters.join(" and ")));
    list(&format!("/me/mailFolders/inbox/messages?$top={}&$select={SELECT}&$orderby=receivedDateTime desc{filter}", count.clamp(1, 50)))
}

/// Only what the sender wrote: quoted replies, forwarded mail and phone
/// signatures cut off.
pub fn own_text(body: &str) -> String {
    let mut out = Vec::new();
    for line in body.lines() {
        let t = line.trim();
        let quote_start = t.starts_with("-----Original Message")
            || t.starts_with("________________________________")
            || (t.starts_with("From:") && !out.is_empty())
            || (t.starts_with("On ") && t.ends_with("wrote:"))
            || t.starts_with("Sent from my ")
            || t.starts_with("Get Outlook for ");
        if quote_start {
            break;
        }
        if !t.starts_with('>') {
            out.push(line.trim_end());
        }
    }
    out.join("\n").trim().to_string()
}

/// The person's own words from their latest sent mail (to learn how they write).
pub fn sent_texts(n: usize) -> Result<Vec<String>, String> {
    ready()?;
    let found = list(&format!("/me/mailFolders/sentitems/messages?$top={}&$orderby=sentDateTime desc&$select=body,subject", n.clamp(1, 50)))?;
    Ok(found.iter().map(|m| own_text(m["body"]["content"].as_str().unwrap_or(""))).filter(|t| !t.is_empty()).collect())
}

/// Mail sent between two times (for follow-ups).
pub fn sent_between(from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<Value>, String> {
    ready()?;
    let f = format!("sentDateTime ge {} and sentDateTime le {}", from.format("%Y-%m-%dT%H:%M:%SZ"), to.format("%Y-%m-%dT%H:%M:%SZ"));
    list(&format!("/me/mailFolders/sentitems/messages?$filter={}&$top=40&$orderby=sentDateTime desc&$select=subject,from,toRecipients,sentDateTime,bodyPreview,conversationId", enc(&f)))
}

/// Mail received since a time, newest first (for "tell me when …").
pub fn received_since(since: DateTime<Utc>) -> Result<Vec<Value>, String> {
    ready()?;
    let f = format!("receivedDateTime ge {}", since.format("%Y-%m-%dT%H:%M:%SZ"));
    list(&format!("/me/messages?$filter={}&$top=50&$orderby=receivedDateTime desc&$select=id,subject,from,receivedDateTime,conversationId,isDraft", enc(&f)))
}

/// Every message in a conversation (who answered, when).
pub fn conversation(id: &str) -> Result<Vec<Value>, String> {
    ready()?;
    list(&format!("/me/messages?$filter={}&$select=from,receivedDateTime,isDraft&$top=50", enc(&format!("conversationId eq '{}'", id.replace('\'', "''")))))
}

// ---- tools

pub fn capabilities() -> Vec<Capability> {
    let tool = |name: &str, description: &str, risk: RiskLevel, properties: Value, required: &[&str]| {
        let mut c = Capability::new(name, CapabilityKind::NativeTool, description, risk);
        c.input_schema = json!({ "type": "object", "properties": properties, "required": required });
        c.source = "mail".into();
        c.tags = ["mail", "email", "inbox", "outlook", "message", "reply", "draft", "send", "unread"].iter().map(|t| t.to_string()).collect();
        c
    };
    let id = json!({ "type": "string", "description": "The message's id (from mail_inbox or mail_search)." });
    vec![
        tool(
            "mail_inbox",
            "The user's Outlook inbox, newest first: who, subject, when, a preview. unread (default true) and focused (default true: leaves newsletters out).",
            RiskLevel::ReadOnly,
            json!({ "unread": { "type": "boolean" }, "focused": { "type": "boolean" }, "count": { "type": "integer", "description": "How many (default 15)." } }),
            &[],
        ),
        tool(
            "mail_search",
            "Search the user's mail (all folders) by words, a person or a subject.",
            RiskLevel::ReadOnly,
            json!({ "query": { "type": "string", "description": "e.g. \"from:dana budget\", \"invoice october\"." }, "count": { "type": "integer" } }),
            &["query"],
        ),
        tool("mail_read", "One message in full: sender, recipients, the text, attachments' names.", RiskLevel::ReadOnly, json!({ "id": id }), &["id"]),
        tool("mail_thread", "The whole conversation a message belongs to, oldest first (to summarize a thread).", RiskLevel::ReadOnly, json!({ "id": id }), &["id"]),
        tool(
            "mail_draft",
            "Write a draft into the user's Drafts folder (nothing is sent): a reply to a message (reply_to, reply_all) or a new message (to, subject). Show the user the draft; mail_send sends it once they agree.",
            RiskLevel::LowWrite,
            json!({
                "reply_to": { "type": "string", "description": "A message id to answer." },
                "reply_all": { "type": "boolean" },
                "to": { "type": "array", "items": { "type": "string" }, "description": "Email addresses (a new message, or extra for a reply)." },
                "cc": { "type": "array", "items": { "type": "string" } },
                "subject": { "type": "string" },
                "body": { "type": "string", "description": "The text, written as the user would, in Markdown (paragraphs, **bold**, lists, links): it's sent as an HTML email." },
            }),
            &["body"],
        ),
        tool("mail_send", "Send a draft (from mail_draft). The user approves first, seeing who it goes to and what it says.", RiskLevel::Write, json!({ "id": { "type": "string", "description": "The draft's id." } }), &["id"]),
        tool(
            "mail_tidy",
            "Tidy the user's own mailbox: mark read or unread, flag or unflag, archive, or move to Deleted Items.",
            RiskLevel::LowWrite,
            json!({ "id": id, "action": { "type": "string", "enum": ["read", "unread", "flag", "unflag", "archive", "delete"] } }),
            &["id", "action"],
        ),
    ]
}

/// Sending needs the person's yes: everything else is theirs alone.
pub fn approval(name: &str, args: &Value) -> Option<Ask> {
    if name != "mail_send" {
        return None;
    }
    let id = args["id"].as_str().unwrap_or("");
    let d = graph_with(reqwest::Method::GET, &format!("/me/messages/{}?$select=subject,toRecipients,ccRecipients,body,isDraft", enc(id)), None, TEXT).ok();
    let (to, subject, text) = d.as_ref().map_or_else(
        || ("?".to_string(), "?".to_string(), String::new()),
        |d| {
            let mut people: Vec<String> = d["toRecipients"].as_array().into_iter().flatten().map(who).collect();
            people.extend(d["ccRecipients"].as_array().into_iter().flatten().map(|c| format!("cc {}", who(c))));
            (people.join(", "), d["subject"].as_str().unwrap_or("").to_string(), d["body"]["content"].as_str().unwrap_or("").trim().chars().take(600).collect())
        },
    );
    Some(Ask { what: format!("send an email to {to}: {subject}"), detail: text, why: "it goes out from your mailbox".into(), dangerous: false })
}

/// A draft's text as the HTML email it becomes: Markdown (paragraphs, lists,
/// bold, links, tables) rendered, in Outlook's usual font; HTML left as it is.
pub fn html(body: &str) -> String {
    let t = body.trim();
    let looks_html = t.starts_with('<') && t.contains("</");
    let inner = if looks_html {
        t.to_string()
    } else {
        use pulldown_cmark::{Options, Parser, html::push_html};
        let mut out = String::new();
        push_html(&mut out, Parser::new_ext(t, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH));
        out
    };
    format!("<div style=\"font-family: Aptos, Calibri, Arial, sans-serif; font-size: 11pt; color: #000000;\">{inner}</div>")
}

/// Put the answer above the quoted message in Outlook's own reply (inside its `<body>`).
fn above_quote(reply_html: &str, answer: &str) -> String {
    let lower = reply_html.to_lowercase();
    match lower.find("<body").and_then(|i| lower[i..].find('>').map(|j| i + j + 1)) {
        Some(at) => format!("{}{answer}<br>{}", &reply_html[..at], &reply_html[at..]),
        None => format!("{answer}<br>{reply_html}"),
    }
}

fn recipients(v: &Value) -> Result<Vec<Value>, String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|a| if a.contains('@') { Ok(json!({ "emailAddress": { "address": a.trim() } })) } else { Err(format!("{a:?} isn't an email address")) })
        .collect()
}

/// Run a mail tool as the person this thread works for (sending approved by the caller).
pub fn call(name: &str, args: &Value) -> Result<Value, String> {
    ready()?;
    match name {
        "mail_inbox" => {
            let found = inbox(args["unread"] != false, args["focused"] != false, args["count"].as_u64().unwrap_or(15) as usize)?;
            Ok(json!({ "messages": found.iter().map(brief).collect::<Vec<_>>() }))
        }
        "mail_search" => {
            let q = args["query"].as_str().unwrap_or("").replace('"', "");
            let n = args["count"].as_u64().unwrap_or(15).clamp(1, 40);
            let found = list(&format!("/me/messages?$search={}&$top={n}&$select={SELECT}", enc(&format!("\"{q}\""))))?;
            Ok(json!({ "messages": found.iter().map(brief).collect::<Vec<_>>() }))
        }
        "mail_read" => {
            let id = args["id"].as_str().unwrap_or("");
            let m = graph_with(reqwest::Method::GET, &format!("/me/messages/{}?$select={SELECT},body&$expand=attachments($select=name,size)", enc(id)), None, TEXT)?;
            let mut v = brief(&m);
            v["to"] = json!(m["toRecipients"].as_array().into_iter().flatten().map(who).collect::<Vec<_>>());
            v["cc"] = json!(m["ccRecipients"].as_array().into_iter().flatten().map(who).collect::<Vec<_>>());
            v["text"] = json!(m["body"]["content"].as_str().unwrap_or("").trim().chars().take(8000).collect::<String>());
            v["attachment_names"] = json!(m["attachments"].as_array().into_iter().flatten().filter_map(|a| a["name"].as_str()).collect::<Vec<_>>());
            Ok(v)
        }
        "mail_thread" => {
            let id = args["id"].as_str().unwrap_or("");
            let m = graph(reqwest::Method::GET, &format!("/me/messages/{}?$select=conversationId", enc(id)), None)?;
            let conv = m["conversationId"].as_str().ok_or("no conversation for that message")?;
            let mut found = list(&format!("/me/messages?$filter={}&$select={SELECT},body&$top=30", enc(&format!("conversationId eq '{}'", conv.replace('\'', "''")))))?;
            found.sort_by(|a, b| a["receivedDateTime"].as_str().cmp(&b["receivedDateTime"].as_str()));
            Ok(json!({ "messages": found.iter().map(|m| json!({ "from": who(&m["from"]), "received": when(&m["receivedDateTime"]), "text": m["body"]["content"].as_str().unwrap_or("").trim().chars().take(2500).collect::<String>() })).collect::<Vec<_>>() }))
        }
        "mail_draft" => {
            let body_text = args["body"].as_str().unwrap_or("").trim();
            if body_text.is_empty() {
                return Err("a draft needs some text".into());
            }
            let to = recipients(&args["to"])?;
            let cc = recipients(&args["cc"])?;
            let draft = match args["reply_to"].as_str().filter(|r| !r.is_empty()) {
                Some(original) => {
                    let verb = if args["reply_all"] == true { "createReplyAll" } else { "createReply" };
                    let d = graph(reqwest::Method::POST, &format!("/me/messages/{}/{verb}", enc(original)), Some(&json!({})))?;
                    let id = d["id"].as_str().ok_or("Outlook made no draft")?.to_string();
                    // The answer (HTML) above the quoted message Outlook put in.
                    let full = graph(reqwest::Method::GET, &format!("/me/messages/{}?$select=body,toRecipients,ccRecipients", enc(&id)), None)?;
                    let quoted = full["body"]["content"].as_str().unwrap_or("");
                    let content = if full["body"]["contentType"] == "html" { above_quote(quoted, &html(body_text)) } else { format!("{}<br><pre>{}</pre>", html(body_text), quoted.replace('<', "&lt;")) };
                    let mut patch = json!({ "body": { "contentType": "html", "content": content } });
                    if !to.is_empty() {
                        let mut all = full["toRecipients"].as_array().cloned().unwrap_or_default();
                        all.extend(to);
                        patch["toRecipients"] = json!(all);
                    }
                    if !cc.is_empty() {
                        let mut all = full["ccRecipients"].as_array().cloned().unwrap_or_default();
                        all.extend(cc);
                        patch["ccRecipients"] = json!(all);
                    }
                    graph(reqwest::Method::PATCH, &format!("/me/messages/{}", enc(&id)), Some(&patch))?
                }
                None => {
                    if to.is_empty() {
                        return Err("a new message needs someone to send it to".into());
                    }
                    let msg = json!({
                        "subject": args["subject"].as_str().unwrap_or(""),
                        "body": { "contentType": "html", "content": html(body_text) },
                        "toRecipients": to,
                        "ccRecipients": cc,
                    });
                    graph(reqwest::Method::POST, "/me/messages", Some(&msg))?
                }
            };
            Ok(json!({
                "draft": draft["id"],
                "to": draft["toRecipients"].as_array().into_iter().flatten().map(who).collect::<Vec<_>>(),
                "subject": draft["subject"],
                "note": "a draft in the user's Drafts folder: nothing is sent until mail_send (which they approve)",
            }))
        }
        "mail_send" => {
            let id = args["id"].as_str().unwrap_or("");
            let d = graph(reqwest::Method::GET, &format!("/me/messages/{}?$select=isDraft,subject", enc(id)), None)?;
            if d["isDraft"] != true {
                return Err("that isn't a draft: write one with mail_draft first".into());
            }
            graph(reqwest::Method::POST, &format!("/me/messages/{}/send", enc(id)), None)?;
            Ok(json!({ "sent": d["subject"] }))
        }
        "mail_tidy" => {
            let id = args["id"].as_str().unwrap_or("");
            let path = format!("/me/messages/{}", enc(id));
            match args["action"].as_str().unwrap_or("") {
                "read" | "unread" => graph(reqwest::Method::PATCH, &path, Some(&json!({ "isRead": args["action"] == "read" })))?,
                "flag" | "unflag" => graph(reqwest::Method::PATCH, &path, Some(&json!({ "flag": { "flagStatus": if args["action"] == "flag" { "flagged" } else { "notFlagged" } } })))?,
                "archive" => graph(reqwest::Method::POST, &format!("{path}/move"), Some(&json!({ "destinationId": "archive" })))?,
                "delete" => graph(reqwest::Method::POST, &format!("{path}/move"), Some(&json!({ "destinationId": "deleteditems" })))?,
                other => return Err(format!("{other:?}: read, unread, flag, unflag, archive or delete")),
            };
            Ok(json!({ "done": args["action"], "id": id }))
        }
        other => Err(format!("{other} isn't a mail tool")),
    }
}

/// `/mail [all|search <words>]`.
pub fn command(arg: &str) -> Result<String, String> {
    let a = arg.trim();
    let found = match a.split_once(' ').map_or((a, ""), |(x, y)| (x, y.trim())) {
        ("search", q) if !q.is_empty() => call("mail_search", &json!({ "query": q }))?,
        ("all", _) => call("mail_inbox", &json!({ "unread": true, "focused": false }))?,
        ("", _) => call("mail_inbox", &json!({}))?,
        _ => return Err("usage: /mail [all|search <words>]".into()),
    };
    let msgs = found["messages"].as_array().cloned().unwrap_or_default();
    if msgs.is_empty() {
        return Ok("no new mail from people".into());
    }
    Ok(msgs
        .iter()
        .map(|m| format!("{}{} · {} · {}", if m["important"] == true { "! " } else { "" }, m["received"].as_str().unwrap_or(""), m["from"].as_str().unwrap_or("?"), m["subject"].as_str().unwrap_or("")))
        .collect::<Vec<_>>()
        .join("\n"))
}

// ---- the briefing and the app

/// Mail at a glance: focused unread (people, not newsletters), flagged.
pub fn glance(since: DateTime<Utc>) -> Result<Value, String> {
    let unread = inbox(true, true, 25)?;
    let fresh: Vec<Value> = unread
        .iter()
        .filter(|m| m["receivedDateTime"].as_str().and_then(|s| DateTime::parse_from_rfc3339(s).ok()).is_some_and(|t| t.with_timezone(&Utc) > since))
        .map(brief)
        .collect();
    let flagged = list(&format!("/me/messages?$filter={}&$top=10&$select={SELECT}", enc("flag/flagStatus eq 'flagged'")))?;
    Ok(json!({ "unread": unread.len(), "new": fresh, "recent": unread.iter().take(8).map(brief).collect::<Vec<_>>(), "flagged": flagged.iter().map(brief).collect::<Vec<_>>() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_in_a_few_fields() {
        let m = json!({
            "id": "AAA", "subject": "Budget", "from": { "emailAddress": { "name": "Dana Doe", "address": "dana@fbcad.org" } },
            "receivedDateTime": "2026-10-07T15:00:00Z", "bodyPreview": "Can you look at the numbers", "isRead": false,
            "importance": "high", "hasAttachments": true, "flag": { "flagStatus": "flagged" }, "inferenceClassification": "focused",
        });
        let b = brief(&m);
        assert_eq!(b["from"], "Dana Doe <dana@fbcad.org>");
        assert!(b["unread"] == true && b["important"] == true && b["attachments"] == true && b["flagged"] == true);
        assert!(b.get("other").is_none());
        assert_eq!(who(&json!({ "emailAddress": { "address": "x@y.z" } })), "x@y.z");
    }

    #[test]
    fn drafts_are_html() {
        let h = html("Hi Dana,\n\nThe numbers:\n\n- **Q3**: up 4%\n- Q4: flat\n\nThanks,\nGarrett");
        assert!(h.starts_with("<div style=") && h.contains("<p>Hi Dana,</p>") && h.contains("<li><strong>Q3</strong>: up 4%</li>"), "{h}");
        assert!(html("<p>already <b>html</b></p>").contains("<p>already <b>html</b></p>"), "HTML stays");
        let reply = above_quote("<html><head><style></style></head><body dir=\"ltr\"><div>original</div></body></html>", "<p>yes</p>");
        assert_eq!(reply, "<html><head><style></style></head><body dir=\"ltr\"><p>yes</p><br><div>original</div></body></html>");
    }

    #[test]
    fn only_the_senders_own_words() {
        let body = "Hi Dana,\n\nSure, I'll send it Friday.\n\nThanks,\nGarrett\n\nSent from my iPhone\n\nFrom: Dana <d@x.org>\nSent: Monday\n> old";
        assert_eq!(own_text(body), "Hi Dana,\n\nSure, I'll send it Friday.\n\nThanks,\nGarrett");
        assert_eq!(own_text("Yes.\n________________________________\nFrom: x"), "Yes.");
        assert_eq!(own_text("Agreed.\n\nOn Tue, Oct 6, Dana wrote:\n> numbers?"), "Agreed.");
    }

    #[test]
    fn only_sending_asks() {
        assert!(approval("mail_draft", &json!({ "body": "hi" })).is_none(), "a draft is only theirs");
        assert!(approval("mail_tidy", &json!({ "id": "x", "action": "archive" })).is_none());
        assert!(approval("mail_inbox", &json!({})).is_none());
        assert!(recipients(&json!(["dana"])).unwrap_err().contains("email address"));
    }
}

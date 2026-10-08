//! One search for everything a person has with lyra: their conversations,
//! notes and lists, memories, mail, Teams chats and OneDrive/SharePoint files,
//! asked at once (the app's Ctrl-K). Each part searches only their own; one
//! that's slow or not connected is left out rather than holding the rest.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::mem::Mem;

/// Everything matching `query` for `user` (`owner`: whether they're lyra's owner).
pub fn everything(query: &str, user: &str, mem: Option<Arc<Mem>>) -> Value {
    let q = query.trim().to_string();
    if q.chars().count() < 2 {
        return json!({ "query": q, "groups": [] });
    }
    let (tx, rx) = std::sync::mpsc::channel::<(usize, &'static str, Vec<Value>)>();
    let spawn = |order: usize, name: &'static str, f: Box<dyn FnOnce() -> Vec<Value> + Send>| {
        let (tx, user) = (tx.clone(), user.to_string());
        std::thread::spawn(move || {
            let found = crate::acting::run(&user, f);
            let _ = tx.send((order, name, found));
        });
    };
    let (q1, u1) = (q.clone(), user.to_string());
    spawn(0, "Conversations", Box::new(move || {
        let Some(dir) = crate::sessions::dir() else { return vec![] };
        crate::sessions::search(&crate::sessions::list_for(&dir, &u1), &q1, 6)
            .into_iter()
            .map(|h| json!({ "title": if h.title.is_empty() { "(untitled)".into() } else { h.title }, "detail": h.snippet, "open": { "session": h.id }, "at": h.updated }))
            .collect()
    }));
    let q2 = q.clone();
    spawn(1, "Notes", Box::new(move || {
        crate::notes::call("note_find", &json!({ "query": q2 }))
            .map(|v| v["notes"].as_array().cloned().unwrap_or_default().into_iter().take(6).map(|n| json!({ "title": n["title"], "detail": n["preview"], "open": { "page": "notes" } })).collect())
            .unwrap_or_default()
    }));
    if let Some(mem) = mem {
        let (q3, u3) = (q.clone(), user.to_string());
        spawn(2, "Memories", Box::new(move || {
            let scope = (!crate::acting::is_owner(&u3)).then(|| format!("user:{u3}"));
            mem.recall(scope.as_deref(), &q3, 6, false)
                .map(|found| found.into_iter().map(|r| json!({ "title": r.memory.content.chars().take(140).collect::<String>(), "detail": r.memory.short_id(), "open": { "page": "memory" } })).collect())
                .unwrap_or_default()
        }));
    }
    if crate::mail::connected_for(user) {
        let q4 = q.clone();
        spawn(3, "Mail", Box::new(move || {
            crate::mail::call("mail_search", &json!({ "query": q4, "count": 6 }))
                .map(|v| v["messages"].as_array().cloned().unwrap_or_default().into_iter().map(|m| json!({ "title": m["subject"], "detail": format!("{} · {}", m["from"].as_str().unwrap_or(""), m["received"].as_str().unwrap_or("")), "open": { "link": m["link"] } })).collect())
                .unwrap_or_default()
        }));
    }
    if crate::teams::connected_for(user) {
        let q5 = q.to_lowercase();
        spawn(4, "Teams", Box::new(move || {
            crate::teams::chats(50)
                .map(|list| {
                    list.into_iter()
                        .filter(|c| [&c["chat"], &c["last"], &c["last_from"]].iter().any(|v| v.as_str().is_some_and(|s| s.to_lowercase().contains(&q5))))
                        .take(6)
                        .map(|c| json!({ "title": c["chat"], "detail": format!("{}: {}", c["last_from"].as_str().unwrap_or(""), c["last"].as_str().unwrap_or("")), "open": { "link": c["link"] } }))
                        .collect()
                })
                .unwrap_or_default()
        }));
    }
    if crate::calendar::connected_for(user) && crate::calendar::has(user, "Files.Read.All") {
        let q6 = q.clone();
        spawn(5, "Files", Box::new(move || {
            crate::files::search(&q6, 6)
                .map(|list| list.into_iter().map(|f| json!({ "title": f["name"], "detail": format!("{} · {}", f["where"].as_str().unwrap_or(""), f["by"].as_str().unwrap_or("")), "open": { "link": f["link"] } })).collect())
                .unwrap_or_default()
        }));
    }
    drop(tx);
    // What answers within 8 seconds.
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    let mut groups: Vec<(usize, &str, Vec<Value>)> = Vec::new();
    while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(g) => groups.push(g),
            Err(_) => break,
        }
    }
    groups.sort_by_key(|g| g.0);
    json!({
        "query": q,
        "groups": groups.into_iter().filter(|g| !g.2.is_empty()).map(|(_, name, items)| json!({ "name": name, "items": items })).collect::<Vec<_>>(),
    })
}

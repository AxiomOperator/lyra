//! `lyra serve`: the same lyra (memory, skills, agents, plans, goals) with
//! no terminal, reached from the PWA over a WebSocket. The app state drives
//! both front ends; here it's mirrored to the devices as small updates
//! (a new message, text appended to one, the status line) and the devices'
//! messages and approvals come back in. Push notifications go out when
//! nobody has lyra open.

use std::time::{Duration, Instant};

use lyra_web::{Hub, Inbound, Notification};
use serde_json::{Value, json};

use crate::{App, Level, Message, StreamEvent};

/// A message as the PWA shows it.
fn web_message(app: &App, m: &Message) -> Value {
    let tools: Vec<String> = m
        .tool_calls
        .iter()
        .map(|c| format!("{} {}", c.function.name, c.function.arguments.chars().take(160).collect::<String>()))
        .collect();
    json!({
        "role": m.role,
        "content": m.content,
        "reasoning": m.reasoning,
        "tools": tools,
        "stats": m.stats.as_ref().map(|s| crate::ui::reply_stats(s, &app.pricing)),
        "memories": m.memories,
        "skills": m.skills,
        "agents": m.agents,
    })
}

/// The status line, pending approvals and agents.
fn status(app: &App) -> Value {
    let active = app.agents.as_ref().map(|a| a.active.lock().map(|v| v.clone()).unwrap_or_default()).unwrap_or_default();
    json!({
        "phase": crate::ui::phase_text(app),
        "waiting": app.waiting,
        "model": app.model,
        "session": app.session_id,
        "title": app.messages.iter().find(|m| m.role == "user").map(|m| m.content.lines().next().unwrap_or("").chars().take(60).collect::<String>()),
        "approvals": app.approvals.iter().map(|r| json!({
            "id": r.id, "agent": r.agent, "what": r.what, "detail": r.detail, "why": r.why, "dangerous": r.dangerous,
        })).collect::<Vec<_>>(),
        "agents": app.agents_panel.iter().map(|a| json!({ "title": a.title, "working": active.contains(&a.title), "enabled": a.enabled })).collect::<Vec<_>>(),
    })
}

/// What the devices were last sent, to send only what changed.
#[derive(Default)]
pub struct Mirror {
    sent: Vec<Value>,
    status: Value,
    session: String,
}

impl Mirror {
    /// The updates that bring the devices up to date with the app.
    pub fn updates(&mut self, app: &App) -> Vec<Value> {
        let mut out = Vec::new();
        if self.session != app.session_id {
            // Another conversation (resumed or new): send it whole.
            self.session = app.session_id.clone();
            self.sent = app.messages.iter().map(|m| web_message(app, m)).collect();
            out.push(json!({ "type": "reset", "messages": self.sent }));
        } else {
            if app.messages.len() < self.sent.len() {
                self.sent.truncate(app.messages.len());
                out.push(json!({ "type": "truncate", "length": app.messages.len() }));
            }
            // Only the last few messages ever change (the reply being written).
            let from = self.sent.len().saturating_sub(3);
            for (i, m) in app.messages.iter().enumerate().skip(from) {
                let now = web_message(app, m);
                match self.sent.get(i) {
                    None => out.push(json!({ "type": "add", "index": i, "message": now })),
                    Some(before) if *before != now => out.push(change(i, before, &now)),
                    Some(_) => continue,
                }
                if i < self.sent.len() {
                    self.sent[i] = now;
                } else {
                    self.sent.push(now);
                }
            }
        }
        let s = status(app);
        if s != self.status {
            self.status = s.clone();
            out.push(json!({ "type": "status", "status": s }));
        }
        out
    }

    /// Everything, for a device that just connected (call after `updates`).
    pub fn snapshot(&self, seq: u64) -> Value {
        let commands: Vec<Value> = crate::commands::all()
            .iter()
            .map(|e| json!({ "usage": e.usage, "description": e.description, "completion": crate::commands::completion(e) }))
            .collect();
        json!({ "type": "snapshot", "seq": seq, "messages": self.sent, "status": self.status, "commands": commands })
    }
}

/// Text added to the end of a message (a reply streaming in) is sent as just
/// the new text; anything else replaces the message.
fn change(i: usize, before: &Value, now: &Value) -> Value {
    let (old_c, new_c) = (before["content"].as_str().unwrap_or(""), now["content"].as_str().unwrap_or(""));
    let (old_r, new_r) = (before["reasoning"].as_str().unwrap_or(""), now["reasoning"].as_str().unwrap_or(""));
    let mut same_rest = now.clone();
    same_rest["content"] = before["content"].clone();
    same_rest["reasoning"] = before["reasoning"].clone();
    if same_rest == *before && new_c.starts_with(old_c) && new_r.starts_with(old_r) {
        json!({ "type": "append", "index": i, "text": &new_c[old_c.len()..], "reasoning": &new_r[old_r.len()..] })
    } else {
        json!({ "type": "replace", "index": i, "message": now })
    }
}

/// A short, plain preview of a reply for a notification.
fn preview(text: &str) -> String {
    let plain: String = text.replace(['*', '`', '#', '_'], "").split_whitespace().collect::<Vec<_>>().join(" ");
    if plain.chars().count() > 160 { format!("{}…", plain.chars().take(160).collect::<String>()) } else { plain }
}

/// Run lyra for the devices until the process is stopped.
pub fn run(app: &mut App, hub: &Hub, inbound: std::sync::mpsc::Receiver<Inbound>, notify: bool) {
    let mut mirror = Mirror::default();
    let mut printed = app.logged;
    let mut last_status = Instant::now();
    loop {
        let mut changed = false;
        // Events from lyra's own work (replies streaming, agents, plans…).
        match app.rx.recv_timeout(Duration::from_millis(40)) {
            Ok(first) => {
                let mut next = Some(first);
                while let Some(event) = next.take() {
                    let note = notification(&event);
                    let done = matches!(event, StreamEvent::Done(_));
                    app.handle(event);
                    if done && notify {
                        // The reply is in the chat now.
                        let body = app.messages.iter().rev().find(|m| m.role == "assistant").map(|m| preview(&m.content)).unwrap_or_default();
                        if !hub.someone_watching() {
                            hub.notify(Notification { title: "lyra replied".into(), body, tag: "reply".into(), approval: None });
                        }
                    }
                    if let (Some(n), true) = (note, notify)
                        && !hub.someone_watching()
                    {
                        hub.notify(n);
                    }
                    changed = true;
                    next = app.rx.try_recv().ok();
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        }
        // What the devices ask for.
        while let Ok(msg) = inbound.try_recv() {
            changed = true;
            match msg {
                Inbound::Send { text, device } => {
                    if app.waiting && !text.starts_with('/') && app.approvals.is_empty() {
                        app.messages.push(Message::new("info", "lyra is still answering — send it again when the reply is done".into()));
                        continue;
                    }
                    app.log(Level::Info, format!("from {device}: {}", text.chars().take(80).collect::<String>()));
                    app.input = text;
                    app.send();
                }
                Inbound::Approve { id, answer, device } => {
                    app.log(Level::Agent, format!("{device} answered approval {id}: {answer}"));
                    app.answer_approval_id(id, &answer);
                }
                Inbound::Snapshot(reply) => {
                    for u in mirror.updates(app) {
                        hub.publish(u);
                    }
                    let _ = reply.send(mirror.snapshot(hub.seq()));
                }
            }
        }
        if !app.waiting && app.last_schedule_check.elapsed() > Duration::from_secs(10 * 60) {
            app.scheduled();
            changed = true;
        }
        if !app.waiting && !app.plan_busy && app.goals_checked.elapsed() > app.goals_every {
            app.goals_tick();
            changed = true;
        }
        // The phase timer ("thinking 4s") ticks while something is happening.
        if changed || (app.waiting && last_status.elapsed() > Duration::from_secs(1)) {
            last_status = Instant::now();
            for u in mirror.updates(app) {
                hub.publish(u);
            }
        }
        // The activity log goes to stdout (the systemd journal).
        if app.logged > printed {
            let new = ((app.logged - printed) as usize).min(app.activity.len());
            for a in &app.activity[app.activity.len() - new..] {
                println!("{} {}", a.time, a.text);
            }
            printed = app.logged;
        }
    }
}

/// A notification an event deserves (approvals, failures), if any.
fn notification(event: &StreamEvent) -> Option<Notification> {
    match event {
        StreamEvent::Approval(r) => Some(Notification {
            title: format!("{} needs your OK", r.agent),
            body: format!("{}: {}{}", r.what, r.detail.lines().next().unwrap_or(""), if r.dangerous { format!(" — {}", r.why) } else { String::new() }),
            tag: format!("approval-{}", r.id),
            approval: Some(r.id),
        }),
        StreamEvent::Error(e) => Some(Notification { title: "lyra hit an error".into(), body: preview(e), tag: "error".into(), approval: None }),
        StreamEvent::PlanFinished(result) => Some(Notification {
            title: "Plan update".into(),
            body: match result {
                Ok(_) => "A plan stopped running — open lyra to see how it went.".into(),
                Err(e) => preview(e),
            },
            tag: "plan".into(),
            approval: None,
        }),
        _ => None,
    }
}

/// `~/.lyra/systemd` unit for running `lyra serve` all the time.
pub fn service_unit(binary: &str) -> String {
    format!(
        "[Unit]\n\
         Description=lyra (web and phone access)\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         ExecStart={binary} serve\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         # Environment=LYRA_HOME=%h/.lyra\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_text_is_sent_as_appends() {
        let before = json!({ "role": "assistant", "content": "Hel", "reasoning": "", "tools": [] });
        let now = json!({ "role": "assistant", "content": "Hello", "reasoning": "", "tools": [] });
        assert_eq!(change(3, &before, &now), json!({ "type": "append", "index": 3, "text": "lo", "reasoning": "" }));
        let footer = json!({ "role": "assistant", "content": "Hello", "reasoning": "", "tools": [], "stats": "ttft 1s" });
        assert_eq!(change(3, &now, &footer)["type"], "replace", "other fields changed");
        let rewritten = json!({ "role": "assistant", "content": "Bye", "reasoning": "", "tools": [] });
        assert_eq!(change(3, &now, &rewritten)["type"], "replace");
        assert_eq!(preview("**Bold** and `code`\n\nnext"), "Bold and code next");
        assert!(service_unit("/home/me/.cargo/bin/lyra").contains("ExecStart=/home/me/.cargo/bin/lyra serve"));
    }
}

//! lyra asks in the chat, not in prose: a piece a tool call is missing (who an
//! email goes to) as a small form, and the steps of a round of several calls
//! before they run, each one skippable. The tool loop waits for the answer like
//! it waits for an approval: a form until it's filled in or dismissed, the
//! steps a few seconds and then they run.

use std::sync::mpsc::{RecvTimeoutError, Sender};
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::StreamEvent;

/// One thing to fill in.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    /// The argument it becomes.
    pub name: String,
    pub label: String,
    pub hint: String,
    /// Several values (email addresses), written with commas between.
    pub list: bool,
}

/// One call of a round, as the step preview shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub call_id: String,
    pub name: String,
    /// What it's about: the command, path, query or recipient.
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// A call is missing something only the user knows.
    Fill { tool: String, what: String, fields: Vec<Field> },
    /// The calls of a round, before they run (`seconds` to skip any).
    Steps { steps: Vec<Step>, seconds: u64 },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    Filled(Map<String, Value>),
    /// Run them, but not these calls.
    Go { skip: Vec<String> },
    /// Dismissed (or stopped).
    Cancel,
    /// The user is choosing steps to skip: wait for them.
    Hold,
}

pub struct Request {
    pub id: u64,
    /// The call it's about (the first one, for steps): the card shows there.
    pub call_id: String,
    pub kind: Kind,
    pub reply: Sender<Answer>,
}

/// Longest a form waits to be filled in.
const FILL_WAIT: u64 = 300;
/// How long the steps show before they run.
pub const STEPS_WAIT: u64 = 5;

/// Ask, and wait for the answer. A form not filled in (or the run stopped)
/// is a cancel; steps nobody touched run.
pub fn ask(tx: &Sender<StreamEvent>, call_id: &str, kind: Kind) -> Answer {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let wait = match &kind {
        Kind::Fill { .. } => FILL_WAIT,
        Kind::Steps { seconds, .. } => *seconds,
    };
    let steps = matches!(kind, Kind::Steps { .. });
    let (reply, answer) = std::sync::mpsc::channel();
    if tx.send(StreamEvent::Ask(Request { id, call_id: call_id.to_string(), kind, reply })).is_err() {
        return Answer::Cancel;
    }
    let mut wait = wait;
    let got = loop {
        match answer.recv_timeout(Duration::from_secs(wait)) {
            Ok(Answer::Hold) => wait = FILL_WAIT,
            other => break other,
        }
    };
    // The card goes either way.
    let _ = tx.send(StreamEvent::AskDone(id));
    match got {
        Ok(a) => a,
        Err(RecvTimeoutError::Timeout) if steps => Answer::Go { skip: Vec::new() },
        Err(_) => Answer::Cancel,
    }
}

/// What a device sent, as the answer to this request (`None`: not an answer).
pub fn read(kind: &Kind, value: &Value) -> Option<Answer> {
    if value == "cancel" {
        return Some(Answer::Cancel);
    }
    if value == "hold" {
        return matches!(kind, Kind::Steps { .. }).then_some(Answer::Hold);
    }
    match kind {
        Kind::Fill { fields, .. } => {
            let given = value.as_object()?;
            let mut out = Map::new();
            for f in fields {
                let text = given.get(&f.name).and_then(Value::as_str).unwrap_or("").trim();
                if text.is_empty() {
                    continue;
                }
                let v = if f.list { json!(text.split([',', ';']).map(str::trim).filter(|s| !s.is_empty()).collect::<Vec<_>>()) } else { json!(text) };
                out.insert(f.name.clone(), v);
            }
            Some(Answer::Filled(out))
        }
        Kind::Steps { .. } => {
            if value == "go" {
                return Some(Answer::Go { skip: Vec::new() });
            }
            let skip = value["skip"].as_array()?.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
            Some(Answer::Go { skip })
        }
    }
}

/// For the app: the request without its channel.
pub fn view(r: &Request) -> Value {
    match &r.kind {
        Kind::Fill { tool, what, fields } => json!({
            "id": r.id, "call_id": r.call_id, "kind": "fill", "tool": tool, "what": what,
            "fields": fields.iter().map(|f| json!({ "name": f.name, "label": f.label, "hint": f.hint, "list": f.list })).collect::<Vec<_>>(),
        }),
        Kind::Steps { steps, seconds } => json!({
            "id": r.id, "call_id": r.call_id, "kind": "steps", "seconds": seconds,
            "steps": steps.iter().map(|s| json!({ "call_id": s.call_id, "name": s.name, "summary": s.summary })).collect::<Vec<_>>(),
        }),
    }
}

/// A call's subject at a glance, for a step.
pub fn summary(arguments: &str) -> String {
    let v: Value = serde_json::from_str(arguments).unwrap_or(Value::Null);
    let pick = ["command", "path", "query", "url", "subject", "title", "name", "id"].iter().find_map(|k| v[*k].as_str().filter(|s| !s.trim().is_empty()));
    let to = v["to"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", ")).filter(|s| !s.is_empty());
    let text = pick.map(str::to_string).or(to).unwrap_or_default();
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() > 80 { format!("{}…", line.chars().take(79).collect::<String>()) } else { line.to_string() }
}

/// The filled-in values put into the call's arguments.
pub fn merge(arguments: &str, filled: &Map<String, Value>) -> String {
    let mut v: Value = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
    if !v.is_object() {
        v = json!({});
    }
    for (k, x) in filled {
        v[k] = x.clone();
    }
    v.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_are_read_by_kind() {
        let fill = Kind::Fill {
            tool: "mail_draft".into(),
            what: "a new email".into(),
            fields: vec![Field { name: "to".into(), label: "To".into(), hint: String::new(), list: true }, Field { name: "subject".into(), label: "Subject".into(), hint: String::new(), list: false }],
        };
        let Some(Answer::Filled(m)) = read(&fill, &json!({ "to": "a@x.org; b@x.org", "subject": "  " })) else { panic!() };
        assert_eq!(m["to"], json!(["a@x.org", "b@x.org"]));
        assert!(!m.contains_key("subject"), "left empty");
        assert_eq!(read(&fill, &json!("cancel")), Some(Answer::Cancel));
        let steps = Kind::Steps { steps: Vec::new(), seconds: 5 };
        assert_eq!(read(&steps, &json!("go")), Some(Answer::Go { skip: Vec::new() }));
        assert_eq!(read(&steps, &json!({ "skip": ["c2"] })), Some(Answer::Go { skip: vec!["c2".into()] }));
        assert_eq!(read(&steps, &json!(5)), None);
        assert_eq!(read(&steps, &json!("hold")), Some(Answer::Hold));
        assert_eq!(read(&fill, &json!("hold")), None, "a form waits anyway");
        assert_eq!(merge(r#"{"body":"hi"}"#, &m), r#"{"body":"hi","to":["a@x.org","b@x.org"]}"#);
        assert_eq!(summary(r#"{"command":"df -h\nls"}"#), "df -h");
        assert_eq!(summary(r#"{"to":["a@x.org"],"body":"x"}"#), "a@x.org");
    }

    #[test]
    fn steps_left_alone_run_and_a_dropped_form_cancels() {
        let (tx, rx) = std::sync::mpsc::channel();
        let t = std::thread::spawn(move || ask(&tx, "c1", Kind::Steps { steps: Vec::new(), seconds: 0 }));
        assert_eq!(t.join().unwrap(), Answer::Go { skip: Vec::new() });
        drop(rx);
        let (tx, rx) = std::sync::mpsc::channel::<StreamEvent>();
        let t = std::thread::spawn(move || ask(&tx, "c1", Kind::Fill { tool: "t".into(), what: String::new(), fields: Vec::new() }));
        // The app drops the request (stop): the form is a cancel.
        match rx.recv().unwrap() {
            StreamEvent::Ask(r) => drop(r),
            _ => panic!(),
        }
        assert_eq!(t.join().unwrap(), Answer::Cancel);
    }
}

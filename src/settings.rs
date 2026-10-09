//! The settings people change most, for the app's Settings page and
//! `/settings`: the models, working hours, the briefing and recap, and
//! notifications. One list (`FIELDS`) says what each is, how it's checked and
//! where it lives in config.toml; a change is written with `config::update`
//! (comments and everything else kept, never a file lyra can't read back) and
//! lyra reloads. Everything else stays in config.toml (see config.example.toml).

use serde_json::{Value, json};

use lyra_evolution::Behavior;

use crate::config::Config;

/// What a value is, and how it's checked.
#[derive(Clone, Copy)]
pub enum Kind {
    /// A model's name on its server.
    Model,
    /// An http(s) URL.
    Url,
    /// A time of day ("07:30", "5pm").
    Time,
    /// A time of day, or empty (the field's help says what empty means).
    TimeOrEmpty,
    /// "weekdays", "every day", or day names ("mon tue wed thu fri").
    Days,
    /// When, in the routines' words ("every day at 07:30", "weekdays at 8").
    Schedule,
    Bool,
    Int { min: i64, max: i64 },
    Real { min: f64, max: f64 },
    /// Short text ("$").
    Text,
    /// Names, comma-separated on the page; a TOML list in the file.
    List,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Model => "model",
            Kind::Url => "url",
            Kind::Time | Kind::TimeOrEmpty => "time",
            Kind::Days => "days",
            Kind::Schedule => "schedule",
            Kind::Bool => "bool",
            Kind::Int { .. } => "int",
            Kind::Real { .. } => "real",
            Kind::Text => "text",
            Kind::List => "list",
        }
    }
}

pub struct Field {
    /// Where it lives: "model" (the top of the file) or "planner.day_start".
    pub key: &'static str,
    pub group: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: Kind,
    /// Its current value, from the loaded config (or behavior.toml, for `behavior.` keys).
    pub get: fn(&Config, &Behavior) -> Value,
}

pub struct Group {
    pub id: &'static str,
    pub title: &'static str,
    pub help: &'static str,
}

pub const GROUPS: &[Group] = &[
    Group { id: "models", title: "Models", help: "The chat model and the helper models lyra calls. Embedding and reranker models stay in config.toml (changing them means re-embedding memory)." },
    Group { id: "replies", title: "Replies", help: "How far lyra goes in one reply before it stops and asks to continue. A person can have their own limit (Users)." },
    Group { id: "hours", title: "Working hours", help: "Your working day: Plan my day puts focus blocks inside it, and the recap comes at its end." },
    Group { id: "briefing", title: "Briefing and recap", help: "The morning briefing and the end-of-day recap." },
    Group { id: "notify", title: "Notifications", help: "What lyra pushes to paired phones and browsers, and what it does by itself." },
];

fn s(v: &str) -> Value {
    json!(v)
}

pub const FIELDS: &[Field] = &[
    // ---- models
    Field { key: "model", group: "models", label: "Chat model", help: "The model that answers in the chat (its name on the server below).", kind: Kind::Model, get: |c, _| s(&c.model) },
    Field { key: "url", group: "models", label: "Chat server", help: "Its OpenAI-compatible base URL, e.g. http://gpu:8080/v1.", kind: Kind::Url, get: |c, _| s(&c.url) },
    Field { key: "input_cost_per_mtok", group: "models", label: "Input price", help: "Per million prompt tokens, for AI usage (0 for a local model).", kind: Kind::Real { min: 0.0, max: 1000.0 }, get: |c, _| json!(c.input_cost_per_mtok) },
    Field { key: "output_cost_per_mtok", group: "models", label: "Output price", help: "Per million reply tokens.", kind: Kind::Real { min: 0.0, max: 1000.0 }, get: |c, _| json!(c.output_cost_per_mtok) },
    Field { key: "currency", group: "models", label: "Currency", help: "Shown before prices.", kind: Kind::Text, get: |c, _| s(&c.currency) },
    Field { key: "fallback_model.url", group: "models", label: "Fallback server", help: "Answers in the chat model's place while it can't be reached. Empty: none.", kind: Kind::Url, get: |c, _| s(c.fallback_model.as_ref().map_or("", |f| f.url.as_str())) },
    Field { key: "fallback_model.model", group: "models", label: "Fallback model", help: "Its name on that server.", kind: Kind::Model, get: |c, _| s(c.fallback_model.as_ref().map_or("", |f| f.model.as_str())) },
    Field { key: "vision_model.url", group: "models", label: "Vision server", help: "Reads pictures and scanned PDFs for a chat model that can't. Empty: not set up.", kind: Kind::Url, get: |c, _| s(c.vision_model.as_ref().map_or("", |v| v.url.as_str())) },
    Field { key: "vision_model.model", group: "models", label: "Vision model", help: "Its name on that server.", kind: Kind::Model, get: |c, _| s(c.vision_model.as_ref().map_or("", |v| v.model.as_str())) },
    Field { key: "decide.url", group: "models", label: "Decision server", help: "The small model that routes and classifies (llama-server). Empty: not set up.", kind: Kind::Url, get: |c, _| s(c.decide.as_ref().map_or("", |d| d.url.as_str())) },
    Field { key: "decide.model", group: "models", label: "Decision model", help: "Its name on that server.", kind: Kind::Model, get: |c, _| s(c.decide.as_ref().map_or("", |d| d.model.as_str())) },
    // ---- replies (behavior.toml: evolution may tune it too)
    Field { key: "behavior.max_tool_rounds", group: "replies", label: "Tool calls per reply", help: "Rounds of tool calls before lyra stops and offers Continue. Long jobs (an audit of a big codebase) need more; each round is a model call.", kind: Kind::Int { min: 1, max: crate::limits::MAX_TOOL_ROUNDS as i64 }, get: |_, b| json!(b.max_tool_rounds) },
    // ---- working hours
    Field { key: "planner.day_start", group: "hours", label: "Day starts", help: "", kind: Kind::Time, get: |c, _| s(&c.planner.day_start) },
    Field { key: "planner.day_end", group: "hours", label: "Day ends", help: "", kind: Kind::Time, get: |c, _| s(&c.planner.day_end) },
    Field { key: "planner.lunch_start", group: "hours", label: "Lunch from", help: "Kept free.", kind: Kind::Time, get: |c, _| s(&c.planner.lunch_start) },
    Field { key: "planner.lunch_end", group: "hours", label: "Lunch until", help: "", kind: Kind::Time, get: |c, _| s(&c.planner.lunch_end) },
    Field { key: "planner.days", group: "hours", label: "Working days", help: "weekdays, every day, or names: mon tue wed thu fri.", kind: Kind::Days, get: |c, _| s(&c.planner.days) },
    Field { key: "planner.enabled", group: "hours", label: "Plan my day", help: "Focus blocks for tasks in your calendar.", kind: Kind::Bool, get: |c, _| json!(c.planner.enabled) },
    Field { key: "planner.max_blocks", group: "hours", label: "Focus blocks a day", help: "At most.", kind: Kind::Int { min: 0, max: 12 }, get: |c, _| json!(c.planner.max_blocks) },
    // ---- briefing and recap
    Field { key: "briefing.enabled", group: "briefing", label: "Morning briefing", help: "Your day: meetings, tasks, mail, machines.", kind: Kind::Bool, get: |c, _| json!(c.briefing.enabled) },
    Field { key: "briefing.schedule", group: "briefing", label: "Briefing comes", help: "In words: every day at 07:30, weekdays at 8.", kind: Kind::Schedule, get: |c, _| s(&c.briefing.schedule) },
    Field { key: "briefing.notify", group: "briefing", label: "Push the briefing", help: "", kind: Kind::Bool, get: |c, _| json!(c.briefing.notify) },
    Field { key: "recap.enabled", group: "briefing", label: "End-of-day recap", help: "What got done, what's open, tomorrow's first meeting.", kind: Kind::Bool, get: |c, _| json!(c.recap.enabled) },
    Field { key: "recap.at", group: "briefing", label: "Recap comes at", help: "Empty: when your working day ends.", kind: Kind::TimeOrEmpty, get: |c, _| s(&c.recap.at) },
    Field { key: "recap.notify", group: "briefing", label: "Push the recap", help: "", kind: Kind::Bool, get: |c, _| json!(c.recap.notify) },
    // ---- notifications
    Field { key: "proactive.enabled", group: "notify", label: "Meeting prep and mail triage", help: "lyra looks ahead by itself: prep before meetings, tasks from mail, follow-ups.", kind: Kind::Bool, get: |c, _| json!(c.proactive.enabled) },
    Field { key: "proactive.prep_minutes", group: "notify", label: "Meeting prep, minutes before", help: "", kind: Kind::Int { min: 0, max: 240 }, get: |c, _| json!(c.proactive.prep_minutes) },
    Field { key: "proactive.followup_days", group: "notify", label: "Nudge about unanswered mail after (days)", help: "", kind: Kind::Int { min: 1, max: 60 }, get: |c, _| json!(c.proactive.followup_days) },
    Field { key: "proactive.drafts", group: "notify", label: "Draft replies to mail", help: "Drafts are never sent without your yes.", kind: Kind::Bool, get: |c, _| json!(c.proactive.drafts) },
    Field { key: "health.notify", group: "notify", label: "Machine problems", help: "Push when a machine's disk, memory, load or a service has a problem.", kind: Kind::Bool, get: |c, _| json!(c.health.notify) },
    Field { key: "health.disk_percent", group: "notify", label: "A disk is full at (%)", help: "", kind: Kind::Int { min: 50, max: 100 }, get: |c, _| json!(c.health.disk_percent) },
    Field { key: "health.offline_minutes", group: "notify", label: "A machine is quiet after (minutes)", help: "0: never tell.", kind: Kind::Int { min: 0, max: 1440 }, get: |c, _| json!(c.health.offline_minutes) },
    Field { key: "status.notify", group: "notify", label: "Something is down", help: "Push when the chat model, search, PMI or another check goes down, and when it's back.", kind: Kind::Bool, get: |c, _| json!(c.status.notify) },
    Field { key: "status.mute", group: "notify", label: "Never push about", help: "Checks by name, comma-separated (e.g. web search).", kind: Kind::List, get: |c, _| json!(c.status.mute) },
];

fn field(key: &str) -> Option<&'static Field> {
    FIELDS.iter().find(|f| f.key == key)
}

/// The Settings page: each group with its fields and their values now.
pub fn page() -> Value {
    let config = match Config::load() {
        Ok(c) => c,
        Err(e) => return json!({ "error": format!("config.toml doesn't read, so the page can't show it: {e}") }),
    };
    let behavior = crate::config::path().map(|p| behavior_at(&p)).unwrap_or_default();
    let groups: Vec<Value> = GROUPS
        .iter()
        .map(|g| {
            let fields: Vec<Value> = FIELDS
                .iter()
                .filter(|f| f.group == g.id)
                .map(|f| {
                    let mut v = json!({ "key": f.key, "label": f.label, "help": f.help, "kind": f.kind.name(), "value": (f.get)(&config, &behavior) });
                    if let Kind::Int { min, max } = f.kind {
                        v["min"] = json!(min);
                        v["max"] = json!(max);
                    }
                    if let Kind::Real { min, max } = f.kind {
                        v["min"] = json!(min);
                        v["max"] = json!(max);
                    }
                    v
                })
                .collect();
            json!({ "id": g.id, "title": g.title, "help": g.help, "fields": fields })
        })
        .collect();
    json!({ "groups": groups, "path": crate::config::path().map(|p| crate::context::show(&p)) })
}

/// behavior.toml, next to config.toml (defaults when there's none).
fn behavior_path(config: &std::path::Path) -> std::path::PathBuf {
    config.with_file_name("behavior.toml")
}

fn behavior_at(config: &std::path::Path) -> Behavior {
    std::fs::read_to_string(behavior_path(config)).ok().and_then(|t| Behavior::parse(&t).ok()).unwrap_or_default()
}

/// `value` checked for `f`, as it goes into config.toml.
fn check(f: &Field, value: &Value) -> Result<toml_edit::Item, String> {
    let text = || value.as_str().map(str::trim).ok_or_else(|| format!("{} should be text", f.label));
    Ok(match f.kind {
        Kind::Bool => toml_edit::value(value.as_bool().ok_or_else(|| format!("{} is on or off", f.label))?),
        Kind::Int { min, max } => {
            let n = value.as_i64().or_else(|| value.as_str().and_then(|t| t.trim().parse().ok())).ok_or_else(|| format!("{} should be a whole number", f.label))?;
            if !(min..=max).contains(&n) {
                return Err(format!("{} should be between {min} and {max}", f.label));
            }
            toml_edit::value(n)
        }
        Kind::Real { min, max } => {
            let n = value.as_f64().or_else(|| value.as_str().and_then(|t| t.trim().parse().ok())).ok_or_else(|| format!("{} should be a number", f.label))?;
            if !(min..=max).contains(&n) {
                return Err(format!("{} should be between {min} and {max}", f.label));
            }
            toml_edit::value(n)
        }
        Kind::Url => {
            let t = text()?;
            if !(t.starts_with("http://") || t.starts_with("https://")) {
                return Err(format!("{} should start with http:// or https://", f.label));
            }
            toml_edit::value(t)
        }
        Kind::Model | Kind::Text => {
            let t = text()?;
            if t.is_empty() {
                return Err(format!("{} can't be empty", f.label));
            }
            toml_edit::value(t)
        }
        Kind::Time | Kind::TimeOrEmpty => {
            let t = text()?;
            if t.is_empty() && matches!(f.kind, Kind::TimeOrEmpty) {
                return Ok(toml_edit::value(""));
            }
            let at = crate::routines::time_of_day(t).ok_or_else(|| format!("{}: {t:?} isn't a time (like 07:30 or 5pm)", f.label))?;
            toml_edit::value(at.format("%H:%M").to_string())
        }
        Kind::Days => {
            let t = text()?.to_lowercase();
            let names = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];
            let ok = t.contains("weekday") || t.contains("every") || (!t.is_empty() && t.split(|c: char| c == ',' || c.is_whitespace()).filter(|w| !w.is_empty()).all(|w| names.iter().any(|n| w.starts_with(n))));
            if !ok {
                return Err(format!("{}: say weekdays, every day, or day names (mon tue …)", f.label));
            }
            toml_edit::value(t)
        }
        Kind::Schedule => {
            let t = text()?;
            crate::routines::parse_schedule(t).map_err(|e| format!("{}: {e}", f.label))?;
            toml_edit::value(t)
        }
        Kind::List => {
            let list: Vec<String> = match value {
                Value::Array(a) => a.iter().filter_map(Value::as_str).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
                Value::String(t) => t.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect(),
                _ => return Err(format!("{} should be a list", f.label)),
            };
            crate::config::strings(&list)
        }
    })
}

/// The same value, as `get` gives it (to skip what didn't change).
fn as_value(item: &toml_edit::Item) -> Value {
    match item.as_value() {
        Some(toml_edit::Value::String(s)) => json!(s.value()),
        Some(toml_edit::Value::Integer(n)) => json!(n.value()),
        Some(toml_edit::Value::Float(n)) => json!(n.value()),
        Some(toml_edit::Value::Boolean(b)) => json!(b.value()),
        Some(toml_edit::Value::Array(a)) => json!(a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>()),
        _ => Value::Null,
    }
}

fn same(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => (x - y).abs() < 1e-9,
        _ => a == b,
    }
}

/// Change settings (`{ "planner.day_end": "17:00", … }`): every value is
/// checked first, and nothing is written unless all of them are fine. The
/// labels of what changed come back (empty: nothing to do).
pub fn set(changes: &Value) -> Result<Vec<String>, String> {
    set_in(&crate::config::path().ok_or("no home directory")?, changes)
}

fn set_in(path: &std::path::Path, changes: &Value) -> Result<Vec<String>, String> {
    let changes = changes.as_object().ok_or("no changes")?;
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let now: Config = toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let behavior = behavior_at(path);
    let mut writes: Vec<(&Field, toml_edit::Item)> = Vec::new();
    for (key, value) in changes {
        let f = field(key).ok_or_else(|| format!("{key} isn't a setting the app changes (it's in config.toml)"))?;
        // An optional model left empty and not set up yet: nothing to do.
        let optional = f.key.starts_with("vision_model.") || f.key.starts_with("decide.") || f.key.starts_with("fallback_model.");
        if optional && value.as_str().is_some_and(|t| t.trim().is_empty()) {
            if (f.get)(&now, &behavior).as_str().is_some_and(str::is_empty) {
                continue;
            }
            return Err(format!("{} can't be emptied here: remove its section from config.toml to turn it off", f.label));
        }
        let item = check(f, value)?;
        if !same(&as_value(&item), &(f.get)(&now, &behavior)) {
            writes.push((f, item));
        }
    }
    // A helper model set up for the first time needs both its server and its name.
    for section in ["vision_model", "decide", "fallback_model"] {
        let missing = |k: &str| (field(&format!("{section}.{k}")).map(|f| (f.get)(&now, &behavior))).is_some_and(|v| v.as_str().is_some_and(str::is_empty));
        let setting = |k: &str| writes.iter().any(|(f, _)| f.key == format!("{section}.{k}"));
        if (setting("url") && missing("model") && !setting("model")) || (setting("model") && missing("url") && !setting("url")) {
            let label = match section {
                "decide" => "decision",
                "fallback_model" => "fallback",
                _ => "vision",
            };
            return Err(format!("give the {label} model's server and its name together"));
        }
    }
    if writes.is_empty() {
        return Ok(Vec::new());
    }
    let labels = writes.iter().map(|(f, _)| f.label.to_string()).collect();
    let (behavior_writes, writes): (Vec<_>, Vec<_>) = writes.into_iter().partition(|(f, _)| f.key.starts_with("behavior."));
    if !behavior_writes.is_empty() {
        write_behavior(&behavior_path(path), behavior_writes)?;
    }
    if writes.is_empty() {
        return Ok(labels);
    }
    crate::config::update_file(path, |doc| {
        for (f, item) in writes {
            match f.key.split_once('.') {
                Some((section, key)) => {
                    let table = doc.entry(section).or_insert(toml_edit::table());
                    let t = table.as_table_mut().ok_or_else(|| format!("[{section}] in config.toml isn't a table"))?;
                    put(t, key, item);
                }
                None => put(doc.as_table_mut(), f.key, item),
            }
        }
        Ok(())
    })?;
    Ok(labels)
}

/// behavior.toml changed the same way: its other keys and comments kept,
/// never a file that doesn't read back.
fn write_behavior(path: &std::path::Path, writes: Vec<(&Field, toml_edit::Item)>) -> Result<(), String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("{} doesn't parse: {e}", path.display()))?;
    for (f, item) in writes {
        put(doc.as_table_mut(), f.key.trim_start_matches("behavior."), item);
    }
    let out = doc.to_string();
    Behavior::parse(&out).map_err(|e| format!("the change would break {}: {e}", path.display()))?;
    crate::store::write_text(path, &out)
}

/// Set `key`, keeping what's around the old value (a comment after it on its line).
fn put(t: &mut toml_edit::Table, key: &str, item: toml_edit::Item) {
    match (t.get_mut(key).and_then(toml_edit::Item::as_value_mut), item) {
        (Some(old), toml_edit::Item::Value(mut new)) => {
            *new.decor_mut() = old.decor().clone();
            *old = new;
        }
        (_, item) => t[key] = item,
    }
}

/// `/settings`: the common settings and their values; `/settings <key> <value>` changes one.
pub fn command(arg: &str) -> Result<String, String> {
    let arg = arg.trim();
    if let Some((key, value)) = arg.split_once(char::is_whitespace) {
        let f = field(key).ok_or_else(|| format!("{key} isn't one of these settings (/settings lists them)"))?;
        let value = value.trim();
        let v = match f.kind {
            Kind::Bool => json!(matches!(value.to_lowercase().as_str(), "on" | "yes" | "true" | "1")),
            _ => json!(value),
        };
        let changed = set(&json!({ key: v }))?;
        return Ok(if changed.is_empty() { format!("{} is already {value}", f.label) } else { format!("{} set to {value} (in config.toml; reloaded)", f.label) });
    }
    if !arg.is_empty() {
        return Err("/settings, or /settings <key> <value>".into());
    }
    let config = Config::load()?;
    let behavior = crate::config::path().map(|p| behavior_at(&p)).unwrap_or_default();
    let mut out = Vec::new();
    for g in GROUPS {
        out.push(g.title.to_string());
        for f in FIELDS.iter().filter(|f| f.group == g.id) {
            let v = (f.get)(&config, &behavior);
            let shown = match &v {
                Value::Bool(b) => (if *b { "on" } else { "off" }).to_string(),
                Value::String(t) if t.is_empty() => "—".into(),
                Value::String(t) => t.clone(),
                Value::Array(a) if a.is_empty() => "—".into(),
                Value::Array(a) => a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "),
                other => other.to_string(),
            };
            out.push(format!("  {:<34} {shown}   ({})", f.label, f.key));
        }
    }
    out.push("Change one: /settings <key> <value> · everything else is in config.toml".into());
    Ok(out.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(text: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("lyra-settings-{}-{}", std::process::id(), text.len()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(&path, text).unwrap();
        (dir, path)
    }

    #[test]
    fn every_field_reads_from_a_default_config() {
        let c: Config = toml::from_str("").unwrap();
        for f in FIELDS {
            assert!(GROUPS.iter().any(|g| g.id == f.group), "{} has a group", f.key);
            assert!(!(f.get)(&c, &Behavior::default()).is_null(), "{} reads", f.key);
        }
    }

    #[test]
    fn changes_are_checked_then_written_keeping_comments() {
        let (dir, path) = file("# my endpoint\nurl = \"http://x/v1\"\nmodel = \"a\"   # the usual one\n\n[planner]\n# my day\nday_end = \"16:30\"\n");
        let changed = set_in(&path, &json!({ "planner.day_end": "5pm", "model": "b", "recap.notify": false, "status.mute": "web search, PMI" })).unwrap();
        assert_eq!(changed.len(), 4, "{changed:?}");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# my endpoint") && text.contains("# my day") && text.contains("# the usual one"), "{text}");
        let c: Config = toml::from_str(&text).unwrap();
        assert_eq!((c.planner.day_end.as_str(), c.model.as_str(), c.recap.notify), ("17:00", "b", false));
        assert_eq!(c.status.mute, vec!["web search".to_string(), "PMI".to_string()]);
        // The same again: nothing to write.
        assert!(set_in(&path, &json!({ "planner.day_end": "17:00", "model": "b" })).unwrap().is_empty());
        // One bad value: nothing is written.
        let before = std::fs::read_to_string(&path).unwrap();
        assert!(set_in(&path, &json!({ "planner.day_start": "8:00", "health.disk_percent": 120 })).unwrap_err().contains("between 50 and 100"));
        assert!(set_in(&path, &json!({ "planner.day_start": "breakfast" })).is_err());
        assert!(set_in(&path, &json!({ "url": "gpu:8080" })).is_err());
        assert!(set_in(&path, &json!({ "briefing.schedule": "whenever" })).is_err());
        assert!(set_in(&path, &json!({ "web.listen": "0.0.0.0:1" })).unwrap_err().contains("isn't a setting"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        // A helper model needs its server and name together; empty and not set up is fine.
        assert!(set_in(&path, &json!({ "vision_model.url": "http://gpu:8090/v1" })).unwrap_err().contains("together"));
        assert!(set_in(&path, &json!({ "vision_model.url": "", "vision_model.model": "" })).unwrap().is_empty());
        assert_eq!(set_in(&path, &json!({ "vision_model.url": "http://gpu:8090/v1", "vision_model.model": "qwen-vl" })).unwrap().len(), 2);
        let c: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(c.vision_model.unwrap().model, "qwen-vl");
        // The tool-call limit goes to behavior.toml (its comments kept), within 1–64.
        std::fs::write(dir.join("behavior.toml"), "# tuned by evolution\nguidelines = [\"Be brief.\"]\nmax_tool_rounds = 8\n").unwrap();
        assert!(set_in(&path, &json!({ "behavior.max_tool_rounds": 65 })).is_err());
        assert_eq!(set_in(&path, &json!({ "behavior.max_tool_rounds": 40, "planner.days": "mon tue" })).unwrap().len(), 2);
        let b = std::fs::read_to_string(dir.join("behavior.toml")).unwrap();
        assert!(b.contains("# tuned by evolution") && b.contains("Be brief.") && b.contains("max_tool_rounds = 40"), "{b}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

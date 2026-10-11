//! The browser sandbox (`[sandbox]`): lyra's agents use a real browser in a
//! throw-away container on the machine the admin named (`machine`, a paired
//! lyra-node with Docker, or "server"). See `lyra_node::sandbox` for the
//! containers; this side offers the browser tools, keeps one session per
//! conversation (closed when the turn ends), asks before anything that sends
//! a form (`lyra_system::browser::check`, the same rule the machine applies),
//! signs in with a saved password the model never sees (`site-<host>` in
//! secrets.toml: `username:password`), and hands each page's picture to the
//! person (the chat), not the model. Admins only.

use std::collections::HashMap;
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

use lyra_capabilities::{Capability, CapabilityKind, RiskLevel};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::caps::{Ask, Remote};

/// `[sandbox]`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    /// The paired machine that runs the containers ("server": this one).
    pub machine: String,
    /// For the server's own (a paired machine has its own in node.toml).
    #[serde(flatten)]
    pub here: lyra_node::sandbox::Settings,
}

static SETTINGS: RwLock<Option<Settings>> = RwLock::new(None);

pub fn configure(s: Settings) {
    *SETTINGS.write().unwrap_or_else(|e| e.into_inner()) = Some(s);
}

pub fn settings() -> Settings {
    SETTINGS.read().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}

fn on_server(s: &Settings) -> bool {
    matches!(s.machine.trim().to_lowercase().as_str(), "" | "server" | "local" | "localhost")
}

/// The server's own sandbox status, looked at most once a minute (Docker is slow to ask).
fn here_status(s: &Settings) -> Value {
    static SEEN: Mutex<Option<(Instant, Value)>> = Mutex::new(None);
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((at, v)) = seen.as_ref()
        && at.elapsed() < Duration::from_secs(60)
    {
        return v.clone();
    }
    let v = lyra_node::sandbox::status(&s.here);
    *seen = Some((Instant::now(), v.clone()));
    v
}

/// How the sandbox is now (none: it's off).
pub fn status(remote: Option<&dyn Remote>) -> Option<Value> {
    let s = settings();
    if !s.enabled {
        return None;
    }
    let mut v = if on_server(&s) {
        here_status(&s)
    } else {
        let online = remote.is_some_and(|r| r.machines().iter().any(|(m, on)| m.eq_ignore_ascii_case(&s.machine) && *on));
        match remote.and_then(|r| r.sandbox(&s.machine)).filter(|v| v.is_object()) {
            Some(mut v) if online => {
                v["online"] = json!(true);
                v
            }
            Some(_) | None if !online => json!({ "ready": false, "why": format!("{} isn't connected", s.machine) }),
            _ => json!({ "ready": false, "why": format!("{} has no sandbox yet (lyra-node sandbox setup)", s.machine) }),
        }
    };
    v["machine"] = json!(if on_server(&s) { "server".to_string() } else { s.machine.clone() });
    v["sessions"] = json!(sessions());
    Some(v)
}

/// The browser tools are offered (it's on, and its machine is ready).
pub fn ready(remote: Option<&dyn Remote>) -> bool {
    status(remote).is_some_and(|v| v["ready"] == true)
}

// ---- sessions (one per conversation)

#[derive(Default)]
struct Open {
    url: String,
    title: String,
    since: Option<chrono::DateTime<chrono::Utc>>,
    /// The last page's elements: what an action is on.
    elements: Vec<Value>,
    /// Fields a saved password went into: their values never go back.
    secret_fields: Vec<u64>,
}

static OPEN: Mutex<Option<HashMap<String, Open>>> = Mutex::new(None);

fn with_open<T>(f: impl FnOnce(&mut HashMap<String, Open>) -> T) -> T {
    f(OPEN.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new))
}

/// This turn's session: its conversation's.
fn key() -> String {
    crate::usage::job().unwrap_or_else(|| "adhoc".into()).chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(40).collect()
}

thread_local! {
    /// The last page's picture, for the chat (taken by `take_picture`).
    static PICTURE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// The picture of the page a browser tool just went to (for the person).
pub fn take_picture() -> Option<String> {
    PICTURE.with(|p| p.borrow_mut().take())
}

fn send(s: &Settings, remote: Option<&dyn Remote>, key: &str, action: &str, args: &Value, approved: bool) -> Result<Value, String> {
    if on_server(s) {
        return lyra_node::sandbox::step(&s.here, key, action, args, approved);
    }
    let remote = remote.ok_or("machines aren't connected here")?;
    remote.call(&s.machine, json!({ "type": "browser", "session": key, "action": action, "args": args, "approved": approved }), Duration::from_secs(120))
}

/// The open sessions (Running now).
pub fn sessions() -> Vec<Value> {
    with_open(|all| {
        all.iter().map(|(k, o)| json!({ "session": k, "url": o.url, "title": o.title, "since": o.since.map(|t| t.to_rfc3339()) })).collect()
    })
}

/// Close a conversation's session (its turn ended, or Stop). Whether there was one.
pub fn end(remote: Option<&dyn Remote>, session: &str) -> bool {
    let key: String = session.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(40).collect();
    if with_open(|all| all.remove(&key)).is_none() {
        return false;
    }
    let _ = send(&settings(), remote, &key, "close", &json!({}), true);
    true
}

// ---- the tools

const ACTIONS: [&str; 10] = ["open", "read", "click", "fill", "select", "submit", "back", "screenshot", "login", "close"];

pub fn capabilities() -> Vec<Capability> {
    let tool = |name: &str, description: &str, risk: RiskLevel, properties: Value, required: &[&str]| {
        let mut c = Capability::new(name, CapabilityKind::NativeTool, description, risk);
        c.input_schema = json!({ "type": "object", "properties": properties, "required": required });
        c.source = "sandbox".into();
        c.tags = ["browser", "website", "web page", "form", "portal", "sign in", "log in", "click", "fill in", "submit", "book"].iter().map(|t| t.to_string()).collect();
        c
    };
    let n = json!({ "type": "integer", "description": "The element's number from the last read." });
    vec![
        tool(
            "browser_open",
            "Open a page in a real browser (a throw-away sandbox) to use it: sign in, fill in and send forms, click through sites without an API, pages that need JavaScript. Answers with the page's text and its numbered links, buttons and fields. For just reading a page, web_fetch is quicker.",
            RiskLevel::ReadOnly,
            json!({ "url": { "type": "string" } }),
            &["url"],
        ),
        tool("browser_read", "The browser's page again: its text and numbered links, buttons and fields.", RiskLevel::ReadOnly, json!({}), &[]),
        tool("browser_click", "Click a link or button on the browser's page, by its number. A button that sends a form waits for the user's yes.", RiskLevel::LowWrite, json!({ "n": n }), &["n"]),
        tool("browser_fill", "Type into a field on the browser's page, by its number (never card or payment details).", RiskLevel::LowWrite, json!({ "n": n, "text": { "type": "string" } }), &["n", "text"]),
        tool("browser_select", "Choose an option in a list on the browser's page.", RiskLevel::LowWrite, json!({ "n": n, "value": { "type": "string" } }), &["n", "value"]),
        tool("browser_submit", "Send a form on the browser's page (its send button's number, or the page's form). Waits for the user's yes.", RiskLevel::LowWrite, json!({ "n": n }), &[]),
        tool("browser_back", "Go back a page in the browser.", RiskLevel::ReadOnly, json!({}), &[]),
        tool("browser_screenshot", "Show the user the browser's page as it looks now.", RiskLevel::ReadOnly, json!({}), &[]),
        tool(
            "browser_login",
            "Sign in on the browser's page with the user's saved username and password for this site (lyra fills them in; you never see them). Give the username and password fields' numbers, and the sign-in button's to send it. Asks the user first.",
            RiskLevel::LowWrite,
            json!({ "username": n, "password": n, "submit": n }),
            &["username", "password"],
        ),
        tool("browser_close", "Close the browser (it closes by itself when this reply ends).", RiskLevel::ReadOnly, json!({}), &[]),
    ]
}

fn action_of(name: &str) -> Option<&'static str> {
    let a = name.strip_prefix("browser_")?;
    ACTIONS.iter().find(|x| **x == a).copied()
}

fn element(key: &str, n: Option<u64>) -> Option<Value> {
    let n = n?;
    with_open(|all| all.get(key).and_then(|o| o.elements.iter().find(|e| e["n"].as_u64() == Some(n)).cloned()))
}

fn host(url: &str) -> String {
    url.split("://").nth(1).unwrap_or("").split(['/', ':', '?', '#']).next().unwrap_or("").to_lowercase()
}

/// The secret's name for a site's saved sign-in.
pub fn secret_name(host: &str) -> String {
    format!("site-{}", host.trim_start_matches("www.").replace('.', "-"))
}

/// Whether this call waits for the user's yes, and what to tell them.
pub fn approval(name: &str, args: &Value) -> Option<Ask> {
    let action = action_of(name)?;
    let key = key();
    let el = element(&key, args["n"].as_u64().or(args["submit"].as_u64()));
    let (title, url) = with_open(|all| all.get(&key).map(|o| (o.title.clone(), o.url.clone()))).unwrap_or_default();
    let check = if action == "login" { lyra_system::browser::check("login", None, false) } else { lyra_system::browser::check(action, el.as_ref(), false) };
    match check {
        lyra_system::Check::Ask { why, dangerous } => {
            let what = match action {
                "login" => format!("sign in on {} with your saved password", host(&url)),
                _ => format!("send the form on {}", if title.is_empty() { host(&url) } else { title.clone() }),
            };
            // What it would send: the fields filled in so far (never passwords).
            let filled: Vec<String> = with_open(|all| all.get(&key).map(|o| {
                o.elements
                    .iter()
                    .filter(|e| e["value"].as_str().is_some_and(|v| !v.is_empty()) && !o.secret_fields.contains(&e["n"].as_u64().unwrap_or(0)))
                    .map(|e| format!("{}: {}", e["label"].as_str().unwrap_or("field"), e["value"].as_str().unwrap_or("")))
                    .collect()
            }))
            .unwrap_or_default();
            let button = el.as_ref().and_then(|e| e["label"].as_str()).map(|l| format!("the \"{l}\" button")).unwrap_or_default();
            let detail = [url.clone(), button, filled.join(" · ")].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join("\n");
            Some(Ask { what, detail, why, dangerous })
        }
        _ => None,
    }
}

/// One browser tool, for an admin's turn (`approved`: the user said yes).
pub fn call(remote: Option<&dyn Remote>, name: &str, args: &Value, approved: bool, member: bool) -> Result<Value, String> {
    if member {
        return Err("the browser sandbox is for admins".into());
    }
    let s = settings();
    if !s.enabled {
        return Err("the browser sandbox is off ([sandbox] enabled)".into());
    }
    let action = action_of(name).ok_or_else(|| format!("{name} isn't a browser tool"))?;
    let key = key();
    if action == "close" {
        return Ok(json!({ "closed": end(remote, &key) }));
    }
    if action == "login" {
        return login(&s, remote, &key, args, approved);
    }
    let mut args = args.clone();
    if matches!(action, "open" | "click" | "submit" | "back") {
        args["shot"] = json!(true);
    }
    // Known before the machine starts its container: the turn's end closes it,
    // even when every step failed (a page that didn't load still started one).
    track(&key);
    let mut v = send(&s, remote, &key, action, &args, approved)?;
    // What was typed, kept with its field: the yes for sending the form shows it.
    if matches!(action, "fill" | "select") {
        let (n, text) = (args["n"].as_u64(), args["text"].as_str().or(args["value"].as_str()).unwrap_or(""));
        with_open(|all| {
            if let Some(e) = all.get_mut(&key).and_then(|o| o.elements.iter_mut().find(|e| e["n"].as_u64() == n)) {
                e["value"] = json!(text.chars().take(80).collect::<String>());
            }
        });
    }
    Ok(kept(&key, &mut v))
}

/// This conversation has a browser (open, or being started).
fn track(key: &str) {
    with_open(|all| {
        all.entry(key.to_string()).or_default().since.get_or_insert_with(chrono::Utc::now);
    });
}

/// A page's answer kept for the session (its elements, page), its picture
/// put aside for the person, and anything a saved password went into left out.
fn kept(key: &str, v: &mut Value) -> Value {
    if let Some(shot) = v.as_object_mut().and_then(|m| m.remove("shot")).and_then(|s| s.as_str().map(str::to_string)) {
        PICTURE.with(|p| *p.borrow_mut() = Some(shot));
        v["picture"] = json!("shown to the user");
    }
    with_open(|all| {
        let o = all.entry(key.to_string()).or_default();
        o.since.get_or_insert_with(chrono::Utc::now);
        if let Some(u) = v["url"].as_str() {
            o.url = u.to_string();
        }
        if let Some(t) = v["title"].as_str() {
            o.title = t.to_string();
        }
        if let Some(list) = v["elements"].as_array_mut() {
            for e in list.iter_mut() {
                if o.secret_fields.contains(&e["n"].as_u64().unwrap_or(0))
                    && let Some(m) = e.as_object_mut()
                {
                    m.remove("value");
                }
            }
            o.elements = list.clone();
        }
    });
    v.clone()
}

fn login(s: &Settings, remote: Option<&dyn Remote>, key: &str, args: &Value, approved: bool) -> Result<Value, String> {
    if !approved {
        return Err("signing in needs the user's yes".into());
    }
    let url = with_open(|all| all.get(key).map(|o| o.url.clone())).ok_or("open the sign-in page first (browser_open)")?;
    let site = host(&url);
    let saved = crate::secrets::token(&secret_name(&site)).ok_or_else(|| format!("no saved sign-in for {site}: the admin saves one with lyra secret {} (username:password)", secret_name(&site)))?;
    let (user, password) = saved.split_once(':').ok_or_else(|| format!("the saved sign-in for {site} should be username:password"))?;
    let (u, p) = (args["username"].as_u64().ok_or("which field is the username? (its number)")?, args["password"].as_u64().ok_or("which field is the password? (its number)")?);
    for (n, text) in [(u, user), (p, password)] {
        if element(key, Some(n)).is_some_and(|e| e["payment"] == true) {
            return Err("that's a payment field".into());
        }
        send(s, remote, key, "fill", &json!({ "n": n, "text": text }), true)?;
    }
    with_open(|all| all.entry(key.to_string()).or_default().secret_fields.extend([u, p]));
    let mut out = json!({ "signed_in": site, "filled": [u, p], "note": "the saved username and password went in; you don't see them" });
    if let Some(b) = args["submit"].as_u64() {
        let mut v = send(s, remote, key, "submit", &json!({ "n": b, "shot": true }), true)?;
        out["page"] = kept(key, &mut v);
    }
    Ok(out)
}

/// `/sandbox [setup|stop <session|all>]`.
pub fn command(remote: Option<&dyn Remote>, arg: &str) -> Result<String, String> {
    let s = settings();
    if !s.enabled {
        return Err("the browser sandbox is off: [sandbox] enabled = true and machine = \"<a paired machine with Docker>\" in config.toml".into());
    }
    let (sub, rest) = arg.trim().split_once(' ').unwrap_or((arg.trim(), ""));
    match sub {
        "setup" => {
            let v = if on_server(&s) {
                lyra_node::sandbox::setup(&s.here).map(|said| json!({ "said": said }))?
            } else {
                let remote = remote.ok_or("machines aren't connected here")?;
                let v = remote.call(&s.machine, json!({ "type": "sandbox_setup" }), Duration::from_secs(1800))?;
                if let Some(st) = v.get("status") {
                    remote.set_sandbox(&s.machine, st.clone());
                }
                v
            };
            Ok(format!("sandbox on {}: {}", if on_server(&s) { "the server" } else { &s.machine }, v["said"].as_str().unwrap_or("done").replace(" · ", "\n")))
        }
        "stop" => {
            let keys: Vec<String> = if rest.trim() == "all" || rest.trim().is_empty() { with_open(|all| all.keys().cloned().collect()) } else { vec![rest.trim().to_string()] };
            let n = keys.iter().filter(|k| end(remote, k)).count();
            Ok(format!("closed {n} browser session{}", if n == 1 { "" } else { "s" }))
        }
        "" | "status" => {
            let v = status(remote).unwrap_or(Value::Null);
            let mut lines = vec![format!(
                "sandbox on {}: {}",
                v["machine"].as_str().unwrap_or("?"),
                if v["ready"] == true { "ready".to_string() } else { format!("not ready ({})", v["why"].as_str().unwrap_or("")) }
            )];
            for o in sessions() {
                lines.push(format!("  {} · {}", o["session"].as_str().unwrap_or(""), o["title"].as_str().filter(|t| !t.is_empty()).or(o["url"].as_str()).unwrap_or("")));
            }
            Ok(lines.join("\n"))
        }
        _ => Err("/sandbox [setup | stop <session|all>]".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The settings are process-wide: tests that change them take turns.
    static SETTINGS_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn a_page_is_kept_its_picture_goes_to_the_person_and_saved_passwords_never_come_back() {
        let key = "test-keep";
        let mut v = json!({ "url": "https://portal.example/login", "title": "Sign in", "shot": "data:image/jpeg;base64,AAAA", "elements": [
            { "n": 1, "kind": "field", "label": "User", "value": "dana" },
            { "n": 2, "kind": "field", "label": "Password", "password": true },
            { "n": 3, "kind": "button", "label": "Sign in", "submits": true },
        ] });
        let out = kept(key, &mut v);
        assert!(out.get("shot").is_none() && out["picture"] == "shown to the user", "the picture isn't for the model");
        assert_eq!(take_picture().as_deref(), Some("data:image/jpeg;base64,AAAA"));
        assert_eq!(take_picture(), None, "taken once");
        // A sign-in's fields: their values are left out from then on.
        with_open(|all| all.get_mut(key).unwrap().secret_fields.push(1));
        let again = kept(key, &mut json!({ "url": "https://portal.example/login", "title": "Sign in", "elements": [
            { "n": 1, "kind": "field", "label": "User", "value": "dana" },
            { "n": 3, "kind": "button", "label": "Sign in", "submits": true },
        ] }));
        assert!(again["elements"][0].get("value").is_none(), "{again}");
        // Sending the form asks, and says where and what.
        crate::usage::set_job(Some(key.into()));
        let ask = approval("browser_click", &json!({ "n": 3 })).unwrap();
        assert!(ask.what.contains("Sign in") && ask.detail.contains("\"Sign in\" button"), "{ask:?}");
        assert!(approval("browser_read", &json!({})).is_none());
        // What's been typed shows in the yes (a saved password's field never does).
        with_open(|all| all.get_mut(key).unwrap().elements.push(json!({ "n": 4, "kind": "field", "label": "Your name", "value": "Dana" })));
        let ask = approval("browser_submit", &json!({ "n": 3 })).unwrap();
        assert!(ask.detail.contains("Your name: Dana") && !ask.detail.contains("User: dana"), "{ask:?}");
        assert_eq!(secret_name("www.portal.example.org"), "site-portal-example-org");
        assert_eq!(host("https://Portal.Example:8443/a?b"), "portal.example");
    }

    #[test]
    fn a_browser_that_never_loaded_a_page_is_still_closed_at_the_end() {
        let _one = SETTINGS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        configure(Settings { enabled: true, machine: "sandbox1".into(), here: Default::default() });
        crate::usage::set_job(Some("test-failed-open".into()));
        // No machine to reach: the step fails, but the session is known.
        assert!(call(None, "browser_open", &json!({ "url": "https://example.com" }), false, false).is_err());
        assert!(sessions().iter().any(|s| s["session"] == "test-failed-open"));
        assert!(end(None, "test-failed-open"), "the turn's end closes it");
        assert!(!sessions().iter().any(|s| s["session"] == "test-failed-open"));
        configure(Settings::default());
    }

    #[test]
    fn members_and_an_off_sandbox_never_get_a_browser() {
        let _one = SETTINGS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        assert!(call(None, "browser_open", &json!({ "url": "https://example.com" }), false, true).unwrap_err().contains("admins"));
        configure(Settings::default());
        assert!(call(None, "browser_open", &json!({ "url": "https://example.com" }), false, false).unwrap_err().contains("off"));
        assert!(status(None).is_none() && !ready(None));
    }
}

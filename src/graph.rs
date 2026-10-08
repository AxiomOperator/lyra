//! Microsoft Graph for each person: lyra's Microsoft sign-in (`[web.entra]`),
//! their connection (the refresh token in their own secrets, `[graph] token`,
//! and what they granted), access tokens, and the requests every Microsoft 365
//! module makes (calendar, mail, Teams, files, meetings). Requests run as the
//! person the thread works for (`acting::current`).

use std::collections::HashMap;
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeZone, Utc};
use serde_json::{Value, json};

const GRAPH: &str = "https://graph.microsoft.com/v1.0";

static ENTRA: RwLock<Option<lyra_web::oidc::Entra>> = RwLock::new(None);

/// The Microsoft app lyra signs in with (and its secret).
pub fn configure(entra: lyra_web::oidc::Entra) {
    *ENTRA.write().unwrap_or_else(|e| e.into_inner()) = Some(entra);
}

/// Calendars can be connected here (Microsoft sign-in is set up).
pub fn available() -> bool {
    ENTRA.read().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|e| e.ready())
}

/// Connections may ask for Teams meeting transcripts (`[web.entra] meetings`).
pub fn meetings_enabled() -> bool {
    ENTRA.read().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|e| e.meetings)
}

pub fn connected_for(user: &str) -> bool {
    crate::secrets::token_for("graph", user).is_some()
}

/// What a person let lyra do (Graph scopes, from their connection).
pub fn granted(user: &str) -> Vec<String> {
    crate::secrets::token_for("graph_scope", user).map(|s| s.split(',').map(str::to_string).collect()).unwrap_or_else(|| vec!["Calendars.ReadWrite".into()])
}

pub fn has(user: &str, scope: &str) -> bool {
    granted(user).iter().any(|s| s.eq_ignore_ascii_case(scope) || s.to_lowercase().ends_with(&format!("/{}", scope.to_lowercase())))
}

/// "calendar, mail, Teams and files" / "calendar and mail" / "calendar".
pub fn granted_text(user: &str) -> String {
    match (has(user, "Mail.ReadWrite"), has(user, "Chat.Read")) {
        (_, true) => "calendar, mail, Teams and files".into(),
        (true, false) => "calendar and mail".into(),
        _ => "calendar".into(),
    }
}

/// Keep what Microsoft granted (its answer lists scopes with spaces; kept with commas).
pub fn keep_scope(user: &str, scope: &str) -> Result<(), String> {
    let list: Vec<&str> = scope.split_whitespace().map(|s| s.rsplit('/').next().unwrap_or(s)).collect();
    if list.is_empty() {
        return Ok(());
    }
    crate::secrets::set_token_for("graph_scope", &list.join(","), user)
}

pub fn forget_access(user: &str) {
    if let Some(m) = ACCESS.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        m.remove(user);
    }
}

/// Access tokens, by person, until they expire.
static ACCESS: Mutex<Option<HashMap<String, (String, Instant)>>> = Mutex::new(None);

fn http() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder().connect_timeout(Duration::from_secs(5)).timeout(Duration::from_secs(30)).build().map_err(|e| e.to_string())
}

/// A current access token for this person: the cached one, or a new one from
/// their refresh token (which Microsoft replaces each time: kept again).
fn access(user: &str) -> Result<String, String> {
    if let Some((t, until)) = ACCESS.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(user)).cloned()
        && Instant::now() < until
    {
        return Ok(t);
    }
    let refresh = crate::secrets::token_for("graph", user).ok_or("your Outlook calendar isn't connected: More → Connect Outlook calendar in the app")?;
    let entra = ENTRA.read().unwrap_or_else(|e| e.into_inner()).clone().filter(|e| e.ready()).ok_or("Microsoft sign-in isn't set up on this server")?;
    // Ask for what they granted (a connection from before mail: the calendar only).
    let scope = if has(user, "OnlineMeetingTranscript.Read.All") {
        lyra_web::oidc::OUTLOOK_MEETINGS
    } else if has(user, "Chat.Read") {
        lyra_web::oidc::OUTLOOK
    } else if has(user, "Mail.ReadWrite") {
        lyra_web::oidc::OUTLOOK_MAIL
    } else {
        lyra_web::oidc::CALENDAR
    }
    .to_string();
    let form = [
        ("grant_type", "refresh_token"),
        ("client_id", entra.client_id.trim()),
        ("client_secret", entra.secret.as_deref().unwrap_or("")),
        ("refresh_token", refresh.as_str()),
        ("scope", scope.as_str()),
    ]
    .iter()
    .map(|(k, v)| format!("{k}={}", lyra_web::oidc::encode(v)))
    .collect::<Vec<_>>()
    .join("&");
    let resp = http()?
        .post(lyra_web::oidc::token_url(&entra))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(form)
        .send()
        .map_err(|e| format!("Microsoft isn't answering: {e}"))?;
    let ok = resp.status().is_success();
    let body: Value = serde_json::from_str(&resp.text().unwrap_or_default()).unwrap_or(json!({}));
    if !ok {
        if body["error"] == "invalid_grant" {
            return Err("the calendar connection has expired: connect it again (More → Connect Outlook calendar)".into());
        }
        return Err(format!("Microsoft refused: {}", body["error_description"].as_str().unwrap_or("?").lines().next().unwrap_or("")));
    }
    let token = body["access_token"].as_str().ok_or("Microsoft sent no access token")?.to_string();
    if let Some(next) = body["refresh_token"].as_str().filter(|t| !t.is_empty()) {
        crate::secrets::set_token_for("graph", next, user)?;
    }
    if let Some(s) = body["scope"].as_str() {
        let _ = keep_scope(user, s);
    }
    let life = body["expires_in"].as_u64().unwrap_or(3600).saturating_sub(120);
    ACCESS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Default::default).insert(user.to_string(), (token.clone(), Instant::now() + Duration::from_secs(life)));
    Ok(token)
}

/// Forget a person's connection (`/calendar disconnect`).
pub fn disconnect(user: &str) -> Result<(), String> {
    forget_access(user);
    crate::secrets::set_token_for("graph_scope", "", user)?;
    crate::secrets::set_token_for("graph", "", user)
}

/// A Graph path ("/me/events") as its URL; a full URL (a next page) as it is.
fn url(path: &str) -> String {
    if path.starts_with("http") { path.to_string() } else { format!("{GRAPH}{path}") }
}

/// One Graph request as the person this thread works for. Times come in UTC.
pub(crate) fn graph(method: reqwest::Method, path: &str, body: Option<&Value>) -> Result<Value, String> {
    graph_with(method, path, body, "outlook.timezone=\"UTC\"")
}

/// A file's bytes (a Graph content path or URL; its download redirect is followed).
pub(crate) fn graph_bytes(path: &str) -> Result<Vec<u8>, String> {
    let token = access(&crate::acting::current())?;
    let resp = http()?.get(url(path)).bearer_auth(token).send().map_err(|e| format!("Microsoft 365 isn't answering: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("couldn't get the file ({})", resp.status()));
    }
    resp.bytes().map(|b| b.to_vec()).map_err(|e| e.to_string())
}

/// The same with its own `Prefer` (mail bodies as text).
pub(crate) fn graph_with(method: reqwest::Method, path: &str, body: Option<&Value>, prefer: &str) -> Result<Value, String> {
    let user = crate::acting::current();
    let token = access(&user)?;
    let mut req = http()?.request(method, url(path)).bearer_auth(token).header("Prefer", prefer);
    if let Some(b) = body {
        req = req.json(b);
    }
    let resp = req.send().map_err(|e| format!("Microsoft 365 isn't answering: {e}"))?;
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    if status.is_success() {
        return Ok(if text.trim().is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::Null) });
    }
    let e: Value = serde_json::from_str(&text).unwrap_or(json!({}));
    Err(match status.as_u16() {
        401 => "Microsoft 365 didn't accept the connection: connect again (More → Outlook → Connect)".into(),
        403 => "Microsoft 365 says lyra may not do that (the connection lacks that permission: connect again, or ask the admin to add it to the app)".into(),
        404 => "Microsoft 365 has no such item (it may have been moved or deleted)".into(),
        code => format!("Microsoft 365 {code}: {}", e["error"]["message"].as_str().unwrap_or("?")),
    })
}

// ---- times (Graph's `dateTime` + `timeZone`, asked for in UTC)

pub(crate) fn utc(v: &Value) -> Option<DateTime<Utc>> {
    let s = v["dateTime"].as_str()?;
    let s = s.split('.').next().unwrap_or(s);
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").ok().map(|n| Utc.from_utc_datetime(&n))
}

pub(crate) fn graph_time(t: DateTime<Utc>) -> Value {
    json!({ "dateTime": t.format("%Y-%m-%dT%H:%M:%S").to_string(), "timeZone": "UTC" })
}

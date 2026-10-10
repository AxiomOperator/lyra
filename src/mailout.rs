//! Email from lyra, only ever to the person it works for: a routine's result
//! ("every morning: the news about X, with links"), the morning briefing, the
//! end-of-day recap, or "email me this" in a chat.
//!
//! Two ways out: lyra's own mailbox, through the email services an admin
//! adds (Settings → Email: as many as they like, each a [`Account`] of a known
//! [`KINDS`] with its own sender, kept in `email/providers.json`, its key in
//! `secrets.toml` as `email-<id>`), tried in order so the next takes over when
//! one fails; or the person's own connected Outlook (sent as them, to
//! themselves). Each person picks (`via`), else lyra's mailbox when there is
//! one, else their Outlook. Another kind of service is one more arm in
//! [`Service::send`] and one more [`KINDS`] entry.
//!
//! Where it goes: the person's own address only (their Microsoft sign-in's,
//! or the one an admin set for an account without Microsoft). Nothing here
//! takes a recipient, so nothing here can email anyone else.

use std::path::PathBuf;
use std::sync::RwLock;
use std::time::Duration;

use chrono::{DateTime, Utc};
use lyra_capabilities::model::{Capability, CapabilityKind, RiskLevel};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// `[email]`: the first setup's one service (before providers were a list).
/// Read once into `email/providers.json`; providers are added in Settings now.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub provider: String,
    pub from: String,
    pub stream: String,
    pub api_url: String,
}

static SETTINGS: RwLock<Option<Settings>> = RwLock::new(None);

pub fn configure(s: Settings) {
    *SETTINGS.write().unwrap_or_else(|e| e.into_inner()) = Some(s);
}

pub fn settings() -> Settings {
    SETTINGS.read().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}

/// A kind of email service lyra can send through.
pub struct Kind {
    pub id: &'static str,
    pub label: &'static str,
    /// What its key is called there.
    pub key: &'static str,
    /// Its API, unless an account says otherwise.
    pub api: &'static str,
    /// Where the key comes from, and what the sender must be.
    pub help: &'static str,
}

pub const KINDS: &[Kind] = &[
    Kind { id: "postmark", label: "Postmark", key: "Server API token", api: "https://api.postmarkapp.com", help: "A server's API Tokens tab. The From address must be a sender signature, or on a verified domain." },
    Kind { id: "resend", label: "Resend", key: "API key", api: "https://api.resend.com", help: "API Keys, with sending access. The From address must be on a verified domain." },
    Kind { id: "sendgrid", label: "SendGrid", key: "API key", api: "https://api.sendgrid.com", help: "Settings → API Keys, with Mail Send. The From address must be a verified sender or on an authenticated domain." },
];

pub fn kind(id: &str) -> Option<&'static Kind> {
    KINDS.iter().find(|k| k.id == id)
}

/// One email service an admin added: which kind, who it sends as. Its key is
/// in secrets.toml, never here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Account {
    pub id: String,
    pub kind: String,
    /// What people see ("Postmark", "Resend (backup)").
    pub name: String,
    /// `lyra <lyra@example.org>`.
    pub from: String,
    /// Postmark's message stream (empty: outbound).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stream: String,
    /// Its API, when not the kind's usual one (a proxy, a test).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub api_url: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub added: DateTime<Utc>,
}

fn yes() -> bool {
    true
}

/// One email to the person: Markdown, sent as HTML with the text alongside.
#[derive(Debug, Clone)]
pub struct Outgoing {
    pub to: String,
    pub subject: String,
    pub markdown: String,
    /// What it is ("routine", "briefing", "recap", "chat"): a tag at the service.
    pub kind: String,
}

/// A way to send.
pub trait Provider {
    /// Its name, as people see it ("Postmark", "your Outlook").
    fn name(&self) -> String;
    /// Send it; the service's id for the message.
    fn send(&self, m: &Outgoing) -> Result<String, String>;
}

/// An added service, with its key.
pub struct Service {
    pub account: Account,
    pub key: String,
}

/// `lyra <lyra@example.org>` as its name and address.
fn split_from(from: &str) -> (String, String) {
    match from.rsplit_once('<') {
        Some((name, rest)) => (name.trim().trim_matches('"').to_string(), rest.trim_end_matches('>').trim().to_string()),
        None => (String::new(), from.trim().to_string()),
    }
}

impl Provider for Service {
    fn name(&self) -> String {
        self.account.name.clone()
    }

    fn send(&self, m: &Outgoing) -> Result<String, String> {
        let a = &self.account;
        let k = kind(&a.kind).ok_or_else(|| format!("lyra doesn't know the email service {:?}", a.kind))?;
        let base = if a.api_url.trim().is_empty() { k.api } else { a.api_url.trim() }.trim_end_matches('/').to_string();
        let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build().map_err(|e| e.to_string())?;
        let html = crate::mail::html(&m.markdown);
        let reach = |e: reqwest::Error| format!("couldn't reach {}: {e}", a.name);
        match a.kind.as_str() {
            "postmark" => {
                let body = json!({
                    "From": a.from, "To": m.to, "Subject": m.subject, "HtmlBody": html, "TextBody": m.markdown,
                    "MessageStream": if a.stream.trim().is_empty() { "outbound" } else { a.stream.trim() }, "Tag": m.kind,
                });
                let resp = client.post(format!("{base}/email")).header("Accept", "application/json").header("X-Postmark-Server-Token", &self.key).json(&body).send().map_err(reach)?;
                let status = resp.status();
                let v: Value = resp.json().unwrap_or_default();
                // Postmark says what went wrong in Message (ErrorCode 0 is success).
                if !status.is_success() || v["ErrorCode"].as_i64().unwrap_or(0) != 0 {
                    return Err(format!("{} refused it ({status}): {}", a.name, v["Message"].as_str().unwrap_or("no reason given")));
                }
                Ok(v["MessageID"].as_str().unwrap_or("").to_string())
            }
            "resend" => {
                let body = json!({ "from": a.from, "to": [m.to], "subject": m.subject, "html": html, "text": m.markdown, "tags": [{ "name": "kind", "value": m.kind }] });
                let resp = client.post(format!("{base}/emails")).bearer_auth(&self.key).json(&body).send().map_err(reach)?;
                let status = resp.status();
                let v: Value = resp.json().unwrap_or_default();
                if !status.is_success() {
                    return Err(format!("{} refused it ({status}): {}", a.name, v["message"].as_str().unwrap_or("no reason given")));
                }
                Ok(v["id"].as_str().unwrap_or("").to_string())
            }
            "sendgrid" => {
                let (name, address) = split_from(&a.from);
                let mut from = json!({ "email": address });
                if !name.is_empty() {
                    from["name"] = json!(name);
                }
                let body = json!({
                    "personalizations": [{ "to": [{ "email": m.to }] }], "from": from, "subject": m.subject,
                    "content": [{ "type": "text/plain", "value": m.markdown }, { "type": "text/html", "value": html }], "categories": [m.kind],
                });
                let resp = client.post(format!("{base}/v3/mail/send")).bearer_auth(&self.key).json(&body).send().map_err(reach)?;
                let status = resp.status();
                let id = resp.headers().get("x-message-id").and_then(|h| h.to_str().ok()).unwrap_or("").to_string();
                if !status.is_success() {
                    let v: Value = resp.json().unwrap_or_default();
                    let why = v["errors"].as_array().map(|e| e.iter().filter_map(|x| x["message"].as_str()).collect::<Vec<_>>().join("; ")).filter(|w| !w.is_empty());
                    return Err(format!("{} refused it ({status}): {}", a.name, why.as_deref().unwrap_or("no reason given")));
                }
                Ok(id)
            }
            other => Err(format!("lyra doesn't know the email service {other:?}")),
        }
    }
}

/// The person's own Outlook: sent as them, to themselves (it's in their Sent Items).
pub struct Outlook {
    pub user: String,
}

impl Provider for Outlook {
    fn name(&self) -> String {
        "your Outlook".into()
    }

    fn send(&self, m: &Outgoing) -> Result<String, String> {
        let msg = json!({
            "message": {
                "subject": m.subject,
                "body": { "contentType": "HTML", "content": crate::mail::html(&m.markdown) },
                "toRecipients": [{ "emailAddress": { "address": m.to } }],
            },
            "saveToSentItems": true,
        });
        crate::acting::run(&self.user, || crate::graph::graph(reqwest::Method::POST, "/me/sendMail", Some(&msg)))?;
        Ok(String::new())
    }
}

// ---- the services an admin added

fn accounts_path() -> Option<PathBuf> {
    Some(crate::config::home()?.join("email").join("providers.json"))
}

fn secret(id: &str) -> String {
    format!("email-{id}")
}

/// Every service added, in the order they're tried. A first setup's `[email]`
/// (one Postmark) becomes the first, once.
pub fn accounts() -> Vec<Account> {
    let Some(path) = accounts_path() else { return Vec::new() };
    if path.exists() {
        return crate::store::read_json::<Option<Vec<Account>>>(&path).unwrap_or_default();
    }
    let s = settings();
    if s.provider.trim().is_empty() || cfg!(test) {
        return Vec::new();
    }
    let kind = s.provider.trim().to_lowercase();
    let a = Account { id: kind.clone(), kind: kind.clone(), name: crate::mailout::kind(&kind).map_or(kind.clone(), |k| k.label.to_string()), from: s.from, stream: s.stream, api_url: s.api_url, enabled: true, added: Utc::now() };
    if let Some(key) = crate::secrets::token(&kind) {
        let _ = crate::secrets::set_token(&secret(&a.id), &key);
    }
    let _ = crate::store::write_json(&path, &vec![a.clone()]);
    vec![a]
}

fn save_accounts(f: impl FnOnce(&mut Vec<Account>) -> Result<(), String>) -> Result<(), String> {
    let mut all = accounts();
    f(&mut all)?;
    crate::store::write_json(&accounts_path().ok_or("no lyra home")?, &all)
}

/// One added service, ready to send (its key read now).
fn service(a: &Account) -> Result<Service, String> {
    let key = crate::secrets::token(&secret(&a.id)).ok_or_else(|| format!("{}'s key isn't set (Settings → Email)", a.name))?;
    Ok(Service { account: a.clone(), key })
}

/// lyra's own mailbox: the services that can send, in order.
fn lyra_mailbox() -> Result<Vec<Box<dyn Provider>>, String> {
    let all: Vec<Account> = accounts().into_iter().filter(|a| a.enabled).collect();
    if all.is_empty() {
        return Err("lyra's mailbox isn't set up (Settings → Email)".into());
    }
    let (ok, bad): (Vec<_>, Vec<_>) = all.iter().map(service).partition(Result::is_ok);
    if ok.is_empty() {
        return Err(bad.into_iter().filter_map(Result::err).collect::<Vec<_>>().join("; "));
    }
    Ok(ok.into_iter().filter_map(Result::ok).map(|s| Box::new(s) as Box<dyn Provider>).collect())
}

/// How this person's email would go out now (the first, then the ones that
/// take over), or why it can't.
pub fn providers_for(user: &str) -> Result<Vec<Box<dyn Provider>>, String> {
    let outlook = || -> Result<Vec<Box<dyn Provider>>, String> {
        if crate::graph::connected_for(user) { Ok(vec![Box::new(Outlook { user: user.to_string() })]) } else { Err("your Outlook isn't connected (Profile → Connections → Outlook)".into()) }
    };
    match prefs(user).via.as_str() {
        "outlook" => outlook(),
        "lyra" => lyra_mailbox(),
        _ => lyra_mailbox().or_else(|lyra| outlook().map_err(|o| format!("{lyra}; and {o}"))),
    }
}

/// How a person's email goes out: "lyra" (lyra's mailbox), "outlook", or "" (lyra's when set up, else Outlook).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Prefs {
    pub via: String,
    /// The morning briefing, by email too.
    pub briefing: bool,
    /// The end-of-day recap, by email too.
    pub recap: bool,
    /// The last few sent (or not): newest first.
    pub recent: Vec<Sent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Sent {
    pub at: DateTime<Utc>,
    pub subject: String,
    pub kind: String,
    /// "Postmark", "your Outlook".
    pub via: String,
    /// Why it didn't go (empty: it went).
    #[serde(default)]
    pub error: String,
}

fn prefs_path(user: &str) -> Option<PathBuf> {
    if user == lyra_web::users::OWNER { Some(crate::config::home()?.join("email").join("prefs.json")) } else { Some(crate::context::user_dir(user)?.join("email").join("prefs.json")) }
}

pub fn prefs(user: &str) -> Prefs {
    prefs_path(user).and_then(|p| crate::store::read_json::<Option<Prefs>>(&p)).unwrap_or_default()
}

fn save_prefs(user: &str, f: impl FnOnce(&mut Prefs)) -> Result<Prefs, String> {
    let path = prefs_path(user).ok_or("no lyra home")?;
    crate::store::JsonStore::<Prefs>::new(path).update(f)?;
    Ok(prefs(user))
}

/// Their own address: the one on their account (Microsoft's, or an admin's).
pub fn address(user: &str) -> Option<String> {
    let dir = crate::config::home()?.join("web");
    lyra_web::Users::open(&dir).get(user).map(|u| u.email).filter(|e| e.contains('@'))
}

/// Send with each in turn until one takes it: (who sent it, or every reason).
fn send_with(providers: &[Box<dyn Provider>], m: &Outgoing) -> (String, Result<(), String>) {
    let mut why = Vec::new();
    for p in providers {
        match p.send(m) {
            Ok(_) => return (p.name(), Ok(())),
            Err(e) => why.push(e),
        }
    }
    (providers.first().map(|p| p.name()).unwrap_or_default(), Err(why.join("; ")))
}

/// Email the person themselves (through `only` when given: a test of one
/// service). Every try is kept in their recent list.
fn send(user: &str, subject: &str, markdown: &str, kind: &str, only: Option<&Account>) -> Result<String, String> {
    let to = address(user).ok_or("there's no email address on your account: an admin can add one (Users)")?;
    let providers: Vec<Box<dyn Provider>> = match only {
        Some(a) => vec![Box::new(service(a)?)],
        None => providers_for(user)?,
    };
    let subject: String = subject.trim().chars().take(200).collect();
    let m = Outgoing { to: to.clone(), subject: if subject.is_empty() { "From lyra".into() } else { subject }, markdown: markdown.to_string(), kind: kind.to_string() };
    let (via, result) = send_with(&providers, &m);
    let _ = save_prefs(user, |p| {
        p.recent.insert(0, Sent { at: Utc::now(), subject: m.subject.clone(), kind: kind.to_string(), via: via.clone(), error: result.as_ref().err().cloned().unwrap_or_default() });
        p.recent.truncate(20);
    });
    result.map(|_| format!("sent to {to} with {via}"))
}

/// Email the person themselves, the way they send.
pub fn send_to_me(user: &str, subject: &str, markdown: &str, kind: &str) -> Result<String, String> {
    send(user, subject, markdown, kind, None)
}

/// For the About me page: their address, how it would go, the choices, what was sent.
pub fn view(user: &str) -> Value {
    let p = prefs(user);
    let how = providers_for(user);
    json!({
        "address": address(user),
        "via": p.via,
        "sends_with": how.as_ref().ok().map(|h| h.iter().map(|x| x.name()).collect::<Vec<_>>().join(", then ")),
        "problem": how.err(),
        "lyra_mailbox": lyra_mailbox().is_ok(),
        "outlook": crate::graph::connected_for(user),
        "briefing": p.briefing,
        "recap": p.recap,
        "recent": p.recent,
    })
}

// ---- adding, changing and removing services (admins: Settings → Email)

/// For the Settings page: the services, in order, and the kinds to add.
pub fn providers_view() -> Value {
    json!({
        "providers": accounts().iter().map(|a| json!({
            "id": a.id, "kind": a.kind, "kind_label": kind(&a.kind).map_or(a.kind.as_str(), |k| k.label), "name": a.name, "from": a.from,
            "stream": a.stream, "api_url": a.api_url, "enabled": a.enabled, "key_set": crate::secrets::token(&secret(&a.id)).is_some(),
        })).collect::<Vec<_>>(),
        "kinds": KINDS.iter().map(|k| json!({ "id": k.id, "label": k.label, "key": k.key, "help": k.help, "api": k.api })).collect::<Vec<_>>(),
    })
}

/// Add a service (no `id`) or change one: kind, name, from, stream, api_url,
/// enabled, and its key when one is given (a new one needs it).
pub fn put(arg: &Value) -> Result<Value, String> {
    let s = |k: &str| arg[k].as_str().unwrap_or("").trim().to_string();
    let id = s("id");
    let key = s("key");
    let mut all = accounts();
    let at = all.iter().position(|a| a.id == id);
    if !id.is_empty() && at.is_none() {
        return Err(format!("no email service {id:?}"));
    }
    let mut a = match at {
        Some(i) => all[i].clone(),
        None => Account { id: String::new(), kind: s("kind"), name: String::new(), from: String::new(), stream: String::new(), api_url: String::new(), enabled: true, added: Utc::now() },
    };
    if at.is_none() || !s("kind").is_empty() {
        a.kind = s("kind").to_lowercase();
    }
    let k = kind(&a.kind).ok_or_else(|| format!("lyra knows these email services: {}", KINDS.iter().map(|k| k.label).collect::<Vec<_>>().join(", ")))?;
    for (field, slot) in [("name", &mut a.name), ("from", &mut a.from), ("stream", &mut a.stream), ("api_url", &mut a.api_url)] {
        if arg.get(field).is_some() {
            *slot = s(field);
        }
    }
    if let Some(on) = arg["enabled"].as_bool() {
        a.enabled = on;
    }
    if a.name.is_empty() {
        a.name = k.label.to_string();
    }
    let (_, address) = split_from(&a.from);
    if !(address.contains('@') && address.split('@').nth(1).is_some_and(|d| d.contains('.'))) {
        return Err("who it's from needs an email address, e.g. lyra <lyra@example.org>".into());
    }
    if !a.api_url.is_empty() && !a.api_url.starts_with("http") {
        return Err("the API address starts with https://".into());
    }
    if at.is_none() {
        if key.is_empty() {
            return Err(format!("{} needs its {}", k.label, k.key));
        }
        let base: String = a.name.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect::<String>().split('-').filter(|w| !w.is_empty()).collect::<Vec<_>>().join("-");
        let base = if base.is_empty() { a.kind.clone() } else { base };
        let mut id = base.clone();
        let mut n = 2;
        while all.iter().any(|x| x.id == id) {
            id = format!("{base}-{n}");
            n += 1;
        }
        a.id = id;
    }
    if !key.is_empty() {
        crate::secrets::set_token(&secret(&a.id), &key)?;
    }
    match at {
        Some(i) => all[i] = a.clone(),
        None => all.push(a.clone()),
    }
    save_accounts(|v| {
        *v = all;
        Ok(())
    })?;
    Ok(json!({ "ok": true, "id": a.id }))
}

/// Remove a service, and its key.
pub fn remove(id: &str) -> Result<Value, String> {
    save_accounts(|all| {
        let before = all.len();
        all.retain(|a| a.id != id);
        if all.len() == before { Err(format!("no email service {id:?}")) } else { Ok(()) }
    })?;
    let _ = crate::secrets::set_token(&secret(id), "");
    Ok(json!({ "ok": true }))
}

/// Try a service sooner (`up`) or later.
pub fn move_one(id: &str, up: bool) -> Result<Value, String> {
    save_accounts(|all| {
        let i = all.iter().position(|a| a.id == id).ok_or_else(|| format!("no email service {id:?}"))?;
        let j = if up { i.checked_sub(1) } else { Some(i + 1).filter(|j| *j < all.len()) };
        if let Some(j) = j {
            all.swap(i, j);
        }
        Ok(())
    })?;
    Ok(json!({ "ok": true }))
}

/// A test email through one service, to the admin trying it.
pub fn test_one(user: &str, id: &str) -> Result<String, String> {
    let a = accounts().into_iter().find(|a| a.id == id).ok_or_else(|| format!("no email service {id:?}"))?;
    send(user, &format!("A test from lyra, through {}", a.name), &format!("This is a test email from **lyra**, sent through **{}**. If you can read it, it works.", a.name), "test", Some(&a))
}

/// A change from the About me page: `via`, `briefing`, `recap`.
pub fn set(user: &str, arg: &Value) -> Result<Value, String> {
    if let Some(v) = arg["via"].as_str()
        && !matches!(v, "" | "lyra" | "outlook")
    {
        return Err("send with lyra, outlook, or leave it to lyra (\"\")".into());
    }
    save_prefs(user, |p| {
        if let Some(v) = arg["via"].as_str() {
            p.via = v.to_string();
        }
        if let Some(b) = arg["briefing"].as_bool() {
            p.briefing = b;
        }
        if let Some(r) = arg["recap"].as_bool() {
            p.recap = r;
        }
    })?;
    Ok(view(user))
}

/// `/email`: how it's set up for you; `/email test`, `/email via lyra|outlook|auto`, `/email briefing|recap on|off`.
pub fn command(user: &str, arg: &str) -> Result<String, String> {
    let mut words = arg.split_whitespace();
    let on = |w: Option<&str>| match w {
        Some("on") => Ok(true),
        Some("off") => Ok(false),
        _ => Err("on or off".to_string()),
    };
    match words.next() {
        None => {
            let v = view(user);
            let mut out = vec![format!("Your address: {}", v["address"].as_str().unwrap_or("none (an admin adds it: /users email <you> <address>)"))];
            out.push(match v["sends_with"].as_str() {
                Some(w) => format!("Sent with: {w}"),
                None => format!("Can't send yet: {}", v["problem"].as_str().unwrap_or("?")),
            });
            out.push(format!("Briefing by email: {} · recap by email: {}", if v["briefing"] == true { "on" } else { "off" }, if v["recap"] == true { "on" } else { "off" }));
            for s in prefs(user).recent.iter().take(5) {
                out.push(format!("  {} {} · {}{}", s.at.with_timezone(&chrono::Local).format("%m-%d %H:%M"), s.subject, s.via, if s.error.is_empty() { String::new() } else { format!(" · not sent: {}", s.error) }));
            }
            out.push("/email test · /email via lyra|outlook|auto · /email briefing|recap on|off · routines: /routine edit <name> email on".into());
            Ok(out.join("\n"))
        }
        Some("test") => send_to_me(user, "A test from lyra", "This is a test email from **lyra**. If you can read it, lyra can email you: routine results, your briefing and your recap.", "test"),
        Some("via") => {
            let via = match words.next() {
                Some("lyra") => "lyra",
                Some("outlook") => "outlook",
                Some("auto") => "",
                _ => return Err("/email via lyra|outlook|auto".into()),
            };
            set(user, &json!({ "via": via }))?;
            Ok(match providers_for(user) {
                Ok(p) => format!("Your email goes with {}.", p.iter().map(|x| x.name()).collect::<Vec<_>>().join(", then ")),
                Err(e) => format!("Saved, but it can't send yet: {e}"),
            })
        }
        Some(k @ ("briefing" | "recap")) => {
            let b = on(words.next()).map_err(|e| format!("/email {k} {e}"))?;
            set(user, &json!({ k: b }))?;
            Ok(format!("Your {k} {} by email.", if b { "comes" } else { "doesn't come" }))
        }
        Some(other) => Err(format!("/email {other}? (/email, /email test, /email via …, /email briefing|recap on|off)")),
    }
}

pub fn capabilities() -> Vec<Capability> {
    let mut c = Capability::new(
        "email_me",
        CapabilityKind::NativeTool,
        "Email the user (only them, at their own address): a summary, a list, a write-up with links, in Markdown. For \"email me this\", and for routines that end with an email. It can't send to anyone else; for that use mail_draft.",
        RiskLevel::LowWrite,
    );
    c.input_schema = json!({ "type": "object", "properties": {
        "subject": { "type": "string" },
        "body": { "type": "string", "description": "The email in Markdown: headings, lists, **bold**, [links](https://…)." },
    }, "required": ["subject", "body"] });
    c.source = "mailout".into();
    c.tags = ["email", "mail", "send", "me", "summary", "digest", "newsletter", "report", "inbox"].iter().map(|t| t.to_string()).collect();
    vec![c]
}

/// `email_me`, for whoever this turn works for.
pub fn call(name: &str, args: &Value) -> Result<Value, String> {
    if name != "email_me" {
        return Err(format!("{name} isn't an email tool"));
    }
    let user = crate::acting::current();
    let sent = send_to_me(&user, args["subject"].as_str().unwrap_or(""), args["body"].as_str().unwrap_or(""), "chat")?;
    Ok(json!({ "sent": sent }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// A one-request fake Postmark: answers `reply` with `status`, hands back what it got.
    fn fake(status: u16, reply: Value) -> (String, std::thread::JoinHandle<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let t = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut buf = vec![0u8; 65536];
            let mut got = Vec::new();
            loop {
                let n = s.read(&mut buf).unwrap();
                got.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&got).to_string();
                if let Some(at) = text.find("\r\n\r\n") {
                    let len = text.lines().find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0))).unwrap_or(0);
                    if got.len() >= at + 4 + len {
                        break;
                    }
                }
            }
            let body = reply.to_string();
            let _ = write!(s, "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}", body.len());
            String::from_utf8_lossy(&got).to_string()
        });
        (base, t)
    }

    fn svc(kind: &str, base: &str) -> Service {
        Service { account: Account { id: kind.into(), kind: kind.into(), name: format!("{kind} main"), from: "lyra <lyra@example.org>".into(), stream: String::new(), api_url: base.into(), enabled: true, added: Utc::now() }, key: "the-key".into() }
    }

    fn mail() -> Outgoing {
        Outgoing { to: "dana@example.org".into(), subject: "AI news".into(), markdown: "- [One](https://a.example)".into(), kind: "routine".into() }
    }

    fn body_of(req: &str) -> Value {
        serde_json::from_str(&req[req.find("\r\n\r\n").unwrap() + 4..]).unwrap()
    }

    #[test]
    fn postmark_gets_the_email_and_its_token() {
        let (base, t) = fake(200, json!({ "ErrorCode": 0, "Message": "OK", "MessageID": "m-1" }));
        assert_eq!(svc("postmark", &base).send(&mail()).unwrap(), "m-1");
        let req = t.join().unwrap();
        assert!(req.starts_with("POST /email "));
        assert!(req.to_lowercase().contains("x-postmark-server-token: the-key"));
        let body = body_of(&req);
        assert_eq!((body["To"].as_str(), body["From"].as_str(), body["MessageStream"].as_str(), body["Tag"].as_str()), (Some("dana@example.org"), Some("lyra <lyra@example.org>"), Some("outbound"), Some("routine")));
        assert!(body["HtmlBody"].as_str().unwrap().contains("<a href=\"https://a.example\">One</a>"));
    }

    #[test]
    fn resend_and_sendgrid_get_theirs() {
        let (base, t) = fake(200, json!({ "id": "r-1" }));
        assert_eq!(svc("resend", &base).send(&mail()).unwrap(), "r-1");
        let req = t.join().unwrap();
        assert!(req.starts_with("POST /emails ") && req.to_lowercase().contains("authorization: bearer the-key"));
        assert_eq!(body_of(&req)["to"], json!(["dana@example.org"]));
        let (base, t) = fake(202, json!({}));
        svc("sendgrid", &base).send(&mail()).unwrap();
        let req = t.join().unwrap();
        assert!(req.starts_with("POST /v3/mail/send "));
        let body = body_of(&req);
        assert_eq!(body["personalizations"][0]["to"][0]["email"], "dana@example.org");
        assert_eq!(body["from"], json!({ "email": "lyra@example.org", "name": "lyra" }));
        assert_eq!(body["content"][1]["type"], "text/html");
    }

    #[test]
    fn the_next_service_takes_over_when_one_fails() {
        let (first, t1) = fake(422, json!({ "ErrorCode": 400, "Message": "The 'From' address you supplied is not a Sender Signature." }));
        let (second, t2) = fake(200, json!({ "id": "r-2" }));
        let providers: Vec<Box<dyn Provider>> = vec![Box::new(svc("postmark", &first)), Box::new(svc("resend", &second))];
        let (via, result) = send_with(&providers, &mail());
        assert_eq!((via.as_str(), result), ("resend main", Ok(())));
        t1.join().unwrap();
        t2.join().unwrap();
        // Both refusing: every reason.
        let (a, t1) = fake(422, json!({ "ErrorCode": 400, "Message": "not a Sender Signature" }));
        let (b, t2) = fake(403, json!({ "message": "domain not verified" }));
        let providers: Vec<Box<dyn Provider>> = vec![Box::new(svc("postmark", &a)), Box::new(svc("resend", &b))];
        let e = send_with(&providers, &mail()).1.unwrap_err();
        t1.join().unwrap();
        t2.join().unwrap();
        assert!(e.contains("Sender Signature") && e.contains("domain not verified"), "{e}");
        assert_eq!(split_from("\"lyra\" <lyra@example.org>"), ("lyra".into(), "lyra@example.org".into()));
    }

    #[test]
    fn email_choices_are_each_persons_and_nothing_goes_without_an_address() {
        crate::config::with_test_home(|home| {
            set("owner", &json!({ "briefing": true, "via": "lyra" })).unwrap();
            set("dana", &json!({ "recap": true })).unwrap();
            assert!(prefs("owner").briefing && !prefs("owner").recap);
            assert!(prefs("dana").recap && !prefs("dana").briefing, "Dana's are her own");
            assert!(home.join("email/prefs.json").exists() && home.join("users/dana/email/prefs.json").exists());
            assert!(set("owner", &json!({ "via": "someone@else" })).is_err(), "only lyra, outlook or lyra decides");
            let e = send_to_me("dana", "s", "b", "test").unwrap_err();
            assert!(e.contains("no email address"), "{e}");
        });
    }

    #[test]
    fn the_tool_takes_no_recipient() {
        let c = &capabilities()[0];
        assert!(c.input_schema["properties"].get("to").is_none(), "only ever the person themselves");
    }
}

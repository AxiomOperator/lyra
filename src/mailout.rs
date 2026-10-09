//! Email from lyra (`[email]`), only ever to the person it works for: a
//! routine's result ("every morning: the news about X, with links"), the
//! morning briefing, the end-of-day recap, or "email me this" in a chat.
//!
//! Two ways out: lyra's own mailbox through an email service with an API key
//! (`provider`, Postmark first; its key in `secrets.toml` under the
//! provider's name), or the person's own connected Outlook (sent as them, to
//! themselves). Each person picks (`via`), else lyra's mailbox when it's set
//! up, else their Outlook. Another service is one more [`Provider`].
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

/// `[email]`.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// The email service lyra's own mailbox sends through: "postmark" (empty: none).
    pub provider: String,
    /// Who it's from, e.g. `lyra <lyra@example.org>` (an address the service lets you send from).
    pub from: String,
    /// Postmark's message stream (default "outbound").
    pub stream: String,
    /// The service's API address, when not its usual one (a proxy, a test).
    pub api_url: String,
}

static SETTINGS: RwLock<Option<Settings>> = RwLock::new(None);

pub fn configure(s: Settings) {
    *SETTINGS.write().unwrap_or_else(|e| e.into_inner()) = Some(s);
}

pub fn settings() -> Settings {
    SETTINGS.read().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}

/// The services lyra's mailbox can send through, and what each needs.
pub const PROVIDERS: &[(&str, &str)] = &[("postmark", "Postmark (a Server API token)")];

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

/// Postmark's email API (`POST /email`, the server token in a header).
pub struct Postmark {
    pub token: String,
    pub from: String,
    pub stream: String,
    /// `https://api.postmarkapp.com` (a test points it elsewhere).
    pub base: String,
}

impl Provider for Postmark {
    fn name(&self) -> String {
        "Postmark".into()
    }

    fn send(&self, m: &Outgoing) -> Result<String, String> {
        let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(30)).build().map_err(|e| e.to_string())?;
        let body = json!({
            "From": self.from,
            "To": m.to,
            "Subject": m.subject,
            "HtmlBody": crate::mail::html(&m.markdown),
            "TextBody": m.markdown,
            "MessageStream": if self.stream.trim().is_empty() { "outbound" } else { self.stream.trim() },
            "Tag": m.kind,
        });
        let resp = client
            .post(format!("{}/email", self.base.trim_end_matches('/')))
            .header("Accept", "application/json")
            .header("X-Postmark-Server-Token", &self.token)
            .json(&body)
            .send()
            .map_err(|e| format!("couldn't reach Postmark: {e}"))?;
        let status = resp.status();
        let v: Value = resp.json().unwrap_or_default();
        // Postmark says what went wrong in Message (ErrorCode 0 is success).
        if !status.is_success() || v["ErrorCode"].as_i64().unwrap_or(0) != 0 {
            return Err(format!("Postmark refused it ({status}): {}", v["Message"].as_str().unwrap_or("no reason given")));
        }
        Ok(v["MessageID"].as_str().unwrap_or("").to_string())
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

/// lyra's own mailbox, when an admin set it up (the service, who it's from, its key).
fn lyra_mailbox() -> Result<Box<dyn Provider>, String> {
    let s = settings();
    match s.provider.trim().to_lowercase().as_str() {
        "" => Err("lyra's mailbox isn't set up (Settings → Email)".into()),
        "postmark" => {
            let token = crate::secrets::token("postmark").ok_or("Postmark's key isn't set (Settings → Email)")?;
            if !s.from.contains('@') {
                return Err("[email] from isn't set: who lyra's emails come from".into());
            }
            Ok(Box::new(Postmark { token, from: s.from.clone(), stream: s.stream.clone(), base: if s.api_url.trim().is_empty() { "https://api.postmarkapp.com".into() } else { s.api_url.trim().to_string() } }))
        }
        other => Err(format!("lyra doesn't know the email service {other:?} (it knows {})", PROVIDERS.iter().map(|p| p.0).collect::<Vec<_>>().join(", "))),
    }
}

/// How this person's email would go out now, or why it can't.
pub fn provider_for(user: &str) -> Result<Box<dyn Provider>, String> {
    let outlook = || -> Result<Box<dyn Provider>, String> {
        if crate::graph::connected_for(user) { Ok(Box::new(Outlook { user: user.to_string() })) } else { Err("your Outlook isn't connected (More → Outlook)".into()) }
    };
    match prefs(user).via.as_str() {
        "outlook" => outlook(),
        "lyra" => lyra_mailbox(),
        _ => lyra_mailbox().or_else(|lyra| outlook().map_err(|o| format!("{lyra}; and {o}"))),
    }
}

/// Email the person themselves. Every try is kept in their recent list.
pub fn send_to_me(user: &str, subject: &str, markdown: &str, kind: &str) -> Result<String, String> {
    let to = address(user).ok_or("there's no email address on your account: an admin can add one (Users)")?;
    let provider = provider_for(user)?;
    let subject: String = subject.trim().chars().take(200).collect();
    let m = Outgoing { to: to.clone(), subject: if subject.is_empty() { "From lyra".into() } else { subject }, markdown: markdown.to_string(), kind: kind.to_string() };
    let result = provider.send(&m);
    let via = provider.name();
    let _ = save_prefs(user, |p| {
        p.recent.insert(0, Sent { at: Utc::now(), subject: m.subject.clone(), kind: kind.to_string(), via: via.clone(), error: result.as_ref().err().cloned().unwrap_or_default() });
        p.recent.truncate(20);
    });
    result.map(|_| format!("sent to {to} with {via}"))
}

/// For the About me page: their address, how it would go, the choices, what was sent.
pub fn view(user: &str) -> Value {
    let p = prefs(user);
    let how = provider_for(user);
    json!({
        "address": address(user),
        "via": p.via,
        "sends_with": how.as_ref().ok().map(|h| h.name()),
        "problem": how.err(),
        "lyra_mailbox": lyra_mailbox().is_ok(),
        "outlook": crate::graph::connected_for(user),
        "briefing": p.briefing,
        "recap": p.recap,
        "recent": p.recent,
    })
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
            Ok(match provider_for(user) {
                Ok(p) => format!("Your email goes with {}.", p.name()),
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

    #[test]
    fn postmark_gets_the_email_and_its_token() {
        let (base, t) = fake(200, json!({ "ErrorCode": 0, "Message": "OK", "MessageID": "m-1" }));
        let p = Postmark { token: "server-token".into(), from: "lyra <lyra@example.org>".into(), stream: String::new(), base };
        let m = Outgoing { to: "dana@example.org".into(), subject: "AI news".into(), markdown: "- [One](https://a.example)".into(), kind: "routine".into() };
        assert_eq!(p.send(&m).unwrap(), "m-1");
        let req = t.join().unwrap();
        assert!(req.starts_with("POST /email "));
        assert!(req.to_lowercase().contains("x-postmark-server-token: server-token"));
        let body: Value = serde_json::from_str(&req[req.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        assert_eq!((body["To"].as_str(), body["From"].as_str(), body["MessageStream"].as_str(), body["Tag"].as_str()), (Some("dana@example.org"), Some("lyra <lyra@example.org>"), Some("outbound"), Some("routine")));
        assert!(body["HtmlBody"].as_str().unwrap().contains("<a href=\"https://a.example\">One</a>"));
    }

    #[test]
    fn postmark_says_why_it_refused() {
        let (base, t) = fake(422, json!({ "ErrorCode": 400, "Message": "The 'From' address you supplied is not a Sender Signature." }));
        let p = Postmark { token: "t".into(), from: "x@example.org".into(), stream: "outbound".into(), base };
        let e = p.send(&Outgoing { to: "a@b.org".into(), subject: "s".into(), markdown: "b".into(), kind: "test".into() }).unwrap_err();
        t.join().unwrap();
        assert!(e.contains("Sender Signature"), "{e}");
    }

    #[test]
    fn the_tool_takes_no_recipient() {
        let c = &capabilities()[0];
        assert!(c.input_schema["properties"].get("to").is_none(), "only ever the person themselves");
    }
}

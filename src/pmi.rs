//! PMI, the user's project-management app (`[pmi]`, token in
//! `secrets.toml`): their tasks and reminders, projects and reporting, as
//! lyra tools. Changes to the user's personal space run at once; anything
//! others can see (a project's or team's tasks, comments, status updates,
//! risks) waits for the user's yes, so those go through the Project Manager
//! agent. Reminders are PMI's own (it delivers them); lyra reads and sets them.

use std::sync::Mutex;
use std::time::Duration;

use chrono::{DateTime, Local, NaiveDate, Utc};
use lyra_capabilities::{Capability, CapabilityKind, RiskLevel};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::RwLock;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::caps::Ask;

/// `[pmi]`.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    /// The API's address (ends in /api).
    pub url: String,
    /// The organization to use (its slug); empty: the first one.
    pub org: String,
    /// Push again when a reminder that went off is still open.
    pub nag: bool,
    pub nag_minutes: u64,
    pub nag_max: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self { enabled: true, url: "https://pmi.fbcad.org/api".into(), org: String::new(), nag: true, nag_minutes: 30, nag_max: 3 }
    }
}

static SETTINGS: RwLock<Option<Settings>> = RwLock::new(None);

pub fn configure(s: Settings) {
    *SETTINGS.write().unwrap_or_else(|e| e.into_inner()) = Some(s);
    *WHO.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

pub fn settings() -> Settings {
    SETTINGS.read().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}

/// PMI is on and has a token.
pub fn configured() -> bool {
    settings().enabled && crate::secrets::token("pmi").is_some()
}

// ---- the client

fn http() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder().connect_timeout(Duration::from_secs(5)).timeout(Duration::from_secs(30)).build().map_err(|e| e.to_string())
}

/// One request to the API; 204 is null. Errors read PMI's problem answer.
pub fn request(method: reqwest::Method, path: &str, body: Option<&Value>) -> Result<Value, String> {
    let s = settings();
    if !s.enabled {
        return Err("PMI is off ([pmi] enabled)".into());
    }
    #[cfg(test)]
    let test = TEST.lock().unwrap_or_else(|e| e.into_inner()).clone();
    #[cfg(not(test))]
    let test: Option<(String, String)> = None;
    let (base, token) = match test {
        Some(t) => t,
        None => (s.url.clone(), crate::secrets::token("pmi").ok_or("no PMI token yet: /pmi token <token> (Your account → Security in PMI)")?),
    };
    let url = format!("{}{path}", base.trim_end_matches('/'));
    let mut req = http()?.request(method, &url).bearer_auth(token).header("Accept", "application/json");
    if let Some(b) = body {
        req = req.json(b);
    }
    let resp = req.send().map_err(|e| format!("PMI isn't answering: {e}"))?;
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    if status.is_success() {
        return Ok(if text.trim().is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::String(text)) });
    }
    let problem: Value = serde_json::from_str(&text).unwrap_or(json!({}));
    let why = [problem["title"].as_str(), problem["detail"].as_str()].into_iter().flatten().collect::<Vec<_>>().join(": ");
    Err(match status.as_u16() {
        401 => "PMI rejected the token (/pmi token <new token>)".to_string(),
        403 if why.is_empty() => "PMI says you may not do that".to_string(),
        code => format!("PMI {code}: {}", if why.is_empty() { text.chars().take(200).collect() } else { why }),
    })
}

fn get(path: &str) -> Result<Value, String> {
    request(reqwest::Method::GET, path, None)
}

/// Who the token is and the organization lyra uses.
#[derive(Debug, Clone)]
pub struct Who {
    pub user: String,
    pub org_id: String,
    pub org: String,
}

static WHO: Mutex<Option<Who>> = Mutex::new(None);

/// Tests: a fake PMI's address and token instead of the settings and secrets.
#[cfg(test)]
static TEST: Mutex<Option<(String, String)>> = Mutex::new(None);

pub fn who() -> Result<Who, String> {
    if let Some(w) = WHO.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        return Ok(w);
    }
    let me = get("/v1/auth/me")?;
    let slug = settings().org;
    let orgs = me["organizations"].as_array().cloned().unwrap_or_default();
    let org = orgs
        .iter()
        .find(|o| slug.is_empty() || o["slug"].as_str() == Some(slug.as_str()))
        .ok_or_else(|| if orgs.is_empty() { "the PMI account isn't in an organization".to_string() } else { format!("no organization {slug:?} ([pmi] org)") })?;
    let w = Who {
        user: str_of(&me["user"]["name"]),
        org_id: str_of(&org["id"]),
        org: str_of(&org["name"]),
    };
    *WHO.lock().unwrap_or_else(|e| e.into_inner()) = Some(w.clone());
    Ok(w)
}

/// A path inside the organization.
fn org(path: &str) -> Result<String, String> {
    Ok(format!("/v1/orgs/{}{path}", who()?.org_id))
}

fn str_of(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

// ---- compact views (what the model sees)

/// A task in a few fields.
pub fn brief(t: &Value) -> Value {
    let container = &t["container"];
    let place = if container["type"] == "user" { "personal".to_string() } else { format!("{} {}", str_of(&container["type"]), str_of(&container["name"])) };
    let mut v = json!({
        "id": t["id"],
        "title": t["title"],
        "status": t["status"],
        "where": place,
        "due": t["dueDate"],
        "priority": t["priority"],
    });
    let names: Vec<String> = t["assignees"].as_array().into_iter().flatten().map(|a| str_of(&a["name"])).collect();
    if !names.is_empty() {
        v["assignees"] = json!(names);
    }
    if t["blocked"] == true {
        v["blocked"] = json!(true);
    }
    if let Some(a) = t["approvalStatus"].as_str() {
        v["approval"] = json!(a);
    }
    if let (Some(d), Some(n)) = (t["checklist"]["done"].as_u64(), t["checklist"]["total"].as_u64()) {
        v["checklist"] = json!(format!("{d}/{n}"));
    }
    if t["repeat"].is_string() {
        v["repeat"] = t["repeat"].clone();
    }
    if t["parentId"].is_string() {
        v["subtask_of"] = t["parentId"].clone();
    }
    v
}

fn tasks_of(v: &Value) -> Vec<Value> {
    v["tasks"].as_array().cloned().unwrap_or_default()
}

/// Open tasks first by due date (none last), then by priority.
fn sort_tasks(tasks: &mut [Value]) {
    let rank = |p: &str| match p {
        "urgent" => 0,
        "high" => 1,
        "medium" => 2,
        _ => 3,
    };
    tasks.sort_by(|a, b| {
        let (da, db) = (a["dueDate"].as_str().unwrap_or("9999"), b["dueDate"].as_str().unwrap_or("9999"));
        da.cmp(db).then(rank(a["priority"].as_str().unwrap_or("")).cmp(&rank(b["priority"].as_str().unwrap_or(""))))
    });
}

/// Within a due window: overdue, today, week (the next 7 days), or any.
fn due_in(t: &Value, due: &str, today: NaiveDate) -> bool {
    let d = t["dueDate"].as_str().and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
    match due {
        "overdue" => d.is_some_and(|d| d < today),
        "today" => d.is_some_and(|d| d <= today),
        "week" => d.is_some_and(|d| d <= today + chrono::Duration::days(7)),
        _ => true,
    }
}

// ---- names to ids

/// Something named, found among `items` by `field`: exact first, then contains.
fn pick<'a>(items: &'a [Value], field: &str, name: &str, what: &str) -> Result<&'a Value, String> {
    let n = name.trim().to_lowercase();
    if let Some(x) = items.iter().find(|i| str_of(&i["id"]) == name.trim() || str_of(&i[field]).to_lowercase() == n) {
        return Ok(x);
    }
    let found: Vec<&Value> = items.iter().filter(|i| str_of(&i[field]).to_lowercase().contains(&n)).collect();
    match found.as_slice() {
        [one] => Ok(one),
        [] => Err(format!("no {what} named {name:?}")),
        many => Err(format!("{} {what}s match {name:?}: {} — say which", many.len(), many.iter().map(|x| str_of(&x[field])).collect::<Vec<_>>().join(", "))),
    }
}

fn projects() -> Result<Vec<Value>, String> {
    Ok(get(&org("/projects")?)?["projects"].as_array().cloned().unwrap_or_default())
}

fn project(name: &str) -> Result<Value, String> {
    let all = projects()?;
    pick(&all, "name", name, "project").cloned()
}

fn team(name: &str) -> Result<Value, String> {
    let all = get(&org("/teams")?)?["teams"].as_array().cloned().unwrap_or_default();
    pick(&all, "name", name, "team").cloned()
}

fn person(name: &str) -> Result<Value, String> {
    let q = name.trim();
    if q.len() < 2 {
        return Err(format!("who is {q:?}?"));
    }
    let found = get(&format!("{}?q={}", org("/search")?, urlencode(q)))?["people"].as_array().cloned().unwrap_or_default();
    pick(&found, "name", q, "person").cloned()
}

/// `text` without a leading word, ignoring case ("Project Website" → "Website").
fn after<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    text.get(..word.len()).filter(|h| h.eq_ignore_ascii_case(word)).map(|_| text[word.len()..].trim())
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Where a new task goes: personal unless a project or team is named
/// ("project Website", "team IT", or just a name: projects first).
fn container(place: &str) -> Result<(Value, String), String> {
    let p = place.trim();
    let lower = p.to_lowercase();
    if p.is_empty() || matches!(lower.as_str(), "personal" | "me" | "mine" | "my tasks" | "my list") {
        return Ok((json!({ "type": "user" }), "personal".into()));
    }
    if let Some(name) = after(p, "team ") {
        let t = team(name)?;
        return Ok((json!({ "type": "team", "teamId": t["id"] }), format!("team {}", str_of(&t["name"]))));
    }
    let name = after(p, "project ").unwrap_or(p);
    match project(name) {
        Ok(pr) => Ok((json!({ "type": "project", "projectId": pr["id"] }), format!("project {}", str_of(&pr["name"])))),
        Err(e) => match team(name) {
            Ok(t) => Ok((json!({ "type": "team", "teamId": t["id"] }), format!("team {}", str_of(&t["name"])))),
            Err(_) => Err(e),
        },
    }
}

fn when(text: &str) -> Result<crate::when::When, String> {
    let t = text.trim();
    if let Ok(d) = NaiveDate::parse_from_str(t, "%Y-%m-%d") {
        return Ok(crate::when::When { date: d, time: None });
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(t) {
        let l = dt.with_timezone(&Local);
        return Ok(crate::when::When { date: l.date_naive(), time: Some(l.time()) });
    }
    crate::when::parse(t, Local::now()).ok_or_else(|| format!("can't read the time {t:?}: try \"tomorrow\", \"friday 3pm\", \"in 2h\", \"oct 14\""))
}

// ---- tools

const TAGS: &[&str] = &["pmi", "task", "tasks", "todo", "reminder", "remind", "project", "projects", "deadline", "due"];

pub fn capabilities() -> Vec<Capability> {
    let tool = |name: &str, description: &str, risk: RiskLevel, properties: Value, required: &[&str]| {
        let mut c = Capability::new(name, CapabilityKind::NativeTool, description, risk);
        c.input_schema = json!({ "type": "object", "properties": properties, "required": required });
        c.source = "pmi".into();
        c.tags = TAGS.iter().map(|t| t.to_string()).collect();
        c.permissions = vec![if risk == RiskLevel::ReadOnly { "pmi.read" } else { "pmi.write" }.into()];
        c
    };
    let id = json!({ "type": "string", "description": "The task's id (from pmi_tasks or pmi_search)." });
    let project = json!({ "type": "string", "description": "The project's name (or id)." });
    vec![
        tool(
            "pmi_tasks",
            "The user's tasks in PMI (their task and project system): their personal tasks and those assigned to them, or a project's or team's. Open ones unless include_done.",
            RiskLevel::ReadOnly,
            json!({
                "scope": { "type": "string", "description": "mine (default: personal + assigned), personal, assigned, following, \"project <name>\" or \"team <name>\"." },
                "due": { "type": "string", "enum": ["any", "overdue", "today", "week"], "description": "today includes overdue; week is the next 7 days." },
                "include_done": { "type": "boolean" },
            }),
            &[],
        ),
        tool("pmi_task", "One PMI task in full: description, subtasks, checklist, latest comments, the user's reminder.", RiskLevel::ReadOnly, json!({ "id": id }), &["id"]),
        tool("pmi_search", "Search PMI for tasks, projects, teams and people by name.", RiskLevel::ReadOnly, json!({ "query": { "type": "string" } }), &["query"]),
        tool(
            "pmi_add_task",
            "Add a task in PMI. \"Remind me Friday at 3 to call the vendor\" is a personal task with a reminder. Personal tasks are added at once; one in a project or team (others see it) needs the user's approval: delegate those to the Project Manager.",
            RiskLevel::LowWrite,
            json!({
                "title": { "type": "string", "description": "Short, as the user said it, without the date words." },
                "where": { "type": "string", "description": "Empty for the user's personal tasks, or \"project <name>\" / \"team <name>\"." },
                "due": { "type": "string", "description": "When it's due, as said: \"friday\", \"tomorrow\", \"oct 14\", or YYYY-MM-DD." },
                "remind": { "type": "string", "description": "When to remind the user, as said: \"friday 3pm\", \"in 2h\" (a day alone: 9:00). PMI sends the reminder." },
                "description": { "type": "string" },
                "priority": { "type": "string", "enum": ["low", "medium", "high", "urgent"] },
                "repeat": { "type": "string", "enum": ["daily", "weekdays", "weekly", "biweekly", "monthly", "yearly"] },
                "assignees": { "type": "array", "items": { "type": "string" }, "description": "People's names (project or team tasks)." },
            }),
            &["title"],
        ),
        tool(
            "pmi_update_task",
            "Change a PMI task: title, due date, status (todo, in_progress), priority, description. To finish one use pmi_complete.",
            RiskLevel::LowWrite,
            json!({
                "id": id,
                "title": { "type": "string" },
                "due": { "type": "string", "description": "As said, or \"none\" to clear it." },
                "status": { "type": "string", "enum": ["todo", "in_progress"] },
                "priority": { "type": "string", "enum": ["low", "medium", "high", "urgent"] },
                "description": { "type": "string" },
            }),
            &["id"],
        ),
        tool(
            "pmi_complete",
            "Mark a PMI task done. PMI keeps a closing comment: use what the user said about it (\"sent the PO to Dana\"), else leave it empty.",
            RiskLevel::LowWrite,
            json!({ "id": id, "comment": { "type": "string" } }),
            &["id"],
        ),
        tool(
            "pmi_remind",
            "Set (or clear) the user's own reminder on a PMI task; PMI notifies them then. Only the user sees it.",
            RiskLevel::LowWrite,
            json!({ "id": id, "when": { "type": "string", "description": "\"tomorrow 9am\", \"in 1h\", \"friday 3pm\"." }, "clear": { "type": "boolean" } }),
            &["id"],
        ),
        tool("pmi_comment", "Comment on a PMI task (others see it: needs the user's approval).", RiskLevel::Write, json!({ "id": id, "text": { "type": "string" } }), &["id", "text"]),
        tool(
            "pmi_inbox",
            "The user's PMI inbox: what others did on their tasks, mentions, reminders that went off, approvals. mark_read marks items read.",
            RiskLevel::LowWrite,
            json!({ "unread": { "type": "boolean", "description": "Only unread (default true)." }, "mark_read": { "type": "array", "items": { "type": "string" }, "description": "Inbox item ids to mark read." } }),
            &[],
        ),
        tool("pmi_waiting", "What's waiting on the user in PMI: task and project transfers to accept, tasks to approve.", RiskLevel::ReadOnly, json!({}), &[]),
        tool(
            "pmi_projects",
            "The projects the user can see in PMI, with health (on track, at risk, off track), open and overdue tasks, open risks and the last status update.",
            RiskLevel::ReadOnly,
            json!({ "include_closed": { "type": "boolean", "description": "Also completed and cancelled ones." } }),
            &[],
        ),
        tool("pmi_project", "One PMI project: details, members, the latest status updates and open risks.", RiskLevel::ReadOnly, json!({ "project": project }), &["project"]),
        tool(
            "pmi_draft_update",
            "Have PMI draft a project's status update (health, progress, risks, next steps) from its tasks. Nothing is posted.",
            RiskLevel::ReadOnly,
            json!({ "project": project }),
            &["project"],
        ),
        tool(
            "pmi_post_update",
            "Post a status update on a PMI project (others see it: needs the user's approval).",
            RiskLevel::Write,
            json!({
                "project": project,
                "health": { "type": "string", "enum": ["on_track", "at_risk", "off_track"] },
                "progress": { "type": "string" },
                "risks": { "type": "string" },
                "next_steps": { "type": "string" },
            }),
            &["project", "health", "progress", "risks", "next_steps"],
        ),
        tool(
            "pmi_add_risk",
            "Add a risk, issue or decision to a PMI project (others see it: needs the user's approval).",
            RiskLevel::Write,
            json!({
                "project": project,
                "kind": { "type": "string", "enum": ["risk", "issue", "decision"] },
                "title": { "type": "string" },
                "details": { "type": "string" },
                "impact": { "type": "string", "enum": ["low", "medium", "high"] },
                "likelihood": { "type": "string", "enum": ["low", "medium", "high"] },
                "response": { "type": "string", "description": "What's being done about it." },
                "owner": { "type": "string", "description": "A person's name." },
                "review": { "type": "string", "description": "When to look at it again." },
            }),
            &["project", "kind", "title", "details", "impact"],
        ),
    ]
}

/// A task's container type ("user" is personal), for deciding approvals.
fn container_of(id: &str) -> Result<(String, String), String> {
    let t = get(&org(&format!("/tasks/{id}"))?)?;
    Ok((str_of(&t["container"]["type"]), str_of(&t["container"]["name"])))
}

/// Whether a call needs the user's yes first, decided here: the user's
/// personal space and their own reminders don't; anything others see does.
pub fn approval(name: &str, args: &Value) -> Option<Ask> {
    let title = || args["title"].as_str().or(args["text"].as_str()).unwrap_or("").chars().take(300).collect::<String>();
    let id = args["id"].as_str().unwrap_or("");
    let shared = |what: String, detail: String, place: &str| Some(Ask { what, detail, why: format!("others in {place} will see it"), dangerous: false });
    match name {
        "pmi_add_task" => {
            let place = args["where"].as_str().unwrap_or("");
            match container(place) {
                Ok((_, p)) if p == "personal" => None,
                Ok((_, p)) => shared(format!("add a task to {p} in PMI"), title(), &p),
                // Unknown place: the call will say so, nothing changes.
                Err(_) => None,
            }
        }
        "pmi_update_task" | "pmi_complete" => match container_of(id) {
            Ok((kind, _)) if kind == "user" => None,
            Ok((kind, place)) => {
                let what = if name == "pmi_complete" { "complete a task" } else { "change a task" };
                shared(format!("{what} in {kind} {place} (PMI)"), args.to_string().chars().take(300).collect(), &format!("{kind} {place}"))
            }
            Err(_) => None,
        },
        "pmi_comment" => {
            let place = container_of(id).map(|(k, p)| format!("{k} {p}")).unwrap_or_else(|_| "PMI".into());
            shared(format!("comment on a task in {place}"), title(), &place)
        }
        "pmi_post_update" => {
            let p = str_of(&args["project"]);
            shared(format!("post a status update on project {p} ({})", str_of(&args["health"])), str_of(&args["progress"]).chars().take(300).collect(), &format!("project {p}"))
        }
        "pmi_add_risk" => {
            let p = str_of(&args["project"]);
            shared(format!("add a {} to project {p}", str_of(&args["kind"])), title(), &format!("project {p}"))
        }
        _ => None,
    }
}

/// Run a PMI tool (asked about first by the caller when `approval` says so).
pub fn call(name: &str, args: &Value) -> Result<Value, String> {
    let out = run(name, args);
    if out.is_ok() && !matches!(name, "pmi_tasks" | "pmi_task" | "pmi_search" | "pmi_waiting" | "pmi_projects" | "pmi_project" | "pmi_draft_update") {
        mark_stale();
    }
    out
}

fn run(name: &str, args: &Value) -> Result<Value, String> {
    let today = Local::now().date_naive();
    match name {
        "pmi_tasks" => {
            let scope = args["scope"].as_str().unwrap_or("mine").trim().to_string();
            let lower = scope.to_lowercase();
            let mut tasks = match lower.as_str() {
                "" | "mine" | "my tasks" => {
                    let mut all = tasks_of(&get(&format!("{}?personal=true", org("/tasks")?))?);
                    for t in tasks_of(&get(&org("/tasks/assigned")?)?) {
                        if !all.iter().any(|x| x["id"] == t["id"]) {
                            all.push(t);
                        }
                    }
                    all
                }
                "personal" => tasks_of(&get(&format!("{}?personal=true", org("/tasks")?))?),
                "assigned" => tasks_of(&get(&org("/tasks/assigned")?)?),
                "following" => tasks_of(&get(&org("/tasks/following")?)?),
                _ if after(&scope, "team ").is_some() => tasks_of(&get(&format!("{}?teamId={}", org("/tasks")?, str_of(&team(after(&scope, "team ").unwrap_or(""))?["id"])))?),
                _ => {
                    let name = after(&scope, "project ").unwrap_or(&scope);
                    tasks_of(&get(&format!("{}?projectId={}", org("/tasks")?, str_of(&project(name)?["id"])))?)
                }
            };
            let due = args["due"].as_str().unwrap_or("any");
            tasks.retain(|t| (args["include_done"] == true || t["status"] != "done") && due_in(t, due, today));
            sort_tasks(&mut tasks);
            let total = tasks.len();
            Ok(json!({ "today": today.to_string(), "count": total, "tasks": tasks.iter().take(60).map(brief).collect::<Vec<_>>() }))
        }
        "pmi_task" => {
            let t = get(&org(&format!("/tasks/{}", str_of(&args["id"])))?)?;
            let mut v = brief(&t);
            v["description"] = json!(str_of(&t["description"]).chars().take(2000).collect::<String>());
            v["owner"] = t["owner"]["name"].clone();
            v["reminder"] = t["reminder"].clone();
            v["following"] = t["following"].clone();
            v["subtasks"] = json!(t["subtasks"].as_array().into_iter().flatten().map(brief).collect::<Vec<_>>());
            v["checklist_items"] = json!(t["checklistItems"].as_array().into_iter().flatten().map(|i| format!("[{}] {}", if i["done"] == true { "x" } else { " " }, str_of(&i["text"]))).collect::<Vec<_>>());
            let comments: Vec<Value> = t["comments"].as_array().into_iter().flatten().collect::<Vec<_>>().into_iter().rev().take(10).rev().map(|c| json!({ "by": c["author"]["name"], "at": c["createdAt"], "text": str_of(&c["body"]).chars().take(400).collect::<String>() })).collect();
            v["comments"] = json!(comments);
            Ok(v)
        }
        "pmi_search" => {
            let q = str_of(&args["query"]);
            let r = get(&format!("{}?q={}", org("/search")?, urlencode(q.trim())))?;
            Ok(json!({
                "tasks": tasks_of(&r).iter().map(brief).collect::<Vec<_>>(),
                "projects": r["projects"].as_array().into_iter().flatten().map(|p| json!({ "id": p["id"], "name": p["name"], "archived": p["archived"] })).collect::<Vec<_>>(),
                "teams": r["teams"].as_array().into_iter().flatten().map(|t| json!({ "id": t["id"], "name": t["name"] })).collect::<Vec<_>>(),
                "people": r["people"].as_array().into_iter().flatten().map(|p| json!({ "name": p["name"], "username": p["username"] })).collect::<Vec<_>>(),
            }))
        }
        "pmi_add_task" => {
            let title = str_of(&args["title"]);
            if title.trim().is_empty() {
                return Err("a task needs a title".into());
            }
            let (container, place) = container(args["where"].as_str().unwrap_or(""))?;
            let remind = args["remind"].as_str().filter(|s| !s.trim().is_empty()).map(when).transpose()?;
            let due = args["due"].as_str().filter(|s| !s.trim().is_empty()).map(when).transpose()?.or(remind);
            let mut body = json!({ "container": container, "title": title.trim() });
            if let Some(d) = due {
                body["dueDate"] = json!(d.date.to_string());
            }
            for (k, f) in [("description", "description"), ("priority", "priority"), ("repeat", "repeat")] {
                if let Some(v) = args[k].as_str().filter(|v| !v.is_empty()) {
                    body[f] = json!(v);
                }
            }
            let names: Vec<String> = args["assignees"].as_array().into_iter().flatten().filter_map(|n| n.as_str().map(str::to_string)).collect();
            if !names.is_empty() {
                body["assigneeIds"] = json!(names.iter().map(|n| person(n).map(|p| p["userId"].clone())).collect::<Result<Vec<_>, _>>()?);
            }
            let t = request(reqwest::Method::POST, &org("/tasks")?, Some(&body))?;
            let mut v = brief(&t);
            if let Some(r) = remind.and_then(|r| r.at()) {
                let id = str_of(&t["id"]);
                match request(reqwest::Method::PUT, &org(&format!("/tasks/{id}/reminder"))?, Some(&json!({ "at": r.with_timezone(&Utc).to_rfc3339() }))) {
                    Ok(_) => v["reminder"] = json!(r.format("%a %b %-d %H:%M").to_string()),
                    Err(e) => v["reminder_error"] = json!(e),
                }
            }
            v["added_to"] = json!(place);
            Ok(v)
        }
        "pmi_update_task" => {
            let id = str_of(&args["id"]);
            let mut body = json!({});
            for k in ["title", "status", "priority", "description"] {
                if let Some(v) = args[k].as_str().filter(|v| !v.is_empty()) {
                    body[k] = json!(v);
                }
            }
            if let Some(d) = args["due"].as_str().filter(|d| !d.trim().is_empty()) {
                body["dueDate"] = if matches!(d.trim().to_lowercase().as_str(), "none" | "no date" | "clear") { Value::Null } else { json!(when(d)?.date.to_string()) };
            }
            if body.as_object().is_some_and(|m| m.is_empty()) {
                return Err("nothing to change".into());
            }
            Ok(brief(&request(reqwest::Method::PATCH, &org(&format!("/tasks/{id}"))?, Some(&body))?))
        }
        "pmi_complete" => {
            let id = str_of(&args["id"]);
            let comment = args["comment"].as_str().map(str::trim).filter(|c| !c.is_empty()).unwrap_or("Done (via lyra)");
            Ok(brief(&request(reqwest::Method::PATCH, &org(&format!("/tasks/{id}"))?, Some(&json!({ "status": "done", "closingComment": comment })))?))
        }
        "pmi_remind" => {
            let id = str_of(&args["id"]);
            let path = org(&format!("/tasks/{id}/reminder"))?;
            if args["clear"] == true {
                request(reqwest::Method::DELETE, &path, None)?;
                return Ok(json!({ "id": id, "reminder": null }));
            }
            let at = when(args["when"].as_str().unwrap_or(""))?.at().ok_or("that time doesn't exist here")?;
            let t = request(reqwest::Method::PUT, &path, Some(&json!({ "at": at.with_timezone(&Utc).to_rfc3339() })))?;
            Ok(json!({ "id": id, "title": t["title"], "reminder": at.format("%a %b %-d %H:%M").to_string() }))
        }
        "pmi_comment" => {
            let id = str_of(&args["id"]);
            request(reqwest::Method::POST, &org(&format!("/tasks/{id}/comments"))?, Some(&json!({ "body": str_of(&args["text"]) })))?;
            Ok(json!({ "commented": id }))
        }
        "pmi_inbox" => {
            let mut marked = Vec::new();
            for item in args["mark_read"].as_array().into_iter().flatten().filter_map(Value::as_str) {
                request(reqwest::Method::POST, &org(&format!("/inbox/{item}/read"))?, None)?;
                marked.push(item.to_string());
            }
            let unread = args["unread"] != false;
            let r = get(&format!("{}?filter={}", org("/inbox")?, if unread { "unread" } else { "all" }))?;
            let items: Vec<Value> = r["items"].as_array().into_iter().flatten().take(30).map(inbox_item).collect();
            Ok(json!({ "unread": r["unread"], "items": items, "marked_read": marked }))
        }
        "pmi_waiting" => Ok(waiting()?),
        "pmi_projects" => {
            let all = get(&format!("{}?today={today}", org("/portfolio")?))?["projects"].as_array().cloned().unwrap_or_default();
            let open = |p: &&Value| args["include_closed"] == true || !matches!(p["status"].as_str(), Some("completed" | "cancelled"));
            Ok(json!({ "projects": all.iter().filter(open).map(portfolio_item).collect::<Vec<_>>() }))
        }
        "pmi_project" => {
            let p = project(&str_of(&args["project"]))?;
            let id = str_of(&p["id"]);
            let full = get(&org(&format!("/projects/{id}"))?)?;
            let updates = get(&org(&format!("/projects/{id}/updates"))?)?["updates"].as_array().cloned().unwrap_or_default();
            let risks = get(&org(&format!("/projects/{id}/risks"))?)?["risks"].as_array().cloned().unwrap_or_default();
            Ok(json!({
                "id": id,
                "name": full["name"],
                "status": full["status"],
                "health": full["health"],
                "priority": full["priority"],
                "dates": format!("{} → {}", full["startDate"].as_str().unwrap_or("?"), full["endDate"].as_str().unwrap_or("?")),
                "description": str_of(&full["description"]).chars().take(1500).collect::<String>(),
                "manager": full["manager"]["name"],
                "leads": full["leads"].as_array().into_iter().flatten().map(|l| str_of(&l["name"])).collect::<Vec<_>>(),
                "members": full["members"].as_array().into_iter().flatten().map(|m| str_of(&m["name"])).collect::<Vec<_>>(),
                "updates": updates.iter().take(3).map(|u| json!({ "at": u["createdAt"], "by": u["author"]["name"], "health": u["health"], "progress": u["progress"], "risks": u["risks"], "next_steps": u["nextSteps"] })).collect::<Vec<_>>(),
                "open_risks": risks.iter().filter(|r| r["closed"] != true).map(|r| json!({ "kind": r["kind"], "title": r["title"], "severity": r["severity"], "owner": r["owner"]["name"], "response": r["response"] })).collect::<Vec<_>>(),
            }))
        }
        "pmi_draft_update" => {
            let p = project(&str_of(&args["project"]))?;
            let mut d = request(reqwest::Method::POST, &org("/ai/status-update")?, Some(&json!({ "projectId": p["id"], "today": today.to_string() })))?;
            d["project"] = p["name"].clone();
            d["note"] = json!("a draft: nothing is posted until pmi_post_update");
            Ok(d)
        }
        "pmi_post_update" => {
            let p = project(&str_of(&args["project"]))?;
            let body = json!({ "health": args["health"], "progress": str_of(&args["progress"]), "risks": str_of(&args["risks"]), "nextSteps": str_of(&args["next_steps"]) });
            request(reqwest::Method::POST, &org(&format!("/projects/{}/updates", str_of(&p["id"])))?, Some(&body))?;
            Ok(json!({ "posted": p["name"], "health": args["health"] }))
        }
        "pmi_add_risk" => {
            let p = project(&str_of(&args["project"]))?;
            let owner = args["owner"].as_str().filter(|o| !o.trim().is_empty()).map(person).transpose()?.map(|o| o["userId"].clone()).unwrap_or(Value::Null);
            let review = args["review"].as_str().filter(|r| !r.trim().is_empty()).map(when).transpose()?.map(|w| json!(w.date.to_string())).unwrap_or(Value::Null);
            let likelihood = args["likelihood"].as_str().filter(|l| !l.is_empty()).map_or(Value::Null, |l| json!(l));
            let body = json!({
                "kind": args["kind"], "title": str_of(&args["title"]), "details": str_of(&args["details"]), "ownerId": owner,
                "impact": args["impact"], "likelihood": likelihood, "response": str_of(&args["response"]), "reviewDate": review,
            });
            request(reqwest::Method::POST, &org(&format!("/projects/{}/risks", str_of(&p["id"])))?, Some(&body))?;
            Ok(json!({ "added": str_of(&args["kind"]), "project": p["name"], "title": args["title"] }))
        }
        other => Err(format!("{other} isn't a PMI tool")),
    }
}

/// An inbox item in a line or two.
fn inbox_item(i: &Value) -> Value {
    let task = if i["reminder"].is_object() { &i["reminder"]["task"] } else { &i["activity"]["task"] };
    let mut v = json!({ "id": i["id"], "reason": i["reason"], "at": i["createdAt"], "read": !i["readAt"].is_null() });
    if task.is_object() {
        v["task"] = json!({ "id": task["id"], "title": task["title"], "where": task["container"]["name"] });
    }
    if i["activity"].is_object() {
        v["what"] = i["activity"]["kind"].clone();
        v["by"] = i["activity"]["actor"]["name"].clone();
    }
    if i["points"].is_object() {
        v["points"] = json!(format!("{} ({:+})", str_of(&i["points"]["label"]), i["points"]["points"].as_i64().unwrap_or(0)));
    }
    v
}

fn portfolio_item(p: &Value) -> Value {
    json!({
        "id": p["id"],
        "name": p["name"],
        "status": p["status"],
        "health": p["health"],
        "reported_health": p["reportedHealth"],
        "tasks": p["tasks"],
        "risks": p["risks"],
        "last_update": p["lastUpdateAt"],
        "leads": p["leads"].as_array().into_iter().flatten().map(|l| str_of(&l["name"])).collect::<Vec<_>>(),
    })
}

/// Transfers and approvals that wait on the user.
pub fn waiting() -> Result<Value, String> {
    let pending = |v: &Value| v["status"] == "pending" && v["canDecide"] == true;
    let tasks: Vec<Value> = get(&org("/task-transfers")?)?["transfers"].as_array().into_iter().flatten().filter(|t| pending(t)).map(|t| json!({ "id": t["id"], "task": t["task"]["title"], "expires": t["expiresAt"] })).collect();
    let projects: Vec<Value> = get(&org("/transfers")?)?["transfers"].as_array().into_iter().flatten().filter(|t| pending(t)).map(|t| json!({ "id": t["id"], "project": t["project"]["name"], "expires": t["expiresAt"] })).collect();
    let approvals: Vec<Value> = get(&format!("{}?filter=unread", org("/inbox")?))?["items"].as_array().into_iter().flatten().filter(|i| i["reason"] == "approval").map(inbox_item).collect();
    Ok(json!({ "task_transfers": tasks, "project_transfers": projects, "approvals": approvals }))
}

// ---- the live view (lyra serve): what's open, what waits, kept fresh by PMI's events

/// The user's PMI at a glance, for the Tasks page, the briefing and the panels.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub at: Option<DateTime<Utc>>,
    /// Why it couldn't be read (the rest is the last good read).
    pub error: Option<String>,
    /// Following PMI's live events.
    pub live: bool,
    pub user: String,
    pub org: String,
    /// Open tasks, personal and assigned, soonest due first.
    pub tasks: Vec<Value>,
    pub waiting: Value,
    pub inbox_unread: u64,
    pub inbox: Vec<Value>,
    /// Open projects with their health.
    pub projects: Vec<Value>,
}

impl State {
    /// (overdue, due today, waiting on the user).
    pub fn counts(&self, today: NaiveDate) -> (usize, usize, usize) {
        let due = |t: &&Value| t["due"].as_str().and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
        let overdue = self.tasks.iter().filter(|t| due(t).is_some_and(|d| d < today)).count();
        let today_n = self.tasks.iter().filter(|t| due(t) == Some(today)).count();
        let waiting = ["task_transfers", "project_transfers", "approvals"].iter().map(|k| self.waiting[*k].as_array().map_or(0, Vec::len)).sum();
        (overdue, today_n, waiting)
    }

    /// "2 overdue · 3 today · 1 waiting" (or "nothing due").
    pub fn line(&self, today: NaiveDate) -> String {
        if let Some(e) = &self.error {
            return format!("⚠ {e}");
        }
        let (o, t, w) = self.counts(today);
        let parts: Vec<String> = [(o, "overdue"), (t, "today"), (w, "waiting")].iter().filter(|(n, _)| *n > 0).map(|(n, w)| format!("{n} {w}")).collect();
        if parts.is_empty() { format!("{} open · nothing due", self.tasks.len()) } else { parts.join(" · ") }
    }
}

/// Read everything the view needs (a few requests).
pub fn snapshot() -> Result<State, String> {
    let w = who()?;
    let mut tasks = tasks_of(&get(&format!("{}?personal=true", org("/tasks")?))?);
    for t in tasks_of(&get(&org("/tasks/assigned")?)?) {
        if !tasks.iter().any(|x| x["id"] == t["id"]) {
            tasks.push(t);
        }
    }
    tasks.retain(|t| t["status"] != "done" && t["parentId"].is_null());
    sort_tasks(&mut tasks);
    let inbox = get(&format!("{}?filter=unread", org("/inbox")?))?;
    let today = Local::now().date_naive();
    let projects = get(&format!("{}?today={today}", org("/portfolio")?))?["projects"].as_array().cloned().unwrap_or_default();
    Ok(State {
        at: Some(Utc::now()),
        error: None,
        live: false,
        user: w.user,
        org: w.org,
        tasks: tasks.iter().take(200).map(brief).collect(),
        waiting: waiting()?,
        inbox_unread: inbox["unread"].as_u64().unwrap_or(0),
        inbox: inbox["items"].as_array().into_iter().flatten().take(20).map(inbox_item).collect(),
        projects: projects.iter().filter(|p| !matches!(p["status"].as_str(), Some("completed" | "cancelled"))).map(portfolio_item).collect(),
    })
}

/// A change was made through lyra: read again soon.
static STALE: AtomicBool = AtomicBool::new(false);

pub fn mark_stale() {
    STALE.store(true, Ordering::SeqCst);
}

pub fn take_stale() -> bool {
    STALE.swap(false, Ordering::SeqCst)
}

/// What PMI's event stream says.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// Connected (true) or not.
    Live(bool),
    /// Something changed in these areas ("tasks", "projects", …; "resync": everything).
    Changed(Vec<String>),
}

/// Server-Sent Events, a line at a time: `event:` then `data:`, a blank line ends one.
#[derive(Default)]
pub struct Sse {
    event: String,
    data: String,
}

impl Sse {
    pub fn line(&mut self, line: &str) -> Option<Event> {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            let (event, data) = (std::mem::take(&mut self.event), std::mem::take(&mut self.data));
            let v: Value = serde_json::from_str(&data).unwrap_or(Value::Null);
            let kind = v["type"].as_str().map(str::to_string).unwrap_or(event);
            return match kind.as_str() {
                "ready" => Some(Event::Live(true)),
                "change" => Some(Event::Changed(v["areas"].as_array().into_iter().flatten().filter_map(|a| a.as_str().map(str::to_string)).collect())),
                "resync" => Some(Event::Changed(vec!["resync".into()])),
                _ => None,
            };
        }
        if let Some(e) = line.strip_prefix("event:") {
            self.event = e.trim().to_string();
        } else if let Some(d) = line.strip_prefix("data:") {
            if !self.data.is_empty() {
                self.data.push('\n');
            }
            self.data.push_str(d.trim_start());
        }
        None
    }
}

/// Follow PMI's live events for as long as lyra runs (a thread). PMI ends a
/// stream after a minute: that's reconnected at once, quietly. A connection
/// that fails is reported and retried with a growing pause; one is renewed
/// after 10 minutes at most, so a silent one can't hang on.
pub fn follow(tx: std::sync::mpsc::Sender<Event>) {
    use std::io::BufRead;
    let mut pause = 5;
    loop {
        if !configured() {
            std::thread::sleep(Duration::from_secs(60));
            continue;
        }
        let connected = (|| -> Result<(), String> {
            let path = org("/events")?;
            let token = crate::secrets::token("pmi").ok_or("no token")?;
            let client = reqwest::blocking::Client::builder().connect_timeout(Duration::from_secs(10)).timeout(Duration::from_secs(600)).build().map_err(|e| e.to_string())?;
            let resp = client
                .get(format!("{}{path}", settings().url.trim_end_matches('/')))
                .bearer_auth(token)
                .header("Accept", "text/event-stream")
                .send()
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("events: {}", resp.status()));
            }
            let mut sse = Sse::default();
            for line in std::io::BufReader::new(resp).lines() {
                let Ok(line) = line else { break };
                if let Some(e) = sse.line(&line)
                    && tx.send(e).is_err()
                {
                    return Ok(());
                }
            }
            Ok(())
        })();
        match connected {
            Ok(()) => {
                pause = 5;
                std::thread::sleep(Duration::from_secs(1));
            }
            Err(_) => {
                let _ = tx.send(Event::Live(false));
                std::thread::sleep(Duration::from_secs(pause));
                pause = (pause * 2).min(300);
            }
        }
    }
}

// ---- nags: a reminder that went off and is still open, pushed again

/// One reminder being nagged about.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Nag {
    pub task: String,
    pub title: String,
    /// When the reminder went off.
    pub fired: DateTime<Utc>,
    pub sent: u32,
    pub last: Option<DateTime<Utc>>,
}

/// Reminders being nagged about, by inbox item (kept across restarts).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Nags {
    pub items: std::collections::HashMap<String, Nag>,
}

fn nags_path() -> Option<std::path::PathBuf> {
    Some(crate::config::home()?.join("pmi").join("nags.json"))
}

impl Nags {
    pub fn load() -> Self {
        nags_path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        if let Some(p) = nags_path() {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(p, serde_json::to_string_pretty(self).unwrap_or_default());
        }
    }

    /// The reminders to nag about now. Reminders that went off (unread inbox
    /// items) whose task is still open are followed; each is pushed again
    /// every `nag_minutes`, at most `nag_max` times. Read items and finished
    /// tasks drop out. The bool says whether anything changed (to save).
    pub fn due(&mut self, state: &State, s: &Settings, now: DateTime<Utc>) -> (Vec<Nag>, bool) {
        let mut changed = false;
        let open: Vec<&str> = state.tasks.iter().filter_map(|t| t["id"].as_str()).collect();
        let fired: Vec<(&str, &Value)> = state.inbox.iter().filter(|i| i["reason"] == "reminder" && i["read"] != true).filter_map(|i| Some((i["id"].as_str()?, i))).collect();
        let before = self.items.len();
        self.items.retain(|id, n| fired.iter().any(|(f, _)| f == id) && open.contains(&n.task.as_str()));
        changed |= self.items.len() != before;
        for (id, item) in &fired {
            let task = item["task"]["id"].as_str().unwrap_or("");
            if !open.contains(&task) || self.items.contains_key(*id) {
                continue;
            }
            let at = item["at"].as_str().and_then(|a| DateTime::parse_from_rfc3339(a).ok()).map_or(now, |a| a.with_timezone(&Utc));
            // Old ones (from before lyra was looking) aren't brought up again.
            if now - at > chrono::Duration::hours(24) {
                continue;
            }
            self.items.insert(id.to_string(), Nag { task: task.into(), title: str_of(&item["task"]["title"]), fired: at, sent: 0, last: None });
            changed = true;
        }
        if !s.nag {
            return (vec![], changed);
        }
        let every = chrono::Duration::minutes(s.nag_minutes.max(5) as i64);
        let mut out = Vec::new();
        for n in self.items.values_mut() {
            if n.sent < s.nag_max && now - n.last.unwrap_or(n.fired) >= every {
                n.sent += 1;
                n.last = Some(now);
                out.push(n.clone());
                changed = true;
            }
        }
        (out, changed)
    }
}

/// A button on a nag's push: done, remind in an hour, or tomorrow morning.
/// Pressing it is the user's own answer, so it acts even on a shared task.
pub fn push_action(action: &str, task: &str) -> Result<String, String> {
    let t = match action {
        "done" => {
            run("pmi_complete", &json!({ "id": task, "comment": "Done (from the reminder)" }))?;
            "done".to_string()
        }
        "snooze1h" | "tomorrow" => {
            let when = if action == "tomorrow" { "tomorrow 9am" } else { "in 1h" };
            let r = run("pmi_remind", &json!({ "id": task, "when": when }))?;
            format!("reminder moved to {}", str_of(&r["reminder"]))
        }
        other => return Err(format!("unknown action {other}")),
    };
    // The reminder that went off has been answered.
    if let Ok(inbox) = get(&format!("{}?filter=unread", org("/inbox")?)) {
        for i in inbox["items"].as_array().into_iter().flatten().filter(|i| i["reason"] == "reminder" && i["reminder"]["task"]["id"].as_str() == Some(task)) {
            let _ = request(reqwest::Method::POST, &org(&format!("/inbox/{}/read", str_of(&i["id"])))?, None);
        }
    }
    mark_stale();
    Ok(t)
}

// ---- for the terminal and the status page

/// `/pmi`.
pub fn describe() -> String {
    let s = settings();
    if crate::secrets::token("pmi").is_none() {
        return format!("PMI ({}): no token yet. Make one in PMI (Your account → Security) and set it with /pmi token <token>.", s.url);
    }
    match who() {
        Ok(w) => format!("PMI ({}): signed in as {} · organization {} (id {}){}", s.url, w.user, w.org, w.org_id, if s.enabled { "" } else { " · off ([pmi] enabled)" }),
        Err(e) => format!("PMI ({}): {e}", s.url),
    }
}

/// The last `/tasks` list, so `/task done 2` works.
static LISTED: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn listed(n: &str) -> String {
    n.parse::<usize>().ok().and_then(|i| LISTED.lock().unwrap_or_else(|e| e.into_inner()).get(i.wrapping_sub(1)).cloned()).unwrap_or_else(|| n.to_string())
}

/// `/tasks [today|overdue|week|all|project <name>|team <name>]` as text.
pub fn tasks_text(arg: &str) -> Result<String, String> {
    let a = arg.trim();
    let (scope, due) = match a {
        "" | "open" => ("mine", "any"),
        "today" | "overdue" | "week" => ("mine", a),
        "all" => ("mine", "any"),
        s => (s, "any"),
    };
    let v = call("pmi_tasks", &json!({ "scope": scope, "due": due }))?;
    let tasks = v["tasks"].as_array().cloned().unwrap_or_default();
    let today = Local::now().date_naive().to_string();
    *LISTED.lock().unwrap_or_else(|e| e.into_inner()) = tasks.iter().map(|t| str_of(&t["id"])).collect();
    if tasks.is_empty() {
        return Ok("nothing open. /task add <what> [when] adds one".into());
    }
    let mut out = vec![format!("{} open task{}{}:", v["count"], if v["count"] == 1 { "" } else { "s" }, if due == "any" { String::new() } else { format!(" ({due})") })];
    for (i, t) in tasks.iter().enumerate() {
        let due = t["due"].as_str().map(|d| if d < today.as_str() { format!(" · ⚠ due {d}") } else if d == today { " · today".to_string() } else { format!(" · {d}") }).unwrap_or_default();
        out.push(format!("{:>2}. {}{due} · {}{}", i + 1, str_of(&t["title"]), str_of(&t["where"]), if t["status"] == "in_progress" { " · in progress" } else { "" }));
    }
    out.push("/task done <n> [comment] · /task snooze <n> [1h|tomorrow] · /task add <what> [when]".into());
    Ok(out.join("\n"))
}

/// `/task add|done|snooze …`.
pub fn task_command(arg: &str) -> Result<String, String> {
    let (sub, rest) = arg.trim().split_once(' ').map_or((arg.trim(), ""), |(a, b)| (a, b.trim()));
    match sub {
        "add" if !rest.is_empty() => {
            let (title, w) = crate::when::split(rest, Local::now());
            let mut args = json!({ "title": title });
            if let Some(w) = w {
                args["due"] = json!(w.date.to_string());
                if let Some(t) = w.time {
                    args["remind"] = json!(format!("{} {}", w.date, t.format("%H:%M")));
                }
            }
            let t = call("pmi_add_task", &args)?;
            Ok(format!(
                "added to your PMI tasks: {}{}{}",
                str_of(&t["title"]),
                t["due"].as_str().map(|d| format!(" · due {d}")).unwrap_or_default(),
                t["reminder"].as_str().map(|r| format!(" · reminder {r}")).unwrap_or_default()
            ))
        }
        "done" if !rest.is_empty() => {
            let (n, comment) = rest.split_once(' ').map_or((rest, ""), |(a, b)| (a, b.trim()));
            let id = listed(n);
            if approval("pmi_complete", &json!({ "id": id })).is_some() {
                return Err("that task is in a project or team: ask lyra (the Project Manager asks you to approve)".into());
            }
            let t = call("pmi_complete", &json!({ "id": id, "comment": comment }))?;
            Ok(format!("done: {}", str_of(&t["title"])))
        }
        "snooze" if !rest.is_empty() => {
            let (n, until) = rest.split_once(' ').map_or((rest, ""), |(a, b)| (a, b.trim()));
            let until = match until {
                "" | "1h" => "in 1h".to_string(),
                "tomorrow" => "tomorrow 9am".to_string(),
                u => u.to_string(),
            };
            let r = call("pmi_remind", &json!({ "id": listed(n), "when": until }))?;
            Ok(format!("reminder moved: {} · {}", str_of(&r["title"]), str_of(&r["reminder"])))
        }
        _ => Err("usage: /task add <what> [when] · /task done <n|id> [comment] · /task snooze <n|id> [1h|tomorrow|<when>]".into()),
    }
}

/// `/pmi [token <token>]`.
pub fn command(arg: &str) -> Result<String, String> {
    let arg = arg.trim();
    match arg.split_once(' ').map_or((arg, ""), |(a, b)| (a, b.trim())) {
        ("" | "status", _) => Ok(describe()),
        ("token", t) => {
            crate::secrets::set_token("pmi", t)?;
            *WHO.lock().unwrap_or_else(|e| e.into_inner()) = None;
            if t.is_empty() {
                return Ok("PMI token removed".into());
            }
            Ok(format!("PMI token saved (readable by this user only, not backed up). {}", describe()))
        }
        _ => Err("usage: /pmi · /pmi token <token> (empty removes it)".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(id: &str, container: &str, due: Option<&str>, status: &str, priority: &str) -> Value {
        json!({
            "id": id, "title": format!("task {id}"), "status": status, "priority": priority, "dueDate": due,
            "container": { "type": container, "id": "c", "name": if container == "user" { "Me" } else { "Website" } },
            "assignees": [], "blocked": false, "approvalStatus": null, "checklist": { "done": 1, "total": 3 }, "repeat": null, "parentId": null,
        })
    }

    #[test]
    fn tasks_in_a_few_fields() {
        let b = brief(&task("1", "user", Some("2026-10-09"), "todo", "high"));
        assert_eq!(b["where"], "personal");
        assert_eq!(b["checklist"], "1/3");
        assert!(b.get("assignees").is_none() && b.get("repeat").is_none());
        let b = brief(&task("2", "project", None, "todo", "low"));
        assert_eq!(b["where"], "project Website");
    }

    #[test]
    fn due_soonest_and_most_urgent_first() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 7).unwrap();
        let mut ts = vec![
            task("none", "user", None, "todo", "urgent"),
            task("later", "user", Some("2026-10-20"), "todo", "low"),
            task("late-low", "user", Some("2026-10-01"), "todo", "low"),
            task("late-high", "user", Some("2026-10-01"), "todo", "high"),
            task("today", "user", Some("2026-10-07"), "todo", "medium"),
        ];
        sort_tasks(&mut ts);
        let ids: Vec<&str> = ts.iter().map(|t| t["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["late-high", "late-low", "today", "later", "none"]);
        let n = |due: &str| ts.iter().filter(|t| due_in(t, due, today)).count();
        assert_eq!((n("overdue"), n("today"), n("week"), n("any")), (2, 3, 3, 5));
    }

    #[test]
    fn names_pick_one_or_say_which() {
        let items = vec![json!({ "id": "a", "name": "Website" }), json!({ "id": "b", "name": "Website v2" }), json!({ "id": "c", "name": "Phones" })];
        assert_eq!(pick(&items, "name", "website", "project").unwrap()["id"], "a", "exact wins");
        assert_eq!(pick(&items, "name", "phone", "project").unwrap()["id"], "c");
        assert!(pick(&items, "name", "web", "project").unwrap_err().contains("Website, Website v2"));
        assert!(pick(&items, "name", "nope", "project").is_err());
        assert_eq!(urlencode("a b&c"), "a%20b%26c");
    }

    /// What the fake PMI was sent: method, path, body.
    type Seen = std::sync::Arc<Mutex<Vec<(String, String, Value)>>>;

    /// A fake PMI: answers by method and path, and keeps what it was sent.
    fn fake_pmi() -> (String, Seen) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let mut len = 0;
                loop {
                    let mut h = String::new();
                    reader.read_line(&mut h).unwrap();
                    if h.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = h.to_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                    assert!(!h.to_lowercase().starts_with("authorization:") || h.contains("Bearer t0k"), "{h}");
                }
                let mut body = vec![0; len];
                reader.read_exact(&mut body).unwrap();
                let parts: Vec<&str> = first.split_whitespace().collect();
                let (method, path) = (parts[0].to_string(), parts[1].to_string());
                let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                log.lock().unwrap().push((method.clone(), path.clone(), body.clone()));
                let reply = match (method.as_str(), path.as_str()) {
                    ("GET", "/v1/auth/me") => json!({ "user": { "id": "u1", "name": "Garrett" }, "organizations": [{ "id": "o1", "name": "FBCAD", "slug": "fbcad" }] }),
                    ("POST", "/v1/orgs/o1/tasks") => {
                        let mut t = task("t1", "user", body["dueDate"].as_str(), "todo", "medium");
                        t["title"] = body["title"].clone();
                        t
                    }
                    ("GET", "/v1/orgs/o1/tasks/t2") => task("t2", "project", None, "todo", "high"),
                    ("PATCH", p) if p.starts_with("/v1/orgs/o1/tasks/") => task("t1", "user", None, "done", "medium"),
                    _ => json!({ "title": "t" }),
                };
                let text = reply.to_string();
                let mut out = stream;
                let _ = write!(out, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len());
            }
        });
        (url, seen)
    }

    #[test]
    fn personal_tasks_with_reminders_and_closing_comments() {
        let (url, seen) = fake_pmi();
        *TEST.lock().unwrap() = Some((url, "t0k".into()));
        *WHO.lock().unwrap() = None;
        let t = call("pmi_add_task", &json!({ "title": "Call the vendor", "remind": "tomorrow 3pm" })).unwrap();
        assert_eq!((t["title"].as_str(), t["where"].as_str(), t["added_to"].as_str()), (Some("Call the vendor"), Some("personal"), Some("personal")));
        assert!(t["reminder"].as_str().is_some_and(|r| r.ends_with("15:00")), "{t}");
        let done = call("pmi_complete", &json!({ "id": "t1" })).unwrap();
        assert_eq!(done["status"], "done");
        // A project's task: asked first.
        let ask = approval("pmi_complete", &json!({ "id": "t2" })).unwrap();
        assert!(ask.what.contains("project Website"), "{ask:?}");
        let log = seen.lock().unwrap().clone();
        let tomorrow = (Local::now().date_naive() + chrono::Duration::days(1)).to_string();
        let post = log.iter().find(|(m, p, _)| m == "POST" && p == "/v1/orgs/o1/tasks").unwrap();
        assert_eq!(post.2, json!({ "container": { "type": "user" }, "title": "Call the vendor", "dueDate": tomorrow }), "due follows the reminder");
        assert!(log.iter().any(|(m, p, b)| m == "PUT" && p == "/v1/orgs/o1/tasks/t1/reminder" && b["at"].is_string()));
        assert!(log.iter().any(|(m, _, b)| m == "PATCH" && b == &json!({ "status": "done", "closingComment": "Done (via lyra)" })));
        *TEST.lock().unwrap() = None;
    }

    #[test]
    fn live_events_read_line_by_line() {
        let mut sse = Sse::default();
        let mut got = Vec::new();
        for line in ["event: ready", "data: {\"type\":\"ready\"}", "", ": keep-alive", "event: ping", "data: {}", "", "data: {\"type\":\"change\",\"areas\":[\"tasks\",\"projects\"]}\r", "\r", "event: resync", "data:", ""] {
            got.extend(sse.line(line));
        }
        assert_eq!(got, vec![Event::Live(true), Event::Changed(vec!["tasks".into(), "projects".into()]), Event::Changed(vec!["resync".into()])]);
    }

    #[test]
    fn the_view_counts_what_needs_the_user() {
        let today = NaiveDate::from_ymd_opt(2026, 10, 7).unwrap();
        let s = State {
            tasks: vec![json!({ "due": "2026-10-01" }), json!({ "due": "2026-10-07" }), json!({ "due": "2026-10-07" }), json!({ "due": null })],
            waiting: json!({ "task_transfers": [1], "project_transfers": [], "approvals": [] }),
            ..Default::default()
        };
        assert_eq!(s.counts(today), (1, 2, 1));
        assert_eq!(s.line(today), "1 overdue · 2 today · 1 waiting");
        assert_eq!(State { tasks: vec![json!({ "due": null })], ..Default::default() }.line(today), "1 open · nothing due");
    }

    #[test]
    fn nags_come_every_so_often_then_stop() {
        let s = Settings { nag_minutes: 30, nag_max: 2, ..Default::default() };
        let fired = Utc::now() - chrono::Duration::minutes(40);
        let state = |read: bool, open: bool| State {
            tasks: if open { vec![json!({ "id": "t1", "title": "Call the vendor" })] } else { vec![] },
            inbox: vec![json!({ "id": "i1", "reason": "reminder", "read": read, "at": fired.to_rfc3339(), "task": { "id": "t1", "title": "Call the vendor" } })],
            ..Default::default()
        };
        let mut nags = Nags::default();
        let now = Utc::now();
        let (due, changed) = nags.due(&state(false, true), &s, now);
        assert!(changed && due.len() == 1 && due[0].title == "Call the vendor", "40 min after it went off");
        assert!(nags.due(&state(false, true), &s, now + chrono::Duration::minutes(10)).0.is_empty(), "not again so soon");
        assert_eq!(nags.due(&state(false, true), &s, now + chrono::Duration::minutes(31)).0.len(), 1);
        assert!(nags.due(&state(false, true), &s, now + chrono::Duration::minutes(90)).0.is_empty(), "at most nag_max");
        // Done (no longer open) or read: forgotten.
        let (due, changed) = nags.due(&state(false, false), &s, now + chrono::Duration::minutes(200));
        assert!(due.is_empty() && changed && nags.items.is_empty());
        let mut nags = Nags::default();
        assert!(nags.due(&state(true, true), &s, now).0.is_empty(), "a read reminder isn't nagged");
        assert!(nags.due(&state(false, true), &s, now + chrono::Duration::days(2)).0.is_empty(), "nor one from days ago");
        assert!(nags.due(&state(false, true), &Settings { nag: false, ..s.clone() }, now).0.is_empty());
    }

    #[test]
    fn personal_needs_no_yes_but_shared_does() {
        // Personal: decided without asking PMI.
        assert!(approval("pmi_add_task", &json!({ "title": "x" })).is_none());
        assert!(approval("pmi_add_task", &json!({ "title": "x", "where": "personal" })).is_none());
        assert!(approval("pmi_remind", &json!({ "id": "1", "when": "tomorrow" })).is_none());
        assert!(approval("pmi_tasks", &json!({})).is_none());
        // Posting and risks always ask.
        let a = approval("pmi_post_update", &json!({ "project": "Website", "health": "at_risk", "progress": "p" })).unwrap();
        assert!(a.what.contains("Website") && a.why.contains("others"), "{a:?}");
        assert!(approval("pmi_add_risk", &json!({ "project": "Website", "kind": "risk", "title": "t" })).is_some());
    }
}

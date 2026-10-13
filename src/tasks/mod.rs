//! lyra's own tasks, reminders and projects, for everyone who uses lyra
//! (members too): `tasks/tasks.db` (`store.rs`). A task is someone's own, or
//! a project's; given to someone else it waits for their Accept / Decline.
//! Each person sets their own reminder on any task they can see; lyra pushes
//! it, with Done / In 1 hour / Tomorrow, and again while it's open (`remind.rs`).
//! Projects have members, health and status updates. The chat's tools and
//! the commands are in `tools.rs`. Everything here works for the person the
//! thread acts for (`acting::current()`); what others see is checked here and
//! asked about by the caller (`tools::approval`).

mod store;
pub mod remind;
pub mod todo;
pub mod tools;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

// ---- settings

/// `[tasks]`: reminders pushed again while a task is still open.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub enabled: bool,
    /// Push a reminder again while the task is still open.
    pub nag: bool,
    pub nag_minutes: u64,
    pub nag_max: u32,
    /// Keep each person's own tasks in step with their Microsoft To Do (both
    /// ways), for those whose connection allows it (`[web.entra] todo`).
    pub todo_sync: bool,
    /// The To Do list they go to (made when it isn't there).
    pub todo_list: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self { enabled: true, nag: true, nag_minutes: 30, nag_max: 3, todo_sync: true, todo_list: "lyra".into() }
    }
}

static SETTINGS: RwLock<Option<Settings>> = RwLock::new(None);

pub fn configure(s: Settings) {
    *SETTINGS.write().unwrap_or_else(|e| e.into_inner()) = Some(s);
}

pub fn settings() -> Settings {
    SETTINGS.read().unwrap_or_else(|e| e.into_inner()).clone().unwrap_or_default()
}

// ---- what's kept

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub done: bool,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Assignee {
    pub user: String,
    /// pending, accepted or declined.
    pub state: String,
    pub by: String,
    pub at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    /// Whose it is (who made it, or who it was made for).
    pub owner: String,
    pub created_by: String,
    pub title: String,
    pub notes: String,
    /// open, in_progress, done or cancelled.
    pub status: String,
    /// low, normal, high or urgent.
    pub priority: String,
    /// YYYY-MM-DD.
    pub due: Option<String>,
    /// daily, weekdays, weekly, biweekly, monthly or yearly.
    pub repeat: Option<String>,
    pub checklist: Vec<Item>,
    pub project_id: Option<String>,
    pub parent_id: Option<String>,
    pub goal_id: Option<String>,
    /// chat, page, mail, meeting, routine …
    pub source: String,
    pub source_ref: Option<String>,
    pub created: String,
    pub updated: String,
    pub completed: Option<String>,
    pub assignees: Vec<Assignee>,
}

impl Task {
    pub fn open(&self) -> bool {
        matches!(self.status.as_str(), "open" | "in_progress")
    }

    /// Who it's been given to and said yes.
    fn accepted(&self, user: &str) -> bool {
        self.assignees.iter().any(|a| a.user == user && a.state == "accepted")
    }

    fn pending(&self, user: &str) -> bool {
        self.assignees.iter().any(|a| a.user == user && a.state == "pending")
    }

    /// Given to someone other than its owner (asked or accepted).
    fn given(&self) -> bool {
        self.assignees.iter().any(|a| a.user != self.owner && a.state != "declined")
    }
}

/// One person's reminder on a task.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Reminder {
    pub task: String,
    pub user: String,
    /// RFC 3339.
    pub at: String,
    /// When it went off (pushed).
    pub fired: Option<String>,
    pub nags: u32,
    pub nag_last: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub task: Option<String>,
    pub project: Option<String>,
    pub at: String,
    pub by: String,
    pub kind: String,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub description: String,
    pub owner: String,
    /// active, on_hold or done.
    pub status: String,
    /// on_track, at_risk or off_track.
    pub health: String,
    pub due: Option<String>,
    pub created: String,
    pub updated: String,
    /// (user, role: owner or member).
    pub members: Vec<(String, String)>,
}

impl Project {
    pub fn member(&self, user: &str) -> bool {
        self.members.iter().any(|(u, _)| u == user)
    }

    fn leads(&self, user: &str) -> bool {
        self.owner == user || self.members.iter().any(|(u, r)| u == user && r == "owner")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Update {
    pub id: String,
    pub project: String,
    pub at: String,
    pub by: String,
    pub health: String,
    pub text: String,
}

// ---- who

fn me() -> String {
    crate::acting::current()
}

fn now() -> String {
    Utc::now().to_rfc3339()
}

fn new_id() -> String {
    lyra_memory::Uuid::new_v4().to_string()
}

fn users() -> Vec<lyra_web::User> {
    crate::config::home().map(|h| lyra_web::Users::open(&h.join("web")).list()).unwrap_or_default()
}

/// An admin (sees every project).
pub fn is_admin(user: &str) -> bool {
    match users().into_iter().find(|u| u.id == user) {
        Some(u) => u.role == lyra_web::users::Role::Admin,
        // No users file yet (the TUI alone): the owner.
        None => user == lyra_web::users::OWNER,
    }
}

/// A person's name, for lists ("you" is left to the caller).
pub fn name_of(user: &str) -> String {
    users().into_iter().find(|u| u.id == user).map(|u| u.name).filter(|n| !n.trim().is_empty()).unwrap_or_else(|| if user == lyra_web::users::OWNER { "the owner".into() } else { user.to_string() })
}

/// Someone who uses lyra, by name, email or id (a first name only one person has will do).
pub fn person(key: &str) -> Result<String, String> {
    let k = key.trim().trim_start_matches('@').to_lowercase();
    if k.is_empty() {
        return Err("whose? (a name)".into());
    }
    if matches!(k.as_str(), "me" | "myself" | "i") {
        return Ok(me());
    }
    let all: Vec<lyra_web::User> = users().into_iter().filter(|u| u.status == lyra_web::Status::Active).collect();
    if let Some(u) = all.iter().find(|u| u.id.to_lowercase() == k || u.email.to_lowercase() == k || u.name.to_lowercase() == k || (!u.username.is_empty() && u.username.to_lowercase() == k)) {
        return Ok(u.id.clone());
    }
    let near: Vec<&lyra_web::User> = all.iter().filter(|u| u.name.to_lowercase().split_whitespace().any(|w| w == k) || u.name.to_lowercase().starts_with(&k)).collect();
    match near.as_slice() {
        [one] => Ok(one.id.clone()),
        [] => Err(format!("nobody called {key} uses lyra")),
        many => Err(format!("{key} could be {}: say which", many.iter().map(|u| u.name.clone()).collect::<Vec<_>>().join(", "))),
    }
}

// ---- changes the loop hears about

static REVISION: AtomicU64 = AtomicU64::new(1);

/// Bumped with every change (the loop sends fresh views when it moves).
pub fn revision() -> u64 {
    REVISION.load(Ordering::Relaxed)
}

fn changed() {
    REVISION.fetch_add(1, Ordering::Relaxed);
}

/// A push for someone, made by a change (a task given to them, an answer).
#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub to: String,
    pub title: String,
    pub body: String,
    pub tag: String,
    /// Push buttons: (action, label).
    pub actions: Vec<(String, String)>,
    pub reference: Option<String>,
}

static NOTICES: Mutex<Vec<Notice>> = Mutex::new(Vec::new());

fn notify(n: Notice) {
    NOTICES.lock().unwrap_or_else(|e| e.into_inner()).push(n);
}

/// The pushes to send now (lyra serve sends them).
pub fn take_notices() -> Vec<Notice> {
    std::mem::take(&mut *NOTICES.lock().unwrap_or_else(|e| e.into_inner()))
}

fn event(task: Option<&Task>, project: Option<&str>, kind: &str, text: &str) {
    let _ = store::add_event(&Event { id: new_id(), task: task.map(|t| t.id.clone()), project: project.map(str::to_string).or_else(|| task.and_then(|t| t.project_id.clone())), at: now(), by: me(), kind: kind.into(), text: text.into() });
}

// ---- reading

/// The projects this person is in (admins: all).
pub fn projects() -> Result<Vec<Project>, String> {
    let user = me();
    store::projects_for(&user, is_admin(&user))
}

fn project_by_id(id: &str) -> Option<Project> {
    let user = me();
    store::projects_for(&user, true).ok()?.into_iter().find(|p| p.id == id)
}

/// A project they're in, by name (or id): exact first, else the one it starts.
pub fn project(name: &str) -> Result<Project, String> {
    let n = name.trim().trim_start_matches("project ").trim().to_lowercase();
    let all = projects()?;
    if let Some(p) = all.iter().find(|p| p.id == name.trim() || p.name.to_lowercase() == n) {
        return Ok(p.clone());
    }
    let near: Vec<&Project> = all.iter().filter(|p| p.name.to_lowercase().contains(&n)).collect();
    match near.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(format!("no project {name} (that you're in)")),
        many => Err(format!("{name} could be {}: say which", many.iter().map(|p| p.name.clone()).collect::<Vec<_>>().join(", "))),
    }
}

/// Can this person see it?
fn sees(t: &Task, user: &str, admin: bool, projects: &[Project]) -> bool {
    t.owner == user
        || t.assignees.iter().any(|a| a.user == user && a.state != "declined")
        || t.project_id.as_ref().is_some_and(|p| admin || projects.iter().any(|pr| pr.id == *p && pr.member(user)))
}

/// Can this person change it (not just answer it)?
fn edits(t: &Task, user: &str, admin: bool, projects: &[Project]) -> bool {
    t.owner == user || t.accepted(user) || t.project_id.as_ref().is_some_and(|p| admin || projects.iter().any(|pr| pr.id == *p && pr.member(user)))
}

/// A task they can see, by id.
pub fn get(id: &str) -> Result<Task, String> {
    let user = me();
    let t = store::tasks_by_id(&[id.trim().to_string()])?.into_iter().next().ok_or_else(|| format!("no task {id}"))?;
    let admin = is_admin(&user);
    if !sees(&t, &user, admin, &store::projects_for(&user, admin).unwrap_or_default()) {
        return Err(format!("no task {id}"));
    }
    Ok(t)
}

fn editable(id: &str) -> Result<Task, String> {
    let t = get(id)?;
    let user = me();
    let admin = is_admin(&user);
    if !edits(&t, &user, admin, &store::projects_for(&user, admin).unwrap_or_default()) {
        return Err(if t.pending(&user) { format!("\"{}\" waits for you to accept it first", t.title) } else { format!("\"{}\" isn't yours to change", t.title) });
    }
    Ok(t)
}

/// The people besides this person who'd see a change to it.
pub fn others(t: &Task) -> Vec<String> {
    let user = me();
    let mut out: Vec<String> = Vec::new();
    let mut add = |u: &str| {
        if u != user && !out.iter().any(|x| x == u) {
            out.push(u.to_string());
        }
    };
    add(&t.owner);
    for a in t.assignees.iter().filter(|a| a.state != "declined") {
        add(&a.user);
    }
    if let Some(p) = t.project_id.as_deref().and_then(project_by_id) {
        for (m, _) in &p.members {
            add(m);
        }
    }
    out
}

/// Which tasks to list.
#[derive(Debug, Clone, PartialEq)]
pub enum Scope {
    /// Their own (not given to someone else) and those given to them and accepted.
    Mine,
    /// Given to them, waiting for their answer.
    Waiting,
    /// They gave to others.
    Given,
    /// Everything they can see.
    All,
    Project(String),
}

impl Scope {
    pub fn parse(s: &str) -> Result<Scope, String> {
        let s = s.trim();
        Ok(match s.to_lowercase().as_str() {
            "" | "mine" | "my" | "personal" | "assigned" => Scope::Mine,
            "waiting" | "to accept" | "requests" => Scope::Waiting,
            "given" | "delegated" | "assigned by me" | "others" => Scope::Given,
            "all" | "everything" => Scope::All,
            _ => Scope::Project(project(s.trim_start_matches("project ").trim())?.id),
        })
    }
}

fn in_scope(t: &Task, user: &str, scope: &Scope) -> bool {
    match scope {
        Scope::Mine => t.accepted(user) || (t.owner == user && !t.given() && (t.project_id.is_none() || t.assignees.is_empty())),
        Scope::Waiting => t.pending(user),
        Scope::Given => t.owner == user && t.assignees.iter().any(|a| a.user != user && a.state != "declined"),
        Scope::All => true,
        Scope::Project(p) => t.project_id.as_deref() == Some(p.as_str()),
    }
}

/// Soonest due first (none last), then the most urgent.
pub fn sort(tasks: &mut [Task]) {
    let rank = |p: &str| match p {
        "urgent" => 0,
        "high" => 1,
        "normal" => 2,
        _ => 3,
    };
    tasks.sort_by_key(|t| (t.due.is_none(), t.due.clone(), rank(&t.priority), t.created.clone()));
}

/// Due within: any, overdue, today (with overdue), week (the next 7 days, with overdue).
pub fn due_in(t: &Task, due: &str, today: NaiveDate) -> bool {
    let d = t.due.as_deref().and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
    match due {
        "overdue" => d.is_some_and(|d| d < today),
        "today" => d.is_some_and(|d| d <= today),
        "week" => d.is_some_and(|d| d <= today + Duration::days(7)),
        _ => true,
    }
}

/// This person's tasks.
pub fn list(scope: &Scope, due: &str, include_done: bool) -> Result<Vec<Task>, String> {
    let user = me();
    let admin = is_admin(&user);
    let ids = match scope {
        Scope::Project(p) => store::project_task_ids(p, include_done)?,
        _ => store::visible_ids(&user, admin, include_done)?,
    };
    let projects = store::projects_for(&user, admin).unwrap_or_default();
    let today = Local::now().date_naive();
    let mut out: Vec<Task> = store::tasks_by_id(&ids)?.into_iter().filter(|t| sees(t, &user, admin, &projects) && in_scope(t, &user, scope) && due_in(t, due, today)).collect();
    sort(&mut out);
    Ok(out)
}

/// Tasks (open and done) whose title or notes have these words.
pub fn search(text: &str) -> Result<Vec<Task>, String> {
    let user = me();
    let ids = store::visible_ids(&user, is_admin(&user), true)?;
    let mut out = store::tasks_by_id(&store::search(&ids, text)?)?;
    sort(&mut out);
    Ok(out)
}

/// Done since `since` (theirs, or given to them).
pub fn done_since(since: DateTime<Utc>) -> Result<Vec<Task>, String> {
    store::tasks_by_id(&store::done_since(&me(), &since.to_rfc3339())?)
}

/// A task's history.
#[cfg(test)]
pub fn history(id: &str) -> Result<Vec<Event>, String> {
    get(id)?;
    store::events_of(id)
}

/// This person's own reminder on a task.
pub fn reminder_of(task: &str) -> Option<Reminder> {
    store::reminders(Some(&me())).ok()?.into_iter().find(|r| r.task == task)
}

// ---- dates and repeats

/// "friday", "tomorrow 3pm", "oct 14", YYYY-MM-DD or RFC 3339.
pub fn when(text: &str) -> Result<crate::when::When, String> {
    let t = text.trim();
    if let Ok(d) = NaiveDate::parse_from_str(t, "%Y-%m-%d") {
        return Ok(crate::when::When { date: d, time: None });
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(t) {
        let l = dt.with_timezone(&Local);
        return Ok(crate::when::When { date: l.date_naive(), time: Some(l.time()) });
    }
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(t, "%Y-%m-%d %H:%M") {
        return Ok(crate::when::When { date: dt.date(), time: Some(dt.time()) });
    }
    crate::when::parse(t, Local::now()).ok_or_else(|| format!("can't read the time {t:?}: try \"tomorrow\", \"friday 3pm\", \"in 2h\", \"oct 14\""))
}

pub const REPEATS: &[&str] = &["daily", "weekdays", "weekly", "biweekly", "monthly", "yearly"];

fn repeat_rule(text: &str) -> Result<Option<String>, String> {
    let t = text.trim().to_lowercase();
    let t = t.trim_start_matches("every ").trim();
    Ok(match t {
        "" | "none" | "never" | "no" => None,
        "day" | "daily" => Some("daily".into()),
        "weekday" | "weekdays" | "workday" | "workdays" => Some("weekdays".into()),
        "week" | "weekly" => Some("weekly".into()),
        "two weeks" | "other week" | "biweekly" | "fortnight" => Some("biweekly".into()),
        "month" | "monthly" => Some("monthly".into()),
        "year" | "yearly" | "annually" => Some("yearly".into()),
        other => return Err(format!("repeat {other}? daily, weekdays, weekly, biweekly, monthly or yearly")),
    })
}

/// The next date after `from` by the rule.
pub fn next_date(from: NaiveDate, rule: &str) -> NaiveDate {
    match rule {
        "daily" => from + Duration::days(1),
        "weekdays" => {
            let mut d = from + Duration::days(1);
            while matches!(d.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun) {
                d += Duration::days(1);
            }
            d
        }
        "weekly" => from + Duration::days(7),
        "biweekly" => from + Duration::days(14),
        "monthly" => from.checked_add_months(chrono::Months::new(1)).unwrap_or(from + Duration::days(30)),
        "yearly" => from.checked_add_months(chrono::Months::new(12)).unwrap_or(from + Duration::days(365)),
        _ => from,
    }
}

fn priority(text: &str) -> Result<String, String> {
    Ok(match text.trim().to_lowercase().as_str() {
        "" | "normal" | "medium" | "none" => "normal".into(),
        "low" => "low".into(),
        "high" | "important" => "high".into(),
        "urgent" | "asap" => "urgent".into(),
        other => return Err(format!("priority {other}? low, normal, high or urgent")),
    })
}

// ---- changing

/// A new task, as asked.
#[derive(Debug, Clone, Default)]
pub struct New {
    pub title: String,
    pub notes: String,
    /// As said ("friday"), or YYYY-MM-DD.
    pub due: String,
    /// This person's reminder, as said ("friday 3pm").
    pub remind: String,
    pub priority: String,
    pub repeat: String,
    /// A project's name, or empty for their own.
    pub project: String,
    pub parent: Option<String>,
    pub goal: Option<String>,
    /// People to give it to (names).
    pub assignees: Vec<String>,
    pub checklist: Vec<String>,
    pub source: String,
    pub source_ref: Option<String>,
}

/// No project: the person's own ("", "none", "personal", "mine" …).
fn own_place(p: &str) -> bool {
    matches!(p.trim().to_lowercase().as_str(), "" | "none" | "no project" | "personal" | "me" | "mine" | "my tasks" | "my list")
}

pub fn add(n: New) -> Result<Task, String> {
    let user = me();
    let title = n.title.trim().to_string();
    if title.is_empty() {
        return Err("a task needs a title".into());
    }
    let project = if own_place(&n.project) { None } else { Some(project(&n.project)?) };
    if let Some(p) = &project
        && !p.member(&user)
        && !is_admin(&user)
    {
        return Err(format!("you're not in project {}", p.name));
    }
    let due = match n.due.trim() {
        "" | "none" => None,
        d => Some(when(d)?.date.to_string()),
    };
    let remind = match n.remind.trim() {
        "" => None,
        r => Some(when(r)?.at().ok_or("that time doesn't exist here")?.with_timezone(&Utc).to_rfc3339()),
    };
    if let Some(parent) = &n.parent {
        get(parent)?;
    }
    let mut assignees = Vec::new();
    for a in &n.assignees {
        let who = person(a)?;
        if who != user && !assignees.contains(&who) {
            assignees.push(who);
        }
    }
    let at = now();
    let t = Task {
        id: new_id(),
        owner: user.clone(),
        created_by: user.clone(),
        title,
        notes: n.notes.trim().to_string(),
        status: "open".into(),
        priority: priority(&n.priority)?,
        due,
        repeat: repeat_rule(&n.repeat)?,
        checklist: n.checklist.iter().filter(|c| !c.trim().is_empty()).map(|c| Item { done: false, text: c.trim().to_string() }).collect(),
        project_id: project.as_ref().map(|p| p.id.clone()),
        parent_id: n.parent.clone(),
        goal_id: n.goal.clone(),
        source: if n.source.is_empty() { "chat".into() } else { n.source.clone() },
        source_ref: n.source_ref.clone(),
        created: at.clone(),
        updated: at.clone(),
        completed: None,
        assignees: Vec::new(),
    };
    store::insert_task(&t)?;
    event(Some(&t), None, "created", "");
    if let Some(r) = &remind {
        store::set_reminder(&t.id, &user, Some(r))?;
    }
    for who in assignees {
        give(&t, &who)?;
    }
    changed();
    get(&t.id)
}

/// Ask someone to take a task (their Accept / Decline).
fn give(t: &Task, who: &str) -> Result<(), String> {
    let user = me();
    store::set_assignee(&t.id, who, "pending", &user, &now())?;
    event(Some(t), None, "assigned", &name_of(who));
    notify(Notice {
        to: who.to_string(),
        title: format!("📋 {} gave you a task", name_of(&user)),
        body: format!("{}{}", t.title, t.due.as_ref().map(|d| format!(" · due {d}")).unwrap_or_default()),
        tag: format!("task-{}", t.id),
        actions: vec![("accept".into(), "Accept".into()), ("decline".into(), "Decline".into())],
        reference: Some(t.id.clone()),
    });
    Ok(())
}

/// What a change sets (`None`: left as it is).
#[derive(Debug, Clone, Default)]
pub struct Change {
    pub title: Option<String>,
    pub notes: Option<String>,
    /// As said; "none" clears it.
    pub due: Option<String>,
    pub priority: Option<String>,
    /// open or in_progress.
    pub status: Option<String>,
    pub repeat: Option<String>,
    /// A project's name; "none" makes it their own again.
    pub project: Option<String>,
    pub checklist: Option<Vec<Item>>,
}

pub fn update(id: &str, c: Change) -> Result<Task, String> {
    let mut t = editable(id)?;
    let mut what = Vec::new();
    if let Some(title) = c.title.filter(|s| !s.trim().is_empty()) {
        t.title = title.trim().to_string();
        what.push("title");
    }
    if let Some(notes) = c.notes {
        t.notes = notes.trim().to_string();
        what.push("notes");
    }
    if let Some(due) = c.due {
        t.due = match due.trim() {
            "" | "none" | "no date" => None,
            d => Some(when(d)?.date.to_string()),
        };
        what.push("due date");
    }
    if let Some(p) = c.priority {
        t.priority = priority(&p)?;
        what.push("priority");
    }
    if let Some(s) = c.status {
        t.status = match s.trim() {
            "open" | "todo" | "to do" => "open".into(),
            "in_progress" | "in progress" | "started" => "in_progress".into(),
            other => return Err(format!("status {other}? open or in_progress (to finish one, complete it)")),
        };
        what.push("status");
    }
    if let Some(r) = c.repeat {
        t.repeat = repeat_rule(&r)?;
        what.push("repeat");
    }
    if let Some(p) = c.project {
        t.project_id = match own_place(&p) {
            true => None,
            false => {
                let pr = project(&p)?;
                if !pr.member(&me()) && !is_admin(&me()) {
                    return Err(format!("you're not in project {}", pr.name));
                }
                Some(pr.id)
            }
        };
        what.push("project");
    }
    if let Some(items) = c.checklist {
        t.checklist = items.into_iter().filter(|i| !i.text.trim().is_empty()).collect();
        what.push("checklist");
    }
    t.updated = now();
    store::save_task(&t)?;
    event(Some(&t), None, "edited", &what.join(", "));
    changed();
    Ok(t)
}

/// Mark it done (a repeating one comes back with its next date). `note`: what
/// they said about it, kept in its history.
pub fn complete(id: &str, note: &str) -> Result<(Task, Option<Task>), String> {
    let mut t = editable(id)?;
    if !t.open() {
        return Ok((t, None));
    }
    let user = me();
    // Each person's reminder on it (read while it's still open: they move to the next one).
    let reminders: Vec<Reminder> = store::reminders(None)?.into_iter().filter(|r| r.task == t.id).collect();
    t.status = "done".into();
    t.completed = Some(now());
    t.updated = now();
    store::save_task(&t)?;
    event(Some(&t), None, "done", note.trim());
    // Who else follows it hears it's done.
    for o in others(&t) {
        notify(Notice { to: o, title: format!("✅ {} finished a task", name_of(&user)), body: t.title.clone(), tag: format!("task-{}", t.id), actions: vec![], reference: Some(t.id.clone()) });
    }
    let next = match t.repeat.clone() {
        Some(rule) => {
            let base = t.due.as_deref().and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()).unwrap_or_else(|| Local::now().date_naive());
            let mut due = next_date(base, &rule);
            // Never a next one that's already past.
            while due < Local::now().date_naive() {
                due = next_date(due, &rule);
            }
            let at = now();
            let n = Task { id: new_id(), due: Some(due.to_string()), status: "open".into(), completed: None, created: at.clone(), updated: at, checklist: t.checklist.iter().map(|i| Item { done: false, text: i.text.clone() }).collect(), assignees: Vec::new(), ..t.clone() };
            store::insert_task(&n)?;
            for a in t.assignees.iter().filter(|a| a.state == "accepted") {
                store::set_assignee(&n.id, &a.user, "accepted", &a.by, &a.at)?;
            }
            // Each person's reminder moves with it (the same time of day, that many days on).
            for r in &reminders {
                if let Ok(old) = DateTime::parse_from_rfc3339(&r.at) {
                    let shift = due - base;
                    store::set_reminder(&n.id, &r.user, Some(&(old.with_timezone(&Utc) + shift).to_rfc3339()))?;
                }
            }
            event(Some(&n), None, "created", &format!("repeats {rule}"));
            Some(n)
        }
        None => None,
    };
    store::set_reminder(&t.id, &user, None)?;
    changed();
    Ok((t, next))
}

pub fn reopen(id: &str) -> Result<Task, String> {
    let mut t = editable(id)?;
    t.status = "open".into();
    t.completed = None;
    t.updated = now();
    store::save_task(&t)?;
    event(Some(&t), None, "reopened", "");
    changed();
    Ok(t)
}

/// Gone from every list (kept, marked, for a later sync). Its owner's, or a
/// project lead's.
pub fn delete(id: &str) -> Result<Task, String> {
    let t = get(id)?;
    let user = me();
    let lead = t.project_id.as_deref().and_then(project_by_id).is_some_and(|p| p.leads(&user));
    if t.owner != user && !lead && !is_admin(&user) {
        return Err(format!("only its owner can delete \"{}\"", t.title));
    }
    store::delete_task(&t.id, &now())?;
    event(Some(&t), None, "deleted", "");
    changed();
    Ok(t)
}

/// Their own reminder on it (`None` clears it). Returns when it is.
pub fn remind(id: &str, at: Option<&str>) -> Result<(Task, Option<DateTime<Local>>), String> {
    let t = get(id)?;
    let when = match at {
        Some(a) => Some(when(a)?.at().ok_or("that time doesn't exist here")?),
        None => None,
    };
    store::set_reminder(&t.id, &me(), when.map(|w| w.with_timezone(&Utc).to_rfc3339()).as_deref())?;
    changed();
    Ok((t, when))
}

pub fn comment(id: &str, text: &str) -> Result<Task, String> {
    let t = get(id)?;
    let user = me();
    if t.pending(&user) {
        return Err("accept it first, or answer with a note when you decline".into());
    }
    if text.trim().is_empty() {
        return Err("say what to comment".into());
    }
    event(Some(&t), None, "comment", text.trim());
    for o in others(&t) {
        notify(Notice { to: o, title: format!("💬 {} on \"{}\"", name_of(&user), t.title), body: text.trim().chars().take(200).collect(), tag: format!("task-{}", t.id), actions: vec![], reference: Some(t.id.clone()) });
    }
    changed();
    Ok(t)
}

/// Give it to someone (they're asked), or take it back from them.
pub fn assign(id: &str, who: &str, remove: bool) -> Result<(Task, String), String> {
    let t = editable(id)?;
    let person = person(who)?;
    if remove {
        store::remove_assignee(&t.id, &person)?;
        event(Some(&t), None, "assigned", &format!("{} taken off", name_of(&person)));
    } else {
        if person == me() {
            return Err("it's yours already: give it to someone else".into());
        }
        if t.assignees.iter().any(|a| a.user == person && a.state != "declined") {
            return Err(format!("{} has it already", name_of(&person)));
        }
        give(&t, &person)?;
    }
    changed();
    Ok((get(&t.id)?, name_of(&person)))
}

/// Their answer to a task given to them.
pub fn answer(id: &str, accept: bool, note: &str) -> Result<Task, String> {
    let user = me();
    let t = store::tasks_by_id(&[id.trim().to_string()])?.into_iter().next().ok_or_else(|| format!("no task {id}"))?;
    let Some(a) = t.assignees.iter().find(|a| a.user == user && a.state == "pending") else {
        return Err(format!("\"{}\" isn't waiting for your answer", t.title));
    };
    let by = a.by.clone();
    store::set_assignee(&t.id, &user, if accept { "accepted" } else { "declined" }, &by, &now())?;
    event(Some(&t), None, if accept { "accepted" } else { "declined" }, note.trim());
    notify(Notice {
        to: by.clone(),
        title: format!("{} {} {}", if accept { "👍" } else { "✋" }, name_of(&user), if accept { "accepted a task" } else { "declined a task" }),
        body: format!("{}{}", t.title, if note.trim().is_empty() { String::new() } else { format!(" — {}", note.trim()) }),
        tag: format!("task-{}", t.id),
        actions: vec![],
        reference: Some(t.id.clone()),
    });
    changed();
    store::tasks_by_id(std::slice::from_ref(&t.id))?.into_iter().next().ok_or_else(|| "it's gone".into())
}

// ---- projects

pub fn add_project(name: &str, description: &str, members: &[String], due: &str) -> Result<Project, String> {
    let user = me();
    let name = name.trim();
    if name.is_empty() {
        return Err("a project needs a name".into());
    }
    if projects()?.iter().any(|p| p.name.eq_ignore_ascii_case(name)) {
        return Err(format!("there's a project called {name} already"));
    }
    let mut m = vec![(user.clone(), "owner".to_string())];
    for who in members {
        let id = person(who)?;
        if !m.iter().any(|(u, _)| *u == id) {
            m.push((id, "member".into()));
        }
    }
    let at = now();
    let p = Project {
        id: new_id(),
        name: name.into(),
        description: description.trim().into(),
        owner: user.clone(),
        status: "active".into(),
        health: "on_track".into(),
        due: match due.trim() {
            "" | "none" => None,
            d => Some(when(d)?.date.to_string()),
        },
        created: at.clone(),
        updated: at,
        members: m.clone(),
    };
    store::insert_project(&p)?;
    event(None, Some(&p.id), "created", name);
    for (who, _) in m.iter().filter(|(u, _)| *u != user) {
        notify(Notice { to: who.clone(), title: format!("📁 {} added you to a project", name_of(&user)), body: p.name.clone(), tag: format!("project-{}", p.id), actions: vec![], reference: None });
    }
    changed();
    Ok(p)
}

/// What a project change sets (`None`: left as it is).
#[derive(Debug, Clone, Default)]
pub struct ProjectChange {
    pub name: Option<String>,
    pub description: Option<String>,
    pub status: Option<String>,
    pub health: Option<String>,
    pub due: Option<String>,
    pub add_members: Vec<String>,
    pub remove_members: Vec<String>,
}

pub fn update_project(name: &str, c: ProjectChange) -> Result<Project, String> {
    let mut p = project(name)?;
    let user = me();
    if !p.member(&user) && !is_admin(&user) {
        return Err(format!("you're not in project {}", p.name));
    }
    if let Some(n) = c.name.filter(|n| !n.trim().is_empty()) {
        p.name = n.trim().into();
    }
    if let Some(d) = c.description {
        p.description = d.trim().into();
    }
    if let Some(s) = c.status {
        p.status = match s.trim() {
            "active" | "on_hold" | "done" => s.trim().into(),
            "on hold" | "paused" => "on_hold".into(),
            "finished" | "closed" | "complete" => "done".into(),
            other => return Err(format!("status {other}? active, on_hold or done")),
        };
    }
    if let Some(h) = c.health {
        p.health = health(&h)?;
    }
    if let Some(d) = c.due {
        p.due = match d.trim() {
            "" | "none" => None,
            d => Some(when(d)?.date.to_string()),
        };
    }
    if !(c.add_members.is_empty() && c.remove_members.is_empty()) && !p.leads(&user) && !is_admin(&user) {
        return Err(format!("only {}'s owner changes who's in it", p.name));
    }
    for who in &c.add_members {
        let id = person(who)?;
        if !p.member(&id) {
            p.members.push((id.clone(), "member".into()));
            event(None, Some(&p.id), "member", &format!("{} added", name_of(&id)));
            notify(Notice { to: id, title: format!("📁 {} added you to a project", name_of(&user)), body: p.name.clone(), tag: format!("project-{}", p.id), actions: vec![], reference: None });
        }
    }
    for who in &c.remove_members {
        let id = person(who)?;
        if id == p.owner {
            return Err("a project keeps its owner".into());
        }
        p.members.retain(|(u, _)| *u != id);
        event(None, Some(&p.id), "member", &format!("{} taken off", name_of(&id)));
    }
    p.updated = now();
    store::save_project(&p)?;
    changed();
    Ok(p)
}

pub fn delete_project(name: &str) -> Result<Project, String> {
    let p = project(name)?;
    let user = me();
    if !p.leads(&user) && !is_admin(&user) {
        return Err(format!("only {}'s owner can delete it", p.name));
    }
    store::delete_project(&p.id, &now())?;
    changed();
    Ok(p)
}

fn health(text: &str) -> Result<String, String> {
    Ok(match text.trim().to_lowercase().replace(' ', "_").as_str() {
        "on_track" | "green" | "good" => "on_track".into(),
        "at_risk" | "amber" | "yellow" => "at_risk".into(),
        "off_track" | "red" | "bad" => "off_track".into(),
        other => return Err(format!("health {other}? on_track, at_risk or off_track")),
    })
}

/// (open, done, overdue) of a project's tasks.
fn counts_of(project: &str) -> (usize, usize, usize) {
    let today = Local::now().date_naive();
    let all = store::project_task_ids(project, true).and_then(|ids| store::tasks_by_id(&ids)).unwrap_or_default();
    let open: Vec<&Task> = all.iter().filter(|t| t.open()).collect();
    (open.len(), all.iter().filter(|t| t.status == "done").count(), open.iter().filter(|t| due_in(t, "overdue", today)).count())
}

/// A status update's draft from a project's tasks (nothing is posted).
pub fn draft_update(name: &str) -> Result<Value, String> {
    let p = project(name)?;
    let today = Local::now().date_naive();
    let ids = store::project_task_ids(&p.id, true)?;
    let all = store::tasks_by_id(&ids)?;
    let week = (Utc::now() - Duration::days(7)).to_rfc3339();
    let done: Vec<String> = all.iter().filter(|t| t.status == "done" && t.completed.as_deref().is_some_and(|c| c >= week.as_str())).map(|t| t.title.clone()).collect();
    let mut open: Vec<Task> = all.into_iter().filter(Task::open).collect();
    sort(&mut open);
    let overdue: Vec<String> = open.iter().filter(|t| due_in(t, "overdue", today)).map(|t| format!("{} (due {})", t.title, t.due.clone().unwrap_or_default())).collect();
    let next: Vec<String> = open.iter().filter(|t| !due_in(t, "overdue", today)).take(5).map(|t| format!("{}{}", t.title, t.due.as_ref().map(|d| format!(" (due {d})")).unwrap_or_default())).collect();
    let suggested = if overdue.len() >= 3 { "off_track" } else if !overdue.is_empty() { "at_risk" } else { "on_track" };
    Ok(json!({
        "project": p.name, "health": suggested,
        "done_this_week": done, "overdue": overdue, "next": next,
        "text": format!(
            "Done this week: {}\nOverdue: {}\nNext: {}",
            if done.is_empty() { "nothing finished".to_string() } else { done.join("; ") },
            if overdue.is_empty() { "none".to_string() } else { overdue.join("; ") },
            if next.is_empty() { "nothing planned".to_string() } else { next.join("; ") }
        ),
    }))
}

/// Post a status update (it sets the project's health; members are told).
pub fn post_update(name: &str, health_text: &str, text: &str) -> Result<Update, String> {
    let mut p = project(name)?;
    let user = me();
    if !p.member(&user) && !is_admin(&user) {
        return Err(format!("you're not in project {}", p.name));
    }
    if text.trim().is_empty() {
        return Err("say what the update is".into());
    }
    let u = Update { id: new_id(), project: p.id.clone(), at: now(), by: user.clone(), health: health(health_text)?, text: text.trim().into() };
    store::add_update(&u)?;
    p.health = u.health.clone();
    p.updated = now();
    store::save_project(&p)?;
    for (m, _) in p.members.iter().filter(|(m, _)| *m != user) {
        notify(Notice { to: m.clone(), title: format!("📁 {}: status update", p.name), body: u.text.chars().take(200).collect(), tag: format!("project-{}", p.id), actions: vec![], reference: None });
    }
    changed();
    Ok(u)
}

pub fn updates(project_id: &str, limit: i64) -> Vec<Update> {
    store::updates_of(project_id, limit).unwrap_or_default()
}

// ---- as the model and the app see them

/// A task in a few fields, for this person.
pub fn brief(t: &Task) -> Value {
    let user = me();
    let place = match t.project_id.as_deref().and_then(project_by_id) {
        Some(p) => format!("project {}", p.name),
        None if t.owner == user => "personal".into(),
        None => format!("{}'s", name_of(&t.owner)),
    };
    let mut v = json!({ "id": t.id, "title": t.title, "status": t.status, "where": place, "due": t.due, "priority": t.priority });
    if t.owner != user {
        v["owner"] = json!(name_of(&t.owner));
    }
    let given: Vec<Value> = t.assignees.iter().map(|a| json!({ "name": name_of(&a.user), "state": a.state })).collect();
    if !given.is_empty() {
        v["assignees"] = json!(given);
    }
    if t.pending(&user) {
        v["waiting_for_you"] = json!(true);
    }
    if !t.checklist.is_empty() {
        v["checklist"] = json!(format!("{}/{}", t.checklist.iter().filter(|i| i.done).count(), t.checklist.len()));
    }
    if let Some(r) = &t.repeat {
        v["repeat"] = json!(r);
    }
    if let Some(p) = &t.parent_id {
        v["subtask_of"] = json!(p);
    }
    if let Some(c) = &t.completed {
        v["completed"] = json!(c);
    }
    v
}

/// One task in full: notes, checklist, history, their reminder.
pub fn full(t: &Task) -> Value {
    let mut v = brief(t);
    v["notes"] = json!(t.notes);
    v["items"] = json!(t.checklist);
    v["created"] = json!(t.created);
    v["project_id"] = json!(t.project_id);
    v["mine"] = json!(t.owner == me());
    v["reminder"] = json!(reminder_of(&t.id).map(|r| r.at));
    v["history"] = json!(store::events_of(&t.id).unwrap_or_default().iter().rev().take(20).map(|e| json!({ "at": e.at, "by": name_of(&e.by), "kind": e.kind, "text": e.text })).collect::<Vec<_>>());
    v
}

pub fn project_brief(p: &Project) -> Value {
    let (open, done, overdue) = counts_of(&p.id);
    let last = updates(&p.id, 1).into_iter().next();
    json!({
        "id": p.id, "name": p.name, "description": p.description, "status": p.status, "health": p.health, "due": p.due,
        "owner": name_of(&p.owner), "mine": p.leads(&me()),
        "members": p.members.iter().map(|(u, r)| json!({ "id": u, "name": name_of(u), "role": r })).collect::<Vec<_>>(),
        "tasks": { "open": open, "done": done, "overdue": overdue, "total": open + done },
        "last_update": last.map(|u| json!({ "at": u.at, "by": name_of(&u.by), "health": u.health, "text": u.text })),
    })
}

/// One person's tasks at a glance, for the Tasks page, the briefing and the panels.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub user: String,
    /// Open tasks that are theirs, soonest due first.
    pub tasks: Vec<Value>,
    /// Given to them, waiting for Accept / Decline.
    pub waiting: Vec<Value>,
    /// They gave to others, still open.
    pub given: Vec<Value>,
    /// Their projects, with health.
    pub projects: Vec<Value>,
    /// What others did on their tasks lately.
    pub inbox: Vec<Value>,
    pub inbox_unread: u64,
}

impl State {
    /// (overdue, due today, waiting on them).
    pub fn counts(&self, today: NaiveDate) -> (usize, usize, usize) {
        let due = |t: &&Value| t["due"].as_str().and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
        let overdue = self.tasks.iter().filter(|t| due(t).is_some_and(|d| d < today)).count();
        let today_n = self.tasks.iter().filter(|t| due(t) == Some(today)).count();
        (overdue, today_n, self.waiting.len())
    }

    /// "2 overdue · 3 today · 1 waiting" (or "nothing due").
    pub fn line(&self, today: NaiveDate) -> String {
        if let Some(e) = &self.error {
            return format!("⚠ {e}");
        }
        let (o, t, w) = self.counts(today);
        let parts: Vec<String> = [(o, "overdue"), (t, "today"), (w, "to accept")].iter().filter(|(n, _)| *n > 0).map(|(n, w)| format!("{n} {w}")).collect();
        if parts.is_empty() { format!("{} open · nothing due", self.tasks.len()) } else { parts.join(" · ") }
    }
}

/// Read everything the view needs.
pub fn snapshot() -> Result<State, String> {
    let user = me();
    let mine = list(&Scope::Mine, "any", false)?;
    let waiting = list(&Scope::Waiting, "any", false)?;
    let given = list(&Scope::Given, "any", false)?;
    let projects: Vec<Value> = projects()?.iter().filter(|p| p.status != "done").map(project_brief).collect();
    let (inbox, unread) = inbox(false)?;
    Ok(State {
        at: Some(Utc::now()),
        error: None,
        user: name_of(&user),
        tasks: mine.iter().map(brief).collect(),
        waiting: waiting.iter().map(brief).collect(),
        given: given.iter().map(brief).collect(),
        projects,
        inbox,
        inbox_unread: unread,
    })
}

/// What others did on their tasks in the last two weeks (newest first), and
/// how many since they last looked. `mark_read`: looked at now.
pub fn inbox(mark_read: bool) -> Result<(Vec<Value>, u64), String> {
    let user = me();
    let ids = store::visible_ids(&user, false, true)?;
    let since = (Utc::now() - Duration::days(14)).to_rfc3339();
    let seen = store::inbox_seen(&user)?.unwrap_or_default();
    let events = store::events_by_others(&ids, &user, &since)?;
    let titles: std::collections::HashMap<String, String> = store::tasks_by_id(&events.iter().filter_map(|e| e.task.clone()).collect::<Vec<_>>())?.into_iter().map(|t| (t.id, t.title)).collect();
    let unread = events.iter().filter(|e| e.at > seen).count() as u64;
    if mark_read {
        store::set_inbox_seen(&user, &now())?;
    }
    Ok((
        events
            .iter()
            .take(20)
            .map(|e| {
                json!({
                    "id": e.id, "at": e.at, "by": name_of(&e.by), "what": e.kind, "text": e.text, "unread": e.at > seen,
                    "task": e.task.as_ref().map(|t| json!({ "id": t, "title": titles.get(t).cloned().unwrap_or_default() })),
                })
            })
            .collect(),
        unread,
    ))
}

/// Run `f` as this person (their tasks).
pub fn as_user<T>(user: &str, f: impl FnOnce() -> T) -> T {
    crate::acting::run(user, f)
}

#[cfg(test)]
mod tests;

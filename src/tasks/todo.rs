//! Each person's own tasks kept in step with a list in their Microsoft To Do,
//! both ways (`[tasks] todo_sync`, the connection's Tasks.ReadWrite). Only
//! their own tasks (not a project's) go to To Do; tasks made there come into
//! lyra. What changed on To Do's side comes from Graph's delta query; lyra's
//! side from each task's `updated` against when its link was last synced
//! (`task_links`). Both changed: the newer wins, and the other is kept in the
//! task's history. Reminders aren't synced (both would remind).

use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Event, Task, store};

const SERVICE: &str = "todo";

/// A task as To Do has it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Remote {
    pub id: String,
    pub title: String,
    pub notes: String,
    /// YYYY-MM-DD.
    pub due: Option<String>,
    /// completed, inProgress, notStarted …
    pub status: String,
    /// low, normal, high.
    pub importance: String,
    /// lastModifiedDateTime: what changed there, and when.
    pub modified: String,
}

/// What a sync needs of To Do (Graph, or a stand-in in tests).
pub trait Todo {
    /// The lists: (id, name).
    fn lists(&self) -> Result<Vec<(String, String)>, String>;
    fn create_list(&self, name: &str) -> Result<String, String>;
    /// What changed in a list since `delta` (all of it without one): changed
    /// tasks, removed ids, and the link to ask next time.
    fn changes(&self, list: &str, delta: Option<&str>) -> Result<(Vec<Remote>, Vec<String>, String), String>;
    fn create(&self, list: &str, fields: &Value) -> Result<Remote, String>;
    fn update(&self, list: &str, id: &str, fields: &Value) -> Result<Remote, String>;
    fn delete(&self, list: &str, id: &str) -> Result<(), String>;
}

/// One person's sync, kept between runs (`tasks/todo/<user>.json`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub list_id: String,
    pub list_name: String,
    pub delta: Option<String>,
    pub last: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

fn state_path(user: &str) -> Option<std::path::PathBuf> {
    let safe: String = user.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    Some(crate::config::home()?.join("tasks").join("todo").join(format!("{safe}.json")))
}

pub fn state_of(user: &str) -> State {
    state_path(user).map(|p| crate::store::read_json(&p)).unwrap_or_default()
}

fn save_state(user: &str, s: &State) {
    if let Some(p) = state_path(user) {
        let _ = crate::store::write_json(&p, s);
    }
}

/// What one sync did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    /// Made or changed in lyra from To Do.
    pub pulled: usize,
    /// Made or changed in To Do from lyra.
    pub pushed: usize,
    /// Deleted on one side because it was deleted on the other.
    pub removed: usize,
    /// Changed on both sides (the newer kept).
    pub conflicts: usize,
}

impl Report {
    pub fn any(&self) -> bool {
        self.pulled + self.pushed + self.removed + self.conflicts > 0
    }

    pub fn text(&self) -> String {
        let mut parts = Vec::new();
        for (n, what) in [(self.pulled, "from To Do"), (self.pushed, "to To Do"), (self.removed, "deleted on the other side"), (self.conflicts, "changed on both (the newer kept)")] {
            if n > 0 {
                parts.push(format!("{n} {what}"));
            }
        }
        parts.join(", ")
    }
}

fn status_of(r: &Remote) -> &'static str {
    match r.status.as_str() {
        "completed" => "done",
        "inProgress" => "in_progress",
        _ => "open",
    }
}

fn priority_of(r: &Remote, local: &str) -> String {
    match r.importance.as_str() {
        "high" if local == "urgent" => "urgent".into(),
        "high" => "high".into(),
        "low" => "low".into(),
        _ => "normal".into(),
    }
}

/// A lyra task as To Do's fields.
pub fn fields(t: &Task) -> Value {
    json!({
        "title": t.title,
        "body": { "content": t.notes, "contentType": "text" },
        "importance": match t.priority.as_str() { "urgent" | "high" => "high", "low" => "low", _ => "normal" },
        "status": match t.status.as_str() { "done" | "cancelled" => "completed", "in_progress" => "inProgress", _ => "notStarted" },
        "dueDateTime": t.due.as_ref().map(|d| json!({ "dateTime": format!("{d}T00:00:00.0000000"), "timeZone": "UTC" })),
    })
}

/// To Do's version applied to a lyra task (its title, notes, date, state and priority).
fn apply(t: &mut Task, r: &Remote, at: &str) {
    t.title = if r.title.trim().is_empty() { t.title.clone() } else { r.title.trim().to_string() };
    t.notes = r.notes.trim().to_string();
    t.due = r.due.clone();
    let status = status_of(r);
    if status == "done" && t.status != "done" {
        t.completed = Some(at.to_string());
    } else if status != "done" {
        t.completed = None;
    }
    t.status = status.to_string();
    t.priority = priority_of(r, &t.priority);
    t.updated = at.to_string();
}

/// What a task said before a change, for its history.
fn said(t: &Task) -> String {
    format!("{} · {}{}", t.title, t.status.replace('_', " "), t.due.as_ref().map(|d| format!(" · due {d}")).unwrap_or_default())
}

fn log(task: &Task, by: &str, kind: &str, text: &str, at: &str) {
    let _ = store::add_event(&Event { id: lyra_memory::Uuid::new_v4().to_string(), task: Some(task.id.clone()), project: None, at: at.to_string(), by: by.to_string(), kind: kind.into(), text: text.into() });
}

fn time(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).ok().map(|t| t.with_timezone(&Utc))
}

/// One sync for `user` with the list `list_name` (made when it's not there).
pub fn sync(user: &str, api: &dyn Todo, list_name: &str) -> Result<Report, String> {
    let mut st = state_of(user);
    let mut report = Report::default();
    // The list: the one named, made when there's none; another list starts the links over.
    if st.list_id.is_empty() || st.list_name != list_name {
        let id = match api.lists()?.into_iter().find(|(_, n)| n.eq_ignore_ascii_case(list_name)) {
            Some((id, _)) => id,
            None => api.create_list(list_name)?,
        };
        if !st.list_id.is_empty() && st.list_id != id {
            for (t, _) in store::own_for_sync(user)? {
                store::remove_link(&t.id, SERVICE)?;
            }
        }
        st = State { list_id: id, list_name: list_name.to_string(), ..Default::default() };
    }
    let list = st.list_id.clone();
    let now = || Utc::now().to_rfc3339();

    // To Do's side: what changed there.
    let (changed, removed, delta) = match api.changes(&list, st.delta.as_deref()) {
        Ok(c) => c,
        // A delta link that's too old: everything again.
        Err(_) if st.delta.is_some() => api.changes(&list, None)?,
        Err(e) => return Err(e),
    };
    for id in removed {
        let Some(task) = store::linked(SERVICE, &id)? else { continue };
        let Some(t) = store::tasks_by_id(std::slice::from_ref(&task))?.into_iter().next() else {
            store::remove_link(&task, SERVICE)?;
            continue;
        };
        let synced = store::link_of(&task, SERVICE)?.and_then(|l| l.2).unwrap_or_default();
        store::remove_link(&task, SERVICE)?;
        if t.updated <= synced {
            // Deleted in To Do, unchanged here: gone here too.
            store::delete_task(&task, &now())?;
            log(&t, user, "deleted", "deleted in Microsoft To Do", &now());
            report.removed += 1;
        } else {
            // Changed here since: it goes back to To Do (below).
            report.conflicts += 1;
            log(&t, user, "edited", "deleted in Microsoft To Do, but changed in lyra since: kept, and put back there", &now());
        }
    }
    for r in changed {
        match store::linked(SERVICE, &r.id)? {
            None => {
                // Made in To Do: a task of theirs here.
                let at = now();
                let mut t = Task { id: lyra_memory::Uuid::new_v4().to_string(), owner: user.into(), created_by: user.into(), priority: "normal".into(), source: "todo".into(), source_ref: Some(r.id.clone()), created: at.clone(), ..Default::default() };
                apply(&mut t, &r, &at);
                store::insert_task(&t)?;
                log(&t, user, "created", "from Microsoft To Do", &at);
                store::set_link(&t.id, SERVICE, &r.id, Some(&r.modified), &at)?;
                report.pulled += 1;
            }
            Some(task) => {
                let link = store::link_of(&task, SERVICE)?;
                // The change lyra made itself (its own update coming back): nothing new.
                if link.as_ref().and_then(|l| l.1.as_deref()) == Some(r.modified.as_str()) {
                    continue;
                }
                let Some(mut t) = store::tasks_by_id(std::slice::from_ref(&task))?.into_iter().next() else { continue };
                let synced = link.and_then(|l| l.2).unwrap_or_default();
                let changed_here = t.updated > synced;
                let theirs_newer = match (time(&r.modified), time(&t.updated)) {
                    (Some(a), Some(b)) => a >= b,
                    _ => true,
                };
                if changed_here && !theirs_newer {
                    // Both changed, lyra's is newer: kept, and goes to To Do below.
                    report.conflicts += 1;
                    log(&t, user, "edited", &format!("changed in both: lyra's version kept; To Do's was: {}", said(&Task { title: r.title.clone(), status: status_of(&r).into(), due: r.due.clone(), ..Default::default() })), &now());
                    continue;
                }
                let before = said(&t);
                let at = now();
                apply(&mut t, &r, &at);
                store::save_task(&t)?;
                log(&t, user, "edited", &if changed_here { format!("changed in both: To Do's version kept; lyra's was: {before}") } else { "changed in Microsoft To Do".to_string() }, &at);
                store::set_link(&t.id, SERVICE, &r.id, Some(&r.modified), &at)?;
                if changed_here {
                    report.conflicts += 1;
                } else {
                    report.pulled += 1;
                }
            }
        }
    }
    st.delta = Some(delta);

    // lyra's side: what changed here since each was last synced.
    let recent = (Utc::now() - Duration::days(14)).to_rfc3339();
    for (t, deleted) in store::own_for_sync(user)? {
        let link = store::link_of(&t.id, SERVICE)?;
        if deleted.is_some() {
            if let Some((id, _, _)) = link {
                let _ = api.delete(&list, &id);
                store::remove_link(&t.id, SERVICE)?;
                report.removed += 1;
            }
            continue;
        }
        match link {
            None => {
                // Not there yet: what's open, and what was done lately (not years of finished ones).
                if !t.open() && t.completed.as_deref().is_none_or(|c| c < recent.as_str()) {
                    continue;
                }
                let r = api.create(&list, &fields(&t))?;
                store::set_link(&t.id, SERVICE, &r.id, Some(&r.modified), &now())?;
                report.pushed += 1;
            }
            Some((id, _, synced)) if t.updated.as_str() > synced.as_deref().unwrap_or_default() => {
                let r = match api.update(&list, &id, &fields(&t)) {
                    Ok(r) => r,
                    // Gone from To Do meanwhile: made there again.
                    Err(e) if e.contains("no such item") || e.contains("404") => api.create(&list, &fields(&t))?,
                    Err(e) => return Err(e),
                };
                store::set_link(&t.id, SERVICE, &r.id, Some(&r.modified), &now())?;
                report.pushed += 1;
            }
            Some(_) => {}
        }
    }
    st.last = Some(Utc::now());
    st.error = None;
    save_state(user, &st);
    if report.any() {
        super::changed();
    }
    Ok(report)
}

/// A sync that failed: kept with the person's state (the Tasks page says so).
pub fn failed(user: &str, e: &str) {
    let mut st = state_of(user);
    st.error = Some(e.to_string());
    save_state(user, &st);
}

/// May this person's tasks sync now? (on, connected, and To Do allowed)
pub fn ready(user: &str) -> bool {
    super::settings().todo_sync && crate::graph::connected_for(user) && crate::graph::has(user, lyra_web::oidc::TODO)
}

/// What the Tasks page says about the sync.
pub fn view(user: &str) -> Value {
    let st = state_of(user);
    let wanted = super::settings().todo_sync && crate::graph::todo_enabled();
    json!({
        "on": ready(user),
        "wanted": wanted,
        "connected": crate::graph::connected_for(user),
        "list": super::settings().todo_list,
        "last": st.last,
        "error": st.error,
    })
}

// ---- To Do through Microsoft Graph, as the person the thread works for

pub struct Graph;

fn remote(v: &Value) -> Remote {
    let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
    Remote {
        id: s("id"),
        title: s("title"),
        notes: v["body"]["content"].as_str().unwrap_or("").to_string(),
        due: v["dueDateTime"]["dateTime"].as_str().and_then(|d| d.get(..10)).filter(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").is_ok()).map(str::to_string),
        status: s("status"),
        importance: s("importance"),
        modified: s("lastModifiedDateTime"),
    }
}

impl Todo for Graph {
    fn lists(&self) -> Result<Vec<(String, String)>, String> {
        let v = crate::graph::graph(reqwest::Method::GET, "/me/todo/lists", None)?;
        Ok(v["value"].as_array().into_iter().flatten().map(|l| (l["id"].as_str().unwrap_or("").to_string(), l["displayName"].as_str().unwrap_or("").to_string())).collect())
    }

    fn create_list(&self, name: &str) -> Result<String, String> {
        let v = crate::graph::graph(reqwest::Method::POST, "/me/todo/lists", Some(&json!({ "displayName": name })))?;
        v["id"].as_str().map(str::to_string).ok_or_else(|| "To Do didn't make the list".into())
    }

    fn changes(&self, list: &str, delta: Option<&str>) -> Result<(Vec<Remote>, Vec<String>, String), String> {
        let mut next = delta.map(str::to_string).unwrap_or_else(|| format!("/me/todo/lists/{}/tasks/delta", lyra_web::oidc::encode(list)));
        let (mut changed, mut removed) = (Vec::new(), Vec::new());
        for _ in 0..50 {
            let v = crate::graph::graph(reqwest::Method::GET, &next, None)?;
            for t in v["value"].as_array().into_iter().flatten() {
                if t.get("@removed").is_some() {
                    removed.push(t["id"].as_str().unwrap_or("").to_string());
                } else {
                    changed.push(remote(t));
                }
            }
            if let Some(n) = v["@odata.nextLink"].as_str() {
                next = n.to_string();
            } else if let Some(d) = v["@odata.deltaLink"].as_str() {
                return Ok((changed, removed, d.to_string()));
            } else {
                break;
            }
        }
        Err("To Do's changes didn't finish".into())
    }

    fn create(&self, list: &str, fields: &Value) -> Result<Remote, String> {
        Ok(remote(&crate::graph::graph(reqwest::Method::POST, &format!("/me/todo/lists/{}/tasks", lyra_web::oidc::encode(list)), Some(fields))?))
    }

    fn update(&self, list: &str, id: &str, fields: &Value) -> Result<Remote, String> {
        Ok(remote(&crate::graph::graph(reqwest::Method::PATCH, &format!("/me/todo/lists/{}/tasks/{}", lyra_web::oidc::encode(list), lyra_web::oidc::encode(id)), Some(fields))?))
    }

    fn delete(&self, list: &str, id: &str) -> Result<(), String> {
        crate::graph::graph(reqwest::Method::DELETE, &format!("/me/todo/lists/{}/tasks/{}", lyra_web::oidc::encode(list), lyra_web::oidc::encode(id)), None).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// To Do, in memory: one list, tasks changed "there" by the test.
    #[derive(Default)]
    struct Fake {
        lists: RefCell<Vec<(String, String)>>,
        tasks: RefCell<Vec<Remote>>,
        /// Changed or removed since the last look: (id, removed).
        since: RefCell<Vec<(String, bool)>>,
        clock: RefCell<i64>,
    }

    impl Fake {
        fn stamp(&self) -> String {
            *self.clock.borrow_mut() += 1;
            (Utc::now() + Duration::seconds(*self.clock.borrow())).to_rfc3339()
        }

        /// Someone changes it in To Do.
        fn edit(&self, id: &str, change: impl FnOnce(&mut Remote)) {
            let m = self.stamp();
            let mut all = self.tasks.borrow_mut();
            let r = all.iter_mut().find(|r| r.id == id).unwrap();
            change(r);
            r.modified = m;
            self.since.borrow_mut().push((id.into(), false));
        }

        fn add(&self, title: &str) -> String {
            let id = format!("r{}", self.tasks.borrow().len() + 1);
            let r = Remote { id: id.clone(), title: title.into(), status: "notStarted".into(), importance: "normal".into(), modified: self.stamp(), ..Default::default() };
            self.tasks.borrow_mut().push(r);
            self.since.borrow_mut().push((id.clone(), false));
            id
        }

        fn remove(&self, id: &str) {
            self.tasks.borrow_mut().retain(|r| r.id != id);
            self.since.borrow_mut().push((id.into(), true));
        }

        fn get(&self, id: &str) -> Option<Remote> {
            self.tasks.borrow().iter().find(|r| r.id == id).cloned()
        }

        fn put(&self, id: &str, f: &Value) -> Remote {
            let r = Remote {
                id: id.into(),
                title: f["title"].as_str().unwrap_or("").into(),
                notes: f["body"]["content"].as_str().unwrap_or("").into(),
                due: f["dueDateTime"]["dateTime"].as_str().map(|d| d[..10].to_string()),
                status: f["status"].as_str().unwrap_or("").into(),
                importance: f["importance"].as_str().unwrap_or("").into(),
                modified: self.stamp(),
            };
            let mut all = self.tasks.borrow_mut();
            all.retain(|x| x.id != id);
            all.push(r.clone());
            // lyra's own change comes back in the next delta too (as Graph does).
            self.since.borrow_mut().push((id.into(), false));
            r
        }
    }

    impl Todo for Fake {
        fn lists(&self) -> Result<Vec<(String, String)>, String> {
            Ok(self.lists.borrow().clone())
        }
        fn create_list(&self, name: &str) -> Result<String, String> {
            self.lists.borrow_mut().push(("L1".into(), name.into()));
            Ok("L1".into())
        }
        fn changes(&self, _list: &str, delta: Option<&str>) -> Result<(Vec<Remote>, Vec<String>, String), String> {
            let since: Vec<(String, bool)> = if delta.is_none() { self.tasks.borrow().iter().map(|r| (r.id.clone(), false)).collect() } else { std::mem::take(&mut *self.since.borrow_mut()) };
            self.since.borrow_mut().clear();
            let changed = since.iter().filter(|(_, gone)| !gone).filter_map(|(id, _)| self.get(id)).collect();
            let removed = since.iter().filter(|(_, gone)| *gone).map(|(id, _)| id.clone()).collect();
            Ok((changed, removed, "delta".into()))
        }
        fn create(&self, _list: &str, f: &Value) -> Result<Remote, String> {
            let id = format!("r{}", self.tasks.borrow().len() + 100);
            Ok(self.put(&id, f))
        }
        fn update(&self, _list: &str, id: &str, f: &Value) -> Result<Remote, String> {
            if self.get(id).is_none() {
                return Err("Microsoft 365 has no such item".into());
            }
            Ok(self.put(id, f))
        }
        fn delete(&self, _list: &str, id: &str) -> Result<(), String> {
            self.tasks.borrow_mut().retain(|r| r.id != id);
            Ok(())
        }
    }

    const ME: &str = lyra_web::users::OWNER;

    fn mine() -> Vec<Task> {
        crate::acting::run(ME, || super::super::list(&super::super::Scope::Mine, "any", true)).unwrap()
    }

    #[test]
    fn tasks_go_both_ways_and_a_change_on_both_keeps_the_newer() {
        crate::config::with_test_home(|_| {
            let todo = Fake::default();
            // A lyra task goes to To Do (a list made for it); one made in To Do comes here.
            let t = crate::acting::run(ME, || super::super::add(super::super::New { title: "Renew the cert".into(), due: "2026-11-01".into(), priority: "high".into(), ..Default::default() })).unwrap();
            todo.add("Buy stamps");
            let r = sync(ME, &todo, "lyra").unwrap();
            assert_eq!((r.pushed, r.pulled), (1, 1), "{r:?}");
            assert_eq!(todo.lists.borrow()[0].1, "lyra");
            let there = todo.tasks.borrow().iter().find(|x| x.title == "Renew the cert").cloned().unwrap();
            assert_eq!((there.due.as_deref(), there.importance.as_str()), (Some("2026-11-01"), "high"));
            assert!(mine().iter().any(|x| x.title == "Buy stamps" && x.source == "todo"));
            // Nothing changed: nothing to do (lyra's own changes coming back aren't news).
            assert!(!sync(ME, &todo, "lyra").unwrap().any());
            // Done in To Do: done here.
            todo.edit(&there.id, |x| x.status = "completed".into());
            assert_eq!(sync(ME, &todo, "lyra").unwrap().pulled, 1);
            assert_eq!(mine().iter().find(|x| x.id == t.id).unwrap().status, "done");
            // Changed here: changed there.
            crate::acting::run(ME, || super::super::reopen(&t.id)).unwrap();
            crate::acting::run(ME, || super::super::update(&t.id, super::super::Change { title: Some("Renew the TLS cert".into()), ..Default::default() })).unwrap();
            assert_eq!(sync(ME, &todo, "lyra").unwrap().pushed, 1);
            assert_eq!(todo.get(&there.id).unwrap().title, "Renew the TLS cert");
            // Both changed: To Do's is newer here, so it's kept; lyra's is in the history.
            crate::acting::run(ME, || super::super::update(&t.id, super::super::Change { title: Some("lyra's title".into()), ..Default::default() })).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(5));
            todo.edit(&there.id, |x| x.title = "To Do's title".into());
            let r = sync(ME, &todo, "lyra").unwrap();
            assert_eq!(r.conflicts, 1, "{r:?}");
            assert_eq!(mine().iter().find(|x| x.id == t.id).unwrap().title, "To Do's title");
            let history = crate::acting::run(ME, || super::super::history(&t.id)).unwrap();
            assert!(history.iter().any(|e| e.text.contains("lyra's was: lyra's title")), "{history:?}");
            // Deleted in To Do (unchanged here since): deleted here.
            let stamps = mine().into_iter().find(|x| x.title == "Buy stamps").unwrap();
            let stamps_there = todo.tasks.borrow().iter().find(|x| x.title == "Buy stamps").unwrap().id.clone();
            todo.remove(&stamps_there);
            assert_eq!(sync(ME, &todo, "lyra").unwrap().removed, 1);
            assert!(!mine().iter().any(|x| x.id == stamps.id));
            // Deleted here: deleted there.
            crate::acting::run(ME, || super::super::delete(&t.id)).unwrap();
            assert_eq!(sync(ME, &todo, "lyra").unwrap().removed, 1);
            assert!(todo.get(&there.id).is_none());
            // A project's task stays in lyra.
            crate::acting::run(ME, || super::super::add_project("Website", "", &[], "")).unwrap();
            crate::acting::run(ME, || super::super::add(super::super::New { title: "Project work".into(), project: "Website".into(), ..Default::default() })).unwrap();
            sync(ME, &todo, "lyra").unwrap();
            assert!(!todo.tasks.borrow().iter().any(|x| x.title == "Project work"));
        });
    }
}

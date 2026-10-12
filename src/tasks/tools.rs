//! The tasks tools for the chat and agents, their approvals, and the
//! `/tasks`, `/task` and `/project` commands. A change to the person's own
//! task runs at once; anything someone else sees (a shared task, a project
//! with other members, giving a task to someone, a comment, a status update)
//! waits for their Allow. The old `pmi_*` names still work (`alias`).

use std::sync::Mutex;

use chrono::Local;
use lyra_capabilities::{Capability, CapabilityKind, RiskLevel};
use serde_json::{Value, json};

use super::*;
use crate::caps::Ask;

const TAGS: &[&str] = &["task", "tasks", "todo", "to-do", "reminder", "remind", "project", "projects", "deadline", "due", "assign"];

/// The PMI tools' names, as the tasks tools they became (for saved
/// workflows, skills and habits from before).
pub fn alias(name: &str) -> Option<&'static str> {
    Some(match name {
        "pmi_tasks" => "task_list",
        "pmi_task" => "task_get",
        "pmi_search" => "task_search",
        "pmi_add_task" => "task_add",
        "pmi_update_task" => "task_update",
        "pmi_complete" => "task_complete",
        "pmi_remind" => "task_remind",
        "pmi_comment" => "task_comment",
        "pmi_inbox" => "task_inbox",
        "pmi_waiting" => "task_waiting",
        "pmi_projects" => "task_projects",
        "pmi_project" => "task_project",
        "pmi_draft_update" => "task_draft_update",
        "pmi_post_update" => "task_post_update",
        _ => return None,
    })
}

pub fn capabilities() -> Vec<Capability> {
    let tool = |name: &str, description: &str, risk: RiskLevel, properties: Value, required: &[&str]| {
        let mut c = Capability::new(name, CapabilityKind::NativeTool, description, risk);
        c.input_schema = json!({ "type": "object", "properties": properties, "required": required });
        c.source = "tasks".into();
        c.tags = TAGS.iter().map(|t| t.to_string()).collect();
        c.permissions = vec![if risk == RiskLevel::ReadOnly { "tasks.read" } else { "tasks.write" }.into()];
        c
    };
    let id = json!({ "type": "string", "description": "The task's id (from task_list or task_search)." });
    let project = json!({ "type": "string", "description": "The project's name." });
    let priority = json!({ "type": "string", "enum": ["low", "normal", "high", "urgent"] });
    vec![
        tool(
            "task_list",
            "The user's tasks in lyra: their own and those given to them that they accepted (mine), ones waiting for their Accept (waiting), ones they gave others (given), a project's, or all they can see. Open ones unless include_done.",
            RiskLevel::ReadOnly,
            json!({
                "scope": { "type": "string", "description": "mine (default), waiting, given, all, or \"project <name>\"." },
                "due": { "type": "string", "enum": ["any", "overdue", "today", "week"], "description": "today includes overdue; week is the next 7 days." },
                "include_done": { "type": "boolean" },
            }),
            &[],
        ),
        tool("task_get", "One task in full: notes, checklist, who has it, its history and comments, the user's reminder.", RiskLevel::ReadOnly, json!({ "id": id }), &["id"]),
        tool("task_search", "Search the user's tasks (open and done) by words in their title or notes, and their projects by name.", RiskLevel::ReadOnly, json!({ "query": { "type": "string" } }), &["query"]),
        tool(
            "task_add",
            "Add a task. \"Remind me Friday at 3 to call the vendor\" is the user's own task with a reminder. Their own tasks are added at once; one in a project others are in, or given to someone (they're asked to accept it), waits for the user's Allow.",
            RiskLevel::LowWrite,
            json!({
                "title": { "type": "string", "description": "Short, as the user said it, without the date words." },
                "project": { "type": "string", "description": "Empty for the user's own tasks, or a project's name." },
                "due": { "type": "string", "description": "When it's due, as said: \"friday\", \"tomorrow\", \"oct 14\", or YYYY-MM-DD." },
                "remind": { "type": "string", "description": "When to remind the user, as said: \"friday 3pm\", \"in 2h\" (a day alone: 9:00). lyra pushes it." },
                "notes": { "type": "string" },
                "priority": priority,
                "repeat": { "type": "string", "enum": REPEATS },
                "assignees": { "type": "array", "items": { "type": "string" }, "description": "People (lyra users, by name) to give it to: each is asked to accept it." },
                "checklist": { "type": "array", "items": { "type": "string" } },
                "subtask_of": { "type": "string", "description": "A task's id, to make this its subtask." },
            }),
            &["title"],
        ),
        tool(
            "task_update",
            "Change a task: title, notes, due date, status (open, in_progress), priority, repeat, project, checklist items' done state. To finish one use task_complete.",
            RiskLevel::LowWrite,
            json!({
                "id": id,
                "title": { "type": "string" },
                "notes": { "type": "string" },
                "due": { "type": "string", "description": "As said, or \"none\" to clear it." },
                "status": { "type": "string", "enum": ["open", "in_progress"] },
                "priority": priority,
                "repeat": { "type": "string", "description": "daily, weekdays, weekly, biweekly, monthly, yearly or none." },
                "project": { "type": "string", "description": "A project's name, or \"none\" to make it the user's own." },
                "check": { "type": "array", "items": { "type": "string" }, "description": "Checklist items to tick (by their text)." },
                "add_items": { "type": "array", "items": { "type": "string" }, "description": "Checklist items to add." },
            }),
            &["id"],
        ),
        tool(
            "task_complete",
            "Mark a task done (a repeating one comes back with its next date). Keep what the user said about it (\"sent the PO to Dana\") as the note, else leave it empty.",
            RiskLevel::LowWrite,
            json!({ "id": id, "note": { "type": "string" } }),
            &["id"],
        ),
        tool("task_reopen", "Open a done task again.", RiskLevel::LowWrite, json!({ "id": id }), &["id"]),
        tool("task_delete", "Delete a task (its owner's, or a project owner's).", RiskLevel::Write, json!({ "id": id }), &["id"]),
        tool(
            "task_remind",
            "Set (or clear) the user's own reminder on a task; lyra pushes it then, and again while the task is open. Only the user sees it.",
            RiskLevel::LowWrite,
            json!({ "id": id, "when": { "type": "string", "description": "\"tomorrow 9am\", \"in 1h\", \"friday 3pm\"." }, "clear": { "type": "boolean" } }),
            &["id"],
        ),
        tool("task_comment", "Comment on a task (others on it see it: it waits for the user's Allow).", RiskLevel::Write, json!({ "id": id, "text": { "type": "string" } }), &["id", "text"]),
        tool(
            "task_assign",
            "Give a task to someone who uses lyra (they're asked to accept it), or take it back (remove). Waits for the user's Allow.",
            RiskLevel::Write,
            json!({ "id": id, "person": { "type": "string", "description": "Their name." }, "remove": { "type": "boolean" } }),
            &["id", "person"],
        ),
        tool(
            "task_answer",
            "Accept or decline a task someone gave the user (task_waiting lists them). The person who gave it hears the answer.",
            RiskLevel::Write,
            json!({ "id": id, "accept": { "type": "boolean" }, "note": { "type": "string", "description": "Why, when declining." } }),
            &["id", "accept"],
        ),
        tool("task_waiting", "Tasks others gave the user that wait for their Accept or Decline.", RiskLevel::ReadOnly, json!({}), &[]),
        tool(
            "task_inbox",
            "What others did on the user's tasks lately: comments, finished tasks, answers to tasks they gave. mark_read marks it all read.",
            RiskLevel::LowWrite,
            json!({ "mark_read": { "type": "boolean" } }),
            &[],
        ),
        tool(
            "task_projects",
            "The projects the user is in, with health (on track, at risk, off track), open, done and overdue tasks, and the last status update.",
            RiskLevel::ReadOnly,
            json!({ "include_done": { "type": "boolean", "description": "Also finished ones." } }),
            &[],
        ),
        tool("task_project", "One project: details, members, its open tasks and the latest status updates.", RiskLevel::ReadOnly, json!({ "project": project }), &["project"]),
        tool(
            "task_project_add",
            "Make a project (the user owns it). With other members it waits for the user's Allow; they're told they were added.",
            RiskLevel::LowWrite,
            json!({ "name": { "type": "string" }, "description": { "type": "string" }, "members": { "type": "array", "items": { "type": "string" }, "description": "People's names." }, "due": { "type": "string" } }),
            &["name"],
        ),
        tool(
            "task_project_update",
            "Change a project: name, description, status (active, on_hold, done), health, due date, members to add or remove.",
            RiskLevel::LowWrite,
            json!({
                "project": project,
                "name": { "type": "string" },
                "description": { "type": "string" },
                "status": { "type": "string", "enum": ["active", "on_hold", "done"] },
                "health": { "type": "string", "enum": ["on_track", "at_risk", "off_track"] },
                "due": { "type": "string" },
                "add_members": { "type": "array", "items": { "type": "string" } },
                "remove_members": { "type": "array", "items": { "type": "string" } },
            }),
            &["project"],
        ),
        tool(
            "task_draft_update",
            "Draft a project's status update from its tasks (done this week, overdue, next) with a suggested health. Nothing is posted.",
            RiskLevel::ReadOnly,
            json!({ "project": project }),
            &["project"],
        ),
        tool(
            "task_post_update",
            "Post a status update on a project (its members see it: waits for the user's Allow). It sets the project's health.",
            RiskLevel::Write,
            json!({ "project": project, "health": { "type": "string", "enum": ["on_track", "at_risk", "off_track"] }, "text": { "type": "string", "description": "Progress, what's in the way, next steps." } }),
            &["project", "health", "text"],
        ),
    ]
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(|x| x.as_str().map(str::to_string)).filter(|x| !x.trim().is_empty()).collect()
}

fn names(ids: &[String]) -> String {
    ids.iter().map(|u| name_of(u)).collect::<Vec<_>>().join(", ")
}

/// Whether a call needs the user's Allow first, decided here: their own
/// tasks and reminders don't; anything someone else sees does.
pub fn approval(name: &str, args: &Value) -> Option<Ask> {
    let name = alias(name).unwrap_or(name);
    let user = crate::acting::current();
    let ask = |what: String, detail: String, who: &[String]| Some(Ask { what, detail: detail.chars().take(300).collect(), why: format!("{} will see it", if who.is_empty() { "others".to_string() } else { names(who) }), dangerous: false });
    let task = || get(&s(&args["id"])).ok();
    match name {
        "task_add" => {
            let mut who: Vec<String> = strings(&args["assignees"]).iter().filter_map(|a| person(a).ok()).filter(|u| *u != user).collect();
            let place = if args["project"].is_string() { s(&args["project"]) } else { s(&args["where"]) };
            if !matches!(place.trim().to_lowercase().as_str(), "" | "personal" | "me" | "mine")
                && let Ok(p) = project(&place)
            {
                who.extend(p.members.iter().map(|(u, _)| u.clone()).filter(|u| *u != user));
            }
            who.dedup();
            (!who.is_empty()).then_some(()).and_then(|_| ask(format!("add a task{}", if strings(&args["assignees"]).is_empty() { String::new() } else { format!(" for {}", strings(&args["assignees"]).join(", ")) }), s(&args["title"]), &who))
        }
        "task_update" | "task_complete" | "task_reopen" | "task_delete" | "task_comment" => {
            let t = task()?;
            let who = others(&t);
            if who.is_empty() {
                return None;
            }
            let what = match name {
                "task_complete" => "mark a shared task done",
                "task_reopen" => "reopen a shared task",
                "task_delete" => "delete a shared task",
                "task_comment" => "comment on a task",
                _ => "change a shared task",
            };
            ask(format!("{what}: {}", t.title), if name == "task_comment" { s(&args["text"]) } else { args.to_string() }, &who)
        }
        "task_assign" => {
            let t = task();
            let p = person(&s(&args["person"])).ok();
            let verb = if args["remove"] == true { "take a task back from" } else { "give a task to" };
            ask(format!("{verb} {}", s(&args["person"])), t.map(|t| t.title).unwrap_or_default(), &p.into_iter().collect::<Vec<_>>())
        }
        "task_answer" => {
            let t = store::tasks_by_id(&[s(&args["id"])]).ok()?.into_iter().next()?;
            let by: Vec<String> = t.assignees.iter().filter(|a| a.user == user).map(|a| a.by.clone()).collect();
            ask(format!("{} \"{}\"", if args["accept"] == true { "accept" } else { "decline" }, t.title), s(&args["note"]), &by)
        }
        "task_project_add" => {
            let who: Vec<String> = strings(&args["members"]).iter().filter_map(|m| person(m).ok()).filter(|u| *u != user).collect();
            (!who.is_empty()).then_some(()).and_then(|_| ask(format!("make project {} with {}", s(&args["name"]), strings(&args["members"]).join(", ")), s(&args["description"]), &who))
        }
        "task_project_update" => {
            let p = project(&s(&args["project"])).ok()?;
            let who: Vec<String> = p.members.iter().map(|(u, _)| u.clone()).filter(|u| *u != user).chain(strings(&args["add_members"]).iter().filter_map(|m| person(m).ok())).collect();
            (!who.is_empty()).then_some(()).and_then(|_| ask(format!("change project {}", p.name), args.to_string(), &who))
        }
        "task_post_update" => {
            let p = project(&s(&args["project"])).ok()?;
            let who: Vec<String> = p.members.iter().map(|(u, _)| u.clone()).filter(|u| *u != user).collect();
            (!who.is_empty()).then_some(()).and_then(|_| ask(format!("post a status update on {} ({})", p.name, s(&args["health"])), s(&args["text"]), &who))
        }
        _ => None,
    }
}

/// Run a tasks tool (asked about first by the caller when `approval` says so).
pub fn call(name: &str, args: &Value) -> Result<Value, String> {
    let name = alias(name).unwrap_or(name);
    let today = Local::now().date_naive();
    match name {
        "task_list" => {
            let scope = Scope::parse(&s(&args["scope"]))?;
            let due = args["due"].as_str().unwrap_or("any");
            let tasks = list(&scope, due, args["include_done"] == true)?;
            Ok(json!({ "today": today.to_string(), "count": tasks.len(), "tasks": tasks.iter().take(60).map(brief).collect::<Vec<_>>() }))
        }
        "task_get" => Ok(full(&get(&s(&args["id"]))?)),
        "task_search" => {
            let q = s(&args["query"]);
            let tasks = search(&q)?;
            let ql = q.trim().to_lowercase();
            let found: Vec<Value> = projects()?.iter().filter(|p| !ql.is_empty() && p.name.to_lowercase().contains(&ql)).map(project_brief).collect();
            Ok(json!({ "tasks": tasks.iter().take(30).map(brief).collect::<Vec<_>>(), "projects": found }))
        }
        "task_add" => {
            let t = add(New {
                title: s(&args["title"]),
                notes: s(&args["notes"]).trim().to_string() + &s(&args["description"]),
                due: s(&args["due"]),
                remind: s(&args["remind"]),
                priority: s(&args["priority"]),
                repeat: s(&args["repeat"]),
                project: if args["project"].is_string() { s(&args["project"]) } else { s(&args["where"]) },
                parent: args["subtask_of"].as_str().map(str::to_string),
                assignees: strings(&args["assignees"]),
                checklist: strings(&args["checklist"]),
                source: "chat".into(),
                ..Default::default()
            })?;
            let mut v = brief(&t);
            v["reminder"] = json!(reminder_of(&t.id).map(|r| r.at));
            Ok(v)
        }
        "task_update" => {
            let id = s(&args["id"]);
            let mut checklist = None;
            let (check, add_items) = (strings(&args["check"]), strings(&args["add_items"]));
            if !check.is_empty() || !add_items.is_empty() {
                let mut items = get(&id)?.checklist;
                for c in &check {
                    let c = c.to_lowercase();
                    // The item by its text (or the one that has it).
                    let at = items.iter().position(|i| i.text.to_lowercase() == c).or_else(|| items.iter().position(|i| i.text.to_lowercase().contains(&c)));
                    if let Some(i) = at {
                        items[i].done = true;
                    }
                }
                items.extend(add_items.into_iter().map(|text| Item { done: false, text }));
                checklist = Some(items);
            }
            let opt = |k: &str| args[k].as_str().map(str::to_string);
            let t = update(&id, Change { title: opt("title"), notes: opt("notes").or(opt("description")), due: opt("due"), priority: opt("priority"), status: opt("status").map(|s| if s == "todo" { "open".into() } else { s }), repeat: opt("repeat"), project: opt("project"), checklist })?;
            Ok(brief(&t))
        }
        "task_complete" => {
            let (t, next) = complete(&s(&args["id"]), &(s(&args["note"]) + &s(&args["comment"])))?;
            let mut v = brief(&t);
            if let Some(n) = next {
                v["next"] = brief(&n);
            }
            Ok(v)
        }
        "task_reopen" => Ok(brief(&reopen(&s(&args["id"]))?)),
        "task_delete" => {
            let t = delete(&s(&args["id"]))?;
            Ok(json!({ "deleted": t.title }))
        }
        "task_remind" => {
            let at = if args["clear"] == true { None } else { Some(s(&args["when"])) };
            if at.as_deref().is_some_and(|a| a.trim().is_empty()) {
                return Err("when? (\"tomorrow 9am\", \"in 1h\"), or clear".into());
            }
            let (t, at) = remind(&s(&args["id"]), at.as_deref())?;
            Ok(json!({ "id": t.id, "title": t.title, "reminder": at.map(|a| a.format("%Y-%m-%d %H:%M").to_string()) }))
        }
        "task_comment" => {
            let t = comment(&s(&args["id"]), &s(&args["text"]))?;
            Ok(json!({ "ok": true, "task": t.title }))
        }
        "task_assign" => {
            let (t, who) = assign(&s(&args["id"]), &s(&args["person"]), args["remove"] == true)?;
            Ok(json!({ "task": brief(&t), "said": if args["remove"] == true { format!("taken back from {who}") } else { format!("{who} was asked to accept it") } }))
        }
        "task_answer" => {
            let t = answer(&s(&args["id"]), args["accept"] == true, &s(&args["note"]))?;
            Ok(json!({ "task": t.title, "answer": if args["accept"] == true { "accepted" } else { "declined" } }))
        }
        "task_waiting" => {
            let w = list(&Scope::Waiting, "any", false)?;
            Ok(json!({ "count": w.len(), "tasks": w.iter().map(|t| { let mut v = brief(t); v["from"] = json!(t.assignees.iter().find(|a| a.user == crate::acting::current()).map(|a| name_of(&a.by))); v }).collect::<Vec<_>>() }))
        }
        "task_inbox" => {
            let (items, unread) = inbox(args["mark_read"] == true)?;
            Ok(json!({ "unread": unread, "items": items }))
        }
        "task_projects" => {
            let all: Vec<Value> = projects()?.iter().filter(|p| args["include_done"] == true || args["include_closed"] == true || p.status != "done").map(project_brief).collect();
            Ok(json!({ "count": all.len(), "projects": all }))
        }
        "task_project" => {
            let p = project(&s(&args["project"]))?;
            let mut v = project_brief(&p);
            v["open_tasks"] = json!(list(&Scope::Project(p.id.clone()), "any", false)?.iter().take(40).map(brief).collect::<Vec<_>>());
            v["updates"] = json!(updates(&p.id, 5).iter().map(|u| json!({ "at": u.at, "by": name_of(&u.by), "health": u.health, "text": u.text })).collect::<Vec<_>>());
            Ok(v)
        }
        "task_project_add" => Ok(project_brief(&add_project(&s(&args["name"]), &s(&args["description"]), &strings(&args["members"]), &s(&args["due"]))?)),
        "task_project_update" => {
            let opt = |k: &str| args[k].as_str().map(str::to_string);
            let p = update_project(
                &s(&args["project"]),
                ProjectChange { name: opt("name"), description: opt("description"), status: opt("status"), health: opt("health"), due: opt("due"), add_members: strings(&args["add_members"]), remove_members: strings(&args["remove_members"]) },
            )?;
            Ok(project_brief(&p))
        }
        "task_draft_update" => draft_update(&s(&args["project"])),
        "task_post_update" => {
            // The old PMI shape (progress, risks, next steps) reads as one text.
            let mut text = s(&args["text"]);
            for (k, label) in [("progress", "Progress"), ("next_steps", "Next")] {
                if !s(&args[k]).is_empty() {
                    text += &format!("\n{label}: {}", s(&args[k]));
                }
            }
            let u = post_update(&s(&args["project"]), &s(&args["health"]), text.trim())?;
            Ok(json!({ "ok": true, "health": u.health }))
        }
        "pmi_add_risk" => Err("risks aren't kept in lyra's projects: post it in a status update instead".into()),
        other => Err(format!("unknown tasks tool {other}")),
    }
}

// ---- the commands (the terminal, and the app's buttons)

/// The last `/tasks` list per person, so `/task done 2` works.
static LISTED: Mutex<Option<std::collections::HashMap<String, Vec<String>>>> = Mutex::new(None);

fn listed(n: &str) -> String {
    let user = crate::acting::current();
    n.parse::<usize>()
        .ok()
        .and_then(|i| LISTED.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(|m| m.get(&user)).and_then(|l| l.get(i.wrapping_sub(1)).cloned()))
        .unwrap_or_else(|| n.to_string())
}

/// `/tasks [today|overdue|week|waiting|given|all|project <name>]` as text.
pub fn tasks_text(arg: &str) -> Result<String, String> {
    let a = arg.trim();
    let (scope, due) = match a {
        "" | "open" | "mine" => (Scope::Mine, "any"),
        "today" | "overdue" | "week" => (Scope::Mine, a),
        "projects" => return projects_text(),
        s => (Scope::parse(s)?, "any"),
    };
    let tasks = list(&scope, due, false)?;
    LISTED.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Default::default).insert(crate::acting::current(), tasks.iter().map(|t| t.id.clone()).collect());
    if tasks.is_empty() {
        return Ok(match scope {
            Scope::Waiting => "nothing waits for your answer".into(),
            _ => "nothing open. /task add <what> [when] adds one".into(),
        });
    }
    let today = Local::now().date_naive().to_string();
    let mut out = vec![format!("{} open task{}{}:", tasks.len(), if tasks.len() == 1 { "" } else { "s" }, if due == "any" { String::new() } else { format!(" ({due})") })];
    for (i, t) in tasks.iter().enumerate() {
        let b = brief(t);
        let due = t.due.as_deref().map(|d| if d < today.as_str() { format!(" · ⚠ due {d}") } else if d == today { " · today".to_string() } else { format!(" · {d}") }).unwrap_or_default();
        out.push(format!("{:>2}. {}{due} · {}{}{}", i + 1, t.title, s(&b["where"]), if t.status == "in_progress" { " · in progress" } else { "" }, if b["waiting_for_you"] == true { " · waits for your answer" } else { "" }));
    }
    out.push("/task done <n> [note] · /task snooze <n> [1h|tomorrow] · /task add <what> [when] · /task accept|decline <n>".into());
    Ok(out.join("\n"))
}

fn projects_text() -> Result<String, String> {
    let all = projects()?;
    if all.is_empty() {
        return Ok("no projects yet. /project add <name> makes one".into());
    }
    Ok(all
        .iter()
        .map(|p| {
            let b = project_brief(p);
            format!("{} · {} · {} open{} · {}", p.name, p.health.replace('_', " "), b["tasks"]["open"], if b["tasks"]["overdue"].as_u64().unwrap_or(0) > 0 { format!(", {} overdue", b["tasks"]["overdue"]) } else { String::new() }, p.status.replace('_', " "))
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

/// `/task add|done|snooze|accept|decline|delete …`. Typed by the person
/// themselves: it acts at once.
pub fn task_command(arg: &str) -> Result<String, String> {
    let (sub, rest) = arg.trim().split_once(' ').map_or((arg.trim(), ""), |(a, b)| (a, b.trim()));
    match sub {
        "add" if !rest.is_empty() => {
            let (title, w) = crate::when::split(rest, Local::now());
            let mut n = New { title, source: "page".into(), ..Default::default() };
            if let Some(w) = w {
                n.due = w.date.to_string();
                if let Some(t) = w.time {
                    n.remind = format!("{} {}", w.date, t.format("%H:%M"));
                }
            }
            let t = add(n)?;
            let r = reminder_of(&t.id).and_then(|r| chrono::DateTime::parse_from_rfc3339(&r.at).ok()).map(|a| a.with_timezone(&Local).format("%a %H:%M").to_string());
            Ok(format!("added: {}{}{}", t.title, t.due.map(|d| format!(" · due {d}")).unwrap_or_default(), r.map(|r| format!(" · reminder {r}")).unwrap_or_default()))
        }
        "done" if !rest.is_empty() => {
            let (n, note) = rest.split_once(' ').map_or((rest, ""), |(a, b)| (a, b.trim()));
            let (t, next) = complete(&listed(n), note)?;
            Ok(format!("done: {}{}", t.title, next.and_then(|n| n.due).map(|d| format!(" · next on {d}")).unwrap_or_default()))
        }
        "reopen" if !rest.is_empty() => Ok(format!("open again: {}", reopen(&listed(rest))?.title)),
        "delete" if !rest.is_empty() => Ok(format!("deleted: {}", delete(&listed(rest))?.title)),
        "snooze" | "remind" if !rest.is_empty() => {
            let (n, until) = rest.split_once(' ').map_or((rest, ""), |(a, b)| (a, b.trim()));
            let until = match until {
                "" | "1h" => "in 1h".to_string(),
                "tomorrow" => "tomorrow 9am".to_string(),
                u => u.to_string(),
            };
            let (t, at) = remind(&listed(n), Some(&until))?;
            Ok(format!("reminder: {} · {}", t.title, at.map(|a| a.format("%a %H:%M").to_string()).unwrap_or_default()))
        }
        "accept" | "decline" if !rest.is_empty() => {
            let (n, note) = rest.split_once(' ').map_or((rest, ""), |(a, b)| (a, b.trim()));
            let t = answer(&listed(n), sub == "accept", note)?;
            Ok(format!("{}: {}", if sub == "accept" { "accepted" } else { "declined" }, t.title))
        }
        "give" if !rest.is_empty() => {
            let (n, who) = rest.split_once(' ').ok_or("usage: /task give <n|id> <person>")?;
            let (t, name) = assign(&listed(n), who.trim(), false)?;
            Ok(format!("{name} was asked to accept \"{}\"", t.title))
        }
        _ => Err("usage: /task add <what> [when] · /task done <n|id> [note] · /task snooze <n|id> [1h|tomorrow|<when>] · /task accept|decline <n|id> [note] · /task give <n|id> <person> · /task reopen|delete <n|id>".into()),
    }
}

/// `/project [add <name>|<name>]`.
pub fn project_command(arg: &str) -> Result<String, String> {
    let (sub, rest) = arg.trim().split_once(' ').map_or((arg.trim(), ""), |(a, b)| (a, b.trim()));
    match sub {
        "" | "list" => projects_text(),
        "add" if !rest.is_empty() => {
            let p = add_project(rest, "", &[], "")?;
            Ok(format!("project {} made: add members on the Tasks page, or /project member {} <name>", p.name, p.name))
        }
        "member" if !rest.is_empty() => {
            let (name, who) = rest.rsplit_once(' ').ok_or("usage: /project member <project> <person>")?;
            let p = update_project(name, ProjectChange { add_members: vec![who.to_string()], ..Default::default() })?;
            Ok(format!("{} is in {} now", name_of(&person(who)?), p.name))
        }
        _ => {
            let p = project(arg)?;
            let b = project_brief(&p);
            let tasks = list(&Scope::Project(p.id.clone()), "any", false)?;
            let mut out = vec![format!("{} · {} · {} open, {} done · members: {}", p.name, p.health.replace('_', " "), b["tasks"]["open"], b["tasks"]["done"], p.members.iter().map(|(u, _)| name_of(u)).collect::<Vec<_>>().join(", "))];
            out.extend(tasks.iter().map(|t| format!("  · {}{}", t.title, t.due.as_ref().map(|d| format!(" · {d}")).unwrap_or_default())));
            Ok(out.join("\n"))
        }
    }
}

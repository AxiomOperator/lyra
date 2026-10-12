//! The tasks database (`~/.lyra/tasks/tasks.db`): tasks, who they're given
//! to, each person's reminder, projects and their members, what happened on
//! them, status updates, and links for a later sync. SQL that would also run
//! on PostgreSQL (CLAUDE.md rule 5): bound parameters, TEXT ids and RFC 3339
//! times passed in from Rust, `ON CONFLICT`, constraints in the schema. The
//! only SQLite-only line is the connection setup (WAL, foreign keys).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use sqlx::Row;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool, SqlitePoolOptions};

use super::{Assignee, Event, Project, Reminder, Task, Update};

const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS projects (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        description TEXT NOT NULL DEFAULT '',
        owner TEXT NOT NULL,
        status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'on_hold', 'done')),
        health TEXT NOT NULL DEFAULT 'on_track' CHECK (health IN ('on_track', 'at_risk', 'off_track')),
        due TEXT,
        created TEXT NOT NULL,
        updated TEXT NOT NULL,
        deleted TEXT
    )",
    "CREATE TABLE IF NOT EXISTS project_members (
        project_id TEXT NOT NULL REFERENCES projects (id),
        user_id TEXT NOT NULL,
        role TEXT NOT NULL CHECK (role IN ('owner', 'member')),
        PRIMARY KEY (project_id, user_id)
    )",
    "CREATE TABLE IF NOT EXISTS tasks (
        id TEXT PRIMARY KEY,
        owner TEXT NOT NULL,
        created_by TEXT NOT NULL,
        title TEXT NOT NULL,
        notes TEXT NOT NULL DEFAULT '',
        status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'in_progress', 'done', 'cancelled')),
        priority TEXT NOT NULL DEFAULT 'normal' CHECK (priority IN ('low', 'normal', 'high', 'urgent')),
        due TEXT,
        repeat_rule TEXT,
        checklist TEXT NOT NULL DEFAULT '[]',
        project_id TEXT REFERENCES projects (id),
        parent_id TEXT REFERENCES tasks (id),
        goal_id TEXT,
        source TEXT NOT NULL DEFAULT 'chat',
        source_ref TEXT,
        created TEXT NOT NULL,
        updated TEXT NOT NULL,
        completed TEXT,
        deleted TEXT
    )",
    "CREATE INDEX IF NOT EXISTS tasks_owner ON tasks (owner)",
    "CREATE INDEX IF NOT EXISTS tasks_project ON tasks (project_id)",
    "CREATE TABLE IF NOT EXISTS task_assignees (
        task_id TEXT NOT NULL REFERENCES tasks (id),
        user_id TEXT NOT NULL,
        state TEXT NOT NULL CHECK (state IN ('pending', 'accepted', 'declined')),
        by_user TEXT NOT NULL,
        at TEXT NOT NULL,
        PRIMARY KEY (task_id, user_id)
    )",
    "CREATE TABLE IF NOT EXISTS reminders (
        task_id TEXT NOT NULL REFERENCES tasks (id),
        user_id TEXT NOT NULL,
        at TEXT NOT NULL,
        fired TEXT,
        nags INTEGER NOT NULL DEFAULT 0,
        nag_last TEXT,
        PRIMARY KEY (task_id, user_id)
    )",
    "CREATE TABLE IF NOT EXISTS task_events (
        id TEXT PRIMARY KEY,
        task_id TEXT REFERENCES tasks (id),
        project_id TEXT REFERENCES projects (id),
        at TEXT NOT NULL,
        by_user TEXT NOT NULL,
        kind TEXT NOT NULL CHECK (kind IN ('created', 'edited', 'status', 'done', 'reopened', 'comment', 'assigned', 'accepted', 'declined', 'deleted', 'member')),
        text TEXT NOT NULL DEFAULT ''
    )",
    "CREATE INDEX IF NOT EXISTS task_events_task ON task_events (task_id)",
    "CREATE TABLE IF NOT EXISTS project_updates (
        id TEXT PRIMARY KEY,
        project_id TEXT NOT NULL REFERENCES projects (id),
        at TEXT NOT NULL,
        by_user TEXT NOT NULL,
        health TEXT NOT NULL CHECK (health IN ('on_track', 'at_risk', 'off_track')),
        text TEXT NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS task_links (
        task_id TEXT NOT NULL REFERENCES tasks (id),
        service TEXT NOT NULL,
        external_id TEXT NOT NULL,
        etag TEXT,
        synced TEXT,
        PRIMARY KEY (task_id, service)
    )",
    "CREATE TABLE IF NOT EXISTS inbox_seen (
        user_id TEXT PRIMARY KEY,
        at TEXT NOT NULL
    )",
];

/// The runtime the queries run on (lyra's callers are plain threads).
static RT: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| tokio::runtime::Builder::new_multi_thread().worker_threads(1).thread_name("lyra-tasks").enable_all().build().expect("a runtime for tasks"));

/// One pool per database file (tests each have their own home).
static POOLS: LazyLock<Mutex<HashMap<PathBuf, SqlitePool>>> = LazyLock::new(Default::default);

// The IN (…) lists below are built from "?" marks only (every value is
// bound), so they're wrapped in `AssertSqlSafe`.

/// Run a query's future to its end, from any thread (one inside another
/// runtime hands it to a thread of its own).
fn run<T: Send>(f: impl std::future::Future<Output = Result<T, sqlx::Error>> + Send) -> Result<T, String> {
    let out = if tokio::runtime::Handle::try_current().is_ok() { std::thread::scope(|s| s.spawn(|| RT.block_on(f)).join().unwrap_or_else(|_| Err(sqlx::Error::PoolClosed))) } else { RT.block_on(f) };
    out.map_err(|e| format!("the tasks database: {e}"))
}

fn path() -> Result<PathBuf, String> {
    Ok(crate::config::home().ok_or("no lyra home")?.join("tasks").join("tasks.db"))
}

fn pool() -> Result<SqlitePool, String> {
    let file = path()?;
    if let Some(p) = POOLS.lock().unwrap_or_else(|e| e.into_inner()).get(&file) {
        return Ok(p.clone());
    }
    let p = open(&file)?;
    POOLS.lock().unwrap_or_else(|e| e.into_inner()).insert(file, p.clone());
    Ok(p)
}

fn open(file: &Path) -> Result<SqlitePool, String> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    // SQLite's own settings: here, in the connection setup, only.
    let options = SqliteConnectOptions::new()
        .filename(file)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .foreign_keys(true)
        .busy_timeout(std::time::Duration::from_secs(10));
    let pool = run(SqlitePoolOptions::new().max_connections(4).connect_with(options))?;
    let p = pool.clone();
    run(async move {
        for q in SCHEMA {
            sqlx::query(*q).execute(&p).await?;
        }
        Ok(())
    })?;
    Ok(pool)
}

// ---- reading

const TASK_COLUMNS: &str = "id, owner, created_by, title, notes, status, priority, due, repeat_rule, checklist, project_id, parent_id, goal_id, source, source_ref, created, updated, completed";

fn task_of(r: &sqlx::sqlite::SqliteRow) -> Task {
    let s = |c: &str| r.try_get::<String, _>(c).unwrap_or_default();
    let o = |c: &str| r.try_get::<Option<String>, _>(c).ok().flatten();
    Task {
        id: s("id"),
        owner: s("owner"),
        created_by: s("created_by"),
        title: s("title"),
        notes: s("notes"),
        status: s("status"),
        priority: s("priority"),
        due: o("due"),
        repeat: o("repeat_rule"),
        checklist: serde_json::from_str(&s("checklist")).unwrap_or_default(),
        project_id: o("project_id"),
        parent_id: o("parent_id"),
        goal_id: o("goal_id"),
        source: s("source"),
        source_ref: o("source_ref"),
        created: s("created"),
        updated: s("updated"),
        completed: o("completed"),
        assignees: Vec::new(),
    }
}

/// Tasks by id (with who they're given to), the deleted left out.
pub fn tasks_by_id(ids: &[String]) -> Result<Vec<Task>, String> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let p = pool()?;
    let ids = ids.to_vec();
    run(async move {
        let marks = vec!["?"; ids.len()].join(", ");
        let sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE deleted IS NULL AND id IN ({marks})");
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.clone()));
        for id in &ids {
            query = query.bind(id);
        }
        let rows = query.fetch_all(&p).await?;
        let mut tasks: Vec<Task> = rows.iter().map(task_of).collect();
        let sql = format!("SELECT task_id, user_id, state, by_user, at FROM task_assignees WHERE task_id IN ({marks})");
        let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.clone()));
        for id in &ids {
            query = query.bind(id);
        }
        for r in query.fetch_all(&p).await? {
            let task: String = r.try_get("task_id")?;
            if let Some(t) = tasks.iter_mut().find(|t| t.id == task) {
                t.assignees.push(Assignee { user: r.try_get("user_id")?, state: r.try_get("state")?, by: r.try_get("by_user")?, at: r.try_get("at")? });
            }
        }
        Ok(tasks)
    })
}

/// The ids of the tasks `user` can see: their own, given to them (not
/// declined), and their projects' (admins: every project's).
pub fn visible_ids(user: &str, admin: bool, include_done: bool) -> Result<Vec<String>, String> {
    let p = pool()?;
    let (user, done) = (user.to_string(), if include_done { 1i64 } else { 0 });
    run(async move {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT t.id FROM tasks t WHERE t.deleted IS NULL
               AND (? = 1 OR t.status IN ('open', 'in_progress'))
               AND (t.owner = ?
                    OR EXISTS (SELECT 1 FROM task_assignees a WHERE a.task_id = t.id AND a.user_id = ? AND a.state <> 'declined')
                    OR (t.project_id IS NOT NULL AND (? = 1 OR EXISTS (SELECT 1 FROM project_members m WHERE m.project_id = t.project_id AND m.user_id = ?))))",
        )
        .bind(done)
        .bind(&user)
        .bind(&user)
        .bind(if admin { 1i64 } else { 0 })
        .bind(&user)
        .fetch_all(&p)
        .await?;
        Ok(rows)
    })
}

/// Done in the last `days` that `user` could see (for the recap and searches).
pub fn done_since(user: &str, since: &str) -> Result<Vec<String>, String> {
    let p = pool()?;
    let (user, since) = (user.to_string(), since.to_string());
    run(async move {
        sqlx::query_scalar(
            "SELECT t.id FROM tasks t WHERE t.deleted IS NULL AND t.status = 'done' AND t.completed >= ?
               AND (t.owner = ? OR EXISTS (SELECT 1 FROM task_assignees a WHERE a.task_id = t.id AND a.user_id = ? AND a.state = 'accepted'))",
        )
        .bind(&since)
        .bind(&user)
        .bind(&user)
        .fetch_all(&p)
        .await
    })
}

pub fn insert_task(t: &Task) -> Result<(), String> {
    let p = pool()?;
    let t = t.clone();
    run(async move {
        sqlx::query(
            "INSERT INTO tasks (id, owner, created_by, title, notes, status, priority, due, repeat_rule, checklist, project_id, parent_id, goal_id, source, source_ref, created, updated, completed)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&t.id)
        .bind(&t.owner)
        .bind(&t.created_by)
        .bind(&t.title)
        .bind(&t.notes)
        .bind(&t.status)
        .bind(&t.priority)
        .bind(&t.due)
        .bind(&t.repeat)
        .bind(serde_json::to_string(&t.checklist).unwrap_or_else(|_| "[]".into()))
        .bind(&t.project_id)
        .bind(&t.parent_id)
        .bind(&t.goal_id)
        .bind(&t.source)
        .bind(&t.source_ref)
        .bind(&t.created)
        .bind(&t.updated)
        .bind(&t.completed)
        .execute(&p)
        .await?;
        Ok(())
    })
}

/// Write a task's own fields back (not who it's given to).
pub fn save_task(t: &Task) -> Result<(), String> {
    let p = pool()?;
    let t = t.clone();
    run(async move {
        sqlx::query(
            "UPDATE tasks SET title = ?, notes = ?, status = ?, priority = ?, due = ?, repeat_rule = ?, checklist = ?, project_id = ?, parent_id = ?, goal_id = ?, updated = ?, completed = ? WHERE id = ?",
        )
        .bind(&t.title)
        .bind(&t.notes)
        .bind(&t.status)
        .bind(&t.priority)
        .bind(&t.due)
        .bind(&t.repeat)
        .bind(serde_json::to_string(&t.checklist).unwrap_or_else(|_| "[]".into()))
        .bind(&t.project_id)
        .bind(&t.parent_id)
        .bind(&t.goal_id)
        .bind(&t.updated)
        .bind(&t.completed)
        .bind(&t.id)
        .execute(&p)
        .await?;
        Ok(())
    })
}

/// Marked deleted (kept, so a later sync can tell the other side).
pub fn delete_task(id: &str, at: &str) -> Result<(), String> {
    let p = pool()?;
    let (id, at) = (id.to_string(), at.to_string());
    run(async move {
        sqlx::query("UPDATE tasks SET deleted = ?, updated = ? WHERE id = ?").bind(&at).bind(&at).bind(&id).execute(&p).await?;
        sqlx::query("DELETE FROM reminders WHERE task_id = ?").bind(&id).execute(&p).await?;
        Ok(())
    })
}

pub fn set_assignee(task: &str, user: &str, state: &str, by: &str, at: &str) -> Result<(), String> {
    let p = pool()?;
    let (task, user, state, by, at) = (task.to_string(), user.to_string(), state.to_string(), by.to_string(), at.to_string());
    run(async move {
        sqlx::query(
            "INSERT INTO task_assignees (task_id, user_id, state, by_user, at) VALUES (?, ?, ?, ?, ?)
             ON CONFLICT (task_id, user_id) DO UPDATE SET state = excluded.state, by_user = excluded.by_user, at = excluded.at",
        )
        .bind(&task)
        .bind(&user)
        .bind(&state)
        .bind(&by)
        .bind(&at)
        .execute(&p)
        .await?;
        Ok(())
    })
}

pub fn remove_assignee(task: &str, user: &str) -> Result<(), String> {
    let p = pool()?;
    let (task, user) = (task.to_string(), user.to_string());
    run(async move {
        sqlx::query("DELETE FROM task_assignees WHERE task_id = ? AND user_id = ?").bind(&task).bind(&user).execute(&p).await?;
        Ok(())
    })
}

// ---- reminders (each person's own, on any task they can see)

pub fn set_reminder(task: &str, user: &str, at: Option<&str>) -> Result<(), String> {
    let p = pool()?;
    let (task, user, at) = (task.to_string(), user.to_string(), at.map(str::to_string));
    run(async move {
        match at {
            Some(at) => {
                sqlx::query(
                    "INSERT INTO reminders (task_id, user_id, at, fired, nags, nag_last) VALUES (?, ?, ?, NULL, 0, NULL)
                     ON CONFLICT (task_id, user_id) DO UPDATE SET at = excluded.at, fired = NULL, nags = 0, nag_last = NULL",
                )
                .bind(&task)
                .bind(&user)
                .bind(&at)
                .execute(&p)
                .await?;
            }
            None => {
                sqlx::query("DELETE FROM reminders WHERE task_id = ? AND user_id = ?").bind(&task).bind(&user).execute(&p).await?;
            }
        }
        Ok(())
    })
}

fn reminder_of(r: &sqlx::sqlite::SqliteRow) -> Reminder {
    Reminder {
        task: r.try_get("task_id").unwrap_or_default(),
        user: r.try_get("user_id").unwrap_or_default(),
        at: r.try_get("at").unwrap_or_default(),
        fired: r.try_get("fired").ok().flatten(),
        nags: r.try_get::<i64, _>("nags").unwrap_or(0) as u32,
        nag_last: r.try_get("nag_last").ok().flatten(),
    }
}

/// `user`'s reminders (every person's: `None`), on tasks still open.
pub fn reminders(user: Option<&str>) -> Result<Vec<Reminder>, String> {
    let p = pool()?;
    let user = user.map(str::to_string);
    run(async move {
        let rows = sqlx::query(
            "SELECT r.task_id, r.user_id, r.at, r.fired, r.nags, r.nag_last FROM reminders r JOIN tasks t ON t.id = r.task_id
             WHERE t.deleted IS NULL AND t.status IN ('open', 'in_progress') AND (? IS NULL OR r.user_id = ?)",
        )
        .bind(&user)
        .bind(&user)
        .fetch_all(&p)
        .await?;
        Ok(rows.iter().map(reminder_of).collect())
    })
}

pub fn reminder_fired(task: &str, user: &str, fired: Option<&str>, nags: u32, nag_last: Option<&str>) -> Result<(), String> {
    let p = pool()?;
    let (task, user, fired, nag_last) = (task.to_string(), user.to_string(), fired.map(str::to_string), nag_last.map(str::to_string));
    run(async move {
        sqlx::query("UPDATE reminders SET fired = ?, nags = ?, nag_last = ? WHERE task_id = ? AND user_id = ?")
            .bind(&fired)
            .bind(nags as i64)
            .bind(&nag_last)
            .bind(&task)
            .bind(&user)
            .execute(&p)
            .await?;
        Ok(())
    })
}

// ---- what happened

pub fn add_event(e: &Event) -> Result<(), String> {
    let p = pool()?;
    let e = e.clone();
    run(async move {
        sqlx::query("INSERT INTO task_events (id, task_id, project_id, at, by_user, kind, text) VALUES (?, ?, ?, ?, ?, ?, ?)")
            .bind(&e.id)
            .bind(&e.task)
            .bind(&e.project)
            .bind(&e.at)
            .bind(&e.by)
            .bind(&e.kind)
            .bind(&e.text)
            .execute(&p)
            .await?;
        Ok(())
    })
}

fn event_of(r: &sqlx::sqlite::SqliteRow) -> Event {
    Event {
        id: r.try_get("id").unwrap_or_default(),
        task: r.try_get("task_id").ok().flatten(),
        project: r.try_get("project_id").ok().flatten(),
        at: r.try_get("at").unwrap_or_default(),
        by: r.try_get("by_user").unwrap_or_default(),
        kind: r.try_get("kind").unwrap_or_default(),
        text: r.try_get("text").unwrap_or_default(),
    }
}

/// A task's history, oldest first.
pub fn events_of(task: &str) -> Result<Vec<Event>, String> {
    let p = pool()?;
    let task = task.to_string();
    run(async move {
        let rows = sqlx::query("SELECT id, task_id, project_id, at, by_user, kind, text FROM task_events WHERE task_id = ? ORDER BY at").bind(&task).fetch_all(&p).await?;
        Ok(rows.iter().map(event_of).collect())
    })
}

/// What others did on these tasks since `since`, newest first.
pub fn events_by_others(tasks: &[String], user: &str, since: &str) -> Result<Vec<Event>, String> {
    if tasks.is_empty() {
        return Ok(Vec::new());
    }
    let p = pool()?;
    let (tasks, user, since) = (tasks.to_vec(), user.to_string(), since.to_string());
    run(async move {
        let marks = vec!["?"; tasks.len()].join(", ");
        let sql = format!("SELECT id, task_id, project_id, at, by_user, kind, text FROM task_events WHERE by_user <> ? AND at >= ? AND task_id IN ({marks}) ORDER BY at DESC LIMIT 50");
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql.clone())).bind(&user).bind(&since);
        for t in &tasks {
            q = q.bind(t);
        }
        Ok(q.fetch_all(&p).await?.iter().map(event_of).collect())
    })
}

pub fn inbox_seen(user: &str) -> Result<Option<String>, String> {
    let p = pool()?;
    let user = user.to_string();
    run(async move { sqlx::query_scalar("SELECT at FROM inbox_seen WHERE user_id = ?").bind(&user).fetch_optional(&p).await })
}

pub fn set_inbox_seen(user: &str, at: &str) -> Result<(), String> {
    let p = pool()?;
    let (user, at) = (user.to_string(), at.to_string());
    run(async move {
        sqlx::query("INSERT INTO inbox_seen (user_id, at) VALUES (?, ?) ON CONFLICT (user_id) DO UPDATE SET at = excluded.at").bind(&user).bind(&at).execute(&p).await?;
        Ok(())
    })
}

// ---- projects

fn project_of(r: &sqlx::sqlite::SqliteRow) -> Project {
    Project {
        id: r.try_get("id").unwrap_or_default(),
        name: r.try_get("name").unwrap_or_default(),
        description: r.try_get("description").unwrap_or_default(),
        owner: r.try_get("owner").unwrap_or_default(),
        status: r.try_get("status").unwrap_or_default(),
        health: r.try_get("health").unwrap_or_default(),
        due: r.try_get("due").ok().flatten(),
        created: r.try_get("created").unwrap_or_default(),
        updated: r.try_get("updated").unwrap_or_default(),
        members: Vec::new(),
    }
}

/// The projects `user` is in (admins: every one), with their members.
pub fn projects_for(user: &str, admin: bool) -> Result<Vec<Project>, String> {
    let p = pool()?;
    let user = user.to_string();
    run(async move {
        let rows = sqlx::query(
            "SELECT id, name, description, owner, status, health, due, created, updated FROM projects pr WHERE deleted IS NULL
               AND (? = 1 OR EXISTS (SELECT 1 FROM project_members m WHERE m.project_id = pr.id AND m.user_id = ?)) ORDER BY name",
        )
        .bind(if admin { 1i64 } else { 0 })
        .bind(&user)
        .fetch_all(&p)
        .await?;
        let mut all: Vec<Project> = rows.iter().map(project_of).collect();
        for r in sqlx::query("SELECT project_id, user_id, role FROM project_members").fetch_all(&p).await? {
            let id: String = r.try_get("project_id")?;
            if let Some(pr) = all.iter_mut().find(|x| x.id == id) {
                pr.members.push((r.try_get("user_id")?, r.try_get("role")?));
            }
        }
        Ok(all)
    })
}

pub fn insert_project(pr: &Project) -> Result<(), String> {
    let p = pool()?;
    let pr = pr.clone();
    run(async move {
        let mut tx = p.begin().await?;
        sqlx::query("INSERT INTO projects (id, name, description, owner, status, health, due, created, updated) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)")
            .bind(&pr.id)
            .bind(&pr.name)
            .bind(&pr.description)
            .bind(&pr.owner)
            .bind(&pr.status)
            .bind(&pr.health)
            .bind(&pr.due)
            .bind(&pr.created)
            .bind(&pr.updated)
            .execute(&mut *tx)
            .await?;
        for (user, role) in &pr.members {
            sqlx::query("INSERT INTO project_members (project_id, user_id, role) VALUES (?, ?, ?) ON CONFLICT (project_id, user_id) DO NOTHING").bind(&pr.id).bind(user).bind(role).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    })
}

pub fn save_project(pr: &Project) -> Result<(), String> {
    let p = pool()?;
    let pr = pr.clone();
    run(async move {
        let mut tx = p.begin().await?;
        sqlx::query("UPDATE projects SET name = ?, description = ?, status = ?, health = ?, due = ?, updated = ? WHERE id = ?")
            .bind(&pr.name)
            .bind(&pr.description)
            .bind(&pr.status)
            .bind(&pr.health)
            .bind(&pr.due)
            .bind(&pr.updated)
            .bind(&pr.id)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM project_members WHERE project_id = ?").bind(&pr.id).execute(&mut *tx).await?;
        for (user, role) in &pr.members {
            sqlx::query("INSERT INTO project_members (project_id, user_id, role) VALUES (?, ?, ?)").bind(&pr.id).bind(user).bind(role).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    })
}

pub fn delete_project(id: &str, at: &str) -> Result<(), String> {
    let p = pool()?;
    let (id, at) = (id.to_string(), at.to_string());
    run(async move {
        sqlx::query("UPDATE projects SET deleted = ?, updated = ? WHERE id = ?").bind(&at).bind(&at).bind(&id).execute(&p).await?;
        Ok(())
    })
}

/// Every (open) task in a project.
pub fn project_task_ids(project: &str, include_done: bool) -> Result<Vec<String>, String> {
    let p = pool()?;
    let (project, done) = (project.to_string(), if include_done { 1i64 } else { 0 });
    run(async move {
        sqlx::query_scalar("SELECT id FROM tasks WHERE deleted IS NULL AND project_id = ? AND (? = 1 OR status IN ('open', 'in_progress'))").bind(&project).bind(done).fetch_all(&p).await
    })
}

pub fn add_update(u: &Update) -> Result<(), String> {
    let p = pool()?;
    let u = u.clone();
    run(async move {
        sqlx::query("INSERT INTO project_updates (id, project_id, at, by_user, health, text) VALUES (?, ?, ?, ?, ?, ?)")
            .bind(&u.id)
            .bind(&u.project)
            .bind(&u.at)
            .bind(&u.by)
            .bind(&u.health)
            .bind(&u.text)
            .execute(&p)
            .await?;
        Ok(())
    })
}

/// A project's status updates, newest first.
pub fn updates_of(project: &str, limit: i64) -> Result<Vec<Update>, String> {
    let p = pool()?;
    let project = project.to_string();
    run(async move {
        let rows = sqlx::query("SELECT id, project_id, at, by_user, health, text FROM project_updates WHERE project_id = ? ORDER BY at DESC LIMIT ?").bind(&project).bind(limit).fetch_all(&p).await?;
        Ok(rows
            .iter()
            .map(|r| Update {
                id: r.try_get("id").unwrap_or_default(),
                project: r.try_get("project_id").unwrap_or_default(),
                at: r.try_get("at").unwrap_or_default(),
                by: r.try_get("by_user").unwrap_or_default(),
                health: r.try_get("health").unwrap_or_default(),
                text: r.try_get("text").unwrap_or_default(),
            })
            .collect())
    })
}

/// Tasks whose title or notes contain `text` (plain LIKE), among `ids`.
pub fn search(ids: &[String], text: &str) -> Result<Vec<String>, String> {
    if ids.is_empty() || text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let p = pool()?;
    let (ids, like) = (ids.to_vec(), format!("%{}%", text.trim().to_lowercase()));
    run(async move {
        let marks = vec!["?"; ids.len()].join(", ");
        let sql = format!("SELECT id FROM tasks WHERE (LOWER(title) LIKE ? OR LOWER(notes) LIKE ?) AND id IN ({marks})");
        let mut q = sqlx::query_scalar(sqlx::AssertSqlSafe(sql.clone())).bind(&like).bind(&like);
        for id in &ids {
            q = q.bind(id);
        }
        q.fetch_all(&p).await
    })
}

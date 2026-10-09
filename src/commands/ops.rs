//! Commands about lyra's own upkeep: backups, diagnoses, status, the briefing, routines.


use crate::*;

impl App {
    /// Back up the lyra home in the background (`/backup now`, nightly).
    pub(crate) fn backup_now(&mut self, nightly: bool) -> Result<String, String> {
        let home = config::home().ok_or("no lyra home")?;
        if backup::running() {
            return Err("a backup is already running".into());
        }
        let (settings, tx, mem, store) = (self.backup.clone(), self.tx.clone(), self.mem(), self.memory_store.clone());
        let dir = settings.dir(&home);
        thread::spawn(move || {
            let copy = |to: &std::path::Path| -> Result<(), String> {
                let mem = mem.as_ref().ok_or("memory is off")?;
                mem.run(mem.manager.backup(to))
            };
            let memory = match (&store, &mem) {
                (Some(path), Some(_)) if path.exists() => Some((path.as_path(), &copy as &dyn Fn(&std::path::Path) -> Result<(), String>)),
                _ => None,
            };
            let result = backup::run(&home, &settings, memory);
            let _ = tx.send(StreamEvent::BackedUp { nightly, result });
        });
        Ok(format!("backing up to {}…", context::show(&dir)))
    }

    /// `/diagnose [<machine> <problem>]`: what's been looked into, or look into something now.
    pub(crate) fn diagnose_command(&mut self, arg: &str) -> Result<String, String> {
        let arg = arg.trim();
        if arg.is_empty() {
            return Ok(diagnose::describe());
        }
        if self.hub.is_none() {
            return Err("diagnoses run in lyra serve (the always-on lyra); this one isn't serving".into());
        }
        let (machine, problem) = arg.split_once([' ', '|']).map(|(m, p)| (m.trim().trim_start_matches('@'), p.trim().trim_start_matches('|').trim())).ok_or("usage: /diagnose <machine> <problem>")?;
        if problem.is_empty() {
            return Err("usage: /diagnose <machine> <problem>".into());
        }
        let key = format!("{}:asked:{problem}", machine.to_lowercase());
        // A problem already reported keeps its key, so the panels find the write-up.
        let key = diagnose::all().into_iter().find(|d| d.machine.eq_ignore_ascii_case(machine) && d.problem == problem).map_or(key, |d| d.key);
        if diagnose::queue(&key, machine, problem, true) {
            Ok(format!("looking into it on {machine}: {problem} — the write-up shows in Activity and next to the problem"))
        } else {
            Err("that's being looked into already".into())
        }
    }

    /// `/status [now]`: everything lyra depends on.
    pub(crate) fn status_command(&mut self, arg: &str) -> Result<String, String> {
        let now = arg.trim() == "now";
        if !now && !arg.trim().is_empty() {
            return Err("usage: /status [now]".into());
        }
        if self.hub.is_some() {
            // lyra serve checks every minute and keeps the history.
            if (now && self.admin) || status::latest().is_none() {
                status::request();
                return Ok("checking everything now — /status in a moment shows it (the app's Status page updates by itself)".into());
            }
            return Ok(status::latest().map(|mut b| {
                // A member's view leaves the machines out.
                if !self.admin {
                    b.rows.retain(|r| r.probe.group != "Machines");
                }
                status::describe(&b)
            }).unwrap_or_default());
        }
        if !now && let Some(b) = status::latest() {
            return Ok(status::describe(&b));
        }
        // The terminal on its own: one check, no history.
        let (inputs, tx) = (self.status_inputs(), self.tx.clone());
        thread::spawn(move || {
            let board = status::Board::plain(status::pass(inputs));
            status::set_latest(&board);
            let _ = tx.send(StreamEvent::Notice(status::describe(&board)));
        });
        Ok("checking everything lyra depends on…".into())
    }

    /// `/briefing [now]`: the daily briefing (lyra serve makes it on schedule).
    pub(crate) fn briefing_command(&mut self, arg: &str) -> Result<String, String> {
        let now = arg.trim() == "now";
        if !now && !arg.trim().is_empty() {
            return Err("usage: /briefing [now]".into());
        }
        // Under lyra serve: this person's own briefing (a member's, or the owner's).
        if self.hub.is_some() {
            let mine = briefing::last_for(&self.owner);
            if now || mine.is_none() {
                briefing::request_for(&self.owner);
                return Ok("making your briefing now — it's pushed and shown on the Status page; /briefing in a moment shows it".into());
            }
            return Ok(mine.map(|b| briefing::describe(&b)).unwrap_or_default());
        }
        if !now && let Some(b) = briefing::last() {
            return Ok(briefing::describe(&b));
        }
        // The terminal on its own: what it knows locally (no machines), not saved.
        let at = chrono::Utc::now();
        let since = briefing::window_start(briefing::last().map(|b| b.at), at);
        let b = briefing::gather(&briefing::local_inputs(self.goals.as_deref(), at, since));
        Ok(briefing::describe(&b))
    }

    /// `/routine …`: scheduled things to ask lyra.
    pub(crate) fn routine_command(&mut self, arg: &str) -> Result<String, String> {
        let arg = arg.trim();
        let (sub, rest) = arg.split_once(' ').map_or((arg, ""), |(a, b)| (a, b.trim()));
        let changed = |what: &str, r: &routines::Routine| format!("{} {what} · {}", r.name, routines::describe().lines().find(|l| l.starts_with(&r.name)).unwrap_or(""));
        match sub {
            "" | "list" => Ok(routines::describe()),
            "new" => {
                let parts: Vec<&str> = rest.split('|').map(str::trim).collect();
                let [name, schedule, prompt, more @ ..] = parts.as_slice() else {
                    return Err("usage: /routine new <name> | <schedule> | <what to do> [| notify problems|always|never] [| changes] [| email]".into());
                };
                let mut notify = routines::Notify::Problems;
                let (mut changes, mut email) = (false, false);
                for opt in more {
                    match opt.split_whitespace().collect::<Vec<_>>().as_slice() {
                        ["changes"] | ["changes", "on"] => changes = true,
                        ["email"] | ["email", "on"] => email = true,
                        ["notify", mode] => notify = routines::Notify::parse(mode)?,
                        _ => return Err(format!("unknown option {opt:?}: notify problems|always|never, changes, email")),
                    }
                }
                let r = routines::create(name, schedule, prompt, notify, changes, email)?;
                self.log(Level::Plan, format!("routine {} created: {}", r.name, r.schedule));
                Ok(changed("created", &r))
            }
            "run" => {
                let r = routines::find(rest)?;
                if self.hub.is_none() {
                    return Err("routines run in lyra serve (the always-on lyra); this one isn't serving".into());
                }
                routines::request_run(&r.name);
                Ok(format!("running {} now — its result shows in Activity, /routine show {}", r.name, r.name))
            }
            "pause" | "resume" => {
                let mut r = routines::find(rest)?;
                r.enabled = sub == "resume";
                routines::save(&r)?;
                Ok(changed(if r.enabled { "resumed" } else { "paused" }, &r))
            }
            "delete" => routines::delete(rest).map(|r| format!("deleted the routine {}", r.name)),
            "show" => routines::show(rest),
            "edit" | "notify" => {
                let (name, change) = rest.split_once(' ').ok_or("usage: /routine edit <name> schedule|prompt|notify <value>")?;
                let mut r = routines::find(name)?;
                let (field, value) = if sub == "notify" { ("notify", change.trim()) } else { change.trim().split_once(' ').map_or((change.trim(), ""), |(a, b)| (a, b.trim())) };
                match field {
                    "schedule" => r.schedule = value.into(),
                    "prompt" => r.prompt = value.into(),
                    "notify" => r.notify = routines::Notify::parse(value)?,
                    "changes" => r.changes = matches!(value, "on" | "yes" | "true"),
                    "email" => r.email = matches!(value, "on" | "yes" | "true"),
                    _ => return Err("usage: /routine edit <name> schedule|prompt|notify|changes|email <value>".into()),
                }
                routines::save(&r)?;
                Ok(changed("changed", &r))
            }
            _ => Err("usage: /routine [new|run|pause|resume|delete|show|edit|notify] …".into()),
        }
    }

    /// `/backup [now|list]`.
    pub(crate) fn backup_command(&mut self, arg: &str) -> Result<String, String> {
        let home = config::home().ok_or("no lyra home")?;
        match arg.trim() {
            "" | "list" => Ok(backup::describe(&self.backup.dir(&home), &self.backup)),
            "now" => self.backup_now(false),
            _ => Err("usage: /backup [now|list]".into()),
        }
    }
}

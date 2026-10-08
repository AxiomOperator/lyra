//! The `/command` dispatcher: what each command does, who may run it from a page.


use crate::*;

impl App {
    /// Handle a `/command` typed in the input box.
    pub(crate) fn command(&mut self, line: &str) {
        let (name, _) = line.split_once(' ').unwrap_or((line, ""));
        let result = self.command_result(line);
        let ok = result.is_ok();
        let (role, text) = match result {
            Ok(text) => ("info", text),
            Err(e) => ("error", e),
        };
        // A resumed conversation already says so.
        if !(ok && matches!(name, "/resume" | "/new") && text.is_empty()) {
            self.messages.push(Message::new(role, format!("> {}\n{text}", shown(line))));
        }
        self.after_command(line, ok);
    }

    /// Commands the web app's pages run (memory, skills, goals, model):
    /// the answer goes back to the page instead of into the conversation.
    pub fn quiet_command(&mut self, line: &str) -> Result<String, String> {
        let name = line.split_whitespace().next().unwrap_or("");
        if !PAGE_COMMANDS.contains(&name) {
            return Err(format!("{name} can't be run from a page"));
        }
        let result = self.command_result(line);
        self.after_command(line, result.is_ok());
        if result.is_ok() && !matches!(name, "/memory" | "/approve" | "/reject" | "/deprecate") {
            self.log(Level::Info, format!("from the app: {}", shown(line)));
        }
        result
    }

    pub(crate) fn command_result(&mut self, line: &str) -> Result<String, String> {
        let (name, arg) = line.split_once(' ').unwrap_or((line, ""));
        if !self.admin && !member_may(name, arg) {
            return Err(format!("{name} is for admins"));
        }
        let learning = self.learning.clone();
        let need = || -> Result<Arc<Learning>, String> {
            learning.clone().ok_or_else(|| match &self.learning_status {
                Err(why) => why.clone(),
                Ok(_) => "learning is off".into(),
            })
        };
        let last_run = self.last_run.clone();
        match name {
            "/help" => Ok(format!("{COMMANDS}\n{}\n{}\n{}\n{}\n{HELP_END}", goals::COMMANDS, agents::COMMANDS, evolve::COMMANDS, caps::COMMANDS)),
            // Each person's own skills are theirs to decide; shared ones an admin's.
            "/skills" => need().and_then(|l| l.describe_for(self.personal().as_deref(), self.admin)),
            "/approve" => need().and_then(|l| l.decide("approve", arg, self.personal().as_deref(), self.admin)),
            "/reject" => need().and_then(|l| l.decide("reject", arg, self.personal().as_deref(), self.admin)),
            "/deprecate" => need().and_then(|l| l.decide("deprecate", arg, self.personal().as_deref(), self.admin)),
            "/forget-skill" => need().and_then(|l| l.decide("forget", arg, self.personal().as_deref(), self.admin)),
            "/history" => need().and_then(|l| l.history(arg)),
            "/rollback" => need().and_then(|l| l.rollback(arg)),
            "/outcome" => (|| {
                let outcome = match arg.trim() {
                    "good" | "success" => SkillOutcome::Success,
                    "bad" | "failure" => SkillOutcome::Failure,
                    "partial" => SkillOutcome::Partial,
                    _ => return Err("usage: /outcome good|bad|partial".into()),
                };
                let last = last_run.ok_or("no reply to rate yet")?;
                let mut out = Vec::new();
                if let Some(evo) = last.evo {
                    self.record_evolution_outcome(evo, outcome, None);
                    out.push(format!("recorded {outcome} for the last run"));
                }
                if let (Some(l), false) = (&learning, last.skills.is_empty()) {
                    let notes = l.record_outcome(last.id, outcome, true)?;
                    out.push(format!("recorded {outcome} for {}", last.skills.join(", ")));
                    out.extend(notes);
                }
                if out.is_empty() {
                    return Err("nothing to record it for: the last reply used no skills and evolution is off".into());
                }
                Ok(out.join("\n"))
            })(),
            "/learn" | "/curate" => need().and_then(|l| {
                if l.mode() == Mode::Off {
                    Err("learning mode is off ([learning] mode in config.toml)".into())
                } else if name == "/learn" && self.reviewing {
                    Err("a review is already running".into())
                } else if name == "/curate" && self.curating {
                    Err("a curation is already running".into())
                } else if name == "/learn" {
                    Ok("reviewing the conversation for a lesson…".into())
                } else {
                    Ok("curating the skill collection…".into())
                }
            }),
            "/plan" | "/plans" => self.plan_command(name, arg),
            "/evolve" => self.evolve_command(arg),
            "/caps" => self.caps_command(arg),
            "/goals" | "/goal" => self.goals_command(name, arg),
            "/agents" | "/agent" => self.agents_command(name, arg),
            // Pin, archive, put in a folder (the app's list).
            "/sessions" if ["pin", "unpin", "archive", "unarchive", "folder", "dismiss"].contains(&arg.split_whitespace().next().unwrap_or("")) => {
                sessions::dir().ok_or("no home directory".to_string()).and_then(|d| sessions::keep_command(&d, &self.owner, arg))
            }
            "/sessions" if arg.trim_start().starts_with("search") => {
                let query = arg.trim_start().trim_start_matches("search").trim();
                if query.is_empty() {
                    Err("usage: /sessions search <words>".into())
                } else {
                    sessions::dir().ok_or("no home directory".to_string()).map(|d| sessions::describe_hits(&sessions::search(&sessions::list_for(&d, &self.owner), query, 15), query))
                }
            }
            "/sessions" => sessions::dir().ok_or("no home directory".to_string()).map(|d| {
                format!("this one: {}\n{}\n\nlyra -c continues the latest here · /resume <id> or lyra -r <id> resumes one", self.session_id, sessions::describe(&sessions::list_for(&d, &self.owner), 20))
            }),
            "/resume" => self.resume(arg),
            "/new" => self.new_session(),
            "/machines" => self.machines_command(arg),
            "/devices" => self.devices_command(arg),
            "/users" => self.users_command(arg),
            "/whoami" => Ok(self.whoami()),
            "/usage" => self.usage_command(arg),
            "/recap" => recap::command(&self.owner),
            "/watches" | "/watch" => watches::command(&self.owner, arg),
            "/feedback" => Ok(feedback::command(&feedback::Who { user: self.owner.clone(), name: String::new(), admin: self.admin })),
            "/memory" => {
                let mem = self.mem().ok_or_else(|| match &self.memory_status {
                    Err(why) => why.clone(),
                    Ok(_) => "memory is off".to_string(),
                });
                // Anyone but the owner looks after their own memories only.
                if let (Ok(m), Some(u)) = (&mem, self.personal()) {
                    return m.command_for(arg, &format!("user:{u}"));
                }
                mem.and_then(|m| match arg.split_whitespace().next().unwrap_or("") {
                    "curate" if self.memory_curating => Err("a memory curation is already running".into()),
                    "curate" => Ok("curating memories…".into()),
                    "episode" if self.capturing => Err("a memory capture is already running".into()),
                    "episode" => Ok("recording this conversation as an episode…".into()),
                    "reembed" => Ok("re-embedding memories in the background…".into()),
                    "project" => {
                        let name = arg.split_whitespace().nth(1);
                        match name {
                            None => Ok(m.project().map_or("no current project ([memory] project)".into(), |p| format!("current project: {p} (scope project:{p})"))),
                            Some("none") => {
                                m.set_project(None);
                                Ok("no current project: every project's memories stay out of prompts".into())
                            }
                            Some(p) => {
                                m.set_project(Some(p.to_string()));
                                Ok(format!("current project: {p} — its memories (project:{p}) are recalled, other projects' aren't"))
                            }
                        }
                    }
                    _ => m.command(arg),
                })
            }
            "/model" => self.model_command(arg),
            "/backup" => self.backup_command(arg),
            "/routine" | "/routines" => crate::acting::run(&self.owner.clone(), || self.routine_command(arg)),
            "/status" => self.status_command(arg),
            "/diagnose" => self.diagnose_command(arg),
            "/coding" => Ok(coding::describe()),
            "/briefing" => self.briefing_command(arg),
            "/calendar" => calendar::command(arg, &self.owner.clone()),
            "/mail" => crate::acting::run(&self.owner.clone(), || mail::command(arg)),
            "/today" => crate::acting::run(&self.owner.clone(), || planner::command(arg)),
            "/notes" | "/note" | "/list" => crate::acting::run(&self.owner.clone(), || notes::command(name, arg)),
            "/style" => {
                let (url, model) = (format!("{}/chat/completions", self.base_url.trim_end_matches('/')), self.model.clone());
                crate::acting::run(&self.owner.clone(), || style::command(arg, &url, &model))
            }
            // In the person's own PMI account.
            "/pmi" => pmi::as_user(&self.owner.clone(), || pmi::command(arg)),
            "/tasks" => pmi::as_user(&self.owner.clone(), || pmi::tasks_text(arg)),
            "/task" => pmi::as_user(&self.owner.clone(), || pmi::task_command(arg)),
            _ => Err(format!("unknown command {name} — try /help")),
        }
    }

    /// `/model [name]`: the model in use and what the endpoint offers, or a
    /// switch to another one (every conversation; saved to config.toml).
    pub(crate) fn model_command(&mut self, arg: &str) -> Result<String, String> {
        let name = arg.trim();
        let offered = models(&self.base_url);
        if name.is_empty() {
            return Ok(match offered {
                Ok(list) if !list.is_empty() => format!("model: {}\navailable: {}\n/model <name> switches", self.model, list.join(", ")),
                Ok(_) => format!("model: {} (the endpoint lists none)", self.model),
                Err(e) => format!("model: {} ({e})", self.model),
            });
        }
        if let Ok(list) = &offered
            && !list.is_empty()
            && !list.iter().any(|m| m == name)
        {
            return Err(format!("{name} isn't one the endpoint offers: {}", list.join(", ")));
        }
        let saved = config::update(|doc| {
            doc["model"] = toml_edit::value(name);
            Ok(())
        });
        self.model = name.to_string();
        self.log(Level::Info, format!("model: {name}"));
        Ok(match saved {
            Ok(_) => format!("now using {name} (saved to config.toml)"),
            Err(e) => format!("now using {name} until lyra restarts (not saved: {e})"),
        })
    }

    /// What follows a command that worked: background work it starts, panels to refresh.
    pub(crate) fn after_command(&mut self, line: &str, ok: bool) {
        let (name, arg) = line.split_once(' ').unwrap_or((line, ""));
        let memory_sub = arg.split_whitespace().next().unwrap_or("");
        match name {
            "/memory" if ok && memory_sub == "curate" => self.memory_curate(),
            "/memory" if ok && memory_sub == "episode" => self.capture(true),
            "/memory" if ok && memory_sub == "reembed" => {
                if let Some(mem) = self.mem() {
                    let tx = self.tx.clone();
                    thread::spawn(move || {
                        let mut notes = mem.backfill();
                        if notes.is_empty() {
                            notes.push("every memory already has a current vector".into());
                        }
                        let _ = tx.send(StreamEvent::MemoryNotes(notes));
                    });
                }
            }
            "/memory" if ok && matches!(memory_sub, "forget" | "archive" | "restore" | "purge" | "correct" | "approve" | "reject" | "working" | "project" | "backup") => {
                self.log(Level::Memory, line.to_string());
                self.refresh_memory();
            }
            "/learn" if ok => self.review(true),
            "/curate" if ok => self.curate(),
            "/approve" | "/reject" | "/deprecate" | "/forget-skill" | "/rollback" | "/outcome" if ok => {
                self.refresh_evolution();
                self.log(Level::Learn, line.to_string());
                self.refresh_skills();
            }
            _ => {}
        }
    }

    pub(crate) fn log_context(&mut self) {
        let text = match self.context_files.len() {
            0 => "no SOUL.md / USER.md / AGENT.md found".to_string(),
            n => format!("loaded {n} context file{}", if n == 1 { "" } else { "s" }),
        };
        self.log(Level::Info, text);
    }
}

/// Commands the app's pages and buttons may run (each still checked by role).
pub(crate) const PAGE_COMMANDS: &[&str] = &[
    "/memory", "/approve", "/reject", "/deprecate", "/goal", "/goals", "/model", "/backup", "/routine", "/routines", "/status", "/diagnose", "/coding",
    "/briefing", "/tasks", "/task", "/pmi", "/calendar", "/today", "/mail", "/notes", "/note", "/list", "/style", "/users", "/whoami", "/usage", "/sessions", "/plan", "/recap", "/watches", "/feedback",
];

/// Commands a member (not an admin) may use. Their own memories, goals,
/// routines, tasks and briefing come with per-user data; machines, system
/// tools, coding, devices, backups, agents and skills' approval stay admins'.
pub(crate) fn member_may(name: &str, arg: &str) -> bool {
    match name {
        "/help" | "/skills" | "/history" | "/sessions" | "/resume" | "/new" | "/status" | "/whoami" | "/usage" | "/recap" | "/watches" | "/watch" | "/feedback" => true,
        // Their own skills (the commands check whose each one is).
        "/approve" | "/reject" | "/deprecate" => true,
        // Their own PMI account, routines and goals.
        "/pmi" | "/tasks" | "/task" | "/routine" | "/routines" | "/calendar" | "/mail" | "/today" | "/style" | "/notes" | "/note" | "/list" => true,
        // Looking after their own memories (held to their scope there).
        "/memory" => matches!(arg.split_whitespace().next().unwrap_or(""), "inspect" | "forget" | "archive" | "restore" | "correct"),
        // Goals are tracked and planned, never worked on unattended: no plans,
        // triggers or autonomy for anyone but the owner's admins.
        "/goal" | "/goals" => !matches!(arg.split_whitespace().next().unwrap_or(""), "work" | "when" | "autonomy"),
        // Which model is in use; changing it is the server's.
        "/model" => arg.trim().is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod page_tests {
    /// Every command the app runs from a page or button (web/ui/src) is one it may run.
    #[test]
    fn the_apps_buttons_may_run_their_commands() {
        let mut found = Vec::new();
        let mut stack = vec![std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/web/ui/src"))];
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "tsx") {
                    let text = std::fs::read_to_string(&p).unwrap_or_default();
                    for start in ["run(\"/", "run(`/", "act(`/", "act(\"/"] {
                        for (i, _) in text.match_indices(start) {
                            let rest = &text[i + start.len() - 1..];
                            let cmd: String = rest.chars().take_while(|c| *c == '/' || c.is_ascii_lowercase() || *c == '-').collect();
                            found.push(cmd);
                        }
                    }
                }
            }
        }
        assert!(found.len() > 10, "found the app's commands: {found:?}");
        for c in &found {
            assert!(super::PAGE_COMMANDS.contains(&c.as_str()), "the app runs {c} but pages may not");
        }
    }
}

#[cfg(test)]
mod member_tests {
    #[test]
    fn members_keep_to_their_own() {
        for ok in ["/help", "/new", "/resume", "/sessions", "/status", "/whoami", "/skills"] {
            assert!(super::member_may(ok, ""), "{ok}");
        }
        assert!(super::member_may("/model", "") && !super::member_may("/model", "other-model"), "look, not change");
        assert!(super::member_may("/memory", "forget 1a2b") && !super::member_may("/memory", "curate") && !super::member_may("/memory", "approve 1a2b"));
        for no in ["/outcome", "/machines", "/devices", "/users", "/backup", "/caps", "/evolve", "/plan", "/agent", "/coding", "/diagnose"] {
            assert!(!super::member_may(no, "x"), "{no}");
        }
        // Their own routines and goals, but no plans or autonomy.
        assert!(super::member_may("/routine", "new x | every day at 8 | y") && super::member_may("/goals", "") && super::member_may("/goal", "new Learn Rust"));
        for no in ["work 1a2b", "when 1a2b every 1d", "autonomy autonomous"] {
            assert!(!super::member_may("/goal", no) && !super::member_may("/goals", no), "{no}");
        }
    }
}

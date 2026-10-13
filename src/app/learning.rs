//! What lyra learns from a conversation: how each reply went (verdicts,
//! ratings, the test set), skill reviews and curation, and memory: capture,
//! relations, upkeep and curation.

use crate::*;

impl App {
    /// Read the person's new message for how the last reply went: off the
    /// loop (the decision model, else the fallback model), the verdict comes
    /// back as `Judged`. Everyone's replies count, each person's for their own
    /// skills and memories.
    pub(crate) fn judge_last_run(&mut self, message: &str) {
        self.corrected_skills.clear();
        let Some(last) = self.last_run.take() else { return };
        // Already known for sure (rated, stopped, asked again): nothing to read.
        let known = self.run_ref(last.id).and_then(|r| r.verdict.clone()).or(last.verdict.clone());
        if known.as_ref().is_some_and(|v| v.strength >= outcomes::Strength::Signal) {
            return;
        }
        let asked = self.messages.iter().rev().find(|m| m.role == "user").map(|m| m.content.clone()).unwrap_or_default();
        let reply = self.messages.iter().rev().find(|m| m.role == "assistant").map(|m| m.content.clone()).unwrap_or_default();
        let since = (chrono::Utc::now() - last.at).to_std().unwrap_or_default();
        let (url, model, tx, next) = (format!("{}/chat/completions", self.base_url.trim_end_matches('/')), self.model.clone(), self.tx.clone(), message.to_string());
        let owner = self.owner.clone();
        crate::acting::spawn(move || {
            // Counted for this conversation's person.
            crate::acting::set(&owner);
            if let Some(verdict) = outcomes::judge(&asked, &reply, &next, since, Some((&url, &model))) {
                let _ = tx.send(StreamEvent::Judged { run: last.id, verdict });
            }
        });
    }

    /// A reply's run, wherever it is kept (its message, or the plan just run).
    pub(crate) fn run_ref(&self, id: Uuid) -> Option<&LastRun> {
        self.messages.iter().rev().filter_map(|m| m.run.as_ref()).find(|r| r.id == id).or(self.last_run.as_ref().filter(|r| r.id == id))
    }

    /// How a reply went, recorded everywhere that learns from it: its skills,
    /// its memories, the agents that worked on it, evolution. A weaker verdict
    /// than the one it has already changes nothing.
    pub(crate) fn apply_verdict(&mut self, id: Uuid, verdict: outcomes::Verdict) {
        let Some(run) = self.run_ref(id).cloned() else { return };
        let before = run.verdict.clone();
        if !verdict.replaces(before.as_ref()) || before.as_ref().is_some_and(|b| *b == verdict) {
            return;
        }
        // Kept with the reply (and the plan's run), so it isn't judged twice.
        if let Some(i) = self.messages.iter().rposition(|m| m.run.as_ref().is_some_and(|r| r.id == id)) {
            if let Some(r) = self.messages[i].run.as_mut() {
                r.verdict = Some(verdict.clone());
            }
            self.touch(i);
        }
        if let Some(r) = self.last_run.as_mut().filter(|r| r.id == id) {
            r.verdict = Some(verdict.clone());
        }
        self.save_session();
        let outcome = verdict.outcome;
        let source = match verdict.strength {
            outcomes::Strength::Rated => "rated",
            outcomes::Strength::Signal => "signal",
            outcomes::Strength::Judged => "judged",
            outcomes::Strength::Mild => "hint",
        };
        self.log(Level::Learn, format!("reply {source} {}: {}", verdict.word(), verdict.reason));
        // What they said goes with it, but only the owner's words reach the
        // shared evidence: anyone else's stay with their own skills.
        let feedback = match self.personal() {
            Some(_) => None,
            // Their words; else what they did (stopped it, asked again). A bare 👍 / 👎 adds none.
            None => verdict.said.clone().or_else(|| matches!(verdict.strength, outcomes::Strength::Signal | outcomes::Strength::Mild).then(|| verdict.reason.clone())),
        };
        self.judge_delegations(id, outcome, verdict.said.as_deref().unwrap_or(&verdict.reason));
        if let Some(evo) = run.evo {
            self.record_evolution_outcome(evo, outcome, verdict.correction, feedback.as_deref(), source);
        }
        if outcome == SkillOutcome::Failure && verdict.correction {
            self.corrected_skills = run.skills.clone();
            // What not to do next time, on the tool or skill it was about (4.2).
            if let Some(said) = verdict.said.clone() {
                let (reply, tools) = self.reply_of(id);
                let asked = self.messages.iter().rposition(|m| m.run.as_ref().is_some_and(|r| r.id == id)).and_then(|i| self.messages[..i].iter().rev().find(|m| m.role == "user")).map(|m| m.content.clone()).unwrap_or_default();
                let skills: Vec<String> = run.skills.iter().map(|s| s.trim_end_matches(" (trial)").to_string()).collect();
                let (url, model, tx, owner) = (format!("{}/chat/completions", self.base_url.trim_end_matches('/')), self.model.clone(), self.tx.clone(), self.personal());
                crate::acting::spawn(move || {
                    let Some((tool, skill, note)) = avoid::learn(&url, &model, &asked, &reply, &said, &tools, &skills) else { return };
                    let text = match avoid::add(tool.as_deref(), skill.as_deref(), &note, owner.as_deref()) {
                        Ok((a, true)) => format!("learned what to avoid with {}: {}", a.tool.as_deref().or(a.skill.as_deref()).unwrap_or(""), a.note),
                        Ok((a, false)) => format!("avoid note on {} confirmed again ({}×)", a.tool.as_deref().or(a.skill.as_deref()).unwrap_or(""), a.count),
                        Err(_) => return,
                    };
                    let _ = tx.send(StreamEvent::LearnNotes(vec![text]));
                });
            }
        }
        // Were the memories in that reply's prompt helpful? (M13) A partial says neither.
        if run.memories > 0
            && outcome != SkillOutcome::Partial
            && let Some(mem) = self.mem()
        {
            let (tx, helpful) = (self.tx.clone(), outcome == SkillOutcome::Success);
            crate::acting::spawn(move || {
                let note = match mem.feedback(id, helpful) {
                    Ok(n) => format!("{n} memor{} marked {}", if n == 1 { "y" } else { "ies" }, if helpful { "helpful" } else { "not helpful" }),
                    Err(e) => format!("memory feedback failed: {e}"),
                };
                let _ = tx.send(StreamEvent::MemoryNotes(vec![note]));
            });
        }
        // The owner's 👍: the request and that answer join lyra's tests.
        if verdict.strength == outcomes::Strength::Rated
            && outcome == SkillOutcome::Success
            && self.personal().is_none()
            && let Ok(true) = self.test_from(id, "rated")
        {
            self.log(Level::Evolve, "added to lyra's tests (a reply you liked)".into());
        }
        let Some(learning) = self.learning.clone() else { return };
        if run.skills.is_empty() {
            return;
        }
        // A verdict replacing another overwrites it in the ledger too.
        let overwrite = before.is_some() || verdict.strength == outcomes::Strength::Rated;
        // Only the skills the reply followed get the credit (E2.1).
        let (reply, tools) = self.reply_of(id);
        let (url, model, tx) = (format!("{}/chat/completions", self.base_url.trim_end_matches('/')), self.model.clone(), self.tx.clone());
        let owner = self.owner.clone();
        crate::acting::spawn(move || {
            crate::acting::set(&owner);
            let described: Vec<(String, String)> = run
                .skills
                .iter()
                .map(|n| {
                    let name = n.trim_end_matches(" (trial)").to_string();
                    let what = learning.find(&name).map(|s| format!("{} {}", s.description, s.instructions.chars().take(200).collect::<String>())).unwrap_or_default();
                    (name, what)
                })
                .collect();
            let notes = match outcomes::followed(&reply, &tools, &described, Some((&url, &model))) {
                Some(names) => {
                    let skipped: Vec<&str> = described.iter().map(|(n, _)| n.as_str()).filter(|n| !names.iter().any(|x| x == n)).collect();
                    let mut notes = learning.record_outcome_for(id, &names, outcome, overwrite).unwrap_or_else(|e| vec![format!("recording outcome failed: {e}")]);
                    if !skipped.is_empty() {
                        notes.push(format!("no credit for {} (the reply didn't follow {})", skipped.join(", "), if skipped.len() == 1 { "it" } else { "them" }));
                    }
                    notes
                }
                None => learning.record_outcome(id, outcome, overwrite).unwrap_or_else(|e| vec![format!("recording outcome failed: {e}")]),
            };
            let _ = tx.send(StreamEvent::LearnNotes(notes));
        });
    }

    /// A routine or a goal, when the request shows a pattern (4.5): looked
    /// for off the loop in this person's own saved conversations.
    pub(crate) fn offer_pattern(&self) {
        if self.unattended {
            return;
        }
        let Some(message) = self.messages.iter().rev().find(|m| m.role == "user").map(|m| m.content.clone()) else { return };
        let (owner, session, tx) = (self.owner.clone(), self.session_id.clone(), self.tx.clone());
        crate::acting::spawn(move || {
            let since = chrono::Utc::now() - chrono::Duration::days(14);
            let earlier: Vec<(String, chrono::DateTime<chrono::Utc>)> = sessions::dir()
                .map(|d| sessions::list_for(&d, &owner))
                .unwrap_or_default()
                .into_iter()
                .filter(|s| s.updated > since && s.kind() == "chat")
                .flat_map(|s| {
                    let at = s.updated;
                    let mine = s.id == session;
                    let mut asked: Vec<String> = s.messages.into_iter().filter(|m| m.role == "user").map(|m| m.content).collect();
                    // This conversation: not the message itself.
                    if mine {
                        asked.pop();
                    }
                    asked.into_iter().map(move |t| (t, at))
                })
                .collect();
            let routines: Vec<String> = crate::acting::run(&owner, crate::routines::list).into_iter().map(|r| r.prompt).collect();
            if let Some(o) = patterns::offer(&owner, &message, &earlier, &routines) {
                let _ = tx.send(StreamEvent::Suggest(o));
            }
        });
    }

    /// A reply's text and the tools its turn used, by its run.
    pub(crate) fn reply_of(&self, id: Uuid) -> (String, Vec<String>) {
        let Some(i) = self.messages.iter().rposition(|m| m.run.as_ref().is_some_and(|r| r.id == id)) else { return (String::new(), Vec::new()) };
        let start = self.messages[..i].iter().rposition(|m| m.role == "user").map_or(0, |u| u + 1);
        let tools = self.messages[start..=i].iter().flat_map(|m| m.tool_calls.iter().map(|c| c.function.name.clone())).collect();
        (self.messages[i].content.clone(), tools)
    }

    /// The request a reply answered, the reply, and the turns before, as a
    /// test (`source`: "rated", "added"). `false`: it's a test already.
    pub(crate) fn test_from(&self, id: Uuid, source: &str) -> Result<bool, String> {
        let ev = self.evolution.as_ref().ok_or("evolution is off ([evolution] enabled)")?;
        let i = self.messages.iter().rposition(|m| m.run.as_ref().is_some_and(|r| r.id == id)).ok_or("that reply can't be a test")?;
        let u = self.messages[..i].iter().rposition(|m| m.role == "user").ok_or("there's no request before it")?;
        let before: Vec<&Message> = self.messages[..u].iter().filter(|m| matches!(m.role.as_str(), "user" | "assistant") && !m.content.trim().is_empty()).collect();
        let history = before.iter().rev().take(4).rev().map(|m| (m.role.clone(), m.content.chars().take(800).collect())).collect();
        evolve::add_test(ev, &self.messages[u].content, Some(&self.messages[i].content), history, source)
    }

    /// The person's 👍 / 👎 on a reply (`index`: its message; `None`: the
    /// last one rated or ratable), with what was wrong; `copied` / `read` are
    /// the hints a reply was useful. Returns what to say.
    pub(crate) fn rate(&mut self, index: Option<usize>, word: &str, note: &str) -> Result<String, String> {
        let at = match index {
            Some(i) => Some(i).filter(|i| self.messages.get(*i).is_some_and(|m| m.run.is_some())).ok_or("that reply can't be rated")?,
            None => match self.messages.iter().rposition(|m| m.run.is_some()) {
                Some(i) => i,
                // The plan just run (it has no reply of its own).
                None => return self.rate_plan(word, note),
            },
        };
        let id = self.messages[at].run.as_ref().map(|r| r.id).ok_or("that reply can't be rated")?;
        // An admin's "add to tests": this request and reply join the test set.
        if word == "test" {
            if !self.admin {
                return Err("adding tests is for admins".into());
            }
            return match self.test_from(id, "added")? {
                true => {
                    self.log(Level::Evolve, "added a reply to lyra's tests".into());
                    Ok("added to lyra's tests: changes are measured on it from now on".into())
                }
                false => Ok("that's one of lyra's tests already".into()),
            };
        }
        let verdict = verdict_for(word, note)?;
        let said = verdict.word();
        self.apply_verdict(id, verdict);
        Ok(format!("recorded: that reply was {said}{}", if note.trim().is_empty() { "" } else { " (with what was wrong)" }))
    }

    pub(crate) fn rate_plan(&mut self, word: &str, note: &str) -> Result<String, String> {
        let id = self.last_run.as_ref().map(|r| r.id).ok_or("no reply to rate yet")?;
        let verdict = verdict_for(word, note)?;
        let said = verdict.word();
        self.apply_verdict(id, verdict);
        Ok(format!("recorded: that run was {said}"))
    }

    /// Curate the skill collection in the background.
    pub(crate) fn curate(&mut self) {
        let Some(learning) = self.learning.clone() else { return };
        if self.curating {
            return;
        }
        self.curating = true;
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let (model, tx) = (self.model.clone(), self.tx.clone());
        crate::acting::spawn(move || {
            let _ = tx.send(StreamEvent::Curated(learning.curate(&url, &model, None)));
        });
    }

    /// Re-read the skills panel's contents in the background.
    pub(crate) fn refresh_skills(&self) {
        let Some(learning) = self.learning.clone() else { return };
        let tx = self.tx.clone();
        crate::acting::spawn(move || {
            let _ = tx.send(StreamEvent::Skills(learning.snapshot()));
        });
    }

    /// The model-visible conversation as (role, text) pairs, with tool calls spelled out.
    pub(crate) fn history_text(&self) -> Vec<(&str, String)> {
        self.messages
            .iter()
            .filter(|m| m.is_history())
            .map(|m| {
                let mut text = m.content.clone();
                for call in &m.tool_calls {
                    text += &format!("\n→ {} {}", call.function.name, call.function.arguments);
                }
                (m.role.as_str(), text)
            })
            .collect()
    }

    /// Review the conversation for a reusable lesson in the background: after
    /// every turn when a trigger fires, or on /learn (`forced`). While one runs,
    /// the next waits in a queue (3.4): one per turn, a few at most.
    pub(crate) fn review(&mut self, forced: bool) {
        // The owner's lessons become shared skills; anyone else's are theirs alone.
        let owner = self.personal();
        let Some(learning) = self.learning.clone() else { return };
        if learning.mode() == Mode::Off {
            return;
        }
        let owned = self.history_text();
        let history: Vec<(&str, &str)> = owned.iter().map(|(r, c)| (*r, c.as_str())).collect();
        let trigger = if forced {
            Some("the user asked to review this conversation for a lesson")
        } else {
            learn::last_turn(&history, self.corrected_skills.len()).and_then(|turn| evaluator::trigger(&turn))
        };
        // Nothing the keywords catch: the decision model (when set up) looks at the turn.
        let ask_decide = trigger.is_none() && decide::model().is_some() && history.len() >= 2;
        if trigger.is_none() && !ask_decide {
            return;
        }
        let mut transcript = learn::transcript(&history, 16);
        if !self.corrected_skills.is_empty() {
            transcript += &format!(
                "\n\n[skills that were in the prompt for the corrected answer] {}",
                self.corrected_skills.join(", ")
            );
        }
        let run = self.last_run.as_ref().map(|r| r.id);
        let job = QueuedReview { key: run.unwrap_or_else(Uuid::new_v4), trigger, transcript, run, owner, gate: learn::transcript(&history, 16) };
        if self.reviewing {
            if self.review_queue.iter().any(|q| q.key == job.key) || self.review_queue.len() >= MAX_QUEUED_REVIEWS {
                return;
            }
            self.log(Level::Learn, format!("a review is waiting for the one running ({} queued)", self.review_queue.len() + 1));
            self.review_queue.push_back(job);
            return;
        }
        self.start_review(learning, job);
    }

    /// The next queued review, if any (after one finishes).
    pub(crate) fn next_review(&mut self) {
        self.reviewing = false;
        if let (Some(learning), Some(job)) = (self.learning.clone(), self.review_queue.pop_front()) {
            self.start_review(learning, job);
        }
    }

    pub(crate) fn start_review(&mut self, learning: Arc<Learning>, job: QueuedReview) {
        let QueuedReview { key, trigger, transcript, run, owner, gate } = job;
        if let Some(trigger) = trigger {
            self.log(Level::Learn, format!("reviewing for a lesson: {trigger}"));
        }
        self.reviewing = true;
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let (model, tx) = (self.model.clone(), self.tx.clone());
        let (person, session) = (self.owner.clone(), self.session_id.clone());
        crate::acting::spawn(move || {
            // For this conversation's person (its notes and calls are theirs).
            crate::acting::set(&person);
            crate::usage::set_job(Some(session));
            let trigger = match trigger {
                Some(t) => t,
                None => match decide::gate_both(&key.to_string(), &gate, LESSON_GATE, MEMORY_GATE, (&url, &model)).0 {
                    Some((true, "the decision model")) => "the decision model saw a reusable lesson",
                    Some((true, _)) => "the fallback model saw a reusable lesson",
                    _ => {
                        let _ = tx.send(StreamEvent::GateClosed { memory: false });
                        return;
                    }
                },
            };
            let review = learning.review(&url, &model, trigger, &transcript, run, owner.as_deref());
            let _ = tx.send(StreamEvent::Reviewed(review));
        });
    }

    /// What the decision model decided (or that it's down), into Activity.
    pub(crate) fn decide_notes(&mut self) {
        for (user, session, note) in decide::take_notes() {
            // Down is a warning (its checks moved to the fallback model).
            let level = if note.starts_with('⚠') { Level::Error } else { Level::Info };
            // Kept as theirs whose reply it was about; shown here too.
            if user == self.owner {
                self.log(level, note);
            } else {
                self.log_for(&user, &session, level, note);
            }
        }
    }

    /// Re-read the memory panel's numbers in the background.
    pub(crate) fn refresh_memory(&self) {
        let Some(mem) = self.mem() else { return };
        let tx = self.tx.clone();
        crate::acting::spawn(move || {
            let _ = tx.send(StreamEvent::Memory(mem.snapshot()));
        });
    }

    pub(crate) fn mem(&self) -> Option<Arc<Mem>> {
        self.tools.as_ref().map(|t| t.mem.clone())
    }

    /// Relate a memory the model just saved to similar ones, in the background.
    pub(crate) fn relate_memory(&self, short_id: String) {
        let Some(mem) = self.mem() else { return };
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let (model, tx) = (self.model.clone(), self.tx.clone());
        crate::acting::spawn(move || {
            let review = match mem.run(mem.manager.find(&short_id)) {
                Ok(m) => mem.relate(&url, &model, m.id),
                Err(e) => Review { outcome: Err(e), usage: None },
            };
            let _ = tx.send(StreamEvent::MemoryRelated(review));
        });
    }

    /// Archive expired memories and add missing vectors, in the background.
    pub(crate) fn memory_upkeep(&self) {
        let Some(mem) = self.mem() else { return };
        let tx = self.tx.clone();
        crate::acting::spawn(move || {
            let _ = tx.send(StreamEvent::MemoryNotes(mem.upkeep()));
        });
    }

    /// Review the turn just finished for things worth remembering (M2, M8):
    /// after every turn when the capture trigger fires, or on `/memory episode`.
    pub(crate) fn capture(&mut self, episode: bool) {
        // Anyone but the owner: captured into their own memories only.
        let scope = self.personal().map(|u| format!("user:{u}"));
        let Some(mem) = self.mem() else { return };
        if self.capturing || (!episode && mem.manager.settings().capture == CaptureMode::Off) {
            return;
        }
        let Some(start) = self.messages.iter().rposition(|m| m.role == "user") else { return };
        let calls: Vec<&str> =
            self.messages[start..].iter().flat_map(|m| m.tool_calls.iter().map(|c| c.function.name.as_str())).collect();
        let user = self.messages[start].content.clone();
        let turn = capture::Turn {
            user: &user,
            task_tool_calls: calls.iter().filter(|n| !n.starts_with("memory_") && **n != "working_memory").count(),
            model_saved: calls.iter().any(|n| matches!(*n, "memory_remember" | "memory_correct" | "memory_supersede")),
        };
        let reason = if episode {
            Some("the user asked to record this conversation as an episode; summarize it in \"episode\"")
        } else if outcomes::told_before(&user) {
            // A memory miss (4.6): what they'd said before is worth keeping now.
            Some("the person said they had told lyra this before: remember what they said, so it isn't missed again")
        } else {
            capture::trigger(&turn)
        };
        // Nothing the keywords catch: the decision model (when set up) looks at the turn.
        let ask_decide = reason.is_none() && !turn.model_saved && decide::model().is_some();
        if reason.is_none() && !ask_decide {
            return;
        }
        let owned = self.history_text();
        let history: Vec<(&str, &str)> = owned.iter().map(|(r, c)| (*r, c.as_str())).collect();
        let transcript = learn::transcript(&history, if episode { 30 } else { 8 });
        // Asked with the lesson gate, in one question (whichever comes first asks both).
        let gate = learn::transcript(&history, 16);
        let gate_key = self.last_run.as_ref().map_or_else(|| format!("{}-{}", self.session_id, self.messages.len()), |r| r.id.to_string());
        let reply = self.messages.iter().rev().find(|m| m.role == "assistant").map_or("", |m| m.content.as_str());
        let query = format!("{user}\n{reply}");
        if let Some(reason) = reason {
            self.log(Level::Memory, format!("capturing: {reason}"));
        }
        self.capturing = true;
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let (model, tx, run) = (self.model.clone(), self.tx.clone(), self.last_run.as_ref().map(|r| r.id));
        let started = self.session_started;
        let owner = self.owner.clone();
        crate::acting::spawn(move || {
            // Counted (and kept) for the conversation's person.
            crate::acting::set(&owner);
            let reason = match reason {
                Some(r) => r,
                None => match decide::gate_both(&gate_key, &gate, LESSON_GATE, MEMORY_GATE, (&url, &model)).1 {
                    Some((true, "the decision model")) => "the decision model found something worth remembering",
                    Some((true, _)) => "the fallback model found something worth remembering",
                    _ => {
                        let _ = tx.send(StreamEvent::GateClosed { memory: true });
                        return;
                    }
                },
            };
            let review = mem.capture(&url, &model, reason, &transcript, &query, run, Some(started), scope.as_deref());
            let _ = tx.send(StreamEvent::MemoryReview { curation: false, review });
        });
    }

    /// Curate the memory collection in the background (M9, M18).
    pub(crate) fn memory_curate(&mut self) {
        let Some(mem) = self.mem() else { return };
        if self.memory_curating {
            return;
        }
        self.memory_curating = true;
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let (model, tx) = (self.model.clone(), self.tx.clone());
        crate::acting::spawn(move || {
            let review = mem.curate(&url, &model);
            let _ = tx.send(StreamEvent::MemoryReview { curation: true, review });
        });
    }
}

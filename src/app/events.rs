//! What a turn sends back as it runs (`StreamEvent`): tokens, tool calls and
//! their results, agents, approvals, the end of the reply and what follows it.

use crate::*;

impl App {
    pub(crate) fn handle(&mut self, event: StreamEvent) {
        match event {
            StreamEvent::Agent(e) => self.agent_event(e),
            StreamEvent::Approval(r) => self.approval_requested(r),
            StreamEvent::Ask(r) => {
                let what = match &r.kind {
                    asks::Kind::Fill { tool, fields, .. } => format!("{tool} needs {}: asking", fields.iter().map(|f| f.label.to_lowercase()).collect::<Vec<_>>().join(", ")),
                    asks::Kind::Steps { steps, .. } => format!("{} steps first: {}", steps.len(), steps.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(", ")),
                };
                self.log(Level::Agent, what);
                self.asks.push(r);
            }
            StreamEvent::AskDone(id) => self.asks.retain(|r| r.id != id),
            StreamEvent::ToolArgs { id, arguments } => {
                if let Some(i) = self.messages.iter().rposition(|m| m.tool_calls.iter().any(|c| c.id == id)) {
                    if let Some(c) = self.messages[i].tool_calls.iter_mut().find(|c| c.id == id) {
                        c.function.arguments = arguments;
                    }
                    self.touch(i);
                }
            }
            StreamEvent::AgentBuilt(built) => self.agent_built(built),
            StreamEvent::Token(t) => {
                self.set_phase(Phase::Streaming);
                self.reply().content.push_str(&t);
            }
            StreamEvent::Reasoning(t) => {
                self.set_phase(Phase::Thinking);
                self.reply().reasoning.push_str(&t);
            }
            StreamEvent::ToolCalls(calls) => {
                for call in &calls {
                    let text = format!("{} {}", call.function.name, call.function.arguments);
                    self.log(Level::Tool, text);
                }
                let names: Vec<_> = calls.iter().map(|c| c.function.name.as_str()).collect();
                self.set_phase(Phase::Tools(names.join(", ")));
                self.reply().tool_calls = calls;
            }
            StreamEvent::ToolResult { id, name, content } => {
                self.log(Level::Tool, format!("↳ {content}"));
                self.set_phase(Phase::Waiting);
                if name.starts_with("memory_") || name == "working_memory" {
                    self.refresh_memory();
                }
                if name.starts_with("goal_") {
                    self.refresh_goals();
                }
                // A new fact may update, contradict or support what's known (M3, M4).
                if name == "memory_remember"
                    && let Ok(v) = serde_json::from_str::<Value>(&content)
                    && v["result"] == "remembered"
                    && let Some(id) = v["id"].as_str()
                {
                    self.relate_memory(id.to_string());
                }
                let mut result = Message::new("tool", content);
                result.tool_call_id = Some(id);
                self.messages.push(result);
            }
            StreamEvent::Done(stats) => {
                let was_stopped = stopped(&self.cancel);
                if was_stopped {
                    let reply = self.reply();
                    reply.content.push_str(if reply.content.is_empty() { "_(stopped)_" } else { "\n\n_(stopped)_" });
                }
                self.waiting = false;
                self.started = None;
                self.set_phase(Phase::Idle);
                self.log(
                    Level::Info,
                    format!("done · {} out tokens · {}", stats.output, secs(stats.elapsed)),
                );
                self.totals.add(&stats);
                let skills = self.applied_skills.clone();
                let memories = self.applied_memories.clone();
                let evo = self.record_chat_run(&stats, None);
                // Working memory tracks what both sides mention (M7): the owner's only.
                if let Some(mem) = self.mem().filter(|_| self.personal().is_none()) {
                    let text = self.reply().content.clone();
                    mem.working().note_entities(&text);
                }
                let reply = self.reply();
                reply.stats = Some(stats);
                reply.skills = skills.clone();
                reply.memories = memories.clone();
                if self.show_handled_by {
                    let handled = self.handled_by.clone();
                    self.reply().agents = handled;
                }
                self.last_run = self.run.take().map(|id| LastRun::new(id, skills, memories.len(), evo));
                let run = self.last_run.clone();
                self.reply().run = run;
                // Stopped part way: it wasn't going where they wanted.
                if was_stopped && let Some(id) = self.last_run.as_ref().map(|r| r.id) {
                    self.apply_verdict(id, outcomes::Verdict::new(SkillOutcome::Failure, outcomes::Strength::Signal, "they stopped it part way"));
                }
                self.save_session();
                self.review(false);
                self.capture(false);
                self.offer_pattern();
                // A long conversation is summarized now, not when the next message waits for it.
                self.precompact();
            }
            StreamEvent::Limit(rounds) => {
                let own = limits::own_tool_rounds(&self.owner).is_some();
                self.log(Level::Agent, format!("stopped at the tool-call limit ({rounds} rounds{}): it can be continued", if own { ", this person's own" } else { "" }));
                let done = self.done_so_far().map(|d| format!("\n{d}")).unwrap_or_default();
                self.messages.push(Message::new("info", format!("{}{done}", limits::stopped_note(rounds, own, self.admin))));
                self.can_continue = true;
                self.continue_after_error = false;
                // Only part done: so recorded, and the run notes it hit the limit.
                if let Some(last) = self.last_run.clone() {
                    // Before the outcome is written (both change the same record).
                    if let (Some(evo), Some(evolution)) = (last.evo, self.evolution.clone())
                        && let Err(e) = evolution.manager.mark_limited(evo, rounds as u32)
                    {
                        self.log(Level::Error, format!("noting the limit on the run failed: {e}"));
                    }
                    self.apply_verdict(last.id, outcomes::Verdict::new(SkillOutcome::Partial, outcomes::Strength::Signal, format!("it stopped at the tool-call limit ({rounds} rounds)")));
                }
                self.save_session();
            }
            StreamEvent::Error(e) => {
                let stats = Stats {
                    ttft: None,
                    elapsed: self.started.map(|s| s.elapsed()).unwrap_or_default(),
                    input: 0,
                    cached: 0,
                    output: 0,
                    estimated: true,
                };
                self.record_chat_run(&stats, Some(&e));
                self.waiting = false;
                self.started = None;
                self.run = None;
                self.last_run = None;
                self.set_phase(Phase::Idle);
                self.log(Level::Error, e.clone());
                // What was done before it broke stays: Keep going picks it up (or Try again).
                let done = self.done_so_far().map(|d| format!("\n\n{d}")).unwrap_or_default();
                self.messages.push(Message::new("error", format!("{e}{done}")));
                self.can_continue = self.messages.iter().any(|m| m.role == "user");
                self.continue_after_error = true;
                self.save_session();
            }
            StreamEvent::Log(text) => self.log(Level::Info, text),
            StreamEvent::Seen(text) => {
                // With the message, so later turns know it too.
                if let Some(m) = self.messages.iter_mut().rev().find(|m| m.role == "user") {
                    m.content.push_str(&text);
                }
            }
            StreamEvent::Notice(text) => {
                self.log(Level::Agent, text.clone());
                self.messages.push(Message::new("info", text));
            }
            StreamEvent::Picture { id, data } => {
                let mut m = Message::new("picture", data);
                m.tool_call_id = Some(id);
                self.messages.push(m);
            }
            StreamEvent::GoalsLooked { notes, resume } => self.goals_looked(notes, resume),
            StreamEvent::Compacted { upto, summary } => {
                // Made in the background after an older reply: a newer note (the turn's own) stays.
                if self.compacted.as_ref().is_some_and(|(n, _)| *n >= upto) {
                    return;
                }
                let words = summary.split_whitespace().count();
                self.log(Level::Memory, format!("conversation compacted: its first {upto} messages summarized in {words} words to fit the model's context"));
                self.messages.push(Message::new("info", format!("The older part of this conversation was summarized ({upto} messages, {words} words) so it fits the model's context; everything stays here.")));
                self.compacted = Some((upto, summary));
                self.save_session();
            }
            StreamEvent::Models(results) => {
                for result in &results {
                    match result {
                        Ok(line) => self.log(Level::Info, line.clone()),
                        Err(line) => self.log(Level::Error, line.clone()),
                    }
                }
                self.model_status = Some(results);
            }
            StreamEvent::SkillsApplied { names, tokens } => {
                self.log(Level::Learn, format!("applying skills: {}", names.join(", ")));
                self.applied_skills = names;
                self.applied_skills_tokens = tokens;
            }
            StreamEvent::Reviewed(Review { outcome, usage }) => {
                self.next_review();
                self.totals.add_review(usage.as_ref());
                let tokens = usage_text(usage.as_ref());
                match outcome {
                    Ok(Applied::Created(skill)) => {
                        let id = learn::short_id(skill.id);
                        self.log(Level::Learn, format!("learned {} (proposed) · {tokens}", skill.name));
                        let text = format!(
                            "💡 learned a new skill (proposed): {} — {}\n{}\n/approve {id} to start using it · /reject {id} to discard it",
                            skill.name, skill.description, skill.instructions
                        );
                        self.messages.push(Message::new("info", text));
                    }
                    Ok(Applied::Updated { skill, version }) => {
                        self.log(Level::Learn, format!("refined {} to v{version} · {tokens}", skill.name));
                        let text = format!(
                            "🔧 refined skill {} (v{version}):\n{}\n/history {} · /rollback {}",
                            skill.name, skill.instructions, skill.name, skill.name
                        );
                        self.messages.push(Message::new("info", text));
                    }
                    Ok(Applied::Proposed(p)) => {
                        let id = learn::short_id(p.id);
                        self.log(Level::Learn, format!("proposal: {} · {tokens}", p.change.kind()));
                        let detail = match &p.change {
                            lyra_learning::proposal::Change::Update { instructions, .. } => instructions.clone(),
                            _ => String::new(),
                        };
                        let text = format!(
                            "🔧 proposed a refinement: {}\n{detail}\n/approve {id} to apply it · /reject {id} to discard it",
                            p.reason
                        );
                        self.messages.push(Message::new("info", text));
                    }
                    Ok(Applied::Ignored(why)) => self.log(Level::Learn, format!("no lesson: {why} · {tokens}")),
                    Err(e) => self.log(Level::Error, format!("learning review failed: {e} · {tokens}")),
                }
                self.refresh_skills();
            }
            StreamEvent::Curated(Review { outcome, usage }) => {
                self.curating = false;
                self.totals.add_review(usage.as_ref());
                match outcome {
                    Ok(notes) => {
                        for note in &notes {
                            self.log(Level::Learn, format!("curator: {note}"));
                        }
                        let summary = if notes.is_empty() {
                            "🧹 curated the skills: nothing to change".to_string()
                        } else {
                            format!("🧹 curated the skills:\n{}\n/skills to review", notes.join("\n"))
                        };
                        self.messages.push(Message::new("info", summary));
                    }
                    Err(e) => self.log(Level::Error, format!("curation failed: {e} · {}", usage_text(usage.as_ref()))),
                }
                self.refresh_skills();
            }
            StreamEvent::LearnNotes(notes) => {
                for note in notes {
                    self.log(Level::Learn, note);
                }
                self.refresh_skills();
            }
            StreamEvent::Skills(snapshot) => {
                // Skills are capabilities too.
                if let (Ok(s), Some(caps)) = (&snapshot, &self.caps)
                    && s.active.len() != caps.manager.all().iter().filter(|c| c.kind == lyra_capabilities::CapabilityKind::Skill).count()
                {
                    self.refresh_caps(false);
                }
                if let Err(e) = &snapshot {
                    self.log(Level::Error, format!("skills: {e}"));
                }
                self.skills = Some(snapshot);
            }
            StreamEvent::MemoriesApplied { ids, texts, tokens } => {
                // What they say, for the app's sources under the reply.
                for (id, text) in ids.iter().zip(texts) {
                    self.memory_texts.insert(id.clone(), text);
                }
                self.log(Level::Memory, format!("recalled {} memor{}: {}", ids.len(), if ids.len() == 1 { "y" } else { "ies" }, ids.join(" ")));
                self.applied_memories = ids;
                self.applied_memories_tokens = tokens;
            }
            StreamEvent::BackedUp { nightly, result } => {
                let text = match &result {
                    Ok((b, notes)) => {
                        for n in notes {
                            self.log(Level::Info, format!("backup: {n}"));
                        }
                        format!("💾 backed up lyra: {} ({})", b.name, backup::size_text(b.size))
                    }
                    Err(e) => format!("backup failed: {e}"),
                };
                self.log(if result.is_ok() { Level::Info } else { Level::Error }, text.clone());
                if !nightly {
                    self.messages.push(Message::new(if result.is_ok() { "info" } else { "error" }, text));
                }
            }
            StreamEvent::Judged { run, verdict } => self.apply_verdict(run, verdict),
            StreamEvent::Suggest(offer) => {
                self.log(Level::Plan, format!("offering: {}", offer.command));
                self.messages.push(Message::new("suggest", format!("{}\n\n{}", offer.text, offer.command)));
            }
            StreamEvent::GateClosed { memory } => {
                if memory {
                    self.capturing = false;
                } else {
                    self.next_review();
                }
            }
            StreamEvent::MemoryReview { curation, review: Review { outcome, usage } } => {
                if curation {
                    self.memory_curating = false;
                } else {
                    self.capturing = false;
                }
                self.totals.add_review(usage.as_ref());
                let what = if curation { "memory curation" } else { "memory capture" };
                match outcome {
                    Ok(notes) => {
                        if notes.is_empty() {
                            self.log(Level::Memory, format!("{what}: nothing to change · {}", usage_text(usage.as_ref())));
                        }
                        for note in &notes {
                            self.log(Level::Memory, note.clone());
                        }
                        if curation {
                            let text = if notes.is_empty() {
                                "🧠 curated memories: nothing to change".to_string()
                            } else {
                                format!("🧠 curated memories:\n{}\n/memory to review", notes.join("\n"))
                            };
                            self.messages.push(Message::new("info", text));
                        }
                    }
                    Err(e) => self.log(Level::Error, format!("{what} failed: {e} · {}", usage_text(usage.as_ref()))),
                }
                self.refresh_memory();
            }
            StreamEvent::PlanEvent(e) => {
                self.log(Level::Plan, format!("{}: {}", e.kind, e.message));
                self.reload_plan(e.plan_id);
            }
            StreamEvent::PlanCreated(result) => {
                self.plan_busy = false;
                match result {
                    Ok((goal, plan)) => {
                        let mut text = plan::describe(Some(&goal), &plan);
                        text += "\n/plan run to start · /plan cancel to drop it";
                        self.log(Level::Plan, format!("planned {} steps for: {}", plan.steps.len(), goal.description));
                        self.messages.push(Message::new("info", text));
                        self.current_goal = Some(goal);
                        self.current_plan = Some(plan);
                    }
                    Err(e) => {
                        self.log(Level::Error, format!("planning failed: {e}"));
                        self.messages.push(Message::new("error", format!("couldn't plan that: {e}")));
                    }
                }
            }
            StreamEvent::PlanFinished(result) => {
                self.plan_busy = false;
                self.set_phase(Phase::Idle);
                match result {
                    Ok(outcome) => self.plan_finished(outcome),
                    Err(e) => {
                        self.log(Level::Error, format!("plan run failed: {e}"));
                        self.messages.push(Message::new("error", format!("plan run failed: {e}")));
                    }
                }
            }
            StreamEvent::PlanNotes(notes) => {
                for note in notes {
                    self.log(Level::Plan, note);
                }
            }
            StreamEvent::MemoryRelated(Review { outcome, usage }) => {
                if usage.is_some() {
                    self.totals.add_review(usage.as_ref());
                }
                match outcome {
                    Ok(notes) => {
                        for note in notes {
                            self.log(Level::Memory, note);
                        }
                    }
                    Err(e) => self.log(Level::Error, format!("relating a memory failed: {e}")),
                }
                self.refresh_memory();
            }
            StreamEvent::MemoryNotes(notes) => {
                for note in notes {
                    self.log(Level::Memory, note);
                }
                self.refresh_memory();
            }
            StreamEvent::Evolved { done, show } => {
                self.evolving = None;
                for usage in &done.usage {
                    self.totals.add_review(Some(usage));
                }
                for note in &done.notes {
                    let level = if note.starts_with('✗') || note.contains("failed") { Level::Error } else { Level::Evolve };
                    self.log(level, note.clone());
                }
                if show && !done.notes.is_empty() {
                    self.messages.push(Message::new("info", format!("🧬 {}", done.notes.join("\n"))));
                }
                // A scheduled review's digest: to the owner's devices, opening the Proposals page.
                if let Some(digest) = done.digest {
                    self.log(Level::Evolve, format!("review digest: {}", digest.replace('\n', " · ")));
                    if let Some(hub) = &self.hub {
                        hub.notify(lyra_web::Notification {
                            title: "lyra's review: what's ready".into(),
                            body: digest,
                            tag: "evolution-digest".into(),
                            approval: None,
                            actions: vec![],
                            reference: None,
                            to: lyra_web::To::User(lyra_web::users::OWNER.into()),
                            url: Some("/?page=proposals".into()),
                        });
                    }
                }
                self.tools_changed();
                self.refresh_evolution();
                self.refresh_skills();
            }
            StreamEvent::GoalNotes { notes, show } => {
                self.goals_busy = false;
                for note in &notes {
                    self.log(Level::Plan, note.clone());
                }
                if show && !notes.is_empty() {
                    self.messages.push(Message::new("info", format!("🎯 {}", notes.join("\n"))));
                }
                self.refresh_goals();
            }
            StreamEvent::GoalPlan { result, autonomous } => match result {
                Ok((goal, plan)) => {
                    if autonomous {
                        self.autonomous_plans.insert(plan.id);
                    }
                    let by = if autonomous { "autonomously " } else { "" };
                    self.log(Level::Plan, format!("working {by}on goal with plan {}", lyra_execution::short(plan.id)));
                    let mut text = plan::describe(Some(&goal), &plan);
                    text += "\nrunning it now · /plan cancel stops it";
                    self.messages.push(Message::new("info", text));
                    self.current_goal = Some(goal);
                    self.current_plan = Some(plan);
                    self.refresh_goals();
                }
                Err(e) => {
                    self.plan_busy = false;
                    self.set_phase(Phase::Idle);
                    self.log(Level::Error, e.clone());
                    self.messages.push(Message::new("error", e));
                    self.refresh_goals();
                }
            },
            StreamEvent::Goals(snapshot) => self.goals_panel = Some(snapshot),
            StreamEvent::CapNotes(notes) => {
                for note in notes {
                    self.log(Level::Tool, note);
                }
                self.tools_changed();
            }
            StreamEvent::Evolution(snapshot) => {
                if let Err(e) = &snapshot {
                    self.log(Level::Error, format!("evolution: {e}"));
                }
                self.evolution_panel = Some(snapshot);
            }
            StreamEvent::Memory(snapshot) => {
                if let Err(e) = &snapshot {
                    self.log(Level::Error, format!("memory: {e}"));
                }
                self.memory = Some(snapshot);
            }
        }
    }
}

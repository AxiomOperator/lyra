//! Plans and goals in a conversation: the execution runtime, plans that
//! finish, and the goal loop (looked at off the loop, its notes and next plans).

use crate::*;

impl App {
    /// The plan runtime for autonomous work: the autonomy guard applies (G11).
    pub(crate) fn autonomous_runtime(&self) -> LyraRuntime {
        let mut rt = self.runtime();
        rt.guard = self.goals.as_ref().map(|g| g.guard());
        rt
    }

    pub(crate) fn runtime(&self) -> LyraRuntime {
        LyraRuntime {
            url: format!("{}/chat/completions", self.base_url.trim_end_matches('/')),
            model: self.model.clone(),
            tools: self.tools.clone(),
            learning: self.learning.clone(),
            forbidden_tools: self.forbidden_tools.clone(),
            evolution: self.evolution.clone(),
            caps: self.caps.clone(),
            guard: None,
            tx: self.tx.clone(),
        }
    }

    pub(crate) fn reload_plan(&mut self, id: Uuid) {
        let Some(engine) = &self.engine else { return };
        if let Ok(Some(plan)) = engine.plan(id) {
            // The goal's status changes as the plan runs, so read it each time.
            self.current_goal = engine.goal(plan.goal_id).ok().flatten().map(|(g, _)| g);
            self.current_plan = Some(plan);
        }
    }

    /// Find plans interrupted by a restart (P8) and show the latest plan.
    pub(crate) fn recover_plans(&mut self) {
        let Some(engine) = self.engine.clone() else { return };
        if let Ok(Some(latest)) = engine.plans(1).map(|p| p.into_iter().next()) {
            self.reload_plan(latest.id);
        }
        let (rt, tx) = (self.runtime(), self.tx.clone());
        crate::acting::spawn(move || {
            let notes = engine.recover(&rt).unwrap_or_else(|e| vec![format!("plan recovery failed: {e}")]);
            let _ = tx.send(StreamEvent::PlanNotes(notes));
        });
    }

    /// A plan run ended. Report it, and when it finished, hand the evidence to
    /// memory (P18) and skills (P19): the planner never writes either directly.
    pub(crate) fn plan_finished(&mut self, outcome: RunOutcome) {
        self.goal_plan_finished(&outcome);
        let plan = outcome.plan;
        self.reload_plan(plan.id);
        let goal = self.current_goal.clone();
        let summary = outcome.evaluation.as_ref().map(|e| e.summary.clone()).unwrap_or_default();
        let answer = outcome.evaluation.as_ref().map(|e| e.answer.clone()).filter(|a| !a.trim().is_empty());
        let mut text = match (plan.status, outcome.goal_status) {
            (PlanStatus::Paused, _) => format!("⏸ plan paused: {}", plan.note.clone().unwrap_or_default()),
            (_, Some(status)) => {
                let icon = match status {
                    GoalStatus::Completed => "🎯",
                    GoalStatus::Partial => "◐",
                    _ => "✗",
                };
                let mut t = format!("{icon} goal {status}");
                if !summary.is_empty() {
                    t += &format!(": {summary}");
                }
                if let Some(e) = &outcome.evaluation {
                    for c in &e.criteria {
                        t += &format!("\n  {} {}", if c.met { "✓" } else { "✗" }, c.criterion);
                    }
                }
                if let Some(a) = &answer {
                    t += &format!("\n\n{a}");
                }
                t
            }
            _ => format!("plan {}", plan.status),
        };
        text += &format!("\n\n{}", plan::describe(None, &plan));
        self.messages.push(Message::new("info", text));
        self.log(Level::Plan, format!("plan {} {}", lyra_execution::short(plan.id), plan.status));
        if !plan.status.is_finished() {
            return;
        }
        let Some(engine) = self.engine.clone() else { return };
        let goal_text = goal.as_ref().map_or(String::new(), |g| g.description.clone());
        let evo = self.record_plan_run(&engine, &plan, &goal_text, outcome.goal_status);
        // The user's next message is feedback on the plan, for evolution.
        self.last_run = Some(LastRun::new(plan.id, Vec::new(), 0, evo));
        // P18: the outcome becomes an episode (linked to the plan, with its
        // real start), and durable discoveries are offered to memory, all
        // through the memory manager.
        if let Some(mem) = self.mem() {
            let tx = self.tx.clone();
            let (scope, status) = (mem.manager.settings().default_scope.clone(), outcome.goal_status);
            let episode_summary = format!("Plan: {goal_text}. {summary}");
            let episode_outcome = format!("goal {}{}", status.map_or("unknown".into(), |s| s.to_string()), answer.as_ref().map_or(String::new(), |a| format!(": {}", a.chars().take(300).collect::<String>())));
            let entities: Vec<String> = mem.working().entities.iter().cloned().collect();
            let (plan_id, started) = (plan.id, plan.created_at);
            let transcript = format!("Goal: {goal_text}\n\n{}", plan::results(&plan));
            let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
            let model = self.model.clone();
            crate::acting::spawn(move || {
                let note = match mem.run(mem.manager.add_episode(&scope, &episode_summary, &episode_outcome, entities, Some(plan_id), Some(started))) {
                    Ok(m) => format!("recorded plan episode [{}]", m.short_id()),
                    Err(e) => format!("plan episode not recorded: {e}"),
                };
                let _ = tx.send(StreamEvent::MemoryNotes(vec![note]));
                let reason = "a plan finished; keep the durable facts it discovered or changed (state, configuration, decisions), not the steps themselves";
                let review = mem.capture(&url, &model, reason, &transcript, &goal_text_for(&transcript), Some(plan_id), Some(started), None);
                let _ = tx.send(StreamEvent::MemoryReview { curation: false, review });
            });
            self.capturing = true;
        }
        // P19: recoveries are lessons; let the skill reviewer look at them.
        let metrics = engine.metrics(&plan).unwrap_or_default();
        if (metrics.recoveries > 0 || metrics.replans > 0)
            && let Some(learning) = self.learning.clone()
            && learning.mode() != Mode::Off
            && !self.reviewing
        {
            self.reviewing = true;
            let transcript = format!("Goal: {goal_text}\n\n{}", plan::events(&engine, &plan).unwrap_or_default());
            let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
            let (model, tx, owner) = (self.model.clone(), self.tx.clone(), self.personal());
            self.log(Level::Learn, "reviewing the plan's recoveries for a lesson".into());
            crate::acting::spawn(move || {
                let review = learning.review(&url, &model, "a failed step was replaced by a working one (plan execution)", &transcript, None, owner.as_deref());
                let _ = tx.send(StreamEvent::Reviewed(review));
            });
        }
    }

    // ---- evolution

    pub(crate) fn refresh_goals(&self) {
        let Some(goals) = self.goals.clone() else { return };
        let tx = self.tx.clone();
        crate::acting::spawn(move || {
            let _ = tx.send(StreamEvent::Goals(goals.snapshot()));
        });
    }

    /// A plan ended or paused: if it was for a goal, the goal's progress,
    /// status and (for autonomous work) the session's spending follow (G5).
    pub(crate) fn goal_plan_finished(&mut self, outcome: &RunOutcome) {
        let (Some(goals), Some(engine)) = (self.goals.clone(), self.engine.clone()) else { return };
        let plan = &outcome.plan;
        if self.autonomous_plans.contains(&plan.id) {
            let m = engine.metrics(plan).unwrap_or_default();
            let cost = self.pricing.cost(m.tokens, 0, 0);
            goals.spent(m.model_calls, m.tool_calls, m.replans, cost);
            if plan.status.is_finished() {
                self.autonomous_plans.remove(&plan.id);
            }
        }
        match goals.plan_finished(outcome, plan.usage.tokens) {
            Ok(notes) if !notes.is_empty() => {
                for note in &notes {
                    self.log(Level::Plan, note.clone());
                }
                self.messages.push(Message::new("info", notes.join("\n")));
            }
            Ok(_) => {}
            Err(e) => self.log(Level::Error, format!("goal update failed: {e}")),
        }
        if let Ok(Some(gp)) = goals.manager.plan_goal(plan.id) {
            self.goal_episode(gp.goal_id);
        }
        self.refresh_goals();
    }

    /// A goal that ended becomes a memory episode (goal events feed Memory).
    pub(crate) fn goal_episode(&self, id: Uuid) {
        let (Some(goals), Some(mem)) = (self.goals.clone(), self.mem()) else { return };
        let Ok(Some(g)) = goals.manager.get(id) else { return };
        if !matches!(g.status, lyra_goals::GoalStatus::Completed | lyra_goals::GoalStatus::Failed) {
            return;
        }
        let scope = mem.manager.settings().default_scope.clone();
        let summary = format!("Goal: {}. {}", g.title, g.progress_detail.summary);
        let outcome = format!("goal {} after {} plan(s)", g.status, goals.manager.plans(g.id).map(|p| p.len()).unwrap_or(0));
        let tx = self.tx.clone();
        crate::acting::spawn(move || {
            let note = match mem.run(mem.manager.add_episode(&scope, &summary, &outcome, Vec::new(), None, Some(g.created_at))) {
                Ok(m) => format!("recorded goal episode [{}]", m.short_id()),
                Err(e) => format!("goal episode not recorded: {e}"),
            };
            let _ = tx.send(StreamEvent::MemoryNotes(vec![note]));
        });
    }

    /// The goal loop (G8–G11), every few seconds while idle: fire triggers,
    /// lift blockers whose cause went away, resume interrupted plans, and,
    /// within the autonomy policy, work on the next goal.
    /// The goal loop's look (every `goals_every`): triggers, blockers whose
    /// cause went away and plans to resume are worked out off this thread
    /// (`goals_looked`); then resuming one and autonomy happen here.
    pub(crate) fn goals_tick(&mut self) {
        self.goals_checked = Instant::now();
        let Some(goals) = self.goals.clone() else { return };
        if self.goals_looking {
            return;
        }
        self.goals_looking = true;
        let (caps, engine, tx) = (self.caps.clone(), self.engine.clone(), self.tx.clone());
        let resume_allowed = goals.mode() != lyra_goals::AutonomyMode::Reactive && !self.plan_busy;
        crate::acting::spawn(move || {
            let mut notes = goals.manager.fire_triggers(chrono::Utc::now(), &|c| goals::condition(c, caps.as_deref())).unwrap_or_default();
            let mut resume = None;
            // Blockers whose cause went away.
            if let Some(engine) = engine {
                for b in goals.manager.blockers(None, true).unwrap_or_default() {
                    let lift = match b.blocker_type {
                        lyra_goals::BlockerType::ApprovalRequired => goals
                            .manager
                            .plans(b.goal_id)
                            .ok()
                            .and_then(|p| p.last().cloned())
                            .and_then(|p| engine.plan(p.plan_id).ok().flatten())
                            .is_some_and(|plan| !plan.steps.iter().any(|s| s.needs_approval())),
                        lyra_goals::BlockerType::CapabilityUnavailable => caps.as_ref().is_some_and(|c| {
                            c.manager.all().iter().all(|x| c.manager.health(x) != lyra_capabilities::CapabilityHealth::Unavailable)
                        }),
                        _ => false,
                    };
                    if lift && let Ok(g) = goals.manager.unblock(b.goal_id, &format!("{} resolved", b.blocker_type.as_str())) {
                        notes.push(format!("goal {} unblocked: {} resolved", g.title, b.blocker_type.as_str()));
                    }
                }
                // G8: plans for goals that were interrupted or approved resume (not in reactive mode).
                if resume_allowed {
                    for gp in goals.manager.open_plans().unwrap_or_default() {
                        let Ok(Some(plan)) = engine.plan(gp.plan_id) else { continue };
                        let waiting = plan.steps.iter().any(|s| s.needs_approval() || s.status == lyra_execution::StepStatus::Blocked);
                        let goal_ok = goals.manager.get(gp.goal_id).ok().flatten().is_some_and(|g| g.status == lyra_goals::GoalStatus::Active);
                        if plan.status == PlanStatus::Paused && !waiting && goal_ok {
                            resume = Some(plan.id);
                            break;
                        }
                    }
                }
            }
            let _ = tx.send(StreamEvent::GoalsLooked { notes, resume });
        });
    }

    /// What the goal loop's look found: told, a plan resumed, then autonomy.
    pub(crate) fn goals_looked(&mut self, notes: Vec<String>, resume: Option<Uuid>) {
        self.goals_looking = false;
        let Some(goals) = self.goals.clone() else { return };
        let mode = goals.mode();
        let mut notes = notes;
        if let (Some(id), Some(engine)) = (resume, self.engine.clone())
            && mode != lyra_goals::AutonomyMode::Reactive
            && !self.plan_busy
        {
            notes.push(format!("resuming plan {} for its goal", lyra_execution::short(id)));
            self.plan_busy = true;
            self.autonomous_plans.insert(id);
            let (rt, tx) = (self.autonomous_runtime(), self.tx.clone());
            crate::acting::spawn(move || {
                let _ = tx.send(StreamEvent::PlanFinished(engine.run(id, &rt)));
            });
        }
        for note in &notes {
            self.log(Level::Plan, note.clone());
        }
        if !notes.is_empty() {
            self.refresh_goals();
        }
        // G11: autonomous work, within the policy.
        if mode == lyra_goals::AutonomyMode::Reactive || self.plan_busy || self.engine.is_none() {
            return;
        }
        let note = match goals.may_work(goals.manager.settings.autonomy.cooldown_minutes) {
            Err(why) => why,
            Ok(()) => match goals.manager.next(mode == lyra_goals::AutonomyMode::Assisted) {
                Ok(Some((g, score))) => {
                    self.log(Level::Plan, format!("autonomy picked {} (score {:.2})", g.title, score.total));
                    match self.work_goal(&g.id.to_string(), true) {
                        Ok(text) => text,
                        Err(e) => e,
                    }
                }
                Ok(None) => "nothing to work on".into(),
                Err(e) => e,
            },
        };
        if note != self.autonomy_note {
            self.log(Level::Plan, format!("autonomy: {note}"));
            self.autonomy_note = note;
        }
    }
}

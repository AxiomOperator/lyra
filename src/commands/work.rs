//! Commands for plans, goals, capabilities and evolution.


use crate::*;

impl App {
    /// `/plan …` and `/plans`.
    pub(crate) fn plan_command(&mut self, name: &str, arg: &str) -> Result<String, String> {
        let engine = self.engine.clone().ok_or_else(|| match &self.planning_status {
            Err(why) => why.clone(),
            Ok(_) => "planning is off".to_string(),
        })?;
        if name == "/plans" {
            return plan::list(&engine);
        }
        let anyway = arg.split_whitespace().any(|w| w == "anyway");
        let words: Vec<&str> = arg.split_whitespace().filter(|w| *w != "anyway").collect();
        let is_id = |w: &str| w.len() >= 4 && w.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
        // `/plan run` etc. are commands; anything else is a new request.
        let target = |rest: &[&str]| -> Result<Plan, String> {
            match rest.iter().find(|w| is_id(w)) {
                Some(id) => engine.find(id),
                None => self.current_plan.clone().ok_or_else(|| "no plan yet — /plan <what you want done>".to_string()),
            }
        };
        let command = match words.first().copied() {
            None => Some("show"),
            Some(w @ ("show" | "run" | "resume" | "cancel" | "events" | "checkpoints" | "revisions")) if words.len() <= 2 && words[1..].iter().all(|x| is_id(x)) => Some(w),
            Some("budget") if words.len() <= 3 => Some("budget"),
            Some(w @ ("approve" | "retry" | "skip")) if (2..=3).contains(&words.len()) => Some(w),
            _ => None,
        };
        match command {
            Some("show") => {
                let plan = target(&words)?;
                let goal = engine.goal(plan.goal_id)?.map(|(g, _)| g);
                Ok(plan::describe(goal.as_ref(), &plan))
            }
            Some("events") => plan::events(&engine, &target(&words[1..])?),
            Some("checkpoints") => plan::checkpoints(&engine, &target(&words[1..])?),
            Some("revisions") => plan::revisions(&engine, &target(&words[1..])?),
            Some("budget") => {
                let plan = target(&words[1..])?;
                if words.get(1) == Some(&"raise") {
                    // Add the configured budget again on top of the current one.
                    let note = engine.raise_budget(plan.id, &engine.settings.budget)?;
                    self.reload_plan(plan.id);
                    Ok(format!("{note} — /plan resume to continue"))
                } else {
                    Ok(format!("budget: {} — /plan budget raise adds the configured budget again", lyra_execution::budget::describe(&plan.budget, &plan.usage)))
                }
            }
            Some("run" | "resume") => {
                if self.plan_busy {
                    return Err("a plan is already being created or run".into());
                }
                let plan = target(&words[1..])?;
                if plan.status.is_finished() {
                    return Err(format!("plan {} is already {}", lyra_execution::short(plan.id), plan.status));
                }
                // P1: open questions are asked before anything runs.
                if plan.status == PlanStatus::Draft
                    && !anyway
                    && let Some((goal, _)) = engine.goal(plan.goal_id)?
                    && !goal.ambiguities.is_empty()
                {
                    return Err(format!(
                        "the goal has open questions:\n- {}\nanswer them with a clearer /plan <request>, or /plan run anyway",
                        goal.ambiguities.join("\n- ")
                    ));
                }
                self.plan_busy = true;
                self.set_phase(Phase::Tools(format!("plan {}", lyra_execution::short(plan.id))));
                let (rt, tx, id) = (self.runtime(), self.tx.clone(), plan.id);
                crate::acting::spawn(move || {
                    let _ = tx.send(StreamEvent::PlanFinished(engine.run(id, &rt)));
                });
                Ok(format!("running plan {}…", lyra_execution::short(id)))
            }
            Some("cancel") => {
                let plan = target(&words[1..])?;
                let note = engine.cancel(plan.id, &self.runtime())?;
                self.reload_plan(plan.id);
                Ok(note)
            }
            Some(w @ ("approve" | "retry" | "skip")) => {
                let plan = target(&words[2..])?;
                let note = match w {
                    "approve" => engine.approve(plan.id, words[1], &self.runtime())?,
                    "retry" => engine.retry_step(plan.id, words[1])?,
                    _ => engine.skip_step(plan.id, words[1])?,
                };
                self.reload_plan(plan.id);
                Ok(format!("{note} — /plan resume to continue"))
            }
            _ => {
                if self.plan_busy {
                    return Err("a plan is already being created or run".into());
                }
                self.plan_busy = true;
                let (rt, tx, request) = (self.runtime(), self.tx.clone(), arg.trim().to_string());
                crate::acting::spawn(move || {
                    let _ = tx.send(StreamEvent::PlanCreated(engine.create(&request, &rt)));
                });
                Ok("working out the goal and a plan…".into())
            }
        }
    }

    /// Plan and work on a goal now (G4): a new plan toward it, run right away.
    pub(crate) fn work_goal(&mut self, key: &str, autonomous: bool) -> Result<String, String> {
        let goals = self.goals.clone().ok_or("goals are off")?;
        let engine = self.engine.clone().ok_or("planning is off, and goals are worked through plans")?;
        if self.plan_busy {
            return Err("a plan is already being created or run".into());
        }
        let g = goals.manager.find(key)?;
        if matches!(g.status, lyra_goals::GoalStatus::Proposed | lyra_goals::GoalStatus::Paused) && !autonomous {
            goals.manager.set_status(g.id, lyra_goals::GoalStatus::Active, "the user started work on it")?;
        }
        let g = goals.manager.get(g.id)?.ok_or("goal vanished")?;
        if let Err((kind, why)) = goals.manager.can_progress(&g) {
            return Err(format!("{} can't progress: {} ({})", g.title, why, kind.as_str()));
        }
        let request = goals.request(&g)?;
        self.plan_busy = true;
        self.set_phase(Phase::Tools(format!("goal {}", g.short())));
        let rt = if autonomous { self.autonomous_runtime() } else { self.runtime() };
        let (tx, base) = (self.tx.clone(), engine.settings.budget);
        let title = g.title.clone();
        crate::acting::spawn(move || match engine.create(&request, &rt) {
            Ok((plan_goal, plan)) => {
                let _ = goals.manager.link_plan(g.id, plan.id, autonomous);
                if autonomous {
                    let _ = engine.set_budget(plan.id, goals.remaining_budget(base));
                }
                let plan = engine.plan(plan.id).ok().flatten().unwrap_or(plan);
                let _ = tx.send(StreamEvent::GoalPlan { result: Ok((plan_goal, plan.clone())), autonomous });
                let _ = tx.send(StreamEvent::PlanFinished(engine.run(plan.id, &rt)));
            }
            Err(e) => {
                // A goal that can't even be planned needs information first.
                if autonomous {
                    let _ = goals.manager.block(g.id, lyra_goals::BlockerType::MissingInformation, &format!("couldn't plan it: {e}"));
                }
                let _ = tx.send(StreamEvent::GoalPlan { result: Err(format!("couldn't plan {title}: {e}")), autonomous });
            }
        });
        Ok(format!("working on {} — planning…", g.title))
    }

    /// Run in the background: break a goal into subgoals (G3).
    pub(crate) fn decompose_goal(&mut self, key: &str) -> Result<String, String> {
        let goals = self.goals.clone().ok_or("goals are off")?;
        let g = goals.manager.find(key)?;
        if self.goals_busy {
            return Err("a goal decomposition or review is already running".into());
        }
        self.goals_busy = true;
        // What lyra can do, so subgoals are things it can actually work on.
        let mut context: Vec<String> = Vec::new();
        if let Some(caps) = &self.caps {
            let (found, _) = caps.search(&serde_json::json!({ "query": format!("{} {}", g.title, g.description) }).to_string());
            context.push(format!("capabilities: {found}"));
        }
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let (model, tx, title) = (self.model.clone(), self.tx.clone(), g.title.clone());
        crate::acting::spawn(move || {
            let prompt = lyra_goals::prompts::decompose_prompt(&g, &context.join("\n"));
            let notes = learn::complete(&url, &model, lyra_goals::prompts::DECOMPOSE_PROMPT, &prompt)
                .and_then(|(reply, _)| lyra_goals::prompts::parse_decomposition(&reply, &g))
                .and_then(|subs| goals.manager.decompose(g.id, subs))
                .map(|kids| {
                    let mut notes = vec![format!("{title} → {} subgoals:", kids.len())];
                    notes.extend(kids.iter().map(|k| {
                        let waits = if k.dependencies.is_empty() { String::new() } else { format!(" (after {})", k.dependencies.len()) };
                        format!("  {} {}{waits}", k.short(), k.title)
                    }));
                    notes
                })
                .unwrap_or_else(|e| vec![format!("decomposing {title} failed: {e}")]);
            let _ = tx.send(StreamEvent::GoalNotes { notes, show: true });
        });
        Ok("breaking the goal into subgoals…".into())
    }

    /// Run in the background: review the goal list (G12). In autonomous mode
    /// the suggestions are applied; otherwise they wait for `/goals review apply`.
    pub(crate) fn review_goals(&mut self) -> Result<String, String> {
        let goals = self.goals.clone().ok_or("goals are off")?;
        if self.goals_busy {
            return Err("a goal decomposition or review is already running".into());
        }
        let all = goals.manager.all()?;
        if all.iter().filter(|g| g.status.is_open()).count() < 2 {
            return Ok("fewer than two open goals: nothing to tidy".into());
        }
        self.goals_busy = true;
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let (model, tx) = (self.model.clone(), self.tx.clone());
        crate::acting::spawn(move || {
            let stale: Vec<lyra_goals::Uuid> = goals.manager.stale().unwrap_or_default().iter().map(|g| g.id).collect();
            let prompt = lyra_goals::prompts::review_prompt(&all, &stale);
            let notes = match learn::complete(&url, &model, lyra_goals::prompts::REVIEW_PROMPT, &prompt).and_then(|(r, _)| lyra_goals::prompts::parse_review(&r)) {
                Ok(review) => {
                    for g in all.iter().filter(|g| g.status.is_open()) {
                        let _ = goals.manager.note_review(g.id, "reviewed");
                    }
                    let text = goals::describe_review(&goals, &review);
                    if goals.mode() == lyra_goals::AutonomyMode::Autonomous && !review.is_empty() {
                        let mut notes = vec!["goal review (applied):".to_string()];
                        notes.extend(goals::apply_review(&goals, &review));
                        notes
                    } else {
                        let empty = review.is_empty();
                        *goals.review.lock().unwrap_or_else(|e| e.into_inner()) = Some(review);
                        let mut notes = vec![format!("goal review:\n{text}")];
                        if !empty {
                            notes.push("/goals review apply to make these changes".into());
                        }
                        notes
                    }
                }
                Err(e) => vec![format!("goal review failed: {e}")],
            };
            let _ = tx.send(StreamEvent::GoalNotes { notes, show: true });
        });
        Ok("reviewing the goals…".into())
    }

    /// `/goals …` and `/goal …`
    pub(crate) fn goals_command(&mut self, name: &str, arg: &str) -> Result<String, String> {
        let goals = self.goals.clone().ok_or("goals are off ([goals] enabled)")?;
        let (sub, rest) = arg.trim().split_once(' ').unwrap_or((arg.trim(), ""));
        let rest = rest.trim();
        let result = match (name, sub) {
            ("/goals", "") => goals::list(&goals, false),
            ("/goals", "all") => goals::list(&goals, true),
            ("/goals", "next") => goals::next(&goals),
            ("/goals", "review") if rest == "apply" => {
                let review = goals.review.lock().unwrap_or_else(|e| e.into_inner()).take();
                match review {
                    Some(r) => Ok(goals::apply_review(&goals, &r).join("\n")),
                    None => Err("no review waiting: /goals review".into()),
                }
            }
            ("/goals", "review") => self.review_goals(),
            ("/goals", "autonomy") => {
                if !rest.is_empty() {
                    let mode: lyra_goals::AutonomyMode = serde_json::from_value(serde_json::json!(rest))
                        .map_err(|_| "usage: /goals autonomy reactive|assisted|autonomous")?;
                    goals.set_mode(mode);
                    self.autonomy_note.clear();
                    self.log(Level::Plan, format!("autonomy: {}", mode.as_str()));
                }
                let p = &goals.manager.settings.autonomy;
                let s = goals.session.lock().unwrap_or_else(|e| e.into_inner()).clone();
                Ok(format!(
                    "autonomy {} · per session: {} min, {} plans, {} tool / {} model calls, {} replans, up to {} risk\nthis session: {} plans, {} tool / {} model calls{}",
                    goals.mode().as_str(),
                    p.max_runtime_minutes,
                    p.max_plans,
                    p.max_tool_calls,
                    p.max_model_calls,
                    p.max_replans,
                    p.max_risk,
                    s.plans,
                    s.tool_calls,
                    s.model_calls,
                    s.stopped.as_ref().map_or(String::new(), |(_, why)| format!(" · stopped: {why}"))
                ))
            }
            ("/goal", "new") => goals::create(&goals, rest),
            ("/goal", "work") => self.work_goal(rest, false),
            ("/goal", "decompose") => self.decompose_goal(rest),
            ("/goal", "") => goals::list(&goals, false),
            ("/goal", s)
                if matches!(
                    s,
                    "activate" | "resume" | "pause" | "cancel" | "complete" | "fail" | "unblock" | "priority" | "importance" | "due"
                        | "criteria" | "depends" | "block" | "when"
                ) =>
            {
                let out = goals::edit(&goals, s, rest);
                if out.is_ok() && matches!(s, "complete" | "fail")
                    && let Ok(g) = goals.manager.find(rest.split_whitespace().next().unwrap_or(""))
                {
                    self.goal_episode(g.id);
                }
                out
            }
            ("/goal", _) => goals::show(&goals, arg.trim()),
            _ => Err(goals::COMMANDS.into()),
        };
        self.refresh_goals();
        result
    }

    /// `/caps …`
    pub(crate) fn caps_command(&mut self, arg: &str) -> Result<String, String> {
        let caps = self.caps.clone().ok_or("capabilities are off")?;
        let (sub, rest) = arg.trim().split_once(' ').unwrap_or((arg.trim(), ""));
        match sub {
            "" | "list" => Ok(caps::list(&caps)),
            "search" => caps::search(&caps, rest),
            "show" => caps::show(&caps, rest),
            "allow" => {
                let c = caps.manager.allow(rest.trim())?;
                self.log(Level::Tool, format!("allowed {} for this session", c.id));
                Ok(format!("{} may run without approval for the rest of this session", c.id))
            }
            "health" => {
                self.refresh_caps(true);
                Ok("checking every provider…".into())
            }
            _ => Err(format!("unknown /caps command {sub:?}\n{}", caps::COMMANDS)),
        }
    }

    /// `/evolve …`
    pub(crate) fn evolve_command(&mut self, arg: &str) -> Result<String, String> {
        let evolution = self.evolution.clone().ok_or_else(|| match &self.evolution_status {
            Err(why) => why.clone(),
            Ok(_) => "evolution is off".to_string(),
        })?;
        let (sub, rest) = arg.trim().split_once(' ').unwrap_or((arg.trim(), ""));
        let rest = rest.trim().to_string();
        let background = matches!(sub, "review" | "test" | "compare" | "approve" | "rollback" | "code");
        if background && let Some(what) = self.evolving {
            return Err(format!("evolution is busy ({what}); try again when it's done"));
        }
        match sub {
            "" | "status" => evolve::status(&evolution),
            "list" | "candidates" => evolve::list(&evolution),
            "show" => evolve::show(&evolution, &rest),
            "reject" => {
                let note = evolve::reject(&evolution, &rest)?;
                self.refresh_evolution();
                Ok(note)
            }
            "generations" => evolve::generations(&evolution),
            "history" => evolve::history(&evolution),
            "runs" => evolve::runs(&evolution),
            "review" => {
                if evolution.mode() == lyra_evolution::Mode::Off {
                    return Err("evolution mode is off ([evolution] mode in config.toml); runs are still recorded".into());
                }
                self.evolve_in_background("reviewing", true, evolve::review);
                Ok("looking for problems in recent runs…".into())
            }
            "test" => {
                let c = evolution.manager.find(&rest)?;
                self.evolve_in_background("testing", true, move |env| evolve::test(env, c).into());
                Ok("testing the candidate (static checks, then a sandboxed benchmark)…".into())
            }
            "compare" => {
                evolution.manager.find(&rest)?;
                self.evolve_in_background("comparing", true, move |env| evolve::compare(env, &rest));
                Ok("testing every open candidate for that problem and ranking them…".into())
            }
            "approve" => {
                evolution.manager.find(rest.split_whitespace().next().unwrap_or(""))?;
                self.evolve_in_background("deploying", true, move |env| evolve::approve(env, &rest));
                Ok("applying the change…".into())
            }
            "rollback" => {
                self.evolve_in_background("rolling back", true, move |env| evolve::rollback(env, &rest));
                Ok("rolling back…".into())
            }
            "code" => {
                if rest.is_empty() {
                    return Err("usage: /evolve code <the problem to fix>".into());
                }
                self.evolve_in_background("writing a patch", true, move |env| evolve::propose_code(env, &rest));
                Ok("asking the model for a source patch (it only becomes a candidate)…".into())
            }
            _ => Err(format!("unknown /evolve command {sub:?}\n{}", evolve::COMMANDS)),
        }
    }
}

//! Evolution and capabilities from the app: its environment, background
//! work, the tool list changing, and each chat and plan run recorded.

use crate::*;

impl App {
    pub(crate) fn evolution_env(&self) -> Option<evolve::Env> {
        Some(evolve::Env {
            url: format!("{}/chat/completions", self.base_url.trim_end_matches('/')),
            model: self.model.clone(),
            evolution: self.evolution.clone()?,
            tools: self.tools.clone(),
            learning: self.learning.clone(),
            caps: self.caps.clone(),
            goals: self.goals.clone(),
            system_prompt: self.system_prompt.clone(),
        })
    }

    /// Run evolution work on a background thread; one piece at a time.
    pub(crate) fn evolve_in_background(&mut self, what: &'static str, show: bool, work: impl FnOnce(&evolve::Env) -> evolve::Done + Send + 'static) {
        let Some(env) = self.evolution_env() else { return };
        self.evolving = Some(what);
        let tx = self.tx.clone();
        crate::acting::spawn(move || {
            let _ = tx.send(StreamEvent::Evolved { done: work(&env), show });
        });
    }

    /// Re-read the evolved state (behavior, workflows, composite tools).
    pub(crate) fn reload_evolution(&mut self) {
        let Some(evolution) = self.evolution.clone() else { return };
        for note in evolution.reload(self.tools.as_deref()) {
            self.log(Level::Error, format!("evolution: {note}"));
        }
        // Evolved workflows and composite tools are capabilities (C11).
        self.refresh_caps(false);
        self.tools_changed();
        self.refresh_evolution();
    }

    /// The tool list may have changed (composite tools): recount it.
    pub(crate) fn tools_changed(&mut self) {
        // What the model is offered for an empty query: everything when few,
        // otherwise memory_recall and the search tool (discovery adds the rest).
        let definitions = match (&self.caps, &self.tools) {
            (Some(caps), _) => Some(Value::Array(caps.definitions("", &Default::default()))),
            (None, Some(tools)) => Some(tools.definitions()),
            _ => None,
        };
        self.tools_tokens = definitions.as_ref().map_or(0, |d| learn::approx_tokens(&d.to_string()));
        self.tool_count = definitions.as_ref().and_then(|d| d.as_array().map(Vec::len)).unwrap_or(0);
    }

    /// Rebuild the capability registry (skills, workflows or tools changed)
    /// and, with `health`, check every provider. In the background.
    pub(crate) fn refresh_caps(&self, health: bool) {
        let Some(caps) = self.caps.clone() else { return };
        let tx = self.tx.clone();
        crate::acting::spawn(move || {
            let mut notes = caps.refresh();
            if health {
                notes.extend(caps.check_health());
            }
            let _ = tx.send(StreamEvent::CapNotes(notes));
        });
    }

    // ---- goals (docs/done/goal_manager.md)

    pub(crate) fn refresh_evolution(&self) {
        let Some(evolution) = self.evolution.clone() else { return };
        let tx = self.tx.clone();
        crate::acting::spawn(move || {
            let _ = tx.send(StreamEvent::Evolution(evolution.snapshot()));
        });
    }

    /// E1: record the turn just answered (or failed) for evolution. Returns its id.
    pub(crate) fn record_chat_run(&mut self, stats: &Stats, error: Option<&str>) -> Option<Uuid> {
        // Everyone's turns count. Anyone but the owner's are counts only: no
        // words of theirs (the request, an error's text) in the shared record.
        let person = self.personal();
        let evolution = self.evolution.clone()?;
        let start = self.messages.iter().rposition(|m| m.role == "user")?;
        let turn = &self.messages[start..];
        let task = if person.is_some() { "" } else { turn[0].content.as_str() };
        let mut run = lyra_evolution::RunRecord::new(lyra_evolution::RunKind::Chat, task, 0);
        run.person = person.clone();
        run.model_calls = turn.iter().filter(|m| m.role == "assistant").count().max(1) as u32;
        run.tools_used = turn.iter().flat_map(|m| m.tool_calls.iter().map(|c| c.function.name.clone())).collect();
        run.tool_calls = run.tools_used.len() as u32;
        // Each failed call, by its result's structure (`ok: false`, `error`).
        let names: std::collections::HashMap<&str, &str> = turn.iter().flat_map(|m| m.tool_calls.iter().map(|c| (c.id.as_str(), c.function.name.as_str()))).collect();
        run.errors = turn
            .iter()
            .filter(|m| m.role == "tool")
            .filter_map(|m| {
                let f = outcomes::failure(&m.content)?;
                let tool = m.tool_call_id.as_deref().and_then(|id| names.get(id)).copied().unwrap_or("a tool");
                Some(if person.is_some() { format!("{tool}: {}", f.kind.as_str()) } else { f.message })
            })
            .collect();
        run.retries = run.errors.len() as u32;
        run.tokens = stats.input + stats.output;
        run.duration_ms = stats.elapsed.as_millis() as u64;
        run.skills_used = self.applied_skills.iter().map(|s| s.trim_end_matches(" (trial)").to_string()).collect();
        if let Some(e) = error {
            run.errors.push(if person.is_some() { "the reply failed".into() } else { e.chars().take(300).collect() });
            run.outcome = lyra_evolution::RunOutcome::Failure;
        }
        let id = run.id;
        // The turns it follows, so a benchmark can replay it as asked (owner's only).
        if person.is_none() {
            let before: Vec<&Message> = self.messages[..start].iter().filter(|m| matches!(m.role.as_str(), "user" | "assistant") && !m.content.trim().is_empty()).collect();
            run.history = before.iter().rev().take(4).rev().map(|m| (m.role.clone(), m.content.chars().take(800).collect())).collect();
        }
        // What was in play (4.7): guidelines, the workflow, memories; and how it went.
        if let Some(r) = self.run {
            (run.guidelines, run.workflow) = evolution.in_play(&turn[0].content, r);
        }
        run.memories = self.applied_memories.len();
        run.reply_chars = turn.iter().rev().find(|m| m.role == "assistant").map_or(0, |m| m.content.chars().count());
        run.memory_miss = outcomes::told_before(&turn[0].content);
        // During a trial: the side this turn was on.
        let arm = self.run.and_then(|r| evolution.arm(r));
        // Written now (one row), so an outcome that follows at once (Stop, the
        // tool-call limit) finds it.
        if let Err(e) = evolution.record(run, arm) {
            self.log(Level::Error, format!("recording the run failed: {e}"));
        }
        let tx = self.tx.clone();
        crate::acting::spawn(move || {
            let _ = tx.send(StreamEvent::Evolution(evolution.snapshot()));
        });
        Some(id)
    }

    /// The user's reaction tells how a run went; then check for a regression.
    pub(crate) fn record_evolution_outcome(&self, id: Uuid, outcome: SkillOutcome, corrected: bool, feedback: Option<&str>, source: &'static str) {
        let Some(env) = self.evolution_env() else { return };
        let outcome = match outcome {
            SkillOutcome::Success => lyra_evolution::RunOutcome::Success,
            SkillOutcome::Partial => lyra_evolution::RunOutcome::Partial,
            SkillOutcome::Failure => lyra_evolution::RunOutcome::Failure,
            SkillOutcome::Unknown => return,
        };
        let (tx, feedback) = (self.tx.clone(), feedback.map(str::to_string));
        crate::acting::spawn(move || {
            let mut notes = Vec::new();
            if let Err(e) = env.evolution.manager.set_outcome(id, outcome, corrected, feedback.as_deref(), Some(source)) {
                notes.push(format!("recording the outcome failed: {e}"));
            }
            notes.extend(evolve::monitor(&env));
            let show = notes.iter().any(|n| n.starts_with('⚠') || n.starts_with('↩'));
            let _ = tx.send(StreamEvent::Evolved { done: evolve::Done { notes, ..Default::default() }, show });
        });
    }

    /// E1: record a finished plan run, its outcome being the goal's status.
    /// Returns its telemetry id.
    pub(crate) fn record_plan_run(&self, engine: &Engine, plan: &Plan, goal: &str, status: Option<GoalStatus>) -> Option<Uuid> {
        let evolution = self.evolution.clone()?;
        let m = engine.metrics(plan).unwrap_or_default();
        let mut run = lyra_evolution::RunRecord::new(lyra_evolution::RunKind::Plan, goal, 0);
        run.model_calls = m.model_calls;
        run.tool_calls = m.tool_calls;
        run.tokens = m.tokens;
        run.retries = m.retries;
        run.replans = m.replans;
        run.failed_attempts = m.failed_attempts as u32;
        run.recoveries = m.recoveries as u32;
        run.verification_failures = m.verification_failures as u32;
        run.duration_ms = m.seconds * 1000;
        // Every tool called, inside reasoning and subagent steps too.
        run.tools_used = Engine::tools_used(plan);
        // Learned procedures the plan followed.
        run.skills_used = plan
            .steps
            .iter()
            .filter_map(|s| match &s.action {
                lyra_execution::StepAction::Workflow { workflow, .. } => Some(workflow.clone()),
                _ => None,
            })
            .collect();
        // Every error along the way, including ones later recovered from.
        run.errors = engine.errors(plan).unwrap_or_default();
        run.outcome = match status {
            Some(GoalStatus::Completed) => lyra_evolution::RunOutcome::Success,
            Some(GoalStatus::Partial) => lyra_evolution::RunOutcome::Partial,
            Some(_) => lyra_evolution::RunOutcome::Failure,
            None => lyra_evolution::RunOutcome::Unknown,
        };
        let env = self.evolution_env();
        let (tx, id) = (self.tx.clone(), run.id);
        crate::acting::spawn(move || {
            let mut notes = Vec::new();
            if let Err(e) = evolution.record(run, None) {
                notes.push(format!("recording the plan run failed: {e}"));
            }
            if let Some(env) = env {
                notes.extend(evolve::monitor(&env));
            }
            let show = notes.iter().any(|n| n.starts_with('⚠') || n.starts_with('↩'));
            let _ = tx.send(StreamEvent::Evolved { done: evolve::Done { notes, ..Default::default() }, show });
        });
        Some(id)
    }

    /// Ping the embedding and reranker models in the background.
    pub(crate) fn check_models(&mut self) {
        self.model_status = None;
        let (embedding, reranker) = (self.embedding.clone(), self.reranker.clone());
        let tx = self.tx.clone();
        crate::acting::spawn(move || {
            let results = retrieval::check(embedding.as_ref(), reranker.as_ref());
            let _ = tx.send(StreamEvent::Models(results));
        });
    }
}

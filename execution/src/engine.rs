//! The execution engine: turns a request into a goal and plan, then runs the
//! plan step by step (in parallel where safe), verifying, retrying,
//! replanning, checkpointing and persisting as it goes, and finally judges
//! the goal. The runtime controls execution; the model only proposes.
//!
//! Everything the engine needs from the outside (model calls, tools,
//! planning context, reporting) comes through [`Runtime`], so it has no
//! network code. Persistence is async (sqlx); the engine is synchronous and
//! meant to run on its own thread.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::Result;
use chrono::Utc;
use serde_json::{Value, json};
use tokio::runtime::Handle;
use uuid::Uuid;

use crate::budget;
use crate::graph;
use crate::model::*;
use crate::planner::{self, Evaluation, PlanningContext, output_text, truncate};
use crate::retry;
use crate::store::PlanStore;

/// A focused piece of model work: reasoning, a workflow or a subagent.
#[derive(Debug, Clone)]
pub struct Task {
    pub instruction: String,
    /// What the model needs to know: the goal and results it depends on.
    pub context: String,
    /// Tools it may use; `None` means all of them.
    pub tools: Option<Vec<String>>,
    /// Whether it may use tools that change things. Only steps marked
    /// conditional or unsafe may: a safe step must be safe to repeat.
    pub may_change: bool,
    /// Model calls left in the plan's budget; the work should stop within them.
    pub max_model_calls: Option<u32>,
    /// The helper agent doing it (subagent steps), for its permissions.
    pub agent: Option<String>,
}

/// What focused model work produced, and what it cost.
#[derive(Debug, Clone, Default)]
pub struct Reasoned {
    pub text: String,
    pub model_calls: u32,
    pub tool_calls: u32,
    pub tokens: u64,
    /// Calls that changed something (`tool {arguments}`), so a retry knows
    /// what was already done.
    pub changes: Vec<String>,
    /// Every tool it called, in order (telemetry).
    pub tools_used: Vec<String>,
}

/// Focused model work that failed, and what it had changed before failing.
#[derive(Debug, Clone, Default)]
pub struct ReasonError {
    pub error: String,
    pub changes: Vec<String>,
}

impl From<String> for ReasonError {
    fn from(error: String) -> Self {
        Self { error, changes: Vec::new() }
    }
}

/// What the engine needs from the application.
pub trait Runtime: Send + Sync {
    /// One model call; returns the reply and the tokens it used.
    fn complete(&self, system: &str, user: &str) -> Result<(String, u64), String>;
    /// Relevant memories, skills, tools and agents for planning `goal` (P10).
    fn context(&self, goal: &str) -> PlanningContext;
    /// The tools steps can call, without the rest of the planning context
    /// (used to fill in a step's arguments).
    fn tools(&self) -> Vec<planner::ToolInfo> {
        self.context("").tools
    }
    /// Call a tool. `operation` is a stable id for unsafe actions (P15).
    fn call_tool(&self, tool: &str, arguments: &Value, operation: Option<Uuid>) -> Result<Value, String>;
    /// Let the model work on a task, using tools as allowed.
    fn reason(&self, task: &Task) -> Result<Reasoned, ReasonError>;
    /// The procedure behind a workflow name (a learned skill), if known.
    fn workflow(&self, name: &str) -> Option<String>;
    /// The tools a helper agent may use (P13), if it exists.
    fn agent_tools(&self, agent: &str) -> Option<Vec<String>>;
    /// Report an event as it happens (for the UI).
    fn emit(&self, _event: &ExecutionEvent) {}
    /// A yes/no answer from a fast decision model, with its confidence;
    /// `None` (the default) means ask the chat model through `complete`.
    fn decide_yes(&self, _state: &str, _question: &str) -> Option<(bool, f32)> {
        None
    }
}

#[derive(Debug, Clone)]
pub struct Settings {
    pub budget: Budget,
    /// Most steps run at once.
    pub max_parallel: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self { budget: Budget::default(), max_parallel: 3 }
    }
}

/// How a run ended.
#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub plan: Plan,
    pub goal_status: Option<GoalStatus>,
    pub evaluation: Option<Evaluation>,
}

/// Counts for the evolution system and the UI (P20).
#[derive(Debug, Clone, Default)]
pub struct Metrics {
    pub model_calls: u32,
    pub tool_calls: u32,
    pub tokens: u64,
    pub retries: u32,
    pub replans: u32,
    pub failed_attempts: usize,
    /// Steps that failed at least once and then succeeded.
    pub recoveries: usize,
    pub verification_failures: usize,
    pub seconds: u64,
}

pub struct Engine {
    store: PlanStore,
    rt: Handle,
    pub settings: Settings,
    /// Plans asked to stop while running.
    cancelled: Mutex<HashSet<Uuid>>,
}

/// What executing one step produced.
struct StepRun {
    id: Uuid,
    started: chrono::DateTime<Utc>,
    result: StepResult,
    usage: BudgetUsage,
}

impl Engine {
    pub fn open(path: &Path, rt: Handle, settings: Settings) -> Result<Self> {
        let store = rt.block_on(PlanStore::open(path))?;
        Ok(Self { store, rt, settings, cancelled: Mutex::new(HashSet::new()) })
    }

    pub fn with_store(store: PlanStore, rt: Handle, settings: Settings) -> Self {
        Self { store, rt, settings, cancelled: Mutex::new(HashSet::new()) }
    }

    fn db<T>(&self, f: impl Future<Output = Result<T>>) -> Result<T, String> {
        self.rt.block_on(f).map_err(|e| format!("{e:#}"))
    }

    fn event(&self, runtime: &dyn Runtime, e: ExecutionEvent) {
        let _ = self.db(self.store.record_event(&e));
        runtime.emit(&e);
    }

    // ---- reading

    pub fn plan(&self, id: Uuid) -> Result<Option<Plan>, String> {
        self.db(self.store.plan(id))
    }

    pub fn plans(&self, limit: usize) -> Result<Vec<Plan>, String> {
        self.db(self.store.plans(limit))
    }

    pub fn goal(&self, id: Uuid) -> Result<Option<(Goal, Option<Value>)>, String> {
        self.db(self.store.goal(id))
    }

    pub fn events(&self, plan: Uuid) -> Result<Vec<ExecutionEvent>, String> {
        self.db(self.store.events(plan))
    }

    pub fn checkpoints(&self, plan: Uuid) -> Result<Vec<Checkpoint>, String> {
        self.db(self.store.checkpoints(plan))
    }

    pub fn revisions(&self, plan: Uuid) -> Result<Vec<(u32, u32, PlanRevision)>, String> {
        self.db(self.store.revisions(plan))
    }

    /// A plan by id prefix, or the most recent one for an empty key.
    pub fn find(&self, key: &str) -> Result<Plan, String> {
        let plans = self.plans(200)?;
        let key = key.trim();
        if key.is_empty() {
            return plans.into_iter().next().ok_or("no plans yet".into());
        }
        let matches: Vec<Plan> = plans.into_iter().filter(|p| p.id.to_string().starts_with(key)).collect();
        match matches.len() {
            0 => Err(format!("no plan matches {key:?}")),
            1 => Ok(matches.into_iter().next().unwrap()),
            n => Err(format!("{n} plans match {key:?}; use more of the id")),
        }
    }

    /// What a plan cost and how it went (P20).
    pub fn metrics(&self, plan: &Plan) -> Result<Metrics, String> {
        let attempts = self.db(self.store.attempts(plan.id))?;
        let events = self.events(plan.id)?;
        let failed: HashSet<Uuid> = attempts.iter().filter(|a| !a.success).map(|a| a.step_id).collect();
        let recovered = plan.steps.iter().filter(|s| s.status == StepStatus::Completed && failed.contains(&s.id)).count();
        Ok(Metrics {
            model_calls: plan.usage.model_calls,
            tool_calls: plan.usage.tool_calls,
            tokens: plan.usage.tokens,
            retries: plan.usage.retries,
            replans: plan.usage.replans,
            failed_attempts: attempts.iter().filter(|a| !a.success).count(),
            recoveries: recovered,
            verification_failures: events
                .iter()
                .filter(|e| e.kind == EventKind::VerificationCompleted && e.data["verified"] == false)
                .count(),
            seconds: plan.usage.seconds,
        })
    }

    /// P11: give a plan more room: every limit grows by `extra`'s (limits
    /// `extra` doesn't set stay as they are). A plan paused on its budget can
    /// then be resumed.
    pub fn raise_budget(&self, plan_id: Uuid, extra: &Budget) -> Result<String, String> {
        let mut plan = self.plan(plan_id)?.ok_or("no such plan")?;
        if plan.status.is_finished() {
            return Err(format!("the plan is already {}", plan.status));
        }
        let b = &mut plan.budget;
        let add = |limit: &mut Option<u32>, more: Option<u32>| {
            if let (Some(l), Some(m)) = (limit.as_mut(), more) {
                *l += m;
            }
        };
        add(&mut b.max_model_calls, extra.max_model_calls);
        add(&mut b.max_tool_calls, extra.max_tool_calls);
        add(&mut b.max_replans, extra.max_replans);
        add(&mut b.max_minutes, extra.max_minutes);
        if let (Some(l), Some(m)) = (b.max_tokens.as_mut(), extra.max_tokens) {
            *l += m;
        }
        let text = budget::describe(&plan.budget, &plan.usage);
        if plan.status == PlanStatus::Paused && budget::exhausted(&plan.budget, &plan.usage).is_none() {
            plan.note = Some("budget raised; /plan resume continues".into());
        }
        self.save(&plan)?;
        Ok(format!("budget raised: {text}"))
    }

    /// Replace a plan's budget (e.g. the tighter limits of autonomous work).
    pub fn set_budget(&self, plan_id: Uuid, budget: Budget) -> Result<(), String> {
        let mut plan = self.plan(plan_id)?.ok_or("no such plan")?;
        plan.budget = budget;
        self.save(&plan)
    }

    /// Every error the plan's attempts hit, in order (P20).
    pub fn errors(&self, plan: &Plan) -> Result<Vec<String>, String> {
        Ok(self.db(self.store.attempts(plan.id))?.into_iter().filter_map(|a| a.error).collect())
    }

    /// Every tool the plan's steps called, in order (P20).
    pub fn tools_used(plan: &Plan) -> Vec<String> {
        plan.steps
            .iter()
            .filter_map(|s| s.result.as_ref()?.metadata.get("tools")?.as_array().cloned())
            .flatten()
            .filter_map(|t| t.as_str().map(str::to_string))
            .collect()
    }

    // ---- P1, P2, P10: from a request to a plan

    /// Parse the request into a goal, gather context and generate a plan.
    /// The plan is saved as a draft; nothing runs yet.
    pub fn create(&self, request: &str, runtime: &dyn Runtime) -> Result<(Goal, Plan), String> {
        let mut usage = BudgetUsage::default();
        let mut goal = ask(runtime, &mut usage, planner::GOAL_PROMPT, request, |r| planner::parse_goal(request, r))?;
        let ctx = runtime.context(&goal.description);
        let (system, prompt) = (planner::plan_system_prompt(), planner::plan_prompt(&goal, &ctx));
        let mut steps = ask(runtime, &mut usage, &system, &prompt, |r| planner::parse_plan(r, &ctx))?;
        guard(&goal, &mut steps);
        let now = Utc::now();
        let plan = Plan {
            id: Uuid::new_v4(),
            goal_id: goal.id,
            version: 1,
            status: PlanStatus::Draft,
            steps,
            budget: self.settings.budget,
            usage,
            note: (!goal.ambiguities.is_empty()).then(|| format!("open questions: {}", goal.ambiguities.join("; "))),
            created_at: now,
            updated_at: now,
        };
        goal.status = GoalStatus::Pending;
        self.db(self.store.save_goal(&goal))?;
        self.db(self.store.save_plan(&plan))?;
        self.event(runtime, ExecutionEvent::new(plan.id, None, EventKind::GoalCreated, goal.description.clone()));
        self.event(runtime, ExecutionEvent::new(plan.id, None, EventKind::PlanCreated, format!("{} steps", plan.steps.len())));
        Ok((goal, plan))
    }

    // ---- P14 and controls

    /// Approve a step's current action (P14). Approval is bound to the action
    /// and its arguments; if a revision changes them, it's needed again.
    pub fn approve(&self, plan_id: Uuid, key: &str, runtime: &dyn Runtime) -> Result<String, String> {
        let mut plan = self.plan(plan_id)?.ok_or("no such plan")?;
        let step = plan.find_step(key).ok_or(format!("no step {key}"))?.clone();
        let fingerprint = step.fingerprint();
        self.db(self.store.record_approval(plan.id, step.id, &fingerprint))?;
        let s = plan.step_mut(step.id).unwrap();
        s.approved = Some(fingerprint);
        if s.status == StepStatus::Blocked {
            s.status = StepStatus::Pending;
        }
        self.db(self.store.save_plan(&plan))?;
        self.event(runtime, ExecutionEvent::new(plan.id, Some(step.id), EventKind::ApprovalGranted, step.title.clone()));
        Ok(format!("approved {} — {}", step.key, step.title))
    }

    /// Ask a running plan to stop, or cancel a stopped one.
    pub fn cancel(&self, plan_id: Uuid, runtime: &dyn Runtime) -> Result<String, String> {
        let mut plan = self.plan(plan_id)?.ok_or("no such plan")?;
        if plan.status.is_finished() {
            return Err(format!("the plan is already {}", plan.status));
        }
        if plan.status == PlanStatus::Running {
            self.cancelled.lock().unwrap().insert(plan_id);
            return Ok("stopping after the current steps…".into());
        }
        plan.status = PlanStatus::Cancelled;
        cancel_open_steps(&mut plan);
        self.finish_goal(&plan, GoalStatus::Cancelled, &json!({"summary": "cancelled"}))?;
        self.db(self.store.save_plan(&plan))?;
        self.event(runtime, ExecutionEvent::new(plan.id, None, EventKind::ExecutionCancelled, "cancelled"));
        Ok("cancelled".into())
    }

    /// Run a failed or blocked step again (after inspecting what happened).
    pub fn retry_step(&self, plan_id: Uuid, key: &str) -> Result<String, String> {
        self.reset_step(plan_id, key, StepStatus::Pending)
    }

    /// Treat a step as done without running it (its dependents go ahead).
    pub fn skip_step(&self, plan_id: Uuid, key: &str) -> Result<String, String> {
        self.reset_step(plan_id, key, StepStatus::Skipped)
    }

    fn reset_step(&self, plan_id: Uuid, key: &str, to: StepStatus) -> Result<String, String> {
        let mut plan = self.plan(plan_id)?.ok_or("no such plan")?;
        if plan.status == PlanStatus::Running {
            return Err("the plan is running".into());
        }
        let id = plan.find_step(key).ok_or(format!("no step {key}"))?.id;
        let s = plan.step_mut(id).unwrap();
        if s.status == StepStatus::Completed {
            return Err(format!("{} already completed", s.key));
        }
        s.status = to;
        s.attempts = 0;
        s.last_error = None;
        let note = format!("{} is now {to}", s.key);
        if plan.status == PlanStatus::Failed {
            plan.status = PlanStatus::Paused;
        }
        self.db(self.store.save_plan(&plan))?;
        Ok(note)
    }

    // ---- P8: resume after a restart

    /// Find plans that were interrupted. A step that was running when the
    /// process stopped is only rerun if repeating it is safe; otherwise it's
    /// blocked until someone inspects it. Returns notes.
    pub fn recover(&self, runtime: &dyn Runtime) -> Result<Vec<String>, String> {
        let mut notes = Vec::new();
        for mut plan in self.db(self.store.unfinished())? {
            if plan.status != PlanStatus::Running {
                notes.push(format!("plan {} is paused: {}", short(plan.id), plan.note.clone().unwrap_or_default()));
                continue;
            }
            let mut blocked = Vec::new();
            for s in plan.steps.iter_mut().filter(|s| s.status == StepStatus::Running) {
                // P8/P15: check the actual state where we can. A tool step whose
                // operation was recorded did take effect; rerunning it just
                // reuses the recorded result.
                let recorded = match (s.operation_id, &s.action) {
                    (Some(op), StepAction::Tool { .. }) => self.db(self.store.operation(op))?.is_some(),
                    _ => false,
                };
                if s.idempotency == Idempotency::Safe || recorded {
                    s.status = StepStatus::Pending;
                } else {
                    s.status = StepStatus::Blocked;
                    s.last_error = Some("was running when lyra stopped; check whether it took effect".into());
                    blocked.push((s.id, s.key.clone()));
                }
            }
            plan.status = PlanStatus::Paused;
            let keys: Vec<String> = blocked.iter().map(|(_, k)| k.clone()).collect();
            plan.note = Some(if blocked.is_empty() {
                "interrupted by a restart; /plan resume continues".into()
            } else {
                format!("interrupted during {}; check them, then /plan retry or /plan skip", keys.join(", "))
            });
            self.db(self.store.save_plan(&plan))?;
            for (id, key) in &blocked {
                self.event(runtime, ExecutionEvent::new(plan.id, Some(*id), EventKind::StepBlocked, format!("{key} needs checking after a restart")));
            }
            self.event(runtime, ExecutionEvent::new(plan.id, None, EventKind::PlanPaused, plan.note.clone().unwrap()));
            notes.push(format!("plan {} {}", short(plan.id), plan.note.clone().unwrap()));
        }
        Ok(notes)
    }

    // ---- P4–P9, P11, P12, P17: running

    /// Run (or resume) a plan until it finishes, fails, pauses for approval
    /// or runs out of budget.
    pub fn run(&self, plan_id: Uuid, runtime: &dyn Runtime) -> Result<RunOutcome, String> {
        let mut plan = self.plan(plan_id)?.ok_or("no such plan")?;
        if plan.status.is_finished() {
            return Err(format!("the plan is already {}", plan.status));
        }
        let (mut goal, _) = self.goal(plan.goal_id)?.ok_or("the plan's goal is missing")?;
        if plan.steps.iter().any(|s| s.status == StepStatus::Running) {
            return Err("a step is marked running; /plan retry or /plan skip it first".into());
        }
        goal.status = GoalStatus::Active;
        self.db(self.store.save_goal(&goal))?;
        plan.status = PlanStatus::Running;
        plan.note = None;
        self.save(&plan)?;
        self.event(runtime, ExecutionEvent::new(plan.id, None, EventKind::PlanStarted, format!("v{}", plan.version)));

        let clock = Instant::now();
        let base_seconds = plan.usage.seconds;
        loop {
            plan.usage.seconds = base_seconds + clock.elapsed().as_secs();
            if self.cancelled.lock().unwrap().remove(&plan.id) {
                plan.status = PlanStatus::Cancelled;
                cancel_open_steps(&mut plan);
                self.save(&plan)?;
                self.finish_goal(&plan, GoalStatus::Cancelled, &json!({"summary": "cancelled while running"}))?;
                self.event(runtime, ExecutionEvent::new(plan.id, None, EventKind::ExecutionCancelled, "cancelled"));
                return Ok(RunOutcome { plan, goal_status: Some(GoalStatus::Cancelled), evaluation: None });
            }
            if let Some(why) = budget::exhausted(&plan.budget, &plan.usage) {
                return self.pause(plan, runtime, EventKind::BudgetExhausted, why);
            }

            let ready: Vec<PlanStep> = graph::ready_steps(&plan.steps).into_iter().cloned().collect();
            if ready.is_empty() {
                if plan.steps.iter().all(|s| s.is_settled()) {
                    return self.conclude(plan, &goal, runtime);
                }
                if plan.steps.iter().any(|s| s.status == StepStatus::Blocked) {
                    let waiting: Vec<String> =
                        plan.steps.iter().filter(|s| s.status == StepStatus::Blocked).map(|s| s.key.clone()).collect();
                    return self.pause(plan, runtime, EventKind::PlanPaused, format!("waiting on {}", waiting.join(", ")));
                }
                let stuck: Vec<String> = graph::blocked_steps(&plan.steps).iter().map(|s| s.key.clone()).collect();
                let why = if stuck.is_empty() { "no step can run".to_string() } else { format!("{} can't run: something they depend on failed", stuck.join(", ")) };
                return self.fail(plan, &goal, runtime, why);
            }

            // Fill in arguments that come from earlier results, so that
            // approval (and execution) see the concrete call.
            let mut bound_any = false;
            for s in ready.iter().filter(|s| planner::has_placeholders(&s.action)) {
                bound_any = true;
                if let Err(e) = self.bind(&mut plan, s.id, runtime) {
                    let error = format!("couldn't fill in its arguments: {e}");
                    let st = plan.step_mut(s.id).unwrap();
                    st.status = StepStatus::Failed;
                    st.failure_class = Some(FailureClass::Dependency);
                    st.last_error = Some(error.clone());
                    self.event(runtime, ExecutionEvent::new(plan.id, Some(s.id), EventKind::StepFailed, format!("{} {error}", s.key)));
                    self.replan(&mut plan, &goal, s.id, runtime)?;
                    self.save(&plan)?;
                    if plan.status == PlanStatus::Failed {
                        self.finish_goal(&plan, GoalStatus::Failed, &json!({"summary": plan.note}))?;
                        return Ok(RunOutcome { plan, goal_status: Some(GoalStatus::Failed), evaluation: None });
                    }
                }
            }
            if bound_any {
                self.save(&plan)?;
                continue;
            }

            // P14: steps that need approval wait for it.
            let mut runnable: Vec<&PlanStep> = Vec::new();
            for s in &ready {
                if s.needs_approval() {
                    let st = plan.step_mut(s.id).unwrap();
                    if st.status != StepStatus::Blocked {
                        st.status = StepStatus::Blocked;
                        st.last_error = Some("needs approval".into());
                        let msg = format!("{} {}: {}", s.key, s.title, serde_json::to_string(&s.action).unwrap_or_default());
                        self.event(runtime, ExecutionEvent::new(plan.id, Some(s.id), EventKind::ApprovalRequested, msg));
                    }
                } else {
                    runnable.push(s);
                }
            }
            if runnable.is_empty() {
                let waiting: Vec<String> =
                    plan.steps.iter().filter(|s| s.status == StepStatus::Blocked).map(|s| s.key.clone()).collect();
                return self.pause(plan, runtime, EventKind::PlanPaused, format!("{} need approval (/plan approve)", waiting.join(", ")));
            }

            // P12: what can run together. P9: a checkpoint before anything that changes things.
            let batch = graph::schedule(&runnable, self.settings.max_parallel);
            if batch.iter().any(|id| plan.step(*id).is_some_and(|s| s.idempotency != Idempotency::Safe)) {
                self.checkpoint(&plan, runtime, "before a step that changes things")?;
            }
            // Persist before running (rule 6).
            for id in &batch {
                let s = plan.step_mut(*id).unwrap();
                s.status = StepStatus::Running;
                s.started_at = Some(Utc::now());
                s.attempts += 1;
                let (key, title) = (s.key.clone(), s.title.clone());
                self.event(runtime, ExecutionEvent::new(plan.id, Some(*id), EventKind::StepStarted, format!("{key} {title}")));
            }
            self.save(&plan)?;

            let snapshot = plan.clone();
            let runs: Vec<StepRun> = std::thread::scope(|scope| {
                let handles: Vec<_> = batch
                    .iter()
                    .map(|id| {
                        let (plan, goal) = (&snapshot, &goal);
                        scope.spawn(move || self.execute(plan, goal, *id, runtime))
                    })
                    .collect();
                handles.into_iter().map(|h| h.join().expect("step thread panicked")).collect()
            });

            let mut backoff = Duration::ZERO;
            for run in runs {
                add_usage(&mut plan.usage, &run.usage);
                if let Some(wait) = self.settle(&mut plan, &goal, run, runtime)? {
                    backoff = backoff.max(wait);
                }
                self.save(&plan)?;
                if plan.status == PlanStatus::Failed {
                    self.finish_goal(&plan, GoalStatus::Failed, &json!({"summary": plan.note}))?;
                    return Ok(RunOutcome { plan, goal_status: Some(GoalStatus::Failed), evaluation: None });
                }
            }
            if !backoff.is_zero() {
                std::thread::sleep(backoff);
            }
        }
    }

    /// Ask the model to fill in a tool step's placeholders from its
    /// dependencies' results.
    fn bind(&self, plan: &mut Plan, id: Uuid, runtime: &dyn Runtime) -> Result<(), String> {
        let step = plan.step(id).unwrap().clone();
        let StepAction::Tool { tool, .. } = &step.action else { return Ok(()) };
        let tools = runtime.tools();
        let info = tools.iter().find(|t| &t.name == tool);
        let mut usage = BudgetUsage::default();
        let arguments = ask(runtime, &mut usage, planner::BIND_PROMPT, &planner::bind_prompt(plan, &step, info), planner::parse_bound);
        add_usage(&mut plan.usage, &usage);
        let arguments = arguments??;
        let msg = format!("{} arguments filled in: {arguments}", step.key);
        let s = plan.step_mut(id).unwrap();
        s.action = StepAction::Tool { tool: tool.clone(), arguments };
        self.event(runtime, ExecutionEvent::new(plan.id, Some(id), EventKind::StepReady, msg));
        Ok(())
    }

    /// Carry out one step's action (P4). Runs on its own thread.
    fn execute(&self, plan: &Plan, goal: &Goal, id: Uuid, runtime: &dyn Runtime) -> StepRun {
        let started = Utc::now();
        let step = plan.step(id).expect("scheduled step exists");
        let mut usage = BudgetUsage::default();
        let mut context = format!("Overall goal: {}\nThis step: {} — {}", goal.description, step.title, step.description);
        if let Some(expected) = &step.expected_outcome {
            context += &format!("\nExpected outcome: {expected}");
        }
        for dep in step.dependencies.iter().filter_map(|d| plan.step(*d)) {
            if let Some(r) = &dep.result {
                context += &format!("\n\nResult of {} ({}):\n{}", dep.key, dep.title, truncate(&output_text(&r.output), 2000));
            }
        }
        // A retry must not redo what an earlier attempt already changed.
        let done_before = changes_of(step.result.as_ref());
        if !done_before.is_empty() {
            context += &format!("\n\nAlready done by an earlier attempt (don't repeat these):\n- {}", done_before.join("\n- "));
        }
        let may_change = step.idempotency != Idempotency::Safe;
        // P11: the step works within what's left of the budget.
        let max_model_calls = plan.budget.max_model_calls.map(|max| max.saturating_sub(plan.usage.model_calls).max(1));
        if budget::running_low(&plan.budget, &plan.usage) {
            context += "\n\nThe plan's budget is running low: be brief and use as few tool calls as you can.";
        }
        let mut changes = Vec::new();
        let mut tools_used = Vec::new();
        let mut reasoned = |task: Task, usage: &mut BudgetUsage| match runtime.reason(&task) {
            Ok(r) => {
                usage.model_calls += r.model_calls;
                usage.tool_calls += r.tool_calls;
                usage.tokens += r.tokens;
                changes = r.changes;
                tools_used = r.tools_used;
                Ok(Value::String(r.text))
            }
            Err(e) => {
                usage.model_calls += 1;
                changes = e.changes;
                Err(e.error)
            }
        };
        let outcome: Result<Value, String> = match &step.action {
            StepAction::Tool { tool, arguments } => {
                usage.tool_calls += 1;
                self.call_once(plan, step, tool, arguments, runtime)
            }
            StepAction::Reasoning { instruction } => {
                reasoned(Task { instruction: instruction.clone(), context, tools: None, may_change, max_model_calls, agent: None }, &mut usage)
            }
            StepAction::Workflow { workflow, input } => match runtime.workflow(workflow) {
                Some(procedure) => {
                    let instruction = format!("Follow this procedure:\n{procedure}\n\nInput: {input}");
                    reasoned(Task { instruction, context, tools: None, may_change, max_model_calls, agent: None }, &mut usage)
                }
                None => Err(format!("no workflow named {workflow}")),
            },
            StepAction::Subagent { agent, task } => match runtime.agent_tools(agent) {
                // P13: a scoped task, not the parent's whole context.
                Some(tools) => {
                    let mut scoped = format!(
                        "You are the {agent} agent. Do this task and report what you found, with the evidence for it.\n\
                         End your report with a line `STATUS: done`, or `STATUS: failed — <why>` if you couldn't do it.\n\
                         Goal it serves: {}",
                        goal.description
                    );
                    if let Some(expected) = &step.expected_outcome {
                        scoped += &format!("\nExpected outcome: {expected}");
                    }
                    if !done_before.is_empty() {
                        scoped += &format!("\n\nAlready done by an earlier attempt (don't repeat these):\n- {}", done_before.join("\n- "));
                    }
                    // P13: the subagent's report says whether it succeeded.
                    reasoned(Task { instruction: task.clone(), context: scoped, tools: Some(tools), may_change, max_model_calls, agent: Some(agent.clone()) }, &mut usage)
                        .and_then(subagent_status)
                }
                None => Err(format!("no agent named {agent}")),
            },
        };
        // Changes accumulate across attempts.
        let mut all_changes = done_before;
        all_changes.extend(changes);
        if let StepAction::Tool { tool, .. } = &step.action {
            tools_used.push(tool.clone());
        }
        let metadata = json!({ "action": step.action.kind(), "changes": all_changes, "tools": tools_used });
        let result = match outcome {
            Ok(output) => StepResult { success: true, output, error: None, metadata },
            Err(e) => StepResult { success: false, output: Value::Null, error: Some(e), metadata },
        };
        StepRun { id, started, result, usage }
    }

    /// P15: a tool step with an operation id runs at most once. If the
    /// operation already completed (a retry, or a resume after a restart),
    /// its recorded result is returned instead of acting again.
    fn call_once(&self, plan: &Plan, step: &PlanStep, tool: &str, arguments: &Value, runtime: &dyn Runtime) -> Result<Value, String> {
        let Some(op) = step.operation_id else { return runtime.call_tool(tool, arguments, None) };
        if let Some(result) = self.db(self.store.operation(op))? {
            self.event(runtime, ExecutionEvent::new(plan.id, Some(step.id), EventKind::StepStarted, format!("{} already done (operation {}); reusing its result", step.key, short(op))));
            return Ok(result);
        }
        let result = runtime.call_tool(tool, arguments, Some(op))?;
        self.db(self.store.record_operation(op, plan.id, step.id, tool, &result))?;
        Ok(result)
    }

    /// Record a step's result, verify it (P5), and decide what happens next:
    /// done, retry (P6) or replan (P7). Returns a backoff before retrying.
    fn settle(&self, plan: &mut Plan, goal: &Goal, run: StepRun, runtime: &dyn Runtime) -> Result<Option<Duration>, String> {
        let id = run.id;
        {
            let s = plan.step_mut(id).unwrap();
            s.result = Some(run.result.clone());
            s.completed_at = Some(Utc::now());
        }
        let step = plan.step(id).unwrap().clone();
        self.db(self.store.record_attempt(plan, &step, run.started, &run.result))?;

        let failure = if run.result.success {
            self.event(runtime, ExecutionEvent::new(plan.id, Some(id), EventKind::VerificationStarted, step.key.clone()));
            let (verdict, usage) = self.verify(&step, &run.result, runtime);
            add_usage(&mut plan.usage, &usage);
            self.db(self.store.record_verification(plan.id, &step, &verdict))?;
            let data = json!({ "verified": verdict.verified, "evidence": verdict.evidence, "reason": verdict.reason });
            self.event(
                runtime,
                ExecutionEvent::new(plan.id, Some(id), EventKind::VerificationCompleted, format!("{} {}", step.key, if verdict.verified { "verified" } else { "not verified" })).data(data),
            );
            let verified = verdict.verified;
            let reason = verdict.reason.clone().unwrap_or_else(|| "not verified".into());
            plan.step_mut(id).unwrap().verification_result = Some(verdict);
            if verified {
                let s = plan.step_mut(id).unwrap();
                s.status = StepStatus::Completed;
                s.last_error = None;
                self.event(runtime, ExecutionEvent::new(plan.id, Some(id), EventKind::StepCompleted, format!("{} {}", step.key, step.title)));
                return Ok(None);
            }
            (FailureClass::Verification, format!("not verified: {reason}"))
        } else {
            let error = run.result.error.clone().unwrap_or_else(|| "failed".into());
            (retry::classify(&error), error)
        };

        let (class, error) = failure;
        let s = plan.step_mut(id).unwrap();
        s.failure_class = Some(class);
        s.last_error = Some(error.clone());
        // Work that already changed something isn't simply run again: the
        // replanner sees what was done and decides.
        let changed = !matches!(s.action, StepAction::Tool { .. }) && !changes_of(s.result.as_ref()).is_empty();
        if !changed && retry::should_retry(&s.retry_policy, class, s.attempts) {
            s.status = StepStatus::Pending;
            let wait = retry::backoff(&s.retry_policy, s.attempts);
            plan.usage.retries += 1;
            let msg = format!("{} attempt {} failed ({class}): {}; retrying in {}s", step.key, step.attempts, truncate(&error, 120), wait.as_secs());
            self.event(runtime, ExecutionEvent::new(plan.id, Some(id), EventKind::RetryScheduled, msg));
            return Ok(Some(wait));
        }
        s.status = StepStatus::Failed;
        self.event(
            runtime,
            ExecutionEvent::new(plan.id, Some(id), EventKind::StepFailed, format!("{} failed ({class}): {}", step.key, truncate(&error, 200))),
        );
        self.replan(plan, goal, id, runtime)?;
        Ok(None)
    }

    /// Check a step really did what it was meant to (P5). Deterministic
    /// strategies first; the model only for judgment calls.
    fn verify(&self, step: &PlanStep, result: &StepResult, runtime: &dyn Runtime) -> (VerificationResult, BudgetUsage) {
        let mut usage = BudgetUsage::default();
        let output = output_text(&result.output);
        let contains = |text: &str, needle: &str| text.to_lowercase().contains(&needle.to_lowercase());
        let v = &step.verification;
        let verdict = match &v.strategy {
            VerificationStrategy::ToolResult => VerificationResult {
                verified: result.success,
                evidence: vec![format!("{} step reported success", step.action.kind())],
                reason: None,
            },
            VerificationStrategy::StateCheck => match v.check.as_deref().filter(|c| !c.is_empty()) {
                Some(check) => VerificationResult {
                    verified: contains(&output, check),
                    evidence: vec![truncate(&output, 200)],
                    reason: Some(format!("output {} {check:?}", if contains(&output, check) { "contains" } else { "doesn't contain" })),
                },
                None => VerificationResult { verified: result.success, evidence: vec![], reason: Some("no check given".into()) },
            },
            VerificationStrategy::FollowUpTool => {
                let tool = v.tool.clone().unwrap_or_default();
                usage.tool_calls += 1;
                // `{{result.x}}` / `{{args.x}}` come from the step being checked.
                let step_args = match &step.action {
                    StepAction::Tool { arguments, .. } => arguments.clone(),
                    _ => json!({}),
                };
                let arguments = crate::check::fill(v.arguments.as_ref().unwrap_or(&json!({})), &result.output, &step_args);
                match runtime.call_tool(&tool, &arguments, None) {
                    Ok(out) => {
                        let text = output_text(&out);
                        let expression = v.check.as_deref().map(|c| crate::check::fill(&json!(c), &result.output, &step_args));
                        let (verified, reason) = crate::check::check(&out, expression.as_ref().and_then(Value::as_str).unwrap_or(""));
                        VerificationResult { verified, evidence: vec![format!("{tool}: {}", truncate(&text, 300))], reason: Some(reason) }
                    }
                    Err(e) => VerificationResult { verified: false, evidence: vec![], reason: Some(format!("{tool} failed: {e}")) },
                }
            }
            VerificationStrategy::ModelEvaluation | VerificationStrategy::Custom(_) => {
                let check = match &v.strategy {
                    VerificationStrategy::Custom(c) => Some(c.as_str()),
                    _ => v.check.as_deref(),
                };
                let prompt = planner::verify_prompt(step, check, &output);
                if let Some((verified, sure)) =
                    runtime.decide_yes(&prompt, "Does the evidence show this step achieved what it was meant to? Don't assume success.")
                {
                    let reason = format!("{} by the decision model ({:.0}% sure)", if verified { "verified" } else { "not verified" }, sure * 100.0);
                    return (VerificationResult { verified, evidence: vec![], reason: Some(reason) }, usage);
                }
                ask(runtime, &mut usage, planner::VERIFY_PROMPT, &prompt, planner::parse_verification).unwrap_or_else(|e| {
                    VerificationResult { verified: false, evidence: vec![], reason: Some(format!("couldn't verify: {e}")) }
                })
            }
        };
        (verdict, usage)
    }

    /// Revise the broken part of the plan (P7), or fail if that's not allowed
    /// or doesn't work.
    fn replan(&self, plan: &mut Plan, goal: &Goal, failed: Uuid, runtime: &dyn Runtime) -> Result<(), String> {
        if !budget::can_replan(&plan.budget, &plan.usage) {
            plan.status = PlanStatus::Failed;
            plan.note = Some(format!("{} failed and the replan budget is used up", plan.step(failed).unwrap().key));
            self.event(runtime, ExecutionEvent::new(plan.id, Some(failed), EventKind::ExecutionFailed, plan.note.clone().unwrap()));
            return Ok(());
        }
        self.event(runtime, ExecutionEvent::new(plan.id, Some(failed), EventKind::ReplanStarted, plan.step(failed).unwrap().key.clone()));
        let mut ctx = runtime.context(&goal.description);
        if budget::running_low(&plan.budget, &plan.usage) {
            ctx.budget_note = Some("the budget is running low: keep the revision as small as possible".into());
        }
        plan.usage.replans += 1;
        let prompt = planner::replan_prompt(goal, plan, plan.step(failed).unwrap(), &ctx);
        let (from, draft) = (plan.version, plan.clone());
        let mut usage = BudgetUsage::default();
        let result = ask(runtime, &mut usage, &planner::replan_system_prompt(), &prompt, |reply| {
            let mut next = draft.clone();
            planner::revise(&mut next, reply, failed, &ctx).map(|r| (next, r))
        });
        add_usage(&mut plan.usage, &usage);
        match result {
            Ok((next, revision)) => {
                let usage = plan.usage;
                *plan = next;
                plan.usage = usage;
                guard(goal, &mut plan.steps);
                self.db(self.store.record_revision(plan.id, from, plan.version, &revision))?;
                let msg = format!(
                    "v{} → v{}: -{} +{} ~{} — {}",
                    from,
                    plan.version,
                    revision.removed_steps.len(),
                    revision.added_steps.len(),
                    revision.modified_steps.len(),
                    revision.reason
                );
                self.event(runtime, ExecutionEvent::new(plan.id, Some(failed), EventKind::PlanRevised, msg));
            }
            Err(e) => {
                plan.status = PlanStatus::Failed;
                plan.note = Some(format!("{} failed and replanning didn't work: {e}", plan.step(failed).unwrap().key));
                self.event(runtime, ExecutionEvent::new(plan.id, Some(failed), EventKind::ExecutionFailed, plan.note.clone().unwrap()));
            }
        }
        Ok(())
    }

    /// Every step is done: judge the goal against its success criteria (P17).
    fn conclude(&self, mut plan: Plan, goal: &Goal, runtime: &dyn Runtime) -> Result<RunOutcome, String> {
        let mut usage = BudgetUsage::default();
        let evaluation =
            ask(runtime, &mut usage, planner::EVALUATE_PROMPT, &planner::evaluate_prompt(goal, &plan), planner::parse_evaluation);
        add_usage(&mut plan.usage, &usage);
        let (status, evaluation) = match evaluation {
            Ok(e) => (e.status(), Some(e)),
            Err(e) => {
                plan.note = Some(format!("couldn't judge the goal: {e}"));
                (GoalStatus::Partial, None)
            }
        };
        plan.status = if status == GoalStatus::Failed { PlanStatus::Failed } else { PlanStatus::Completed };
        if status == GoalStatus::Partial && plan.note.is_none() {
            plan.note = Some("every step finished, but not every success criterion was met".into());
        }
        self.save(&plan)?;
        let summary = evaluation.as_ref().map(|e| json!({ "summary": e.summary, "answer": e.answer, "criteria": e.criteria.iter().map(|c| json!({"criterion": c.criterion, "met": c.met, "evidence": c.evidence})).collect::<Vec<_>>() }));
        self.finish_goal(&plan, status, summary.as_ref().unwrap_or(&json!({})))?;
        let kind = if status == GoalStatus::Failed { EventKind::ExecutionFailed } else { EventKind::ExecutionCompleted };
        self.event(runtime, ExecutionEvent::new(plan.id, None, kind, format!("goal {status}")));
        Ok(RunOutcome { plan, goal_status: Some(status), evaluation })
    }

    fn fail(&self, mut plan: Plan, _goal: &Goal, runtime: &dyn Runtime, why: String) -> Result<RunOutcome, String> {
        plan.status = PlanStatus::Failed;
        plan.note = Some(why.clone());
        self.save(&plan)?;
        self.finish_goal(&plan, GoalStatus::Failed, &json!({ "summary": why }))?;
        self.event(runtime, ExecutionEvent::new(plan.id, None, EventKind::ExecutionFailed, why));
        Ok(RunOutcome { plan, goal_status: Some(GoalStatus::Failed), evaluation: None })
    }

    fn pause(&self, mut plan: Plan, runtime: &dyn Runtime, kind: EventKind, why: String) -> Result<RunOutcome, String> {
        plan.status = PlanStatus::Paused;
        plan.note = Some(why.clone());
        self.save(&plan)?;
        self.event(runtime, ExecutionEvent::new(plan.id, None, kind, why));
        Ok(RunOutcome { plan, goal_status: None, evaluation: None })
    }

    fn finish_goal(&self, plan: &Plan, status: GoalStatus, evaluation: &Value) -> Result<(), String> {
        self.db(self.store.set_goal_outcome(plan.goal_id, status, evaluation))
    }

    fn checkpoint(&self, plan: &Plan, runtime: &dyn Runtime, reason: &str) -> Result<(), String> {
        let c = Checkpoint {
            id: Uuid::new_v4(),
            plan_id: plan.id,
            plan_version: plan.version,
            completed_steps: plan.steps.iter().filter(|s| s.status == StepStatus::Completed).map(|s| s.id).collect(),
            runtime_state: json!({ "usage": plan.usage }),
            reason: reason.to_string(),
            created_at: Utc::now(),
        };
        self.db(self.store.save_checkpoint(&c))?;
        self.event(runtime, ExecutionEvent::new(plan.id, None, EventKind::CheckpointCreated, format!("{} steps done", c.completed_steps.len())));
        Ok(())
    }

    fn save(&self, plan: &Plan) -> Result<(), String> {
        let mut plan = plan.clone();
        plan.updated_at = Utc::now();
        self.db(self.store.save_plan(&plan))
    }
}

/// Ask the model for a structured answer, and once more if the reply can't be
/// used (reasoning models sometimes return an empty answer). Every call is
/// counted in `usage`.
fn ask<T>(
    runtime: &dyn Runtime,
    usage: &mut BudgetUsage,
    system: &str,
    user: &str,
    mut parse: impl FnMut(&str) -> Result<T, String>,
) -> Result<T, String> {
    let mut last = String::new();
    for _ in 0..2 {
        let (reply, tokens) = runtime.complete(system, user)?;
        usage.model_calls += 1;
        usage.tokens += tokens;
        match parse(&reply) {
            Ok(value) => return Ok(value),
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn add_usage(total: &mut BudgetUsage, add: &BudgetUsage) {
    total.model_calls += add.model_calls;
    total.tool_calls += add.tool_calls;
    total.tokens += add.tokens;
}

/// P1/P14: when the goal itself is destructive or external, every step that
/// may change something needs approval, whatever the planner said.
fn guard(goal: &Goal, steps: &mut [PlanStep]) {
    if goal.destructive {
        for s in steps.iter_mut().filter(|s| s.idempotency != Idempotency::Safe && s.approval == ApprovalPolicy::Automatic) {
            s.approval = ApprovalPolicy::RequireApproval;
        }
    }
}

/// A subagent's report: failed when its status line says so; the status line
/// itself is dropped from the output.
fn subagent_status(report: Value) -> Result<Value, String> {
    let text = report.as_str().unwrap_or_default();
    let status = text.lines().rev().find(|l| l.trim().to_lowercase().starts_with("status:"));
    let body = text.lines().filter(|l| Some(*l) != status).collect::<Vec<_>>().join("\n").trim().to_string();
    match status.map(|l| l.trim()["status:".len()..].trim().to_string()) {
        Some(s) if s.to_lowercase().starts_with("failed") => {
            let why = s["failed".len()..].trim_start_matches(|c: char| c == '—' || c == '-' || c == ':' || c.is_whitespace());
            Err(format!("the subagent couldn't do it: {}", if why.is_empty() { body.as_str() } else { why }))
        }
        _ => Ok(Value::String(body)),
    }
}

/// Steps that hadn't finished when a plan was cancelled.
fn cancel_open_steps(plan: &mut Plan) {
    for s in plan.steps.iter_mut().filter(|s| matches!(s.status, StepStatus::Pending | StepStatus::Blocked)) {
        s.status = StepStatus::Cancelled;
    }
}

/// What a step's attempts changed so far (from its result's metadata).
fn changes_of(result: Option<&StepResult>) -> Vec<String> {
    result
        .and_then(|r| r.metadata.get("changes"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|c| c.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

pub fn short(id: Uuid) -> String {
    id.to_string()[..8].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::{AgentInfo, Risk};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A scripted runtime: model replies are taken from queues by prompt kind,
    /// tool results from a closure.
    #[derive(Default)]
    struct Script {
        goal: Mutex<VecDeque<String>>,
        plan: Mutex<VecDeque<String>>,
        replan: Mutex<VecDeque<String>>,
        verify: Mutex<VecDeque<String>>,
        bind: Mutex<VecDeque<String>>,
        evaluate: Mutex<VecDeque<String>>,
        reasoning: Mutex<VecDeque<Result<String, String>>>,
        tool_failures: Mutex<VecDeque<String>>,
        tool_calls: AtomicU32,
        tool_args: Mutex<Vec<Value>>,
        events: Mutex<Vec<EventKind>>,
        /// What a failing reasoning call had changed.
        reason_changes: Mutex<Vec<String>>,
        /// `may_change` of each reasoning task.
        may_change: Mutex<Vec<bool>>,
    }

    fn pop(q: &Mutex<VecDeque<String>>, what: &str) -> Result<(String, u64), String> {
        q.lock().unwrap().pop_front().map(|r| (r, 100)).ok_or(format!("no scripted {what} reply"))
    }

    impl Runtime for Script {
        fn complete(&self, system: &str, _user: &str) -> Result<(String, u64), String> {
            if system == planner::GOAL_PROMPT {
                pop(&self.goal, "goal")
            } else if system.starts_with("You plan") {
                pop(&self.plan, "plan")
            } else if system.starts_with("A step in an AI agent's plan failed") {
                pop(&self.replan, "replan")
            } else if system == planner::VERIFY_PROMPT {
                pop(&self.verify, "verify")
            } else if system == planner::BIND_PROMPT {
                pop(&self.bind, "bind")
            } else {
                pop(&self.evaluate, "evaluate")
            }
        }
        fn context(&self, _goal: &str) -> PlanningContext {
            PlanningContext {
                tools: vec![
                    crate::planner::tests::tool("check", Risk::ReadOnly),
                    crate::planner::tests::tool("deploy", Risk::Mutating),
                    crate::planner::tests::tool("wipe", Risk::Destructive),
                ],
                agents: vec![AgentInfo { name: "researcher".into(), description: "reads".into(), changes_things: false }],
                ..Default::default()
            }
        }
        fn call_tool(&self, tool: &str, arguments: &Value, _op: Option<Uuid>) -> Result<Value, String> {
            self.tool_calls.fetch_add(1, Ordering::SeqCst);
            self.tool_args.lock().unwrap().push(arguments.clone());
            if let Some(err) = self.tool_failures.lock().unwrap().pop_front() {
                return Err(err);
            }
            Ok(json!(format!("{tool} ok: service healthy")))
        }
        fn reason(&self, task: &Task) -> Result<Reasoned, ReasonError> {
            self.may_change.lock().unwrap().push(task.may_change);
            let text = self
                .reasoning
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Ok(format!("did: {}", task.instruction)))
                .map_err(|error| ReasonError { error, changes: self.reason_changes.lock().unwrap().clone() })?;
            Ok(Reasoned { text, model_calls: 1, tokens: 50, ..Default::default() })
        }
        fn workflow(&self, _name: &str) -> Option<String> {
            None
        }
        fn agent_tools(&self, agent: &str) -> Option<Vec<String>> {
            (agent == "researcher").then(|| vec!["check".into()])
        }
        fn emit(&self, e: &ExecutionEvent) {
            self.events.lock().unwrap().push(e.kind);
        }
    }

    const GOAL: &str = r#"{"description":"Deploy the service and verify it","success_criteria":["service deployed","service healthy"]}"#;
    const MET: &str = r#"{"criteria":[{"criterion":"service deployed","met":true},{"criterion":"service healthy","met":true}],"summary":"deployed and healthy"}"#;

    fn engine() -> (tokio::runtime::Runtime, Engine) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let store = rt.block_on(PlanStore::in_memory()).unwrap();
        let e = Engine::with_store(store, rt.handle().clone(), Settings::default());
        (rt, e)
    }

    fn script(plan: &str) -> Script {
        let s = Script::default();
        s.goal.lock().unwrap().push_back(GOAL.into());
        s.plan.lock().unwrap().push_back(plan.into());
        s
    }

    /// Zero backoff, so tests don't sleep.
    fn fast(plan: &mut Plan) {
        for s in &mut plan.steps {
            s.retry_policy.backoff_ms = 0;
        }
    }

    fn prepare(e: &Engine, s: &Script) -> Plan {
        let (_, mut plan) = e.create("deploy it", s).unwrap();
        fast(&mut plan);
        e.save(&plan).unwrap();
        plan
    }

    /// The doc's scenario: step 1 succeeds, step 2 fails transiently and its
    /// retry succeeds, step 3 fails verification, the planner adds a
    /// corrective step, it succeeds, and the goal is met.
    #[test]
    fn retry_replan_and_goal_scenario() {
        let (_rt, e) = engine();
        let s = script(
            r#"{"steps":[
            {"key":"s1","title":"Inspect","action":{"type":"tool","tool":"check","arguments":{}}},
            {"key":"s2","title":"Deploy","depends_on":["s1"],"action":{"type":"tool","tool":"deploy","arguments":{}}},
            {"key":"s3","title":"Verify health","depends_on":["s2"],"action":{"type":"reasoning","instruction":"confirm health"},
             "verification":{"strategy":"model_evaluation"}}]}"#,
        );
        let plan = prepare(&e, &s);

        // Use the failure queue in call order: s1 ok, s2 timeout, s2 ok.
        let order = Mutex::new(VecDeque::from([None, Some("request timed out"), None]));
        struct Ordered<'a> {
            inner: &'a Script,
            order: &'a Mutex<VecDeque<Option<&'static str>>>,
        }
        impl Runtime for Ordered<'_> {
            fn complete(&self, a: &str, b: &str) -> Result<(String, u64), String> {
                self.inner.complete(a, b)
            }
            fn context(&self, g: &str) -> PlanningContext {
                self.inner.context(g)
            }
            fn call_tool(&self, t: &str, a: &Value, o: Option<Uuid>) -> Result<Value, String> {
                if let Some(Some(err)) = self.order.lock().unwrap().pop_front() {
                    return Err(err.into());
                }
                self.inner.call_tool(t, a, o)
            }
            fn reason(&self, t: &Task) -> Result<Reasoned, ReasonError> {
                self.inner.reason(t)
            }
            fn workflow(&self, n: &str) -> Option<String> {
                self.inner.workflow(n)
            }
            fn agent_tools(&self, a: &str) -> Option<Vec<String>> {
                self.inner.agent_tools(a)
            }
            fn emit(&self, ev: &ExecutionEvent) {
                self.inner.emit(ev)
            }
        }
        let rt = Ordered { inner: &s, order: &order };
        // s3's verification fails; the replan replaces it with a corrective step.
        s.verify.lock().unwrap().push_back(r#"{"verified":false,"reason":"health endpoint returned 500"}"#.into());
        s.replan.lock().unwrap().push_back(
            r#"{"remove":["s3"],"add":[{"key":"s4","title":"Restart and re-check","depends_on":["s2"],
               "action":{"type":"tool","tool":"check","arguments":{}},"verification":{"strategy":"state_check","check":"healthy"}}],
               "reason":"restart fixes the 500"}"#
                .into(),
        );
        s.evaluate.lock().unwrap().push_back(MET.into());

        let out = e.run(plan.id, &rt).unwrap();
        assert_eq!(out.goal_status, Some(GoalStatus::Completed));
        assert_eq!(out.plan.status, PlanStatus::Completed);
        assert_eq!(out.plan.version, 2, "plan version 2");
        assert_eq!(out.plan.usage.retries, 1, "one retry");
        assert_eq!(out.plan.usage.replans, 1, "one replan");
        assert!(out.plan.steps.iter().all(|s| s.status == StepStatus::Completed));
        let m = e.metrics(&out.plan).unwrap();
        assert_eq!((m.retries, m.replans, m.verification_failures), (1, 1, 1));
        assert!(m.recoveries >= 1, "the deploy step recovered");
        let events = s.events.lock().unwrap();
        for kind in [EventKind::RetryScheduled, EventKind::PlanRevised, EventKind::CheckpointCreated, EventKind::ExecutionCompleted] {
            assert!(events.contains(&kind), "missing {kind}");
        }
    }

    #[test]
    fn approval_gates_pause_until_approved() {
        let (_rt, e) = engine();
        let s = script(
            r#"{"steps":[{"key":"s1","title":"Wipe cache","action":{"type":"tool","tool":"wipe","arguments":{"path":"/tmp/x"}}}]}"#,
        );
        s.evaluate.lock().unwrap().push_back(r#"{"criteria":[{"criterion":"a","met":true}],"summary":"ok"}"#.into());
        let plan = prepare(&e, &s);
        let out = e.run(plan.id, &s).unwrap();
        assert_eq!(out.plan.status, PlanStatus::Paused);
        assert_eq!(out.plan.steps[0].status, StepStatus::Blocked);
        assert_eq!(s.tool_calls.load(Ordering::SeqCst), 0, "nothing ran without approval");

        e.approve(plan.id, "s1", &s).unwrap();
        let out = e.run(plan.id, &s).unwrap();
        assert_eq!(out.plan.status, PlanStatus::Completed);
        assert_eq!(s.tool_calls.load(Ordering::SeqCst), 1);
    }

    /// A destructive step whose argument comes from an earlier step: the
    /// argument is filled in first, and approval is for the concrete call.
    #[test]
    fn placeholders_are_bound_before_approval() {
        let (_rt, e) = engine();
        let s = script(
            r#"{"steps":[
            {"key":"s1","title":"Find it","action":{"type":"reasoning","instruction":"find the id"}},
            {"key":"s2","title":"Wipe it","depends_on":["s1"],"action":{"type":"tool","tool":"wipe","arguments":{"id":"{{s1}}"}}}]}"#,
        );
        s.reasoning.lock().unwrap().push_back(Ok("The id is abcd1234.".into()));
        s.bind.lock().unwrap().push_back(r#"{"arguments":{"id":"abcd1234"}}"#.into());
        s.evaluate.lock().unwrap().push_back(r#"{"criteria":[{"criterion":"a","met":true}],"summary":"ok"}"#.into());
        let plan = prepare(&e, &s);

        let out = e.run(plan.id, &s).unwrap();
        assert_eq!(out.plan.status, PlanStatus::Paused, "waits for approval");
        let wipe = out.plan.find_step("s2").unwrap();
        assert_eq!(wipe.action, StepAction::Tool { tool: "wipe".into(), arguments: json!({"id": "abcd1234"}) }, "bound before asking");
        assert_eq!(s.tool_calls.load(Ordering::SeqCst), 0);

        e.approve(plan.id, "s2", &s).unwrap();
        let out = e.run(plan.id, &s).unwrap();
        assert_eq!(out.plan.status, PlanStatus::Completed);
        assert_eq!(s.tool_args.lock().unwrap()[0], json!({"id": "abcd1234"}));
    }

    #[test]
    fn unbindable_placeholders_fail_the_step() {
        let (_rt, e) = engine();
        let s = script(
            r#"{"steps":[
            {"key":"s1","action":{"type":"reasoning","instruction":"find"}},
            {"key":"s2","depends_on":["s1"],"action":{"type":"tool","tool":"deploy","arguments":{"id":"{{s1}}"}}}]}"#,
        );
        s.bind.lock().unwrap().push_back(r#"{"error":"s1 found nothing"}"#.into());
        s.replan.lock().unwrap().push_back("no idea".into());
        let plan = prepare(&e, &s);
        let out = e.run(plan.id, &s).unwrap();
        assert_eq!(out.plan.status, PlanStatus::Failed);
        assert!(out.plan.find_step("s2").unwrap().last_error.as_ref().unwrap().contains("s1 found nothing"));
        assert_eq!(s.tool_calls.load(Ordering::SeqCst), 0, "never called with a placeholder");
    }

    #[test]
    fn an_unusable_reply_is_asked_again_once() {
        let (_rt, e) = engine();
        let s = Script::default();
        s.goal.lock().unwrap().extend(["".to_string(), GOAL.to_string()]);
        s.plan.lock().unwrap().push_back(r#"{"steps":[{"key":"s1","action":{"type":"reasoning","instruction":"a"}}]}"#.into());
        let (_, plan) = e.create("deploy it", &s).unwrap();
        assert_eq!(plan.usage.model_calls, 3, "the empty reply still counts");

        let s = Script::default();
        s.goal.lock().unwrap().extend(["".to_string(), "still nothing".to_string()]);
        assert!(e.create("deploy it", &s).unwrap_err().contains("no JSON"));
    }

    #[test]
    fn permission_failures_are_not_retried_and_failed_replans_fail_the_plan() {
        let (_rt, e) = engine();
        let s = script(r#"{"steps":[{"key":"s1","title":"Deploy","action":{"type":"tool","tool":"deploy","arguments":{}}}]}"#);
        let plan = prepare(&e, &s);
        s.tool_failures.lock().unwrap().push_back("permission denied".into());
        s.replan.lock().unwrap().push_back("I can't think of anything".into());
        let out = e.run(plan.id, &s).unwrap();
        assert_eq!(out.plan.status, PlanStatus::Failed);
        assert_eq!(out.plan.usage.retries, 0, "authentication failures aren't retried blindly");
        assert_eq!(out.goal_status, Some(GoalStatus::Failed));
        assert!(out.plan.note.unwrap().contains("replanning didn't work"));
    }

    #[test]
    fn budgets_pause_the_plan() {
        let (_rt, mut e) = engine();
        e.settings.budget = Budget { max_model_calls: Some(3), ..Budget::default() };
        let s = script(
            r#"{"steps":[
            {"key":"s1","action":{"type":"reasoning","instruction":"a"}},
            {"key":"s2","depends_on":["s1"],"action":{"type":"reasoning","instruction":"b"}},
            {"key":"s3","depends_on":["s2"],"action":{"type":"reasoning","instruction":"c"}}]}"#,
        );
        let plan = prepare(&e, &s);
        let out = e.run(plan.id, &s).unwrap();
        assert_eq!(out.plan.status, PlanStatus::Paused);
        assert!(out.plan.note.unwrap().contains("model call budget"));
        assert!(s.events.lock().unwrap().contains(&EventKind::BudgetExhausted));

        // Raising the budget lets it finish.
        assert!(e.raise_budget(plan.id, &Budget { max_model_calls: Some(10), ..Budget::default() }).unwrap().contains("budget raised"));
        s.evaluate.lock().unwrap().push_back(r#"{"criteria":[{"criterion":"a","met":true}],"summary":"ok"}"#.into());
        let out = e.run(plan.id, &s).unwrap();
        assert_eq!(out.plan.status, PlanStatus::Completed);
        assert_eq!(Engine::tools_used(&out.plan), Vec::<String>::new());
    }

    #[test]
    fn a_destructive_goal_puts_every_change_behind_approval() {
        let (_rt, e) = engine();
        let s = Script::default();
        s.goal.lock().unwrap().push_back(r#"{"description":"Rebuild the server","success_criteria":["rebuilt"],"destructive":true}"#.into());
        s.plan.lock().unwrap().push_back(
            r#"{"steps":[{"key":"s1","action":{"type":"tool","tool":"check","arguments":{}}},{"key":"s2","action":{"type":"tool","tool":"deploy","arguments":{}}}]}"#.into(),
        );
        let (_, plan) = e.create("rebuild the server", &s).unwrap();
        assert!(!plan.steps[0].needs_approval(), "reads don't need approval");
        assert!(plan.steps[1].needs_approval(), "changes do, though the planner didn't ask");
    }

    #[test]
    fn a_finished_plan_can_still_miss_the_goal() {
        let (_rt, e) = engine();
        let s = script(r#"{"steps":[{"key":"s1","action":{"type":"tool","tool":"deploy","arguments":{}}}]}"#);
        s.evaluate.lock().unwrap().push_back(
            r#"{"criteria":[{"criterion":"service deployed","met":true},{"criterion":"service healthy","met":false,"evidence":"returns 500"}],"summary":"deployed but unhealthy"}"#.into(),
        );
        let plan = prepare(&e, &s);
        let out = e.run(plan.id, &s).unwrap();
        assert_eq!(out.goal_status, Some(GoalStatus::Partial));
        let (goal, eval) = e.goal(plan.goal_id).unwrap().unwrap();
        assert_eq!(goal.status, GoalStatus::Partial);
        assert_eq!(eval.unwrap()["summary"], "deployed but unhealthy");
    }

    #[test]
    fn restart_never_blindly_reruns_an_unsafe_step() {
        let (_rt, e) = engine();
        let s = script(
            r#"{"steps":[
            {"key":"s1","action":{"type":"tool","tool":"check","arguments":{}}},
            {"key":"s2","action":{"type":"tool","tool":"deploy","arguments":{}},"idempotency":"unsafe"}]}"#,
        );
        let mut plan = prepare(&e, &s);
        // Simulate a crash mid-run: both steps marked running.
        plan.status = PlanStatus::Running;
        for st in &mut plan.steps {
            st.status = StepStatus::Running;
        }
        e.save(&plan).unwrap();

        let notes = e.recover(&s).unwrap();
        assert!(notes[0].contains("s2"), "{notes:?}");
        let plan = e.plan(plan.id).unwrap().unwrap();
        assert_eq!(plan.status, PlanStatus::Paused);
        assert_eq!(plan.steps[0].status, StepStatus::Pending, "safe step will just run again");
        assert_eq!(plan.steps[1].status, StepStatus::Blocked, "unsafe step waits for inspection");

        s.evaluate.lock().unwrap().push_back(r#"{"criteria":[{"criterion":"a","met":true}],"summary":"ok"}"#.into());
        let out = e.run(plan.id, &s).unwrap();
        assert_eq!(out.plan.status, PlanStatus::Paused, "still waiting on s2");
        assert_eq!(s.tool_calls.load(Ordering::SeqCst), 1, "only the safe step ran");
        e.skip_step(plan.id, "s2").unwrap();
        let out = e.run(plan.id, &s).unwrap();
        assert_eq!(out.plan.status, PlanStatus::Completed);
    }

    #[test]
    fn independent_steps_run_in_parallel_and_subagents_are_scoped() {
        let (_rt, e) = engine();
        let s = script(
            r#"{"steps":[
            {"key":"s1","action":{"type":"subagent","agent":"researcher","task":"look"}},
            {"key":"s2","action":{"type":"tool","tool":"check","arguments":{}}},
            {"key":"s3","depends_on":["s1","s2"],"action":{"type":"reasoning","instruction":"combine"}}]}"#,
        );
        s.evaluate.lock().unwrap().push_back(MET.into());
        let plan = prepare(&e, &s);
        let out = e.run(plan.id, &s).unwrap();
        assert_eq!(out.plan.status, PlanStatus::Completed);
        let started: Vec<_> = e
            .events(plan.id)
            .unwrap()
            .into_iter()
            .filter(|ev| ev.kind == EventKind::StepStarted)
            .map(|ev| ev.message)
            .collect();
        assert_eq!(started.len(), 3);
        assert!(started[0].starts_with("s1") && started[1].starts_with("s2"), "s1 and s2 start together: {started:?}");
        let s3 = out.plan.find_step("s3").unwrap();
        assert!(output_text(&s3.result.as_ref().unwrap().output).contains("combine"));
    }

    #[test]
    fn a_recorded_operation_is_not_repeated_after_a_restart() {
        let (_rt, e) = engine();
        let s = script(r#"{"steps":[{"key":"s1","action":{"type":"tool","tool":"deploy","arguments":{}},"idempotency":"unsafe"}]}"#);
        s.evaluate.lock().unwrap().push_back(MET.into());
        s.evaluate.lock().unwrap().push_back(MET.into());
        let plan = prepare(&e, &s);
        assert!(plan.steps[0].operation_id.is_some());
        assert_eq!(e.run(plan.id, &s).unwrap().plan.status, PlanStatus::Completed);
        assert_eq!(s.tool_calls.load(Ordering::SeqCst), 1);

        // Crash right after the deploy took effect, before it was marked done.
        let mut plan = e.plan(plan.id).unwrap().unwrap();
        plan.status = PlanStatus::Running;
        plan.steps[0].status = StepStatus::Running;
        e.save(&plan).unwrap();
        e.recover(&s).unwrap();
        let plan = e.plan(plan.id).unwrap().unwrap();
        assert_eq!(plan.steps[0].status, StepStatus::Pending, "the operation is on record, so resuming is safe");
        assert_eq!(e.run(plan.id, &s).unwrap().plan.status, PlanStatus::Completed);
        assert_eq!(s.tool_calls.load(Ordering::SeqCst), 1, "the recorded result was reused, not deployed again");
    }

    #[test]
    fn only_conditional_reasoning_may_change_things_and_is_not_blindly_retried() {
        let (_rt, e) = engine();
        let s = script(
            r#"{"steps":[
            {"key":"s1","action":{"type":"reasoning","instruction":"look around"}},
            {"key":"s2","depends_on":["s1"],"action":{"type":"reasoning","instruction":"record it"},"idempotency":"conditional"}]}"#,
        );
        let plan = prepare(&e, &s);
        assert!(plan.steps[1].operation_id.is_some());
        s.reasoning.lock().unwrap().push_back(Ok("found it".into()));
        s.reasoning.lock().unwrap().push_back(Err("connection reset".into()));
        *s.reason_changes.lock().unwrap() = vec!["deploy {}".into()];
        let out = e.run(plan.id, &s).unwrap();
        assert_eq!(*s.may_change.lock().unwrap(), [false, true], "safe steps get read-only tools");
        let s2 = out.plan.find_step("s2").unwrap();
        assert_eq!(s2.attempts, 1, "a transient error after a change is not retried blindly");
        assert!(!s.events.lock().unwrap().contains(&EventKind::RetryScheduled));
        assert_eq!(changes_of(s2.result.as_ref()), ["deploy {}"]);
    }

    #[test]
    fn steps_that_change_things_run_alone() {
        let (_rt, e) = engine();
        let s = script(
            r#"{"steps":[
            {"key":"s1","action":{"type":"tool","tool":"deploy","arguments":{}}},
            {"key":"s2","action":{"type":"tool","tool":"check","arguments":{}}}]}"#,
        );
        let plan = prepare(&e, &s);
        let ready: Vec<&PlanStep> = plan.steps.iter().collect();
        assert_eq!(graph::schedule(&ready, 3).len(), 1, "a mutating step doesn't share its batch");
    }

    #[test]
    fn subagent_reports_carry_their_status() {
        assert_eq!(subagent_status(json!("found 3 hosts\nSTATUS: done")).unwrap(), json!("found 3 hosts"));
        assert!(subagent_status(json!("nothing\nSTATUS: failed — no access")).unwrap_err().contains("no access"));
        assert_eq!(subagent_status(json!("plain report")).unwrap(), json!("plain report"));
    }
}

//! Lyra's side of self-evolution: the evolved state the chat and planner obey
//! (behavior settings, workflows, composite tools), run telemetry, the evolver,
//! benchmark and code calls made with the chat model, and the `/evolve`
//! commands. Policy, history and rollback belong to the `EvolutionManager`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Instant;

use chrono::{DateTime, Utc};
use lyra_evolution::detect::SkillHealth;
use lyra_evolution::lab::{self, CodeLab};
use lyra_evolution::{
    Behavior, BenchResult, Candidate, CandidateStatus, Category, Change, CompositeTool, EvolutionManager, Fitness, Mode,
    RunRecord, Snapshot, Stats, Uuid, Validation, WorkflowDef, evolver, fitness, workflow,
};
use serde_json::{Value, json};

use crate::learn::Learning;
use crate::stats::Usage;
use crate::tools::{self, CallContext, Tools};

pub struct Evolution {
    pub manager: EvolutionManager,
    live: RwLock<Live>,
    /// lyra's source checkout, for code evolution.
    source_repo: Option<PathBuf>,
    benchmark_tasks: usize,
    /// The generation the monitor last warned about (propose mode).
    warned: Mutex<Option<u32>>,
    /// For the plan stores of benchmark runs.
    rt: tokio::runtime::Handle,
}

/// The evolved state in use, re-read after every deployment or rollback.
#[derive(Default)]
struct Live {
    behavior: Behavior,
    workflows: Vec<WorkflowDef>,
    /// Composite tools loaded (valid ones only).
    composites: usize,
}

/// What the Evolution panel shows.
pub struct EvolutionSnapshot {
    pub mode: Mode,
    pub stats: Stats,
    pub last_review: Option<DateTime<Utc>>,
    pub guidelines: usize,
    pub workflows: usize,
    pub tools: usize,
    /// Behavior settings that differ from the defaults, as `key=value`.
    pub settings: Vec<String>,
}

/// What background evolution work needs: the chat model and lyra's services.
#[derive(Clone)]
pub struct Env {
    pub url: String,
    pub model: String,
    pub evolution: Arc<Evolution>,
    pub tools: Option<Arc<Tools>>,
    pub learning: Option<Arc<Learning>>,
    pub caps: Option<Arc<crate::caps::Caps>>,
    pub goals: Option<Arc<crate::goals::Goals>>,
    /// The chat's system prompt (context files and tool instructions), for benchmarks.
    pub system_prompt: Option<String>,
}

/// The outcome of background evolution work.
pub struct Done {
    pub notes: Vec<String>,
    /// Model calls made (their tokens count toward the session totals).
    pub usage: Vec<Usage>,
}

impl Evolution {
    pub fn new(manager: EvolutionManager, rt: tokio::runtime::Handle, source_repo: Option<PathBuf>, benchmark_tasks: usize) -> Self {
        Self { manager, live: RwLock::new(Live::default()), source_repo, benchmark_tasks, warned: Mutex::new(None), rt }
    }

    pub fn mode(&self) -> Mode {
        self.manager.settings.mode
    }

    pub fn behavior(&self) -> Behavior {
        self.live.read().unwrap_or_else(|e| e.into_inner()).behavior.clone()
    }

    /// Re-read behavior.toml, the workflows and the composite tools; returns problems found.
    pub fn reload(&self, tools: Option<&Tools>) -> Vec<String> {
        let mut notes = Vec::new();
        let behavior = self.manager.behavior().unwrap_or_else(|e| {
            notes.push(format!("{e} — using the default behavior"));
            Behavior::default()
        });
        let (workflows, errors) = self.manager.workflows();
        notes.extend(errors);
        let mut loaded = 0;
        if let Some(tools) = tools {
            let (composites, errors) = self.manager.composites();
            notes.extend(errors);
            notes.extend(tools.set_composites(composites));
            loaded = tools.composite_names().len();
        }
        *self.live.write().unwrap_or_else(|e| e.into_inner()) = Live { behavior, workflows, composites: loaded };
        notes
    }

    /// Behavior guidelines and the matching workflow, for a chat request's system prompt.
    pub fn chat_section(&self, request: &str) -> Option<String> {
        let live = self.live.read().unwrap_or_else(|e| e.into_inner());
        chat_section(&live.behavior, &live.workflows, request)
    }

    /// How to plan this request: the matching workflow, and whether reasoning
    /// steps should be verified.
    pub fn planning_guidance(&self, request: &str) -> Option<String> {
        let live = self.live.read().unwrap_or_else(|e| e.into_inner());
        planning_guidance(&live.behavior, &live.workflows, request)
    }

    pub fn generation(&self) -> u32 {
        self.manager.generation().map_or(1, |g| g.number)
    }

    /// Save a finished run, stamped with the generation that did it.
    pub fn record(&self, mut run: RunRecord) -> Result<Uuid, String> {
        run.generation = self.generation();
        self.manager.record_run(&run)?;
        Ok(run.id)
    }

    pub fn snapshot(&self) -> Result<EvolutionSnapshot, String> {
        let live = self.live.read().unwrap_or_else(|e| e.into_inner());
        let defaults = Behavior::default();
        let settings = lyra_evolution::behavior::KEYS
            .iter()
            .filter(|(k, _)| live.behavior.get(k) != defaults.get(k))
            .map(|(k, _)| format!("{k}={}", live.behavior.get(k).unwrap_or_default()))
            .collect();
        Ok(EvolutionSnapshot {
            mode: self.mode(),
            stats: self.manager.stats()?,
            last_review: self.manager.last_review()?,
            guidelines: live.behavior.guidelines.len(),
            workflows: live.workflows.len(),
            tools: live.composites,
            settings,
        })
    }

    /// Whether a scheduled review is due: there are runs to look at and the
    /// last review is older than `every_days`.
    pub fn review_due(&self, every_days: Option<i64>) -> bool {
        let Some(days) = every_days else { return false };
        if self.mode() == Mode::Off || self.manager.runs(1).map_or(true, |r| r.is_empty()) {
            return false;
        }
        match self.manager.last_review() {
            Ok(Some(at)) => Utc::now() - at > chrono::Duration::days(days),
            Ok(None) => true,
            Err(_) => false,
        }
    }

    /// Whether the monitor already warned about this generation (propose mode).
    fn warn_once(&self) -> bool {
        let generation = self.generation();
        let mut warned = self.warned.lock().unwrap_or_else(|e| e.into_inner());
        let first = *warned != Some(generation);
        *warned = Some(generation);
        first
    }
}

/// After new outcomes: has the current generation regressed? In auto mode
/// it is rolled back (skill revisions included); otherwise the user is told
/// once per generation.
pub fn monitor(env: &Env) -> Vec<String> {
    let ev = &env.evolution;
    let reason = match ev.manager.regression() {
        Ok(Some(reason)) => reason,
        Ok(None) => return Vec::new(),
        Err(e) => return vec![format!("evolution monitor failed: {e}")],
    };
    if ev.mode() == Mode::Auto {
        return match rollback_to(env, None, &format!("regression: {reason}")) {
            Ok(mut notes) => {
                notes[0] = format!("↩ {reason}; {}", notes[0].trim_start_matches("↩ "));
                notes
            }
            Err(e) => vec![format!("{reason}; rollback failed: {e}")],
        };
    }
    if !ev.warn_once() {
        return Vec::new();
    }
    vec![format!("⚠ {reason} — /evolve rollback to undo the last change")]
}

/// Restore a generation, undo the skill revisions made after it, and reload.
fn rollback_to(env: &Env, to: Option<u32>, why: &str) -> Result<Vec<String>, String> {
    let ev = &env.evolution;
    let (g, skills) = ev.manager.rollback(to, why)?;
    let mut notes = vec![format!("↩ {} — now generation {}", g.reason, g.number)];
    for r in skills {
        if r.agent {
            match env.caps.as_ref().and_then(|c| c.agents()).map(|a| a.registry.rollback(&r.name, Some(r.from_version as u32))) {
                Some(Ok(p)) => notes.push(format!("  agent {} restored (now v{})", p.title, p.version)),
                Some(Err(e)) => notes.push(format!("  ✗ couldn't restore agent {} v{}: {e}", r.name, r.from_version)),
                None => notes.push(format!("  ✗ agents are off; {} wasn't restored to v{}", r.name, r.from_version)),
            }
            continue;
        }
        match env.learning.as_ref().map(|l| l.rollback(&format!("{} {}", r.name, r.from_version), None, true)) {
            Some(Ok(note)) => notes.push(format!("  {note}")),
            Some(Err(e)) => notes.push(format!("  ✗ couldn't restore skill {} v{}: {e}", r.name, r.from_version)),
            None => notes.push(format!("  ✗ learning is off; skill {} wasn't restored to v{}", r.name, r.from_version)),
        }
    }
    notes.extend(ev.reload(env.tools.as_deref()));
    Ok(notes)
}

/// Planner guidance under some evolved state.
fn planning_guidance(behavior: &Behavior, workflows: &[WorkflowDef], request: &str) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(w) = workflow::pick(workflows, request) {
        parts.push(w.guidance());
    }
    if behavior.verify_reasoning_steps {
        parts.push("Give every reasoning step a model_evaluation verification with a clear expected outcome.".to_string());
    }
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// The system prompt section for a request under some evolved state.
fn chat_section(behavior: &Behavior, workflows: &[WorkflowDef], request: &str) -> Option<String> {
    let mut parts: Vec<String> = behavior.prompt_section().into_iter().collect();
    if let Some(w) = workflow::pick(workflows, request) {
        let phases: Vec<String> =
            w.phases.iter().enumerate().map(|(i, p)| format!("{}. {}: {}", i + 1, p.name, p.instruction)).collect();
        parts.push(format!("# Workflow: {}\n\n{}. Work through these phases:\n{}", w.name, w.description, phases.join("\n")));
    }
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

// ---- reviews (detect → evolve → propose)

/// Look for problems in recent runs and propose candidates for them. In auto
/// mode, low-risk candidates are benchmarked and the best one that beats the
/// baseline is deployed.
pub fn review(env: &Env) -> Done {
    let mut done = Done { notes: Vec::new(), usage: Vec::new() };
    if let Err(e) = review_inner(env, &mut done) {
        done.notes.push(format!("evolution review failed: {e}"));
    }
    done
}

fn review_inner(env: &Env, done: &mut Done) -> Result<(), String> {
    let ev = &env.evolution;
    let skills = skill_health(env.learning.as_deref())?;
    // C12: capabilities' track records feed evolution too.
    let records: Vec<lyra_evolution::detect::CapabilityRecord> = env
        .caps
        .as_ref()
        .map(|caps| {
            caps.manager
                .all_stats()
                .into_iter()
                .filter_map(|(id, s)| {
                    Some(lyra_evolution::detect::CapabilityRecord {
                        name: caps.manager.get(&id).map_or(id, |c| c.name),
                        uses: s.uses,
                        success_rate: s.success_rate()?,
                        average_latency_ms: s.average_latency_ms.unwrap_or(0),
                        common_error: s.common_error,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    // Goal events feed evolution: goals that keep getting stuck.
    let goal_records: Vec<lyra_evolution::detect::GoalRecord> = env
        .goals
        .as_ref()
        .and_then(|g| {
            let m = &g.manager;
            let all = m.all().ok()?;
            Some(
                all.into_iter()
                    .filter(|goal| goal.status.is_open())
                    .map(|goal| {
                        let blockers = m.blockers(Some(goal.id), false).unwrap_or_default();
                        let mut counts: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
                        for b in &blockers {
                            *counts.entry(b.blocker_type.as_str()).or_default() += 1;
                        }
                        lyra_evolution::detect::GoalRecord {
                            title: goal.title.clone(),
                            blocks: blockers.len() as u32,
                            common_blocker: counts.into_iter().max_by_key(|(_, n)| *n).map(|(k, _)| k.to_string()),
                            failed_plans: m.plans(goal.id).unwrap_or_default().iter().filter(|p| p.outcome.as_deref() == Some("failed")).count() as u32,
                        }
                    })
                    .collect(),
            )
        })
        .unwrap_or_default();
    let agents = env.caps.as_ref().and_then(|c| c.agents().cloned());
    let agent_records = agents.as_deref().map(crate::agents::evolution_records).unwrap_or_default();
    let ops = ev.manager.detect(&skills.iter().map(|(h, _)| h.clone()).collect::<Vec<_>>(), &records, &goal_records, &agent_records)?;
    if ops.is_empty() {
        let note = format!("no problems found in the last {} runs", ev.manager.settings.window);
        ev.manager.note_review(&note)?;
        done.notes.push(note);
        return Ok(());
    }
    let behavior = ev.behavior();
    let (workflows, _) = ev.manager.workflows();
    let tool_list = tool_list(env.tools.as_deref());
    let skill_list: Vec<(String, String)> = skills.into_iter().map(|(h, instructions)| (h.name, instructions)).collect();
    let agent_list: Vec<(String, String)> = agents.as_ref().map(|a| a.registry.enabled().into_iter().map(|p| (p.name, p.instructions)).collect()).unwrap_or_default();
    let ctx = evolver::Context { behavior: &behavior, workflows: &workflows, tools: &tool_list, skills: &skill_list, agents: &agent_list };
    let mut created: Vec<Candidate> = Vec::new();
    for op in ops.iter().take(ev.manager.settings.max_problems.max(1)) {
        done.notes.push(format!("problem: {}", op.problem));
        let allowed: Vec<Category> = op
            .categories
            .iter()
            .copied()
            .filter(|c| match c {
                Category::Code => false,
                Category::Skill => env.learning.is_some() && !skill_list.is_empty(),
                Category::Tool => env.tools.is_some(),
                Category::Agent => !agent_list.is_empty(),
                _ => true,
            })
            .collect();
        if op.categories.contains(&Category::Code) && ev.source_repo.is_some() {
            done.notes.push(format!("  a code change might help: /evolve code {}", op.problem));
        }
        if allowed.is_empty() {
            continue;
        }
        let evidence = ev.manager.evidence(op)?;
        let prompt = evolver::prompt(op, &evidence, &ctx, &allowed);
        let reply = match crate::learn::complete(&env.url, &env.model, evolver::EVOLVER_PROMPT, &prompt) {
            Ok((text, usage)) => {
                done.usage.extend(usage);
                text
            }
            Err(e) => {
                done.notes.push(format!("  evolver call failed: {e}"));
                continue;
            }
        };
        let (proposals, rejected) = match evolver::parse(&reply, &ctx, &allowed) {
            Ok(parsed) => parsed,
            Err(e) => {
                done.notes.push(format!("  evolver reply unusable: {e}"));
                continue;
            }
        };
        done.notes.extend(rejected.into_iter().map(|r| format!("  turned away: {r}")));
        for c in ev.manager.propose(op, proposals)? {
            done.notes.push(format!("  {} [{}] {}", c.short(), c.level, c.change.summary()));
            created.push(c);
        }
    }
    let summary = format!("{} problem(s), {} candidate(s)", ops.len(), created.len());
    ev.manager.note_review(&summary)?;
    if ev.mode() == Mode::Auto {
        auto_deploy(env, created, done)?;
    } else if !created.is_empty() {
        done.notes.push("/evolve compare <id> tests and ranks a problem's candidates · /evolve approve <id>".into());
    }
    Ok(())
}

/// Auto mode: benchmark the candidates that may deploy on their own and
/// deploy the best of each group, if it beats the baseline.
fn auto_deploy(env: &Env, created: Vec<Candidate>, done: &mut Done) -> Result<(), String> {
    let ev = &env.evolution;
    let mut groups: Vec<Uuid> = created.iter().map(|c| c.group).collect();
    groups.dedup();
    for group in groups {
        let mut tested = Vec::new();
        for c in created.iter().filter(|c| c.group == group && c.level.policy() == lyra_evolution::Policy::AutoEventually) {
            let c = ev.manager.find(&c.id.to_string())?;
            let t = test(env, c);
            done.notes.extend(t.notes);
            done.usage.extend(t.usage);
            if let Some(c) = t.candidate.filter(|c| ev.manager.may_auto_deploy(c)) {
                tested.push(c);
            }
        }
        let score = |c: &Candidate| c.validation.as_ref().and_then(|v| v.fitness_after.as_ref()).map_or(0.0, |f| f.total);
        if let Some(best) = tested.into_iter().max_by(|a, b| score(a).total_cmp(&score(b))) {
            let best = ev.manager.set_status(best, CandidateStatus::Approved, "approved automatically: it beat the baseline")?;
            let note = deploy(env, best, "auto: beat the baseline", done)?;
            done.notes.push(format!("{note} (on its own)"));
        }
    }
    // Deploying one candidate rejects its siblings; say what's still open.
    let ids: Vec<Uuid> = created.iter().map(|c| c.id).collect();
    let waiting = ev.manager.candidates(200)?.iter().filter(|c| ids.contains(&c.id) && c.status == CandidateStatus::Proposed).count();
    if waiting > 0 {
        done.notes.push(format!("{waiting} candidate(s) wait for review: /evolve list"));
    }
    Ok(())
}

/// Active skills' health and instructions, for the detectors and the evolver.
fn skill_health(learning: Option<&Learning>) -> Result<Vec<(SkillHealth, String)>, String> {
    let Some(learning) = learning else { return Ok(Vec::new()) };
    Ok(learning
        .active_skills()?
        .into_iter()
        .map(|s| {
            let health = SkillHealth { name: s.name.clone(), uses: s.usage.use_count, reliability: s.usage.reliability() };
            (health, s.instructions)
        })
        .collect())
}

/// `(name, description, destructive)` for every tool the model is offered.
fn tool_list(tools: Option<&Tools>) -> Vec<(String, String, bool)> {
    let Some(tools) = tools else { return Vec::new() };
    tools
        .definitions()
        .as_array()
        .into_iter()
        .flatten()
        .map(|d| {
            let name = d["function"]["name"].as_str().unwrap_or("").to_string();
            let description = d["function"]["description"].as_str().unwrap_or("").to_string();
            let destructive = tools::is_destructive(&name);
            (name, description, destructive)
        })
        .collect()
}

// ---- validation lab (E7): static checks, then a sandboxed benchmark

pub struct Tested {
    pub candidate: Option<Candidate>,
    pub notes: Vec<String>,
    pub usage: Vec<Usage>,
}

impl From<Tested> for Done {
    fn from(t: Tested) -> Self {
        Done { notes: t.notes, usage: t.usage }
    }
}

/// Validate a candidate: static checks, then (for everything but code) replay
/// recent tasks with the current state and with the candidate's, judge both
/// and compare fitness. Code patches run clippy and the tests in a
/// throwaway worktree instead.
pub fn test(env: &Env, c: Candidate) -> Tested {
    let mut t = Tested { candidate: None, notes: Vec::new(), usage: Vec::new() };
    let id = c.short();
    match test_inner(env, c, &mut t) {
        Ok(c) => t.candidate = Some(c),
        Err(e) => t.notes.push(format!("testing {id} failed: {e}")),
    }
    t
}

fn test_inner(env: &Env, c: Candidate, t: &mut Tested) -> Result<Candidate, String> {
    let ev = &env.evolution;
    if matches!(c.status, CandidateStatus::Deployed | CandidateStatus::Rejected | CandidateStatus::RolledBack) {
        return Err(format!("candidate {} is {}", c.short(), c.status));
    }
    let mut v = Validation { valid: true, ..Default::default() };
    if let Change::Code { base_commit, diff } = &c.change {
        let lab = code_lab(ev)?;
        t.notes.push(format!("{}: checking the patch in a sandbox (clippy, tests)…", c.short()));
        v.checks = lab.validate(&c.short(), base_commit, diff, &lab::cargo_checks());
        v.valid = v.checks.iter().all(|(_, ok, _)| *ok) && v.checks.len() > 1;
        for (name, ok, output) in &v.checks {
            let tail = if *ok { String::new() } else { format!(": {}", last_lines(output, 6)) };
            t.notes.push(format!("  {} {name}{tail}", if *ok { "✓" } else { "✗" }));
        }
        return ev.manager.save_validation(c, v);
    }

    // Static checks: the change applies, is safe, and its parts exist.
    let current = ev.manager.snapshot();
    let mut skill_override = None;
    let next = match &c.change {
        Change::Skill { skill, instructions } => {
            if lyra_evolution::safety_scan(instructions).is_some() {
                v.valid = false;
                v.notes.push("instructions look like they contain a secret".into());
            }
            match env.learning.as_ref().map(|l| l.find(skill)) {
                Some(Ok(s)) => skill_override = Some((s.name, s.instructions, instructions.clone())),
                Some(Err(e)) => {
                    v.valid = false;
                    v.notes.push(e);
                }
                None => {
                    v.valid = false;
                    v.notes.push("learning is off".into());
                }
            }
            current.clone()
        }
        Change::Agent { agent, instructions } => {
            // A15: tried on the agent's own recent tasks, old instructions vs new.
            return validate_agent(env, c.clone(), agent, instructions, v, t);
        }
        change => match ev.manager.apply(&current, change) {
            Ok(next) => next,
            Err(e) => {
                v.valid = false;
                v.notes.push(e);
                current.clone()
            }
        },
    };
    if let (Change::Tool { tool }, Some(tools)) = (&c.change, &env.tools) {
        let base = tools.base_tools();
        if let Err(e) = tool.validate(&lyra_evolution::composite::Available { tools: &base }) {
            v.valid = false;
            v.notes.push(e);
        }
    }
    if !v.valid {
        t.notes.push(format!("{}: ✗ {}", c.short(), v.notes.join("; ")));
        return ev.manager.save_validation(c, v);
    }

    let tasks = benchmark_tasks(ev, &c)?;
    if tasks.is_empty() {
        v.notes.push("no recent tasks to benchmark with".into());
        t.notes.push(format!("{}: ✓ checks passed; no recent tasks to benchmark with", c.short()));
        return ev.manager.save_validation(c, v);
    }
    // Planner-only changes are measured by planning and running the tasks
    // (sandboxed); everything else by answering them in a chat.
    let plans = plan_mode(&c.change);
    let tasks: Vec<BenchTask> = if plans { tasks.into_iter().take(2).collect() } else { tasks };
    let how = if plans { "as plans" } else { "as chats" };
    t.notes.push(format!("{}: benchmarking {} task(s) {how}, baseline vs candidate…", c.short(), tasks.len()));
    let baseline = Variant::new(env, &current, skill_override.clone(), false);
    let candidate = Variant::new(env, &next, skill_override, true);
    let mut before = Vec::new();
    let mut after = Vec::new();
    for task in &tasks {
        if plans {
            before.push(bench_plan(env, &baseline, task, t));
            after.push(bench_plan(env, &candidate, task, t));
        } else {
            before.push(bench(env, &baseline, &task.task, &task.expect, t));
            after.push(bench(env, &candidate, &task.task, &task.expect, t));
        }
    }
    let weights = &ev.manager.settings.fitness;
    let (fb, fa) = (fitness::fitness(&before, weights), fitness::fitness(&after, weights));
    let verdict = if fitness::improves(&fb, &fa) { "improves on the baseline" } else { "doesn't beat the baseline" };
    t.notes.push(format!("  fitness {:.2} → {:.2}: {verdict}", fb.total, fa.total));
    t.notes.push(format!("  {}", describe_fitness("baseline", &fb)));
    t.notes.push(format!("  {}", describe_fitness("candidate", &fa)));
    v.notes.push(format!("benchmarked on {} task(s): {verdict}", tasks.len()));
    v.fitness_before = Some(fb);
    v.fitness_after = Some(fa);
    ev.manager.save_validation(c, v)
}

/// Benchmark an agent change (A15): its recent tasks (and test task), done
/// with the current instructions and the proposed ones, without tools (a
/// benchmark must not change anything), judged by the model.
fn validate_agent(env: &Env, c: Candidate, agent: &str, instructions: &str, mut v: Validation, t: &mut Tested) -> Result<Candidate, String> {
    let ev = &env.evolution;
    let agents = env.caps.as_ref().and_then(|c| c.agents()).ok_or("agents are off")?;
    let current = match agents.registry.find(agent) {
        Ok(p) => p,
        Err(e) => {
            v.valid = false;
            v.notes.push(e);
            return ev.manager.save_validation(c, v);
        }
    };
    if lyra_evolution::safety_scan(instructions).is_some() {
        v.valid = false;
        v.notes.push("instructions look like they contain a secret".into());
        return ev.manager.save_validation(c, v);
    }
    let mut next = current.clone();
    next.instructions = instructions.to_string();
    let tasks = crate::agents::bench_tasks(agents, &current, ev.benchmark_tasks);
    if tasks.is_empty() {
        v.notes.push("no tasks to benchmark the agent with".into());
        t.notes.push(format!("{}: ✓ checks passed; no tasks to benchmark the agent with", c.short()));
        return ev.manager.save_validation(c, v);
    }
    t.notes.push(format!("{}: benchmarking {} on {} task(s), current vs revised instructions…", c.short(), current.title, tasks.len()));
    let mut before = Vec::new();
    let mut after = Vec::new();
    for (task, expect) in &tasks {
        for (p, out) in [(&current, &mut before), (&next, &mut after)] {
            let start = Instant::now();
            let mut r = BenchResult::default();
            match crate::agents::bench_answer(&env.url, &env.model, p, task) {
                Ok((answer, tokens)) => {
                    r.model_calls = 1;
                    r.tokens = tokens;
                    judge(env, task, expect, &answer, &mut r, t);
                }
                Err(e) => {
                    r.errors += 1;
                    t.notes.push(format!("  benchmark call failed: {e}"));
                }
            }
            r.seconds = start.elapsed().as_secs_f32();
            out.push(r);
        }
    }
    let weights = &ev.manager.settings.fitness;
    let (fb, fa) = (fitness::fitness(&before, weights), fitness::fitness(&after, weights));
    let verdict = if fitness::improves(&fb, &fa) { "improves on the baseline" } else { "doesn't beat the baseline" };
    t.notes.push(format!("  fitness {:.2} → {:.2}: {verdict}", fb.total, fa.total));
    v.notes.push(format!("benchmarked {} on {} task(s): {verdict}", current.title, tasks.len()));
    v.fitness_before = Some(fb);
    v.fitness_after = Some(fa);
    ev.manager.save_validation(c, v)
}

fn describe_fitness(name: &str, f: &Fitness) -> String {
    format!(
        "{name}: success {:.0}% · accuracy {:.2} · {:.1} model / {:.1} tool calls · {:.0} tokens",
        f.success_rate * 100.0,
        f.accuracy,
        f.model_calls,
        f.tool_calls,
        f.tokens
    )
}

/// A benchmark task: a recent request, and what a good answer must do.
struct BenchTask {
    task: String,
    expect: String,
}

/// Tasks to replay: the candidate's evidence first, then other recent
/// requests. When the user corrected the original answer, their correction
/// becomes part of the expectation.
fn benchmark_tasks(ev: &Evolution, c: &Candidate) -> Result<Vec<BenchTask>, String> {
    let runs = ev.manager.runs(ev.manager.settings.window * 2)?;
    let mut tasks: Vec<BenchTask> = Vec::new();
    let evidence = runs.iter().filter(|r| c.evidence.contains(&r.id));
    for r in evidence.chain(runs.iter()) {
        let task = r.task.trim();
        if task.len() >= 8 && !task.starts_with('/') && !tasks.iter().any(|t| t.task == task) {
            let mut expect = "a correct, complete and helpful answer to the request".to_string();
            if let Some(f) = r.feedback.as_ref().filter(|_| r.corrected) {
                expect += &format!(
                    ", which also satisfies what the user said after an earlier answer to it was wrong: \"{f}\""
                );
            }
            tasks.push(BenchTask { task: task.to_string(), expect });
        }
        if tasks.len() >= ev.benchmark_tasks {
            break;
        }
    }
    Ok(tasks)
}

/// Changes that only affect planning, so a chat benchmark can't see them.
fn plan_mode(change: &Change) -> bool {
    match change {
        Change::Configuration { key, .. } => {
            matches!(key.as_str(), "plan_step_rounds" | "search_skills_before_planning" | "verify_reasoning_steps")
        }
        Change::Workflow { .. } => true,
        _ => false,
    }
}

/// One version of the agent for the benchmark: its system prompt additions,
/// tools and round limit.
struct Variant {
    behavior: Behavior,
    workflows: Vec<WorkflowDef>,
    composites: Vec<CompositeTool>,
    definitions: Vec<Value>,
    /// `(skill, old instructions, new instructions)` for a skill change.
    skill: Option<(String, String, String)>,
    /// Which side of the comparison this is.
    candidate: bool,
}

impl Variant {
    fn new(env: &Env, s: &Snapshot, skill: Option<(String, String, String)>, candidate: bool) -> Self {
        let behavior = Behavior::parse(&s.behavior).unwrap_or_default();
        let workflows = s.workflows.values().filter_map(|t| WorkflowDef::parse(t).ok()).collect();
        let mut definitions = Vec::new();
        let mut composites = Vec::new();
        if let Some(tools) = &env.tools {
            let base = tools.base_tools();
            let available = lyra_evolution::composite::Available { tools: &base };
            composites = s
                .tools
                .values()
                .filter_map(|t| CompositeTool::parse(t).ok())
                .filter(|c| c.validate(&available).is_ok())
                .collect();
            definitions = tools
                .definitions()
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|d| base.iter().any(|(n, _)| d["function"]["name"] == n.as_str()))
                .collect();
            definitions.extend(composites.iter().map(CompositeTool::definition));
        }
        Self { behavior, workflows, composites, definitions, skill, candidate }
    }

    fn system(&self, env: &Env, task: &str) -> String {
        let mut parts: Vec<String> = env.system_prompt.iter().cloned().collect();
        parts.extend(chat_section(&self.behavior, &self.workflows, task));
        if let Some((name, old, new)) = &self.skill {
            let text = if self.candidate { new } else { old };
            parts.push(format!("# Learned skills\n\n## {name}\n{text}"));
        }
        parts.join("\n\n")
    }
}

/// Run one task headless with a variant and have the model judge the answer.
/// Only read-only tools really run; anything that would change something is
/// simulated, and destructive calls count as safety violations.
fn bench(env: &Env, v: &Variant, task: &str, expect: &str, t: &mut Tested) -> BenchResult {
    let start = Instant::now();
    let mut r = BenchResult::default();
    let mut messages = vec![json!({ "role": "system", "content": v.system(env, task) }), json!({ "role": "user", "content": task })];
    let mut answer = None;
    for _ in 0..v.behavior.max_tool_rounds.max(1) {
        let (message, tokens) = match crate::plan::chat(&env.url, &env.model, &messages, &v.definitions) {
            Ok(reply) => reply,
            Err(e) => {
                r.errors += 1;
                t.notes.push(format!("  benchmark call failed: {e}"));
                break;
            }
        };
        r.model_calls += 1;
        r.tokens += tokens;
        let calls = message["tool_calls"].as_array().cloned().unwrap_or_default();
        if calls.is_empty() {
            answer = message["content"].as_str().map(str::to_string);
            break;
        }
        messages.push(json!({ "role": "assistant", "content": message["content"].as_str().unwrap_or(""), "tool_calls": calls }));
        for call in &calls {
            let name = call["function"]["name"].as_str().unwrap_or("");
            let arguments = call["function"]["arguments"].as_str().unwrap_or("{}");
            let result = sandboxed(env, v, name, arguments, &mut r);
            messages.push(json!({ "role": "tool", "tool_call_id": call["id"], "content": result.to_string() }));
        }
    }
    r.seconds = start.elapsed().as_secs_f32();
    let Some(answer) = answer.filter(|a| !a.trim().is_empty()) else {
        r.errors += 1;
        return r;
    };
    judge(env, task, expect, &answer, &mut r, t);
    r
}

/// Have the model judge an answer; fills `success` and `accuracy`.
fn judge(env: &Env, task: &str, expect: &str, answer: &str, r: &mut BenchResult, t: &mut Tested) {
    // The decision model when it's set up and sure of the verdict; the chat model otherwise.
    if let Some((success, accuracy)) = decide_judgement(task, expect, answer) {
        r.success = success;
        r.accuracy = accuracy;
        return;
    }
    match crate::learn::complete(&env.url, &env.model, evolver::JUDGE_PROMPT, &evolver::judge_prompt(task, expect, answer)) {
        Ok((reply, usage)) => {
            t.usage.extend(usage);
            match evolver::parse_judgement(&reply) {
                Ok(j) => {
                    r.success = j.success;
                    r.accuracy = j.accuracy.clamp(0.0, 1.0);
                }
                Err(e) => t.notes.push(format!("  judge reply unusable: {e}")),
            }
        }
        Err(e) => t.notes.push(format!("  judge call failed: {e}")),
    }
}

/// A benchmark answer graded by the decision model: `(success, accuracy)`.
fn decide_judgement(task: &str, expect: &str, answer: &str) -> Option<(bool, f32)> {
    use crate::decide::{Question, ask, confident};
    let levels: Vec<(String, String)> = [("0", "wrong or useless"), ("25", "mostly wrong"), ("50", "partly right"), ("75", "mostly right"), ("100", "fully right")]
        .iter()
        .map(|(id, d)| (id.to_string(), d.to_string()))
        .collect();
    let questions = [
        ("success".to_string(), Question::Yes("Does the answer accomplish the task as expected?".into())),
        ("accuracy".to_string(), Question::Choice("How accurate and complete is the answer?".into(), levels)),
    ];
    let state = evolver::judge_prompt(task, expect, answer);
    let answers = ask("benchmark judge", &state, &questions)?;
    let success = confident(&answers, "success")?.yes()?;
    // The grade is a scale: its most likely level is used even when spread out.
    let accuracy = answers.get("accuracy").and_then(|a| a.choice.parse::<f32>().ok()).map_or(if success { 1.0 } else { 0.0 }, |p| p / 100.0);
    Some((success, accuracy.clamp(0.0, 1.0)))
}

/// Plan and run a task with a variant on a throwaway plan store, approving
/// steps automatically (tools are sandboxed), then judge the result.
fn bench_plan(env: &Env, v: &Variant, task: &BenchTask, t: &mut Tested) -> BenchResult {
    use lyra_execution::{Budget, Engine, PlanStatus, PlanStore, Settings};
    let start = Instant::now();
    let rt = BenchRuntime { env, v, r: Mutex::new(BenchResult::default()) };
    let store = match env.evolution.rt.block_on(PlanStore::in_memory()) {
        Ok(store) => store,
        Err(e) => {
            t.notes.push(format!("  benchmark plan store: {e:#}"));
            return BenchResult { errors: 1, ..Default::default() };
        }
    };
    let budget = Budget { max_model_calls: Some(30), max_tool_calls: Some(40), max_replans: Some(1), max_minutes: Some(15), max_tokens: Some(200_000) };
    let engine = Engine::with_store(store, env.evolution.rt.clone(), Settings { budget, max_parallel: 1 });
    let mut answer = None;
    match engine.create(&task.task, &rt) {
        Err(e) => t.notes.push(format!("  benchmark planning failed: {e}")),
        Ok((_, plan)) => {
            for _ in 0..4 {
                match engine.run(plan.id, &rt) {
                    Ok(out) if out.plan.status == PlanStatus::Paused => {
                        // Approve what waits for approval: nothing real runs.
                        let waiting: Vec<String> = out.plan.steps.iter().filter(|s| s.needs_approval()).map(|s| s.key.clone()).collect();
                        if waiting.is_empty() {
                            break;
                        }
                        for key in waiting {
                            let _ = engine.approve(plan.id, &key, &rt);
                        }
                    }
                    Ok(out) => {
                        answer = out.evaluation.map(|e| if e.answer.trim().is_empty() { e.summary } else { e.answer });
                        break;
                    }
                    Err(e) => {
                        t.notes.push(format!("  benchmark plan failed: {e}"));
                        break;
                    }
                }
            }
        }
    }
    let mut r = rt.r.into_inner().unwrap_or_else(|e| e.into_inner());
    r.seconds = start.elapsed().as_secs_f32();
    match answer.filter(|a| !a.trim().is_empty()) {
        Some(answer) => judge(env, &task.task, &task.expect, &answer, &mut r, t),
        None => r.errors += 1,
    }
    r
}

/// The planner's runtime during a benchmark: the variant's behavior,
/// workflows and tools, with every tool call sandboxed.
struct BenchRuntime<'a> {
    env: &'a Env,
    v: &'a Variant,
    r: Mutex<BenchResult>,
}

impl BenchRuntime<'_> {
    fn sandboxed(&self, name: &str, arguments: &str) -> Value {
        let mut r = self.r.lock().unwrap_or_else(|e| e.into_inner());
        sandboxed(self.env, self.v, name, arguments, &mut r)
    }
}

impl lyra_execution::Runtime for BenchRuntime<'_> {
    fn complete(&self, system: &str, user: &str) -> Result<(String, u64), String> {
        let (text, usage) = crate::learn::complete(&self.env.url, &self.env.model, system, user)?;
        let tokens = usage.map_or(0, |u| u.prompt_tokens + u.completion_tokens);
        let mut r = self.r.lock().unwrap_or_else(|e| e.into_inner());
        r.model_calls += 1;
        r.tokens += tokens;
        Ok((text, tokens))
    }

    fn context(&self, goal: &str) -> lyra_execution::PlanningContext {
        let learning = self.env.learning.as_ref();
        let skills = learning
            .filter(|_| self.v.behavior.search_skills_before_planning)
            .and_then(|l| l.relevant(goal).ok())
            .map(|found| found.into_iter().map(|r| (r.skill.name, r.skill.description)).collect())
            .unwrap_or_default();
        let memories = self
            .env
            .tools
            .as_ref()
            .and_then(|t| t.mem.recall(None, goal, 6, false).ok())
            .map(|found| found.into_iter().map(|r| r.memory.content).collect())
            .unwrap_or_default();
        let agents = crate::agents::plan_agents(self.env.caps.as_deref());
        lyra_execution::PlanningContext {
            memories,
            skills,
            workflows: learning.and_then(|l| l.active_skills().ok()).map(|a| a.into_iter().map(|s| s.name).collect()).unwrap_or_default(),
            tools: self.tools(),
            agents,
            forbidden_tools: Vec::new(),
            budget_note: None,
            guidance: planning_guidance(&self.v.behavior, &self.v.workflows, goal),
        }
    }

    fn tools(&self) -> Vec<lyra_execution::ToolInfo> {
        self.v
            .definitions
            .iter()
            .map(|d| {
                let name = d["function"]["name"].as_str().unwrap_or("").to_string();
                lyra_execution::ToolInfo::new(
                    &name,
                    d["function"]["description"].as_str().unwrap_or(""),
                    crate::plan::risk(&name),
                    d["function"]["parameters"].clone(),
                )
            })
            .collect()
    }

    fn call_tool(&self, tool: &str, arguments: &Value, _operation: Option<Uuid>) -> Result<Value, String> {
        let value = self.sandboxed(tool, &arguments.to_string());
        match value.get("error").and_then(Value::as_str) {
            Some(e) => Err(e.to_string()),
            None => Ok(value),
        }
    }

    fn reason(&self, task: &lyra_execution::Task) -> Result<lyra_execution::Reasoned, lyra_execution::ReasonError> {
        use lyra_execution::Risk;
        let tools: Vec<Value> = self
            .v
            .definitions
            .iter()
            .filter(|d| {
                let name = d["function"]["name"].as_str().unwrap_or("");
                task.tools.as_ref().is_none_or(|a| a.iter().any(|t| t == name))
                    && match crate::plan::risk(name) {
                        Risk::ReadOnly => true,
                        Risk::Mutating => task.may_change,
                        Risk::Destructive => false,
                    }
            })
            .cloned()
            .collect();
        let system = format!("You are carrying out one step of a larger plan. Do only this step, then reply with the result.\n\n{}", task.context);
        let mut messages = vec![json!({ "role": "system", "content": system }), json!({ "role": "user", "content": task.instruction })];
        let mut out = lyra_execution::Reasoned::default();
        let rounds = task.max_model_calls.map_or(self.v.behavior.plan_step_rounds, |left| self.v.behavior.plan_step_rounds.min(left));
        for _ in 0..rounds.max(1) {
            let (message, tokens) = crate::plan::chat(&self.env.url, &self.env.model, &messages, &tools)?;
            out.model_calls += 1;
            out.tokens += tokens;
            {
                let mut r = self.r.lock().unwrap_or_else(|e| e.into_inner());
                r.model_calls += 1;
                r.tokens += tokens;
            }
            let calls = message["tool_calls"].as_array().cloned().unwrap_or_default();
            if calls.is_empty() {
                out.text = message["content"].as_str().unwrap_or("").trim().to_string();
                return if out.text.is_empty() { Err("the model returned nothing".to_string().into()) } else { Ok(out) };
            }
            messages.push(json!({ "role": "assistant", "content": message["content"].as_str().unwrap_or(""), "tool_calls": calls }));
            for call in &calls {
                let name = call["function"]["name"].as_str().unwrap_or("");
                let result = if tools.iter().any(|d| d["function"]["name"] == name) {
                    out.tool_calls += 1;
                    out.tools_used.push(name.to_string());
                    self.sandboxed(name, call["function"]["arguments"].as_str().unwrap_or("{}"))
                } else {
                    json!({ "error": format!("{name} isn't available for this step") })
                };
                messages.push(json!({ "role": "tool", "tool_call_id": call["id"], "content": result.to_string() }));
            }
        }
        Err(format!("the step didn't finish within {} rounds", self.v.behavior.plan_step_rounds).into())
    }

    fn workflow(&self, name: &str) -> Option<String> {
        match &self.v.skill {
            Some((skill, old, new)) if skill == name => Some(if self.v.candidate { new.clone() } else { old.clone() }),
            _ => self.env.learning.as_ref()?.instructions(name),
        }
    }

    fn agent_tools(&self, agent: &str) -> Option<Vec<String>> {
        crate::agents::step_tools(self.env.caps.as_deref(), agent)
    }
}

/// A tool call during a benchmark.
fn sandboxed(env: &Env, v: &Variant, name: &str, arguments: &str, r: &mut BenchResult) -> Value {
    r.tool_calls += 1;
    if let Some(c) = v.composites.iter().find(|c| c.name == name) {
        let inputs: Value = serde_json::from_str(arguments).unwrap_or(json!({}));
        let calls = match c.calls(&inputs) {
            Ok(calls) => calls,
            Err(e) => {
                r.errors += 1;
                return json!({ "error": e });
            }
        };
        let steps: Vec<Value> = calls
            .into_iter()
            .map(|(tool, args)| json!({ "tool": tool, "result": sandboxed(env, v, &tool, &args.to_string(), r) }))
            .collect();
        return json!({ "steps": steps });
    }
    if tools::is_destructive(name) {
        r.safety_violations += 1;
        return json!({ "error": "destructive tools aren't available here" });
    }
    match &env.tools {
        Some(tools) if tools::is_read_only(name) => {
            let text = tools.run(name, arguments, CallContext::new(None, ""));
            let value: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
            if value.get("error").is_some() {
                r.errors += 1;
            }
            value
        }
        Some(_) if v.definitions.iter().any(|d| d["function"]["name"] == name) => json!({ "ok": true }),
        _ => {
            r.errors += 1;
            json!({ "error": format!("unknown tool {name}") })
        }
    }
}

// ---- deploying

/// Apply a candidate: prompt, configuration, workflow and tool changes become
/// a new generation; skill changes go through the skill manager; code changes
/// become a local branch for the user to review (never merged or pushed).
pub fn approve(env: &Env, key: &str) -> Done {
    let mut done = Done { notes: Vec::new(), usage: Vec::new() };
    match approve_inner(env, key, &mut done) {
        Ok(note) => done.notes.push(note),
        Err(e) => done.notes.push(format!("✗ {e}")),
    }
    done
}

fn approve_inner(env: &Env, arg: &str, done: &mut Done) -> Result<String, String> {
    let ev = &env.evolution;
    let (key, force) = match arg.trim().strip_suffix(" force") {
        Some(key) => (key, true),
        None => (arg.trim(), false),
    };
    let c = ev.manager.find(key)?;
    if matches!(c.status, CandidateStatus::Deployed | CandidateStatus::Rejected | CandidateStatus::RolledBack) {
        return Err(format!("candidate {} is {}", c.short(), c.status));
    }
    if c.level.policy() == lyra_evolution::Policy::Never {
        return Err(format!("{} changes are never made by evolution", c.level));
    }
    // The validation lab comes before promotion: tested, passing, and not
    // worse than what's running (unless the user insists).
    let Some(v) = &c.validation else {
        return Err(format!("test it first: /evolve test {} (or /evolve compare {} for its whole group)", c.short(), c.short()));
    };
    if !v.valid {
        return Err(format!("candidate {} failed its checks: /evolve show {}", c.short(), c.short()));
    }
    if let (Some(b), Some(a)) = (&v.fitness_before, &v.fitness_after)
        && !fitness::improves(b, a)
        && !force
    {
        return Err(format!(
            "candidate {} didn't beat the current agent ({:.2} → {:.2}); /evolve approve {} force to deploy it anyway",
            c.short(),
            b.total,
            a.total,
            c.short()
        ));
    }
    let c = ev.manager.set_status(c, CandidateStatus::Approved, if force { "approved by the user despite its benchmark" } else { "approved by the user" })?;
    if let Change::Code { base_commit, diff } = c.change.clone() {
        let lab = code_lab(ev)?;
        let message = format!("evolution: {}\n\n{}", c.problem, c.rationale);
        let branch = lab.branch(&c.short(), &base_commit, &diff, &message)?;
        ev.manager.mark_deployed_elsewhere(c, &format!("committed to branch {branch}"))?;
        return Ok(format!(
            "✓ committed the patch to local branch {branch} in {} — review and merge it yourself; \
             the running binary is unchanged",
            ev.source_repo.as_ref().map_or(String::new(), |p| crate::context::show(p))
        ));
    }
    deploy(env, c, if force { "approved despite its benchmark" } else { "approved" }, done)
}

/// Deploy a tested candidate as a new generation: files for prompt,
/// configuration, workflow and tool changes; a new skill version (recorded
/// in the generation) for skill changes.
fn deploy(env: &Env, c: Candidate, why: &str, done: &mut Done) -> Result<String, String> {
    let ev = &env.evolution;
    if let Change::Tool { tool } = &c.change {
        // Re-check against the tools as they are now.
        let tools = env.tools.as_ref().ok_or("tools are off (memory is disabled)")?;
        let base = tools.base_tools();
        tool.validate(&lyra_evolution::composite::Available { tools: &base })?;
    }
    if let Change::Agent { agent, instructions } = c.change.clone() {
        let agents = env.caps.as_ref().and_then(|c| c.agents()).ok_or("agents are off")?;
        let mut p = agents.registry.find(&agent)?;
        let from = p.version;
        p.instructions = instructions;
        let p = agents.registry.update(p, &format!("evolution: {}", c.rationale))?;
        let revision = lyra_evolution::SkillRevision { name: p.name.clone(), from_version: from as i64, to_version: p.version as i64, agent: true };
        let g = ev.manager.deploy_skill(c, revision, why)?;
        return Ok(format!("✓ refined agent {} to v{} — generation {} (/evolve rollback undoes it)", p.title, p.version, g.number));
    }
    if let Change::Skill { skill, instructions } = c.change.clone() {
        let learning = env.learning.as_ref().ok_or("learning is off")?;
        let to = learning.refine(&skill, &instructions, &format!("evolution: {}", c.rationale))?;
        let revision = lyra_evolution::SkillRevision { name: skill.clone(), from_version: to - 1, to_version: to, agent: false };
        let g = ev.manager.deploy_skill(c, revision, why)?;
        return Ok(format!("✓ refined skill {skill} to v{to} — generation {} (/evolve rollback undoes it)", g.number));
    }
    let summary = c.change.summary();
    let g = ev.manager.deploy(c, why)?;
    done.notes.extend(ev.reload(env.tools.as_deref()));
    Ok(format!("✓ deployed {summary} — generation {} (/evolve rollback undoes it)", g.number))
}

pub fn rollback(env: &Env, arg: &str) -> Done {
    let to = arg.trim().trim_start_matches('g');
    let to = if to.is_empty() { Ok(None) } else { to.parse::<u32>().map(Some).map_err(|_| format!("not a generation number: {arg}")) };
    let notes = match to.and_then(|to| rollback_to(env, to, "requested by the user")) {
        Ok(notes) => notes,
        Err(e) => vec![format!("✗ {e}")],
    };
    Done { notes, usage: Vec::new() }
}

/// Test every open candidate in a group on the same tasks and rank them.
pub fn compare(env: &Env, key: &str) -> Done {
    let mut done = Done { notes: Vec::new(), usage: Vec::new() };
    let ev = &env.evolution;
    let group = match ev.manager.find(key) {
        Ok(c) => c.group,
        Err(e) => {
            done.notes.push(format!("✗ {e}"));
            return done;
        }
    };
    let open: Vec<Candidate> = ev
        .manager
        .candidates(500)
        .unwrap_or_default()
        .into_iter()
        .filter(|c| c.group == group && matches!(c.status, CandidateStatus::Proposed | CandidateStatus::Testing))
        .collect();
    let mut ranked = Vec::new();
    for c in open {
        let c = if c.validation.is_some() {
            c
        } else {
            let t = test(env, c);
            done.notes.extend(t.notes);
            done.usage.extend(t.usage);
            match t.candidate {
                Some(c) => c,
                None => continue,
            }
        };
        ranked.push(c);
    }
    let score = |c: &Candidate| match &c.validation {
        Some(v) if v.valid => v.fitness_after.as_ref().map_or(0.0, |f| f.total),
        _ => -1.0,
    };
    ranked.sort_by(|a, b| score(b).total_cmp(&score(a)));
    done.notes.push("ranking:".into());
    for (i, c) in ranked.iter().enumerate() {
        let v = c.validation.as_ref();
        let fit = match v.map(|v| (v.valid, &v.fitness_before, &v.fitness_after)) {
            Some((false, _, _)) => "failed checks".to_string(),
            Some((true, Some(b), Some(a))) => {
                format!("{:.2} → {:.2}{}", b.total, a.total, if fitness::improves(b, a) { " ✓ beats the baseline" } else { "" })
            }
            _ => "checked, not benchmarked".into(),
        };
        done.notes.push(format!("  {}. {} {} — {fit}", i + 1, c.short(), truncate(&c.change.summary(), 70)));
    }
    if let Some(best) = ranked.first().filter(|c| score(c) >= 0.0) {
        done.notes.push(format!("/evolve approve {} deploys the best one (and rejects the rest)", best.short()));
    }
    done
}

// ---- code evolution (E6)

fn code_lab(ev: &Evolution) -> Result<CodeLab, String> {
    let repo = ev.source_repo.clone().ok_or("code evolution is off: set [evolution] source_repo to lyra's source checkout")?;
    Ok(CodeLab::new(repo, ev.manager.home()))
}

/// Ask the model for a source patch for a problem: it picks files, then
/// writes a diff against the current commit. The patch is only stored as a
/// candidate; `/evolve test` checks it in a sandbox.
pub fn propose_code(env: &Env, problem: &str) -> Done {
    let mut done = Done { notes: Vec::new(), usage: Vec::new() };
    if let Err(e) = propose_code_inner(env, problem, &mut done) {
        done.notes.push(format!("✗ code proposal failed: {e}"));
    }
    done
}

fn propose_code_inner(env: &Env, problem: &str, done: &mut Done) -> Result<(), String> {
    let ev = &env.evolution;
    let lab = code_lab(ev)?;
    let base = lab.head()?;
    let files = lab.files()?;
    // Protected files (the evolution system, safety checks) aren't offered.
    let outline = files
        .iter()
        .filter(|f| !lab::PROTECTED.iter().any(|p| f.starts_with(p) || **f == p.trim_end_matches('/')))
        .map(|f| lab.outline(&base, f))
        .collect::<Vec<_>>()
        .join("\n");
    let errors: Vec<String> = ev.manager.runs(ev.manager.settings.window)?.into_iter().flat_map(|r| r.errors).take(10).collect();
    let mut brief = format!("Problem: {problem}\n");
    if !errors.is_empty() {
        brief += &format!("Recent errors:\n{}\n", errors.iter().map(|e| format!("- {e}")).collect::<Vec<_>>().join("\n"));
    }
    let (reply, usage) = crate::learn::complete(&env.url, &env.model, evolver::CODE_FILES_PROMPT, &format!("{brief}\nSource files (with the functions they define):\n{}", outline))?;
    done.usage.extend(usage);
    let pick = evolver::parse_files(&reply)?;
    let mut sources = String::new();
    for f in &pick.files {
        let text = lab.read(&base, f)?;
        sources += &format!("\n=== {f} ===\n{}\n", text.chars().take(60_000).collect::<String>());
    }
    let (reply, usage) = crate::learn::complete(
        &env.url,
        &env.model,
        evolver::CODE_PATCH_PROMPT,
        &format!("{brief}Plan: {}\n\nFiles:{sources}", pick.plan),
    )?;
    done.usage.extend(usage);
    let diff = evolver::parse_diff(&reply)?;
    if lyra_evolution::safety_scan(&diff).is_some() {
        return Err("the patch looks like it contains a secret".into());
    }
    if let Some(why) = lab::refuse(&diff) {
        return Err(why);
    }
    let c = ev.manager.propose_code(problem, &pick.plan, &base, &diff)?;
    done.notes.push(format!(
        "proposed code change {} ({}) — /evolve show {} to read it · /evolve test {} to check it in a sandbox",
        c.short(),
        pick.files.join(", "),
        c.short(),
        c.short()
    ));
    Ok(())
}

// ---- commands that answer right away

pub const COMMANDS: &str = "\
/evolve                      evolution status: generation, success rate, pending candidates
/evolve review               look for problems in recent runs and propose fixes now
/evolve list · show <id>     candidates · one candidate's change, evidence and test results
/evolve test <id>            check a candidate and benchmark it against the current agent
/evolve compare <id>         test every open candidate for the same problem and rank them
/evolve approve <id> [force] deploy a tested candidate (code: commit to a local branch);
                             force deploys one that didn't beat the current agent
/evolve reject <id>          discard it
/evolve rollback [gen]       restore the previous (or a given) generation
/evolve generations|history|runs   deployed generations · evolution events · recorded runs
/evolve code <problem>       propose a source patch (needs [evolution] source_repo)";

pub fn status(ev: &Evolution) -> Result<String, String> {
    let s = ev.snapshot()?;
    let st = &s.stats;
    let mut out = vec![format!("generation {} · mode {:?} · {} runs recorded (last {})", st.generation, s.mode, st.runs, ev.manager.settings.window)];
    out.push(format!(
        "success {} over {} judged runs · {} corrections · avg {:.1} model / {:.1} tool calls",
        st.success_rate.map_or("—".into(), |r| format!("{:.0}%", r * 100.0)),
        st.judged,
        st.corrections,
        st.avg_model_calls,
        st.avg_tool_calls
    ));
    out.push(format!("evolved: {} guideline(s) · {} workflow(s) · {} composite tool(s)", s.guidelines, s.workflows, s.tools));
    if !s.settings.is_empty() {
        out.push(format!("settings: {}", s.settings.join(" · ")));
    }
    out.push(format!("{} candidate(s) waiting · last review {}", st.pending, s.last_review.map_or("never".into(), |t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string())));
    out.push(String::new());
    out.push(COMMANDS.into());
    Ok(out.join("\n"))
}

pub fn list(ev: &Evolution) -> Result<String, String> {
    let candidates = ev.manager.candidates(30)?;
    if candidates.is_empty() {
        return Ok("no candidates yet — /evolve review looks for problems".into());
    }
    Ok(candidates
        .iter()
        .map(|c| {
            let fit = c
                .validation
                .as_ref()
                .map(|v| match (&v.fitness_before, &v.fitness_after) {
                    (Some(b), Some(a)) => format!(" · {:.2}→{:.2}", b.total, a.total),
                    _ if !v.valid => " · failed checks".into(),
                    _ => " · checked".into(),
                })
                .unwrap_or_default();
            format!("{} {} [{}] {}{fit} — {}", c.short(), c.status, c.level, truncate(&c.change.summary(), 70), truncate(&c.problem, 60))
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

pub fn show(ev: &Evolution, key: &str) -> Result<String, String> {
    let c = ev.manager.find(key)?;
    let mut out = vec![
        format!("candidate {} · {} · level {} ({:?}) · confidence {:.2}", c.short(), c.status, c.level, c.level.policy(), c.confidence),
        format!("problem: {}", c.problem),
        format!("rationale: {}", c.rationale),
        format!("change: {}", c.change.summary()),
    ];
    match &c.change {
        Change::Workflow { workflow } => out.push(workflow.render()),
        Change::Tool { tool } => out.push(tool.render()),
        Change::Skill { instructions, .. } | Change::Agent { instructions, .. } => out.push(instructions.clone()),
        Change::Code { base_commit, diff } => {
            out.push(format!("against {}", &base_commit[..base_commit.len().min(10)]));
            out.push(diff.clone());
        }
        _ => {}
    }
    out.push(format!("evidence: {} run(s)", c.evidence.len()));
    let runs = ev.manager.runs(ev.manager.settings.window * 2)?;
    for r in runs.iter().filter(|r| c.evidence.contains(&r.id)).take(5) {
        out.push(format!("  [{} {}] {}", r.kind, r.outcome, truncate(&r.task, 80)));
    }
    if let Some(v) = &c.validation {
        out.push(format!("validation: {}", if v.valid { "passed checks" } else { "failed checks" }));
        out.extend(v.notes.iter().map(|n| format!("  {n}")));
        if let Some(f) = &v.fitness_before {
            out.push(format!("  {}", describe_fitness("baseline", f)));
        }
        if let Some(f) = &v.fitness_after {
            out.push(format!("  {}", describe_fitness("candidate", f)));
        }
        for (name, ok, output) in &v.checks {
            out.push(format!("  {} {name}", if *ok { "✓" } else { "✗" }));
            if !ok {
                out.push(last_lines(output, 12));
            }
        }
    } else {
        out.push(format!("not tested yet — /evolve test {}", c.short()));
    }
    Ok(out.join("\n"))
}

pub fn reject(ev: &Evolution, key: &str) -> Result<String, String> {
    let c = ev.manager.find(key)?;
    if c.status == CandidateStatus::Deployed {
        return Err("it's deployed — /evolve rollback undoes it".into());
    }
    let c = ev.manager.reject(c, "rejected by the user")?;
    Ok(format!("rejected {}", c.short()))
}

pub fn generations(ev: &Evolution) -> Result<String, String> {
    Ok(ev
        .manager
        .generations()?
        .iter()
        .take(20)
        .map(|g| format!("gen {} · {} · {}", g.number, g.created_at.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M"), g.reason))
        .collect::<Vec<_>>()
        .join("\n"))
}

pub fn history(ev: &Evolution) -> Result<String, String> {
    let events = ev.manager.events(30)?;
    if events.is_empty() {
        return Ok("nothing has happened yet".into());
    }
    Ok(events
        .iter()
        .rev()
        .map(|e| {
            let fit = match (e.fitness_before, e.fitness_after) {
                (Some(b), Some(a)) => format!(" ({b:.2}→{a:.2})"),
                _ => String::new(),
            };
            format!("{} {}: {}{fit}", e.created_at.with_timezone(&chrono::Local).format("%m-%d %H:%M"), e.kind, truncate(&e.description, 120))
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

pub fn runs(ev: &Evolution) -> Result<String, String> {
    let runs = ev.manager.runs(20)?;
    if runs.is_empty() {
        return Ok("no runs recorded yet".into());
    }
    Ok(runs
        .iter()
        .map(|r| {
            let mut line = format!(
                "{} g{} {} {} · {}m/{}t · {}s — {}",
                r.created_at.with_timezone(&chrono::Local).format("%m-%d %H:%M"),
                r.generation,
                r.kind,
                r.outcome,
                r.model_calls,
                r.tool_calls,
                r.duration_ms / 1000,
                truncate(&r.task, 60)
            );
            if r.corrected {
                line += " · corrected";
            }
            if !r.errors.is_empty() {
                line += &format!(" · {} error(s)", r.errors.len());
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

fn truncate(text: &str, max: usize) -> String {
    let text = text.replace('\n', " ");
    if text.chars().count() <= max {
        return text;
    }
    format!("{}…", text.chars().take(max.saturating_sub(1)).collect::<String>())
}

fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use lyra_evolution::workflow::Phase;

    #[test]
    fn chat_section_has_guidelines_and_the_matching_workflow() {
        let mut behavior = Behavior::default();
        assert_eq!(chat_section(&behavior, &[], "hi"), None);
        behavior.guidelines.push("Answer briefly.".into());
        behavior.recall_before_answering = true;
        let w = WorkflowDef {
            name: "release".into(),
            description: "shipping a new version".into(),
            triggers: vec!["release".into()],
            phases: vec![Phase { name: "check".into(), instruction: "run the tests".into() }],
        };
        let section = chat_section(&behavior, std::slice::from_ref(&w), "cut a release").unwrap();
        assert!(section.contains("- Answer briefly."));
        assert!(section.contains("memory_recall"));
        assert!(section.contains("# Workflow: release"));
        assert!(section.contains("1. check: run the tests"));
        assert!(!chat_section(&behavior, &[w], "hello").unwrap().contains("Workflow"));
    }

    /// The plan-mode benchmark against a real model. Opt in with
    /// `LYRA_LIVE_URL=http://host:port/v1 LYRA_LIVE_MODEL=name cargo test live_plan_benchmark -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_plan_benchmark() {
        let (Ok(url), Ok(model)) = (std::env::var("LYRA_LIVE_URL"), std::env::var("LYRA_LIVE_MODEL")) else { return };
        crate::learn::configure(crate::learn::Structured { max_tokens: 8192, thinking: false });
        let rt = tokio::runtime::Runtime::new().unwrap();
        let home = std::env::temp_dir().join(format!("lyra-live-{}", Uuid::new_v4()));
        let manager = EvolutionManager::open(&home, rt.handle().clone(), Default::default()).unwrap();
        let ev = Arc::new(Evolution::new(manager, rt.handle().clone(), None, 1));
        let mut run = RunRecord::new(lyra_evolution::RunKind::Plan, "Write a three-line haiku about autumn and check it has three lines", 1);
        run.outcome = lyra_evolution::RunOutcome::Partial;
        ev.manager.record_run(&run).unwrap();
        let op = lyra_evolution::Opportunity {
            kind: "plans_need_rework".into(),
            problem: "plans need rework".into(),
            categories: vec![Category::Configuration],
            evidence: vec![run.id],
            details: json!({}),
        };
        let change = Change::Configuration { key: "verify_reasoning_steps".into(), from: json!(false), to: json!(true) };
        let c = ev.manager.propose(&op, vec![evolver::Proposal { change, rationale: "verify".into(), confidence: 0.6 }]).unwrap().remove(0);
        let env = Env { url: format!("{}/chat/completions", url.trim_end_matches('/')), model, evolution: ev, tools: None, learning: None, caps: None, goals: None, system_prompt: None };
        let t = test(&env, c);
        for n in &t.notes {
            println!("{n}");
        }
        let v = t.candidate.expect("tested").validation.expect("validated");
        assert!(v.valid);
        assert!(t.notes.iter().any(|n| n.contains("as plans")), "planner-only change benchmarked as plans");
        assert!(v.fitness_before.is_some() && v.fitness_after.is_some());
        let _ = std::fs::remove_dir_all(home);
    }
}

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
    /// Answers per task on each side of a benchmark.
    benchmark_samples: usize,
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
    /// While a generation is on trial: the one before it, which the other
    /// turns still use (the control), and the trial's share of turns.
    control: Option<Control>,
}

struct Control {
    behavior: Behavior,
    workflows: Vec<WorkflowDef>,
    number: u32,
    share: f32,
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
    /// The change on trial, in short ("g5 on 30% · 8/20 · 75% vs 60%").
    pub trial: Option<String>,
    /// Tests in the test set.
    pub tests: usize,
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
#[derive(Default)]
pub struct Done {
    pub notes: Vec<String>,
    /// Model calls made (their tokens count toward the session totals).
    pub usage: Vec<Usage>,
    /// A scheduled review's digest for the owner (what's ready to approve).
    pub digest: Option<String>,
}

impl Evolution {
    pub fn new(manager: EvolutionManager, rt: tokio::runtime::Handle, source_repo: Option<PathBuf>, benchmark_tasks: usize) -> Self {
        Self { manager, live: RwLock::new(Live::default()), source_repo, benchmark_tasks, benchmark_samples: 2, warned: Mutex::new(None), rt }
    }

    /// Answers per task on each side of a benchmark (`[evolution] benchmark_samples`).
    pub fn with_samples(mut self, n: usize) -> Self {
        self.benchmark_samples = n.clamp(1, 5);
        self
    }

    /// During a trial, which side a turn (by its run id) is on: `Some(true)`
    /// the generation on trial, `Some(false)` the one before it. The same run
    /// always lands on the same side.
    pub fn arm(&self, run: Uuid) -> Option<bool> {
        let live = self.live.read().unwrap_or_else(|e| e.into_inner());
        let c = live.control.as_ref()?;
        Some(((run.as_u128() % 10_000) as f32) / 10_000.0 < c.share)
    }

    /// The behavior a turn uses (the control's, for a turn on that side).
    pub fn behavior_for(&self, run: Uuid) -> Behavior {
        let live = self.live.read().unwrap_or_else(|e| e.into_inner());
        match (self.arm_in(&live, run), &live.control) {
            (Some(false), Some(c)) => c.behavior.clone(),
            _ => live.behavior.clone(),
        }
    }

    /// Behavior guidelines and the matching workflow for this turn's side.
    pub fn chat_section_for(&self, request: &str, run: Uuid) -> Option<String> {
        let live = self.live.read().unwrap_or_else(|e| e.into_inner());
        match (self.arm_in(&live, run), &live.control) {
            (Some(false), Some(c)) => chat_section(&c.behavior, &c.workflows, request),
            _ => chat_section(&live.behavior, &live.workflows, request),
        }
    }

    /// What a turn had in play (4.7): its side's guidelines and the workflow that matched.
    pub fn in_play(&self, request: &str, run: Uuid) -> (Vec<String>, Option<String>) {
        let live = self.live.read().unwrap_or_else(|e| e.into_inner());
        let (behavior, workflows) = match (self.arm_in(&live, run), &live.control) {
            (Some(false), Some(c)) => (&c.behavior, &c.workflows),
            _ => (&live.behavior, &live.workflows),
        };
        (behavior.guidelines.clone(), workflow::pick(workflows, request).map(|w| w.name.clone()))
    }

    fn arm_in(&self, live: &Live, run: Uuid) -> Option<bool> {
        live.control.as_ref().map(|c| ((run.as_u128() % 10_000) as f32) / 10_000.0 < c.share)
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
        // On trial: the generation before it, for the other turns.
        let control = match self.manager.trial() {
            Ok(Some((g, parent))) => Some(Control {
                behavior: Behavior::parse(&parent.snapshot.behavior).unwrap_or_default(),
                workflows: parent.snapshot.workflows.values().filter_map(|t| WorkflowDef::parse(t).ok()).collect(),
                number: parent.number,
                share: g.trial.map_or(0.3, |t| t.share),
            }),
            Ok(None) => None,
            Err(e) => {
                notes.push(format!("couldn't read the trial: {e}"));
                None
            }
        };
        *self.live.write().unwrap_or_else(|e| e.into_inner()) = Live { behavior, workflows, composites: loaded, control };
        notes
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
    /// During a trial, `arm` says which side the run was on (`Some(false)`:
    /// the control, so it counts for the generation before).
    pub fn record(&self, mut run: RunRecord, arm: Option<bool>) -> Result<Uuid, String> {
        run.generation = self.generation();
        if let Some(on_trial) = arm {
            run.arm = Some(if on_trial { "trial" } else { "control" }.into());
            if !on_trial && let Some(c) = &self.live.read().unwrap_or_else(|e| e.into_inner()).control {
                run.generation = c.number;
            }
        }
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
            trial: self.manager.trial_standing().ok().flatten().map(|t| {
                let rate = |a: &lyra_evolution::Arm| a.success_rate().map_or("—".into(), |r| format!("{:.0}%", r * 100.0));
                format!("g{} on {:.0}% · {}/{} · {} vs {}", t.generation, t.share * 100.0, t.trial.judged, t.needed, rate(&t.trial), rate(&t.control))
            }),
            tests: lyra_evolution::golden::list(self.manager.home()).0.len(),
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
    // A generation on trial is decided by its trial (E8), in any mode.
    match ev.manager.trial_verdict() {
        Ok(Some((true, why))) => {
            return match ev.manager.end_trial(lyra_evolution::TrialStatus::Kept, &why) {
                Ok(_) => {
                    let mut notes = vec![format!("✓ {why}: it's kept")];
                    notes.extend(ev.reload(env.tools.as_deref()));
                    notes
                }
                Err(e) => vec![format!("ending the trial failed: {e}")],
            };
        }
        Ok(Some((false, why))) => {
            if let Err(e) = ev.manager.end_trial(lyra_evolution::TrialStatus::Lost, &why) {
                return vec![format!("ending the trial failed: {e}")];
            }
            return match rollback_to(env, None, &why) {
                Ok(mut notes) => {
                    notes[0] = format!("↩ {why}; {}", notes[0].trim_start_matches("↩ "));
                    notes
                }
                Err(e) => vec![format!("{why}; rollback failed: {e}")],
            };
        }
        Ok(None) => {}
        Err(e) => return vec![format!("evolution trial check failed: {e}")],
    }
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
    let mut done = Done::default();
    if let Err(e) = review_inner(env, &mut done) {
        done.notes.push(format!("evolution review failed: {e}"));
    }
    done
}

/// Active skills' health with their instructions.
type SkillsSeen = Vec<(SkillHealth, String)>;

/// What the detectors find in recent runs, skills, capabilities, goals and
/// agents; with the active skills (and their instructions) they looked at.
fn problems(env: &Env) -> Result<(Vec<lyra_evolution::Opportunity>, SkillsSeen), String> {
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
    Ok((ops, skills))
}

/// How lyra is doing (2.6, admins): outcomes week by week and how they came
/// in, the problems the detectors see now, each generation's and skill's
/// record, the trial and the test set. Counts only: no one's words.
pub fn health(env: &Env) -> Result<Value, String> {
    let ev = &env.evolution;
    let h = ev.manager.health(8)?;
    let (ops, _) = problems(env)?;
    let mut skills = h.skills.clone();
    skills.sort_by_key(|x| std::cmp::Reverse(x.with.runs));
    skills.truncate(15);
    Ok(json!({
        "mode": format!("{:?}", ev.mode()).to_lowercase(),
        "generation": ev.generation(),
        "weeks": h.weeks,
        "sources": h.sources,
        "all": h.all,
        "owner_runs": h.owner_runs,
        "other_runs": h.other_runs,
        "problems": ops.iter().take(8).map(|o| json!({ "kind": o.kind, "problem": o.problem })).collect::<Vec<_>>(),
        "generations": h.generations,
        "skills": skills,
        "guidelines": h.guidelines,
        "workflows": h.workflows,
        "trial": h.trial,
        "tests": lyra_evolution::golden::list(ev.manager.home()).0.len(),
        "pending": ev.manager.candidates(200)?.iter().filter(|c| matches!(c.status, CandidateStatus::Proposed | CandidateStatus::Testing)).count(),
    }))
}

fn review_inner(env: &Env, done: &mut Done) -> Result<(), String> {
    let ev = &env.evolution;
    let (ops, skills) = problems(env)?;
    let agents = env.caps.as_ref().and_then(|c| c.agents().cloned());
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
    let past = ev.manager.candidates(500)?;
    for op in ops.iter().take(ev.manager.settings.max_problems.max(1)) {
        done.notes.push(format!("problem: {}", op.problem));
        // Attempt memory (3.2): what was tried for this problem before.
        let before = attempts(&past, &op.problem);
        if let Some(open) = before.iter().find(|c| matches!(c.status, CandidateStatus::Proposed | CandidateStatus::Testing)) {
            done.notes.push(format!("  already proposed for it: {} (/evolve show {})", open.short(), open.short()));
            continue;
        }
        if let Some(recent) = before.iter().find(|c| matches!(c.status, CandidateStatus::Rejected | CandidateStatus::RolledBack) && Utc::now() - c.updated_at < chrono::Duration::days(RETRY_AFTER_DAYS)) {
            done.notes.push(format!("  tried {} days ago ({} {}): not again yet", (Utc::now() - recent.updated_at).num_days(), recent.short(), recent.status));
            continue;
        }
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
        let mut prompt = evolver::prompt(op, &evidence, &ctx, &allowed);
        if !before.is_empty() {
            prompt += "\n\nTried before for this problem (don't propose these again; learn from how they went):\n";
            for c in before.iter().take(6) {
                let how = c.validation.as_ref().and_then(|v| v.comparison.as_ref().map(|x| x.note.clone())).unwrap_or_default();
                prompt += &format!("- {} — {}{}\n", c.change.summary(), c.status, if how.is_empty() { String::new() } else { format!(" ({how})") });
            }
        }
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
        // Propose mode (3.1): the most likely candidate for each problem is
        // tested now, so the Proposals page shows it ready, with its score.
        let mut groups: Vec<Uuid> = created.iter().map(|c| c.group).collect();
        groups.dedup();
        for group in groups.into_iter().take(MAX_TESTED_PER_REVIEW) {
            let Some(best) = created.iter().filter(|c| c.group == group && !matches!(c.change, Change::Code { .. })).max_by(|a, b| a.confidence.total_cmp(&b.confidence)) else { continue };
            let t = test(env, ev.manager.find(&best.id.to_string())?);
            done.notes.extend(t.notes);
            done.usage.extend(t.usage);
        }
        done.notes.push("/evolve compare <id> tests and ranks a problem's candidates · /evolve approve <id> · or the Proposals page".into());
    }
    Ok(())
}

/// Problems a review tests a candidate for at once (each is a full benchmark).
const MAX_TESTED_PER_REVIEW: usize = 3;
/// A change rejected or rolled back isn't proposed again for this long.
const RETRY_AFTER_DAYS: i64 = 14;

/// A problem as itself, whatever its numbers this time ("4 of 4 answers" = "5 of 6").
pub fn fingerprint(problem: &str) -> String {
    let t: String = problem.to_lowercase().chars().map(|c| if c.is_ascii_digit() { '#' } else { c }).collect();
    let mut out = String::new();
    for w in t.split_whitespace() {
        let w = if w.chars().all(|c| c == '#' || c == '.' || c == '%' || c == ',') { "#" } else { w };
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
    }
    out
}

/// Candidates proposed before for the same problem, newest first.
fn attempts<'a>(past: &'a [Candidate], problem: &str) -> Vec<&'a Candidate> {
    let f = fingerprint(problem);
    past.iter().filter(|c| fingerprint(&c.problem) == f).collect()
}

/// A scheduled review (3.1): the review, then a digest for the owner of
/// what's ready to approve (in the app; the Proposals page has the details).
pub fn scheduled_review(env: &Env) -> Done {
    let mut done = review(env);
    let ev = &env.evolution;
    let ready: Vec<Candidate> = ev
        .manager
        .candidates(200)
        .unwrap_or_default()
        .into_iter()
        .filter(|c| matches!(c.status, CandidateStatus::Proposed | CandidateStatus::Testing))
        .collect();
    let passed: Vec<&Candidate> = ready.iter().filter(|c| c.validation.as_ref().is_some_and(|v| v.valid && v.improves())).collect();
    let trial = ev.manager.trial_standing().ok().flatten();
    if ready.is_empty() && trial.is_none() {
        return done;
    }
    let mut lines = Vec::new();
    if !passed.is_empty() {
        lines.push(format!("{} change(s) beat the current lyra and are ready to approve:", passed.len()));
        lines.extend(passed.iter().take(5).map(|c| format!("• {} — {}", truncate(&c.change.summary(), 80), c.validation.as_ref().and_then(|v| v.comparison.as_ref()).map_or(String::new(), |x| x.note.clone()))));
    }
    let waiting = ready.len() - passed.len();
    if waiting > 0 {
        lines.push(format!("{waiting} other proposal(s) wait for a look."));
    }
    if let Some(t) = trial {
        lines.push(format!("Generation {} is on trial: {} of its {} replies have an outcome.", t.generation, t.trial.judged, t.needed));
    }
    done.digest = Some(lines.join("\n"));
    done
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
        Done { notes: t.notes, usage: t.usage, digest: None }
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
    // An API: its description read again (no benchmark: it adds tools, it doesn't change answers).
    if let Change::Api { api } = &c.change {
        match api_config(api).and_then(crate::caps::Caps::try_openapi) {
            Ok((client, _)) => v.notes.push(format!("read {} operations from {}", client.capabilities().len(), api.spec)),
            Err(e) => {
                v.valid = false;
                v.notes.push(e);
            }
        }
        t.notes.push(format!("{}: {} {}", c.short(), if v.valid { "✓" } else { "✗" }, v.notes.join("; ")));
        return ev.manager.save_validation(c, v);
    }
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
    // Plans are slow: fewer of them, once each.
    let (tasks, samples): (Vec<BenchTask>, usize) = if plans { (tasks.into_iter().take(4).collect(), 1) } else { (tasks, ev.benchmark_samples) };
    let how = if plans { "as plans" } else { "as chats" };
    t.notes.push(format!("{}: benchmarking {} task(s) {how}, {samples} answer(s) each side, judged pair by pair…", c.short(), tasks.len()));
    let baseline = Variant::new(env, &current, skill_override.clone(), false);
    let candidate = Variant::new(env, &next, skill_override, true);
    let compared = pairwise(env, &tasks, samples, t, |task, cand, t| {
        let v = if cand { &candidate } else { &baseline };
        if plans { bench_plan(env, v, task, t) } else { bench(env, v, task, t) }
    });
    v.notes.push(format!("benchmarked on {} task(s): {}", tasks.len(), compared.2.note));
    v.fitness_before = Some(compared.0);
    v.fitness_after = Some(compared.1);
    v.comparison = Some(compared.2);
    ev.manager.save_validation(c, v)
}

/// Both sides answer every task `samples` times; each pair is judged blind;
/// the candidate passes when its gain beats the noise (E7).
fn pairwise(env: &Env, tasks: &[BenchTask], samples: usize, t: &mut Tested, mut answer: impl FnMut(&BenchTask, bool, &mut Tested) -> (BenchResult, Option<String>)) -> (Fitness, Fitness, lyra_evolution::Comparison) {
    use lyra_evolution::compare::Pref;
    let judge_by = judge_model(env);
    let (mut before, mut after, mut per_task) = (Vec::new(), Vec::new(), Vec::new());
    for task in tasks {
        let mut prefs = Vec::new();
        for _ in 0..samples.max(1) {
            let (mut a, answer_a) = answer(task, false, t);
            let (mut b, answer_b) = answer(task, true, t);
            let pref = match (&answer_a, &answer_b) {
                (Some(x), Some(y)) => pair_judge(&judge_by, task, x, y, t),
                (None, Some(_)) => Some(Pref::Better),
                (Some(_), None) => Some(Pref::Worse),
                (None, None) => None,
            };
            // The pair's verdict is each side's score too (for the fitness shown).
            let (sa, sb) = match pref {
                Some(Pref::Better) => (0.0, 1.0),
                Some(Pref::Worse) => (1.0, 0.0),
                Some(Pref::Tie) => (0.5, 0.5),
                None => (0.0, 0.0),
            };
            (a.accuracy, a.success, b.accuracy, b.success) = (sa, sa >= 0.5, sb, sb >= 0.5);
            prefs.extend(pref);
            before.push(a);
            after.push(b);
        }
        per_task.push(prefs);
    }
    let weights = &env.evolution.manager.settings.fitness;
    let (fb, fa) = (fitness::fitness(&before, weights), fitness::fitness(&after, weights));
    let c = lyra_evolution::compare::compare(&per_task, &fb, &fa, &judge_by.1);
    t.notes.push(format!("  {} better · {} worse · {} as good (judged by {}): {}", c.wins, c.losses, c.ties, c.judge, c.note));
    t.notes.push(format!("  {}", describe_fitness("baseline", &fb)));
    t.notes.push(format!("  {}", describe_fitness("candidate", &fa)));
    (fb, fa, c)
}

/// Who judges: the fallback model when there is one other than the chat
/// model (a model doesn't grade itself), else the chat model. `(url, model)`.
fn judge_model(env: &Env) -> (String, String) {
    match crate::fallback::first_up().filter(|f| f.model != env.model && !crate::known_down::is_down("fallback")) {
        Some(f) => (format!("{}/chat/completions", f.url.trim_end_matches('/')), f.model),
        None => (env.url.clone(), env.model.clone()),
    }
}

const PAIR_PROMPT: &str = "\
You compare two answers to the same request and say which is better: more correct, more complete, more helpful, and doing what was asked. \
Length and style don't matter unless they get in the way. If a reference answer is given, someone liked it: a good answer needn't match it word for word. \
Reply with JSON only: {\"better\": \"1\" | \"2\" | \"tie\", \"reason\": \"a few words\"}.";

/// Which answer is better, shown in a random order so the judge can't
/// favour a side: the candidate's (`b`) better, worse or as good.
fn pair_judge(judge: &(String, String), task: &BenchTask, a: &str, b: &str, t: &mut Tested) -> Option<lyra_evolution::compare::Pref> {
    use lyra_evolution::compare::Pref;
    let candidate_first = Uuid::new_v4().as_u128().is_multiple_of(2);
    let (first, second) = if candidate_first { (b, a) } else { (a, b) };
    let clip = |x: &str| x.chars().take(6000).collect::<String>();
    let mut prompt = String::new();
    if !task.history.is_empty() {
        prompt += "Earlier in the conversation:\n";
        for (role, text) in &task.history {
            prompt += &format!("{role}: {}\n", text.chars().take(600).collect::<String>());
        }
        prompt += "\n";
    }
    prompt += &format!("Request: {}\n\nWhat a good answer does: {}\n\n", task.task, task.expect);
    if let Some(r) = &task.reference {
        prompt += &format!("Reference answer:\n{}\n\n", clip(r));
    }
    prompt += &format!("Answer 1:\n{}\n\nAnswer 2:\n{}", clip(first), clip(second));
    let reply = match crate::learn::complete(&judge.0, &judge.1, PAIR_PROMPT, &prompt) {
        Ok((reply, usage)) => {
            t.usage.extend(usage);
            reply
        }
        Err(e) => {
            t.notes.push(format!("  judge call failed: {e}"));
            return None;
        }
    };
    let v: Value = reply.find('{').zip(reply.rfind('}')).and_then(|(s, e)| serde_json::from_str(reply.get(s..=e)?).ok())?;
    let said = v["better"].as_str().map(str::to_string).or_else(|| v["better"].as_u64().map(|n| n.to_string()))?;
    Some(match (said.trim(), candidate_first) {
        ("tie", _) => Pref::Tie,
        ("1", true) | ("2", false) => Pref::Better,
        ("1", false) | ("2", true) => Pref::Worse,
        _ => {
            t.notes.push(format!("  judge reply unusable: {}", reply.chars().take(120).collect::<String>()));
            return None;
        }
    })
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
    t.notes.push(format!("{}: benchmarking {} on {} task(s), current vs revised instructions, judged pair by pair…", c.short(), current.title, tasks.len()));
    let tasks: Vec<BenchTask> = tasks.into_iter().map(|(task, expect)| BenchTask { task, expect, reference: None, history: Vec::new() }).collect();
    let (fb, fa, compared) = pairwise(env, &tasks, ev.benchmark_samples, t, |task, cand, t| {
        let start = Instant::now();
        let mut r = BenchResult::default();
        let p = if cand { &next } else { &current };
        let answer = match crate::agents::bench_answer(&env.url, &env.model, p, &task.task) {
            Ok((answer, tokens)) => {
                r.model_calls = 1;
                r.tokens = tokens;
                Some(answer).filter(|a| !a.trim().is_empty())
            }
            Err(e) => {
                r.errors += 1;
                t.notes.push(format!("  benchmark call failed: {e}"));
                None
            }
        };
        r.seconds = start.elapsed().as_secs_f32();
        (r, answer)
    });
    v.notes.push(format!("benchmarked {} on {} task(s): {}", current.title, tasks.len(), compared.note));
    v.fitness_before = Some(fb);
    v.fitness_after = Some(fa);
    v.comparison = Some(compared);
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

/// A benchmark task: a request (with the turns it followed), what a good
/// answer must do, and an answer someone liked.
struct BenchTask {
    task: String,
    expect: String,
    reference: Option<String>,
    history: Vec<(String, String)>,
}

/// Tasks to replay: the candidate's evidence first (the problem it's for),
/// then the test set (`evolution/golden/`), then other recent requests.
/// When the user corrected the original answer, their correction becomes
/// part of the expectation.
fn benchmark_tasks(ev: &Evolution, c: &Candidate) -> Result<Vec<BenchTask>, String> {
    let runs = ev.manager.runs(ev.manager.settings.window * 2)?;
    let mut tasks: Vec<BenchTask> = Vec::new();
    let usable = |task: &str, tasks: &[BenchTask]| task.len() >= 8 && !task.starts_with('/') && !tasks.iter().any(|t| t.task == task);
    let from_run = |r: &RunRecord| {
        let mut expect = "a correct, complete and helpful answer to the request".to_string();
        if let Some(f) = r.feedback.as_ref().filter(|_| r.corrected) {
            expect += &format!(", which also satisfies what the user said after an earlier answer to it was wrong: \"{f}\"");
        }
        BenchTask { task: r.task.trim().to_string(), expect, reference: None, history: r.history.clone() }
    };
    for r in runs.iter().filter(|r| c.evidence.contains(&r.id)) {
        if usable(r.task.trim(), &tasks) {
            tasks.push(from_run(r));
        }
    }
    for g in lyra_evolution::golden::list(ev.manager.home()).0 {
        if tasks.len() >= ev.benchmark_tasks {
            break;
        }
        if usable(g.task.trim(), &tasks) {
            tasks.push(BenchTask { task: g.task.trim().to_string(), expect: g.expect, reference: g.reference, history: g.history });
        }
    }
    for r in &runs {
        if tasks.len() >= ev.benchmark_tasks {
            break;
        }
        if usable(r.task.trim(), &tasks) {
            tasks.push(from_run(r));
        }
    }
    tasks.truncate(ev.benchmark_tasks.max(1));
    Ok(tasks)
}

/// Add a request (and the answer someone liked) to the test set; `false`
/// when it's there already.
pub fn add_test(ev: &Evolution, task: &str, reference: Option<&str>, history: Vec<(String, String)>, source: &str) -> Result<bool, String> {
    let task = task.trim();
    if task.chars().count() < 8 || task.starts_with('/') {
        return Err("that's too short to test with".into());
    }
    let t = lyra_evolution::golden::GoldenTask::new(task, reference, history, source);
    lyra_evolution::golden::add(ev.manager.home(), &t).map_err(|e| format!("couldn't save the test: {e}"))
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

    /// The system prompt as a live turn builds it (E7): the chat's prompt,
    /// the evolved section, what's remembered about the request, the skills
    /// that match it (this variant's version of a changed one), the tool rules.
    fn system(&self, env: &Env, task: &str) -> String {
        let mut parts: Vec<String> = env.system_prompt.iter().cloned().collect();
        parts.extend(chat_section(&self.behavior, &self.workflows, task));
        // Read only: nothing is recorded as used.
        if let Some(found) = env.tools.as_ref().and_then(|t| t.mem.recall(None, task, 6, false).ok()).filter(|f| !f.is_empty()) {
            let lines: Vec<String> = found.iter().map(|r| format!("- {}", r.memory.content.chars().take(300).collect::<String>())).collect();
            parts.push(format!("# What you remember that may help\n\n{}", lines.join("\n")));
        }
        let mut skills: Vec<(String, String)> = env
            .learning
            .as_ref()
            .and_then(|l| l.relevant(task).ok())
            .map(|found| found.into_iter().map(|r| (r.skill.name, r.skill.instructions)).collect())
            .unwrap_or_default();
        if let Some((name, old, new)) = &self.skill {
            let text = if self.candidate { new } else { old };
            skills.retain(|(n, _)| n != name);
            skills.insert(0, (name.clone(), text.clone()));
        }
        if !skills.is_empty() {
            let each: Vec<String> = skills.iter().map(|(n, i)| format!("## {n}\n{i}")).collect();
            parts.push(format!("# Learned skills\n\n{}", each.join("\n\n")));
        }
        if !self.definitions.is_empty() {
            parts.push(crate::turn::TOOL_RULES.to_string());
        }
        parts.join("\n\n")
    }
}

/// Run one task headless with a variant and have the model judge the answer.
/// Only read-only tools really run; anything that would change something is
/// simulated, and destructive calls count as safety violations.
fn bench(env: &Env, v: &Variant, bt: &BenchTask, t: &mut Tested) -> (BenchResult, Option<String>) {
    let start = Instant::now();
    let mut r = BenchResult::default();
    let task = bt.task.as_str();
    // A follow-up is asked after what it follows.
    let mut messages = vec![json!({ "role": "system", "content": v.system(env, task) })];
    messages.extend(bt.history.iter().filter(|(role, _)| matches!(role.as_str(), "user" | "assistant")).map(|(role, text)| json!({ "role": role, "content": text })));
    messages.push(json!({ "role": "user", "content": task }));
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
    let answer = answer.filter(|a| !a.trim().is_empty());
    if answer.is_none() {
        r.errors += 1;
    }
    (r, answer)
}

/// Plan and run a task with a variant on a throwaway plan store, approving
/// steps automatically (tools are sandboxed), then judge the result.
fn bench_plan(env: &Env, v: &Variant, task: &BenchTask, t: &mut Tested) -> (BenchResult, Option<String>) {
    use lyra_execution::{Budget, Engine, PlanStatus, PlanStore, Settings};
    let start = Instant::now();
    let rt = BenchRuntime { env, v, r: Mutex::new(BenchResult::default()) };
    let store = match env.evolution.rt.block_on(PlanStore::in_memory()) {
        Ok(store) => store,
        Err(e) => {
            t.notes.push(format!("  benchmark plan store: {e:#}"));
            return (BenchResult { errors: 1, ..Default::default() }, None);
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
    let answer = answer.filter(|a| !a.trim().is_empty());
    if answer.is_none() {
        r.errors += 1;
    }
    (r, answer)
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
    let mut done = Done::default();
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
    if let Change::Api { api } = c.change.clone() {
        return deploy_api(env, c, &api);
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
        // Skills follow the learning mode: off means nothing changes them.
        if learning.mode() == lyra_learning::Mode::Off {
            return Err("learning is off ([learning] mode): skills aren't changed".into());
        }
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
    // A trial rolled back by hand ends as lost.
    if matches!(env.evolution.manager.trial(), Ok(Some(_))) {
        let _ = env.evolution.manager.end_trial(lyra_evolution::TrialStatus::Lost, "rolled back by hand");
    }
    let to = arg.trim().trim_start_matches('g');
    let to = if to.is_empty() { Ok(None) } else { to.parse::<u32>().map(Some).map_err(|_| format!("not a generation number: {arg}")) };
    let notes = match to.and_then(|to| rollback_to(env, to, "requested by the user")) {
        Ok(notes) => notes,
        Err(e) => vec![format!("✗ {e}")],
    };
    Done { notes, ..Default::default() }
}

/// Test every open candidate in a group on the same tasks and rank them.
pub fn compare(env: &Env, key: &str) -> Done {
    let mut done = Done::default();
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

// ---- APIs proposed from a URL (approved by an admin, then live)

/// How a service's description says to sign in: (header, scheme), or why
/// lyra can't (OAuth needs a person in a browser).
fn sign_in(spec: &Value) -> Result<Option<(String, String)>, String> {
    let schemes = spec["components"]["securitySchemes"].as_object().or_else(|| spec["securityDefinitions"].as_object());
    let Some(schemes) = schemes.filter(|s| !s.is_empty()) else { return Ok(None) };
    for s in schemes.values() {
        match (s["type"].as_str().unwrap_or(""), s["scheme"].as_str().unwrap_or("").to_lowercase().as_str()) {
            ("http", "bearer") => return Ok(Some(("Authorization".into(), "Bearer".into()))),
            ("http", "basic") | ("basic", _) => return Ok(Some(("Authorization".into(), "Basic".into()))),
            ("apiKey", _) if s["in"] == "header" => return Ok(Some((s["name"].as_str().unwrap_or("X-API-Key").to_string(), String::new()))),
            _ => {}
        }
    }
    Err("it signs in a way lyra can't use on its own (OAuth or a key in the address): add it to config.toml by hand if you still want it".into())
}

/// A lyra name for the service: the one asked for, else its title's.
fn api_name(asked: &str, title: &str, taken: &[String]) -> Result<String, String> {
    let from = if asked.trim().is_empty() { title } else { asked };
    let base: String = from.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let base = base.split('-').filter(|p| !p.is_empty()).take(3).collect::<Vec<_>>().join("-");
    if base.is_empty() {
        return Err("give it a name: /evolve api <url> <name>".into());
    }
    if taken.contains(&base) {
        return Err(format!("there's already an API called {base} (give another name)"));
    }
    Ok(base)
}

/// The provider's entry in config.toml, as TOML (also what it's checked with).
fn api_toml(api: &lyra_evolution::ApiProvider) -> String {
    let mut t = toml_edit::Table::new();
    t["name"] = toml_edit::value(api.name.clone());
    t["spec"] = toml_edit::value(api.spec.clone());
    if let Some(b) = &api.base_url {
        t["base_url"] = toml_edit::value(b.clone());
    }
    if let Some(s) = &api.auth_secret {
        t["auth_secret"] = toml_edit::value(s.clone());
    }
    if let Some(h) = &api.auth_header {
        t["auth_header"] = toml_edit::value(h.clone());
    }
    if let Some(s) = &api.auth_scheme {
        t["auth_scheme"] = toml_edit::value(s.clone());
    }
    t.to_string()
}

fn api_config(api: &lyra_evolution::ApiProvider) -> Result<lyra_capabilities::openapi::OpenApiConfig, String> {
    toml::from_str(&api_toml(api)).map_err(|e| format!("its settings don't read: {e}"))
}

/// Read the service's OpenAPI description at `url` and propose it as an
/// API candidate (an admin approves it: /evolve approve, or the app's
/// Proposals page). Nothing is added until then; its writes will still ask.
pub fn propose_api(ev: &Evolution, caps: Option<&crate::caps::Caps>, url: &str, name: &str, why: &str) -> Result<String, String> {
    let url = url.trim();
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("give the address of its OpenAPI description (https://…/openapi.json)".into());
    }
    let taken = caps.map(|c| c.openapi_names()).unwrap_or_default();
    // A first look, under a scratch name, to read its title and sign-in.
    let probe = lyra_evolution::ApiProvider { name: format!("proposed-{}", &Uuid::new_v4().simple().to_string()[..6]), spec: url.into(), base_url: None, auth_secret: None, auth_header: None, auth_scheme: None, title: String::new(), reads: 0, writes: 0, sample: Vec::new() };
    let (client, text) = crate::caps::Caps::try_openapi(api_config(&probe)?)?;
    let spec: Value = serde_json::from_str(&text).or_else(|_| serde_yaml_ng::from_str(&text)).map_err(|e| format!("its description doesn't read: {e}"))?;
    let title = spec["info"]["title"].as_str().unwrap_or("").trim().to_string();
    let name = api_name(name, &title, &taken)?;
    let auth = sign_in(&spec)?;
    let caps_found = client.capabilities();
    if caps_found.is_empty() {
        return Err("its description has no operations lyra can use".into());
    }
    let writes = caps_found.iter().filter(|c| c.risk != lyra_capabilities::RiskLevel::ReadOnly).count();
    let api = lyra_evolution::ApiProvider {
        auth_secret: auth.as_ref().map(|_| format!("api-{name}")),
        auth_header: auth.as_ref().map(|(h, _)| h.clone()),
        auth_scheme: auth.as_ref().map(|(_, s)| s.clone()),
        name,
        spec: url.into(),
        base_url: None,
        title,
        reads: caps_found.len() - writes,
        writes,
        sample: Vec::new(),
    };
    // Its operations under the name it'll have (they were read under a scratch one).
    let mut api = api;
    api.sample = caps_found
        .iter()
        .take(8)
        .map(|c| format!("{}.{} — {}", api.name, c.id.split_once('.').map_or(c.id.as_str(), |(_, op)| op), c.description.chars().take(80).collect::<String>()))
        .collect();
    let op = lyra_evolution::Opportunity {
        kind: "api_proposed".into(),
        problem: if why.trim().is_empty() { format!("lyra could use {}", if api.title.is_empty() { &api.name } else { &api.title }) } else { why.trim().to_string() },
        categories: vec![lyra_evolution::Category::Tool],
        evidence: Vec::new(),
        details: json!({ "url": url }),
    };
    let proposal = lyra_evolution::evolver::Proposal { change: lyra_evolution::Change::Api { api: api.clone() }, rationale: format!("Its OpenAPI description at {url} gives {} operations.", api.reads + api.writes), confidence: 0.7 };
    let c = ev.manager.propose(&op, vec![proposal])?.into_iter().next().ok_or("nothing was proposed")?;
    // Checked already (it read, it has operations): ready for an admin's yes.
    let v = Validation { valid: true, notes: vec![format!("read {} operations from {url}", api.reads + api.writes)], fitness_before: None, fitness_after: None, checks: Vec::new(), comparison: None };
    let c = ev.manager.save_validation(c, v)?;
    let key = api.auth_secret.as_ref().map(|s| format!(" It needs a key: lyra secret {s} on the server (before or after approving)."), ).unwrap_or_default();
    Ok(format!(
        "Proposed API {} ({}): {} read and {} write operations. Candidate {}: an admin approves it with /evolve approve {} or on the Proposals page; until then nothing is added, and its writes will still ask each time.{key}",
        api.name,
        if api.title.is_empty() { url } else { &api.title },
        api.reads,
        api.writes,
        c.short(),
        c.short()
    ))
}

/// An approved API: into config.toml (`[[capabilities.openapi]]`) and live now.
fn deploy_api(env: &Env, c: Candidate, api: &lyra_evolution::ApiProvider) -> Result<String, String> {
    let caps = env.caps.as_ref().ok_or("capabilities are off")?;
    if caps.openapi_names().contains(&api.name) {
        return Err(format!("there's already an API called {}", api.name));
    }
    let n = caps.add_openapi(api_config(api)?)?;
    let path = crate::config::path().ok_or("no home directory")?;
    let entry: toml_edit::Table = api_toml(api).parse::<toml_edit::DocumentMut>().map_err(|e| e.to_string())?.as_table().clone();
    crate::config::update_file(&path, |doc| {
        let caps = doc.entry("capabilities").or_insert(toml_edit::table()).as_table_mut().ok_or("[capabilities] in config.toml isn't a table")?;
        let list = caps.entry("openapi").or_insert(toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()));
        list.as_array_of_tables_mut().ok_or("capabilities.openapi in config.toml isn't a list of tables")?.push(entry);
        Ok(())
    })?;
    caps.refresh();
    let key = api.auth_secret.as_ref().filter(|s| crate::secrets::token(s).is_none()).map(|s| format!(" It needs its key before it works: lyra secret {s} on the server, then /reload."));
    env.evolution.manager.mark_deployed_elsewhere(c, &format!("added to config.toml as [[capabilities.openapi]] {}", api.name))?;
    Ok(format!("✓ added API {}: {n} operations, live now (its writes ask each time). It's in config.toml under [[capabilities.openapi]]; remove that entry to take it away.{}", api.name, key.unwrap_or_default()))
}

/// `/evolve api <url> [name]`.
pub fn propose_api_command(env: &Env, rest: &str) -> Done {
    let mut done = Done::default();
    let (url, name) = rest.trim().split_once(' ').map_or((rest.trim(), ""), |(u, n)| (u, n.trim()));
    done.notes.push(match propose_api(&env.evolution, env.caps.as_deref(), url, name, "") {
        Ok(t) => t,
        Err(e) => format!("✗ couldn't propose it: {e}"),
    });
    done
}

/// Ask the model for a source patch for a problem: it picks files, then
/// writes a diff against the current commit. The patch is only stored as a
/// candidate; `/evolve test` checks it in a sandbox.
pub fn propose_code(env: &Env, problem: &str) -> Done {
    let mut done = Done::default();
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
/evolve code <problem>       propose a source patch (needs [evolution] source_repo)
/evolve trial · keep         the change on trial and how it's doing · keep it now
/evolve tests [add | remove <id>]  lyra's tests (from replies rated 👍) · add the last reply · remove one";

/// The change on trial, side by side with the generation before it.
pub fn trial_text(ev: &Evolution) -> Result<String, String> {
    let Some(s) = ev.manager.trial_standing()? else { return Ok("nothing is on trial".into()) };
    let rate = |a: &lyra_evolution::Arm| a.success_rate().map_or("—".into(), |r| format!("{:.0}%", r * 100.0));
    Ok(format!(
        "generation {} is on trial on {:.0}% of turns since {}\n  on trial: {} turns, {} with an outcome, {} good · corrected {}\n  generation {}: {} turns, {} with an outcome, {} good · corrected {}\n  decided once it has {} turns with an outcome (and the other side half that) · /evolve keep keeps it now · /evolve rollback ends it",
        s.generation,
        s.share * 100.0,
        s.started.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M"),
        s.trial.runs, s.trial.judged, rate(&s.trial), s.trial.corrected,
        s.parent, s.control.runs, s.control.judged, rate(&s.control), s.control.corrected,
        s.needed
    ))
}

/// Keep the change on trial now (an admin's call).
pub fn keep(env: &Env) -> Result<String, String> {
    let ev = &env.evolution;
    let g = ev.manager.end_trial(lyra_evolution::TrialStatus::Kept, "kept by hand before its trial ended")?;
    let notes = ev.reload(env.tools.as_deref());
    Ok(format!("✓ generation {} is kept{}", g.number, if notes.is_empty() { String::new() } else { format!("\n{}", notes.join("\n")) }))
}

/// lyra's tests, newest first.
pub fn tests_text(ev: &Evolution) -> String {
    let (tests, errors) = lyra_evolution::golden::list(ev.manager.home());
    let mut out = vec![format!("{} test(s) in {} (benchmarks use up to {} tasks, {} answer(s) each side)", tests.len(), crate::context::show(&lyra_evolution::golden::dir(ev.manager.home())), ev.benchmark_tasks, ev.benchmark_samples)];
    for t in tests.iter().take(30) {
        out.push(format!("  {} [{}] {}{}", t.id, t.source, truncate(&t.task, 80), if t.history.is_empty() { "" } else { " (a follow-up)" }));
    }
    out.extend(errors.into_iter().map(|e| format!("  ✗ {e}")));
    if tests.is_empty() {
        out.push("  none yet: rate replies 👍 (the owner's become tests), or /evolve tests add".into());
    }
    out.join("\n")
}

pub fn remove_test(ev: &Evolution, id: &str) -> Result<String, String> {
    match lyra_evolution::golden::remove(ev.manager.home(), id) {
        Ok(true) => Ok(format!("removed test {id}")),
        Ok(false) => Err(format!("no single test matches {id:?}: /evolve tests lists them")),
        Err(e) => Err(format!("couldn't remove it: {e}")),
    }
}

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
    out.push(format!("evolved: {} guideline(s) · {} workflow(s) · {} composite tool(s) · {} test(s)", s.guidelines, s.workflows, s.tools, s.tests));
    if let Some(t) = &s.trial {
        out.push(format!("on trial: {t} (/evolve trial)"));
    }
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
        Change::Api { api } => {
            out.push(api_toml(api));
            out.extend(api.sample.iter().map(|s| format!("  {s}")));
        }
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
        out.push(format!("  [{} {}] {}", r.kind, r.outcome, if r.person.is_some() { "(another person's request)".to_string() } else { truncate(&r.task, 80) }));
    }
    if let Some(v) = &c.validation {
        out.push(format!("validation: {}", if v.valid { "passed checks" } else { "failed checks" }));
        out.extend(v.notes.iter().map(|n| format!("  {n}")));
        if let Some(cmp) = &v.comparison {
            out.push(format!("  judged pair by pair by {}: {} better · {} worse · {} as good over {} task(s) × {}", cmp.judge, cmp.wins, cmp.losses, cmp.ties, cmp.tasks, cmp.samples));
        }
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
                if r.person.is_some() { "(another person's request)".to_string() } else { truncate(&r.task, 60) }
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

    /// The pairwise benchmark against real models: answers from one, judged
    /// by another (the fallback). Opt in with `LYRA_LIVE_URL=… LYRA_LIVE_MODEL=…
    /// LYRA_LIVE_JUDGE_URL=… LYRA_LIVE_JUDGE_MODEL=… cargo test live_pairwise_benchmark -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_pairwise_benchmark() {
        let var = |k: &str| std::env::var(k).ok();
        let (Some(url), Some(model), Some(judge_url), Some(judge)) = (var("LYRA_LIVE_URL"), var("LYRA_LIVE_MODEL"), var("LYRA_LIVE_JUDGE_URL"), var("LYRA_LIVE_JUDGE_MODEL")) else { return };
        crate::learn::configure(crate::learn::Structured { max_tokens: 2048, thinking: false });
        crate::fallback::configure(Some(crate::fallback::Settings { url: judge_url, model: judge.clone(), background: false, api_key: String::new(), api_key_secret: String::new(), price: Default::default() }), None);
        crate::config::with_test_home(|home| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let manager = EvolutionManager::open(home, rt.handle().clone(), Default::default()).unwrap();
            let ev = Arc::new(Evolution::new(manager, rt.handle().clone(), None, 2).with_samples(1));
            add_test(&ev, "Explain in a few sentences why backups should be tested by restoring them.", None, vec![], "added").unwrap();
            add_test(&ev, "List three things to check before restarting a production database server.", None, vec![], "added").unwrap();
            let op = lyra_evolution::Opportunity { kind: "k".into(), problem: "answers are too long".into(), categories: vec![Category::Prompt], evidence: vec![], details: json!({}) };
            let change = Change::Prompt { add: vec!["Answer every request in exactly one word, even when more is needed.".into()], remove: vec![] };
            let c = ev.manager.propose(&op, vec![evolver::Proposal { change, rationale: "shorter".into(), confidence: 0.5 }]).unwrap().remove(0);
            let env = Env { url: format!("{}/chat/completions", url.trim_end_matches('/')), model, evolution: ev, tools: None, learning: None, caps: None, goals: None, system_prompt: None };
            let t = test(&env, c);
            for n in &t.notes {
                println!("{n}");
            }
            let cmp = t.candidate.expect("tested").validation.expect("validated").comparison.expect("judged pair by pair");
            assert_eq!(cmp.judge, judge, "the fallback judged, not the model under test");
            assert_eq!(cmp.wins + cmp.losses + cmp.ties, 2, "{cmp:?}");
            assert!(!cmp.passes, "one-word answers shouldn't win: {cmp:?}");
        });
        crate::fallback::configure(None, None);
    }

    /// A tiny web server answering every request with `body`.
    fn serve(body: &'static str) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/openapi.json", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        url
    }

    /// A chat model answering every request with `answer(request)`; the requests are kept.
    fn fake_model(answer: impl Fn(&Value) -> String + Send + Sync + 'static) -> (String, Arc<Mutex<Vec<Value>>>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1/chat/completions", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let kept = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut len = 0;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some(v) = line.to_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0u8; len];
                let _ = reader.read_exact(&mut body);
                let request: Value = serde_json::from_slice(&body).unwrap_or(json!({}));
                let content = answer(&request);
                kept.lock().unwrap().push(request);
                let reply = json!({ "choices": [{ "message": { "role": "assistant", "content": content } }], "usage": { "prompt_tokens": 10, "completion_tokens": 5 } }).to_string();
                let mut stream = stream;
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len());
            }
        });
        (url, seen)
    }

    #[test]
    fn a_change_passes_only_when_blind_pairs_show_it_better_beyond_the_noise() {
        crate::config::with_test_home(|home| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let manager = EvolutionManager::open(home, rt.handle().clone(), Default::default()).unwrap();
            let ev = Arc::new(Evolution::new(manager, rt.handle().clone(), None, 4).with_samples(2));
            // The candidate's guideline makes the answers careful; the judge prefers careful ones.
            let (url, seen) = fake_model(|req| {
                let system = req["messages"][0]["content"].as_str().unwrap_or("");
                if system.starts_with("You compare two answers") {
                    let user = req["messages"][1]["content"].as_str().unwrap_or("");
                    let second = user.split("Answer 2:").nth(1).unwrap_or("");
                    let first = user.split("Answer 1:").nth(1).unwrap_or("").split("Answer 2:").next().unwrap_or("");
                    return match (first.contains("CAREFUL"), second.contains("CAREFUL")) {
                        (true, false) => r#"{"better": "1", "reason": "checks the dates"}"#,
                        (false, true) => r#"{"better": "2", "reason": "checks the dates"}"#,
                        _ => r#"{"better": "tie", "reason": "the same"}"#,
                    }
                    .into();
                }
                if system.contains("Double-check the dates.") { "CAREFUL: the meeting is Tuesday".into() } else { "The meeting is Monday".into() }
            });
            // The test set: replies the owner liked (one a follow-up).
            assert!(add_test(&ev, "When is the planning meeting this week?", Some("Tuesday at 10."), vec![], "rated").unwrap());
            assert!(add_test(&ev, "And the budget review?", Some("Thursday."), vec![("user".into(), "When is the planning meeting?".into()), ("assistant".into(), "Tuesday.".into())], "rated").unwrap());
            assert!(add_test(&ev, "What's due before Friday's deadline?", None, vec![], "added").unwrap());
            assert!(!add_test(&ev, "what's due before friday's deadline?", None, vec![], "added").unwrap(), "once");
            assert!(tests_text(&ev).contains("3 test(s)"));
            let op = lyra_evolution::Opportunity { kind: "frequent_corrections".into(), problem: "dates are wrong".into(), categories: vec![Category::Prompt], evidence: vec![], details: json!({}) };
            let env = Env { url, model: "m".into(), evolution: ev.clone(), tools: None, learning: None, caps: None, goals: None, system_prompt: None };
            let propose = |add: &str| ev.manager.propose(&op, vec![evolver::Proposal { change: Change::Prompt { add: vec![add.into()], remove: vec![] }, rationale: "r".into(), confidence: 0.6 }]).unwrap().remove(0);

            let t = test(&env, propose("Double-check the dates."));
            let v = t.candidate.expect("tested").validation.expect("validated");
            let cmp = v.comparison.clone().expect("judged pair by pair");
            assert_eq!((cmp.tasks, cmp.samples, cmp.wins, cmp.losses), (3, 2, 6, 0), "{:?}", t.notes);
            assert!(cmp.passes && v.improves(), "{}", cmp.note);
            assert!(t.notes.iter().any(|n| n.contains("6 better · 0 worse")), "{:?}", t.notes);
            // The follow-up was asked after what it follows.
            let asked = seen.lock().unwrap().iter().any(|r| r["messages"].as_array().is_some_and(|m| m.len() == 4 && m[1]["content"] == "When is the planning meeting?" && m[3]["content"] == "And the budget review?"));
            assert!(asked, "the earlier turns were replayed");

            // A change that doesn't change the answers: as good on every pair, not better.
            let t = test(&env, propose("Use British spelling."));
            let v = t.candidate.expect("tested").validation.expect("validated");
            assert!(!v.improves(), "{}", v.comparison.as_ref().unwrap().note);
            assert_eq!(v.comparison.as_ref().map(|c| c.ties), Some(6));
        });
    }

    #[test]
    fn a_scheduled_review_tests_what_it_proposes_and_says_whats_ready_then_doesnt_repeat_it() {
        crate::config::with_test_home(|home| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let settings = lyra_evolution::Settings { trial_share: 0.0, ..Default::default() };
            let manager = EvolutionManager::open(home, rt.handle().clone(), settings).unwrap();
            let ev = Arc::new(Evolution::new(manager, rt.handle().clone(), None, 3).with_samples(1));
            let (url, _seen) = fake_model(|req| {
                let system = req["messages"][0]["content"].as_str().unwrap_or("");
                if system.starts_with("You improve an AI assistant") {
                    return r#"{"candidates": [{"category": "prompt", "add": ["When asked for one line, answer in one short sentence."], "rationale": "they keep asking for shorter", "confidence": 0.8}]}"#.into();
                }
                if system.starts_with("You compare two answers") {
                    let user = req["messages"][1]["content"].as_str().unwrap_or("");
                    let first = user.split("Answer 1:").nth(1).unwrap_or("").split("Answer 2:").next().unwrap_or("");
                    return if first.contains("SHORT") { r#"{"better": "1"}"# } else { r#"{"better": "2"}"# }.into();
                }
                if system.contains("one short sentence") { "SHORT.".into() } else { "A very long answer that goes on and on.".into() }
            });
            // Four replies corrected for being too long.
            for q in ["In one line: what is DNS?", "In one line: what is NTP?", "In one line: what is TLS?", "In one line: what is SSH?"] {
                let mut r = RunRecord::new(lyra_evolution::RunKind::Chat, q, 1);
                r.outcome = lyra_evolution::RunOutcome::Failure;
                r.corrected = true;
                r.feedback = Some("too long".into());
                ev.manager.record_run(&r).unwrap();
            }
            let env = Env { url, model: "m".into(), evolution: ev.clone(), tools: None, learning: None, caps: None, goals: None, system_prompt: None };
            let done = scheduled_review(&env);
            assert!(done.notes.iter().any(|n| n.contains("benchmarking 3 task(s)")), "tested in propose mode: {:?}", done.notes);
            let digest = done.digest.expect("a digest for the owner");
            assert!(digest.starts_with("1 change(s) beat the current lyra and are ready to approve"), "{digest}");
            assert!(digest.contains("one short sentence"), "{digest}");
            // The next review knows it's already proposed.
            let again = review(&env);
            assert!(again.notes.iter().any(|n| n.contains("already proposed for it")), "{:?}", again.notes);
            assert_eq!(ev.manager.candidates(10).unwrap().len(), 1, "not proposed twice");
        });
    }

    #[test]
    fn a_problem_is_known_again_whatever_its_numbers_and_isnt_reproposed() {
        assert_eq!(fingerprint("4 of 4 recent answers were corrected"), fingerprint("5 of 6 recent answers were corrected"));
        assert_eq!(fingerprint("capability web_fetch takes 30.5s on average over 184 uses"), fingerprint("capability web_fetch takes 12.1s on average over 200 uses"));
        assert_ne!(fingerprint("capability web_fetch is slow"), fingerprint("capability files_read is slow"));
        crate::config::with_test_home(|home| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let manager = EvolutionManager::open(home, rt.handle().clone(), Default::default()).unwrap();
            let op = lyra_evolution::Opportunity { kind: "k".into(), problem: "4 of 4 recent answers were corrected".into(), categories: vec![Category::Prompt], evidence: vec![], details: json!({}) };
            let c = manager.propose(&op, vec![evolver::Proposal { change: Change::Prompt { add: vec!["x".into()], remove: vec![] }, rationale: "r".into(), confidence: 0.6 }]).unwrap().remove(0);
            let past = manager.candidates(10).unwrap();
            assert_eq!(attempts(&past, "6 of 7 recent answers were corrected").len(), 1);
            manager.reject(c, "no").unwrap();
            let past = manager.candidates(10).unwrap();
            assert_eq!(attempts(&past, "6 of 7 recent answers were corrected")[0].status, CandidateStatus::Rejected);
        });
    }

    #[test]
    fn a_deployed_change_runs_on_trial_beside_the_old_one_and_its_results_decide() {
        crate::config::with_test_home(|home| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let settings = lyra_evolution::Settings { trial_runs: 4, ..Default::default() };
            let manager = EvolutionManager::open(home, rt.handle().clone(), settings).unwrap();
            let ev = Arc::new(Evolution::new(manager, rt.handle().clone(), None, 1));
            let env = Env { url: String::new(), model: String::new(), evolution: ev.clone(), tools: None, learning: None, caps: None, goals: None, system_prompt: None };
            let op = lyra_evolution::Opportunity { kind: "k".into(), problem: "p".into(), categories: vec![Category::Prompt], evidence: vec![], details: json!({}) };
            let deploy_one = |add: &str| {
                let c = ev.manager.propose(&op, vec![evolver::Proposal { change: Change::Prompt { add: vec![add.into()], remove: vec![] }, rationale: "r".into(), confidence: 0.6 }]).unwrap().remove(0);
                let mut done = Done::default();
                deploy(&env, c, "approved", &mut done).unwrap()
            };
            assert_eq!(ev.arm(Uuid::new_v4()), None, "nothing on trial");
            deploy_one("Cite the source of each fact.");
            // Each turn is on one side, and sees that side's behavior.
            let ids: Vec<Uuid> = (0..400).map(|_| Uuid::new_v4()).collect();
            let on_trial: Vec<&Uuid> = ids.iter().filter(|r| ev.arm(**r) == Some(true)).collect();
            let share = on_trial.len() as f32 / ids.len() as f32;
            assert!((0.2..0.4).contains(&share), "about 30% on trial: {share}");
            let control = ids.iter().find(|r| ev.arm(**r) == Some(false)).unwrap();
            assert!(ev.chat_section_for("hi", *on_trial[0]).is_some_and(|s| s.contains("Cite the source")));
            assert!(ev.chat_section_for("hi", *control).is_none(), "the other turns keep the old behavior");
            assert_eq!(ev.arm(*on_trial[0]), ev.arm(*on_trial[0]), "the same run stays on its side");
            // Its turns did better: kept, and everyone gets it.
            for (arm, good) in [(true, true), (true, true), (true, true), (true, false), (false, true), (false, false), (false, false), (false, false), (false, true)] {
                let mut r = RunRecord::new(lyra_evolution::RunKind::Chat, "a request", 0);
                r.outcome = if good { lyra_evolution::RunOutcome::Success } else { lyra_evolution::RunOutcome::Failure };
                let id = ev.record(r, Some(arm)).unwrap();
                assert_eq!(ev.manager.runs(1).unwrap()[0].generation, if arm { 2 } else { 1 }, "{id}");
            }
            let notes = monitor(&env);
            assert!(notes[0].starts_with("✓ generation 2 won its trial"), "{notes:?}");
            assert!(ev.chat_section_for("hi", *control).is_some_and(|s| s.contains("Cite the source")), "kept for everyone");
            assert!(trial_text(&ev).unwrap().contains("nothing is on trial"));
            // The next one loses its trial: rolled back.
            deploy_one("Answer in one word.");
            assert!(ev.snapshot().unwrap().trial.is_some_and(|t| t.starts_with("g3 on 30%")));
            for (arm, good) in [(true, false), (true, false), (true, false), (true, true), (false, true), (false, true), (false, true), (false, true), (false, false)] {
                let mut r = RunRecord::new(lyra_evolution::RunKind::Chat, "a request", 0);
                r.outcome = if good { lyra_evolution::RunOutcome::Success } else { lyra_evolution::RunOutcome::Failure };
                ev.record(r, Some(arm)).unwrap();
            }
            let notes = monitor(&env);
            assert!(notes[0].starts_with("↩ generation 3 lost its trial"), "{notes:?}");
            assert!(ev.chat_section_for("hi", *control).is_some_and(|s| !s.contains("one word")), "rolled back");
            assert_eq!(ev.arm(*control), None);
        });
    }

    const PARCELS: &str = r#"{"openapi":"3.0.0","info":{"title":"Parcel Tracker","version":"1"},"servers":[{"url":"https://parcels.example/v1"}],
        "paths":{"/parcels/{id}":{"get":{"operationId":"getParcel","summary":"A parcel's status","parameters":[{"name":"id","in":"path","required":true,"schema":{"type":"string"}}],"responses":{"200":{"description":"ok"}}}},
                 "/parcels":{"post":{"operationId":"createParcel","summary":"Send a parcel","responses":{"201":{"description":"made"}}}}},
        "components":{"securitySchemes":{"token":{"type":"http","scheme":"bearer"}}}}"#;

    #[test]
    fn an_api_is_proposed_from_its_description_then_approved_into_place() {
        crate::config::with_test_home(|home| {
            let rt = tokio::runtime::Runtime::new().unwrap();
            let manager = EvolutionManager::open(&home.join("evolution"), rt.handle().clone(), Default::default()).unwrap();
            let ev = Arc::new(Evolution::new(manager, rt.handle().clone(), None, 1));
            let caps_manager = rt.block_on(lyra_capabilities::CapabilityManager::open(&home.join("capabilities"), Default::default())).unwrap();
            let caps = Arc::new(crate::caps::Caps::new(caps_manager, rt.handle().clone(), Vec::new(), Vec::new()));
            let url = serve(PARCELS);
            // Proposed: read, named from its title, its sign-in found; nothing added yet.
            let said = propose_api(&ev, Some(&caps), &url, "", "track parcels").unwrap();
            assert!(said.contains("Proposed API parcel-tracker") && said.contains("1 read and 1 write") && said.contains("lyra secret api-parcel-tracker"), "{said}");
            assert!(caps.openapi_names().is_empty(), "not until approved");
            let c = ev.manager.candidates(5).unwrap().remove(0);
            let Change::Api { api } = &c.change else { panic!("an API candidate") };
            assert_eq!((api.auth_header.as_deref(), api.auth_scheme.as_deref(), api.reads, api.writes), (Some("Authorization"), Some("Bearer"), 1, 1));
            assert!(c.validation.as_ref().is_some_and(|v| v.valid), "checked when proposed");
            assert!(api.sample.iter().all(|s| s.starts_with("parcel-tracker.")), "under its own name: {:?}", api.sample);
            // Approved: live now, and kept in config.toml.
            let env = Env { url: String::new(), model: String::new(), evolution: ev.clone(), tools: None, learning: None, caps: Some(caps.clone()), goals: None, system_prompt: None };
            let mut done = Done::default();
            let out = approve_inner(&env, &c.short(), &mut done).unwrap();
            assert!(out.contains("added API parcel-tracker: 2 operations") && out.contains("needs its key"), "{out}");
            assert_eq!(caps.openapi_names(), ["parcel-tracker"]);
            assert!(caps.manager.get("parcel-tracker.createParcel").is_some_and(|c| c.metadata.requires_approval), "its writes still ask");
            let config = std::fs::read_to_string(crate::config::path().unwrap()).unwrap();
            assert!(config.contains("[[capabilities.openapi]]") && config.contains("name = \"parcel-tracker\"") && config.contains("auth_secret = \"api-parcel-tracker\""), "{config}");
            assert_eq!(ev.manager.find(&c.short()).unwrap().status, CandidateStatus::Deployed);
            // The same name again: refused; and a description that signs in with OAuth.
            assert!(propose_api(&ev, Some(&caps), &url, "", "").unwrap_err().contains("already an API called parcel-tracker"));
            assert!(propose_api(&ev, Some(&caps), "ftp://x", "", "").is_err());
            assert!(sign_in(&json!({ "components": { "securitySchemes": { "o": { "type": "oauth2" } } } })).unwrap_err().contains("OAuth"));
            assert_eq!(sign_in(&json!({ "components": { "securitySchemes": { "k": { "type": "apiKey", "in": "header", "name": "X-Key" } } } })).unwrap(), Some(("X-Key".to_string(), String::new())));
            assert_eq!(sign_in(&json!({})).unwrap(), None);
        });
    }
}

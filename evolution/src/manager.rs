//! The evolution manager: telemetry in, candidates out, and the only way the
//! agent's evolvable state changes. Every deployment is a new generation with
//! a full snapshot of what it replaced, so any generation can be restored.
//!
//! Evolvable state lives in the lyra home as plain files:
//! `config/behavior.toml`, `workflows/<name>.toml`, `tools/<name>.toml`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::Utc;
use serde::Deserialize;
use tokio::runtime::Handle;
use uuid::Uuid;

use crate::behavior::Behavior;
use crate::composite::CompositeTool;
use crate::detect::{self, SkillHealth, Thresholds};
use crate::evolver::Proposal;
use crate::fitness;
use crate::model::*;
use crate::store::EvolutionStore;
use crate::workflow::WorkflowDef;

/// How much evolution may do on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Record telemetry only.
    Off,
    /// Find problems and propose changes; nothing deploys without approval (the default).
    #[default]
    Propose,
    /// Prompt and behavior changes that beat the baseline deploy on their own;
    /// everything else is still proposed. Regressions roll back on their own.
    Auto,
}

/// `[evolution]` in the config.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub mode: Mode,
    /// Recent runs the detectors look at.
    pub window: usize,
    pub thresholds: Thresholds,
    pub fitness: FitnessWeights,
    /// Known outcomes needed before the monitor judges a generation.
    pub monitor_runs: usize,
    /// A drop in success rate this large counts as a regression.
    pub monitor_drop: f32,
    /// Most problems one review proposes candidates for.
    pub max_problems: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::Propose,
            window: 50,
            thresholds: Thresholds::default(),
            fitness: FitnessWeights::default(),
            monitor_runs: 10,
            monitor_drop: 0.15,
            max_problems: 3,
        }
    }
}

/// Telemetry at a glance.
#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub runs: usize,
    /// Runs whose outcome is known.
    pub judged: usize,
    pub success_rate: Option<f32>,
    pub corrections: usize,
    pub avg_tool_calls: f32,
    pub avg_model_calls: f32,
    pub pending: usize,
    pub generation: u32,
}

pub struct EvolutionManager {
    store: EvolutionStore,
    rt: Handle,
    home: PathBuf,
    pub settings: Settings,
}

impl EvolutionManager {
    /// Open the store in `<home>/evolution/evolution.db` and make sure there
    /// is a first generation describing the agent as it is.
    pub fn open(home: &Path, rt: Handle, settings: Settings) -> Result<Self> {
        let store = rt.block_on(EvolutionStore::open(&home.join("evolution").join("evolution.db")))?;
        Self::with_store(store, home, rt, settings)
    }

    pub fn with_store(store: EvolutionStore, home: &Path, rt: Handle, settings: Settings) -> Result<Self> {
        let m = Self { store, rt, home: home.to_path_buf(), settings };
        if m.db(m.store.generations()).map_err(anyhow::Error::msg)?.is_empty() {
            let snapshot = m.snapshot();
            m.add_generation(snapshot, None, None, "the agent as it was when evolution started").map_err(anyhow::Error::msg)?;
        }
        Ok(m)
    }

    fn db<T>(&self, f: impl Future<Output = Result<T>>) -> Result<T, String> {
        self.rt.block_on(f).map_err(|e| format!("{e:#}"))
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    // ---- the evolvable state

    fn behavior_path(&self) -> PathBuf {
        self.home.join("config").join("behavior.toml")
    }

    fn dir(&self, name: &str) -> PathBuf {
        self.home.join(name)
    }

    /// The current behavior settings (defaults when there's no file).
    pub fn behavior(&self) -> Result<Behavior, String> {
        match std::fs::read_to_string(self.behavior_path()) {
            Ok(text) => Behavior::parse(&text),
            Err(_) => Ok(Behavior::default()),
        }
    }

    /// Valid workflow definitions, and errors for the rest.
    pub fn workflows(&self) -> (Vec<WorkflowDef>, Vec<String>) {
        read_dir(&self.dir("workflows"), WorkflowDef::parse)
    }

    /// Composite tool definitions (validated against the tools by the caller), and parse errors.
    pub fn composites(&self) -> (Vec<CompositeTool>, Vec<String>) {
        read_dir(&self.dir("tools"), CompositeTool::parse)
    }

    /// What's on disk now.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            behavior: std::fs::read_to_string(self.behavior_path()).unwrap_or_default(),
            workflows: read_texts(&self.dir("workflows")),
            tools: read_texts(&self.dir("tools")),
        }
    }

    fn write_snapshot(&self, s: &Snapshot) -> Result<(), String> {
        let io = |e: std::io::Error| e.to_string();
        if s.behavior.trim().is_empty() {
            let _ = std::fs::remove_file(self.behavior_path());
        } else {
            std::fs::create_dir_all(self.home.join("config")).map_err(io)?;
            std::fs::write(self.behavior_path(), &s.behavior).map_err(io)?;
        }
        for (dir, files) in [("workflows", &s.workflows), ("tools", &s.tools)] {
            let dir = self.dir(dir);
            std::fs::create_dir_all(&dir).map_err(io)?;
            for (name, _) in read_texts(&dir) {
                if !files.contains_key(&name) {
                    std::fs::remove_file(dir.join(format!("{name}.toml"))).map_err(io)?;
                }
            }
            for (name, text) in files {
                std::fs::write(dir.join(format!("{name}.toml")), text).map_err(io)?;
            }
        }
        Ok(())
    }

    // ---- generations

    pub fn generations(&self) -> Result<Vec<Generation>, String> {
        self.db(self.store.generations())
    }

    pub fn generation(&self) -> Result<Generation, String> {
        self.generations()?.into_iter().next().ok_or("no generation yet".into())
    }

    fn add_generation(&self, snapshot: Snapshot, parent: Option<&Generation>, candidate: Option<Uuid>, reason: &str) -> Result<Generation, String> {
        let g = Generation {
            id: Uuid::new_v4(),
            number: parent.map_or(1, |p| p.number + 1),
            parent: parent.map(|p| p.id),
            snapshot,
            candidate,
            reason: reason.to_string(),
            created_at: Utc::now(),
            skill: None,
        };
        self.db(self.store.add_generation(&g))?;
        Ok(g)
    }

    // ---- E1: telemetry

    pub fn record_run(&self, run: &RunRecord) -> Result<(), String> {
        self.db(self.store.save_run(run))
    }

    /// The user's reaction (or a plan's goal status) tells how a run went;
    /// `feedback` is what they said, kept for the evolver and the benchmark judge.
    pub fn set_outcome(&self, id: Uuid, outcome: RunOutcome, corrected: bool, feedback: Option<&str>) -> Result<(), String> {
        let Some(mut run) = self.db(self.store.run(id))? else { return Ok(()) };
        run.outcome = outcome;
        run.corrected |= corrected;
        if let Some(f) = feedback {
            run.feedback = Some(f.chars().take(500).collect());
        }
        self.db(self.store.save_run(&run))
    }

    pub fn runs(&self, limit: usize) -> Result<Vec<RunRecord>, String> {
        self.db(self.store.runs(limit))
    }

    pub fn stats(&self) -> Result<Stats, String> {
        let runs = self.runs(self.settings.window)?;
        let judged: Vec<&RunRecord> = runs.iter().filter(|r| r.outcome != RunOutcome::Unknown).collect();
        let n = runs.len().max(1) as f32;
        Ok(Stats {
            runs: runs.len(),
            judged: judged.len(),
            success_rate: success_rate(&judged),
            corrections: runs.iter().filter(|r| r.corrected).count(),
            avg_tool_calls: runs.iter().map(|r| r.tool_calls).sum::<u32>() as f32 / n,
            avg_model_calls: runs.iter().map(|r| r.model_calls).sum::<u32>() as f32 / n,
            pending: self.candidates(200)?.iter().filter(|c| c.status == CandidateStatus::Proposed).count(),
            generation: self.generation()?.number,
        })
    }

    // ---- E2: detection

    /// Problems in recent runs, skills, and capabilities' track records (C12).
    pub fn detect(
        &self,
        skills: &[SkillHealth],
        capabilities: &[detect::CapabilityRecord],
        goals: &[detect::GoalRecord],
    ) -> Result<Vec<Opportunity>, String> {
        let runs = self.runs(self.settings.window)?;
        let mut ops = detect::detect(&runs, skills, &self.settings.thresholds);
        ops.extend(detect::detect_capabilities(&runs, capabilities, &self.settings.thresholds));
        ops.extend(detect::detect_goals(goals, &self.settings.thresholds));
        Ok(ops)
    }

    pub fn evidence(&self, op: &Opportunity) -> Result<Vec<RunRecord>, String> {
        Ok(self.runs(self.settings.window * 2)?.into_iter().filter(|r| op.evidence.contains(&r.id)).collect())
    }

    // ---- candidates

    /// Store the evolver's proposals for one opportunity as a group of
    /// competing candidates.
    pub fn propose(&self, op: &Opportunity, proposals: Vec<Proposal>) -> Result<Vec<Candidate>, String> {
        let group = Uuid::new_v4();
        let now = Utc::now();
        let mut out = Vec::new();
        for p in proposals {
            let c = Candidate {
                id: Uuid::new_v4(),
                group,
                level: Level::of(p.change.category()),
                problem: op.problem.clone(),
                rationale: p.rationale,
                change: p.change,
                evidence: op.evidence.clone(),
                confidence: p.confidence,
                status: CandidateStatus::Proposed,
                validation: None,
                created_at: now,
                updated_at: now,
            };
            self.save(&c)?;
            self.event(Some(&c), "proposed", format!("{} ({}): {}", c.change.summary(), c.level, c.problem), None, None, None)?;
            out.push(c);
        }
        Ok(out)
    }

    /// A source patch from the code lab's flow (always manual).
    pub fn propose_code(&self, problem: &str, rationale: &str, base_commit: &str, diff: &str) -> Result<Candidate, String> {
        let now = Utc::now();
        let c = Candidate {
            id: Uuid::new_v4(),
            group: Uuid::new_v4(),
            level: Level::Code,
            problem: problem.into(),
            rationale: rationale.into(),
            change: Change::Code { base_commit: base_commit.into(), diff: diff.into() },
            evidence: Vec::new(),
            confidence: 0.5,
            status: CandidateStatus::Proposed,
            validation: None,
            created_at: now,
            updated_at: now,
        };
        self.save(&c)?;
        self.event(Some(&c), "proposed", format!("{} (code): {problem}", c.change.summary()), None, None, None)?;
        Ok(c)
    }

    pub fn candidates(&self, limit: usize) -> Result<Vec<Candidate>, String> {
        self.db(self.store.candidates(limit))
    }

    pub fn find(&self, key: &str) -> Result<Candidate, String> {
        let key = key.trim();
        if key.len() < 4 {
            return Err("give at least 4 characters of a candidate id".into());
        }
        let matches: Vec<Candidate> = self.candidates(500)?.into_iter().filter(|c| c.id.to_string().starts_with(key)).collect();
        match matches.len() {
            0 => Err(format!("no candidate matches {key:?}")),
            1 => Ok(matches.into_iter().next().unwrap()),
            n => Err(format!("{n} candidates match {key:?}; use more of the id")),
        }
    }

    fn save(&self, c: &Candidate) -> Result<(), String> {
        self.db(self.store.save_candidate(c))
    }

    pub fn set_status(&self, mut c: Candidate, status: CandidateStatus, note: &str) -> Result<Candidate, String> {
        c.status = status;
        c.updated_at = Utc::now();
        self.save(&c)?;
        self.event(Some(&c), status.as_str().as_str(), note.to_string(), None, None, None)?;
        Ok(c)
    }

    pub fn save_validation(&self, mut c: Candidate, v: Validation) -> Result<Candidate, String> {
        let (before, after) = (v.fitness_before.map(|f| f.total), v.fitness_after.map(|f| f.total));
        let note = format!("validated: {}", if v.valid { "passed checks" } else { "failed checks" });
        c.validation = Some(v);
        c.status = CandidateStatus::Testing;
        c.updated_at = Utc::now();
        self.save(&c)?;
        self.event(Some(&c), "tested", note, None, before, after)?;
        Ok(c)
    }

    /// Whether this candidate may deploy without a person (auto mode, a
    /// low-risk level, checks passed and the benchmark improved).
    pub fn may_auto_deploy(&self, c: &Candidate) -> bool {
        let Some(v) = &c.validation else { return false };
        self.settings.mode == Mode::Auto
            && c.level.policy() == Policy::AutoEventually
            && v.valid
            && matches!((&v.fitness_before, &v.fitness_after), (Some(b), Some(a)) if fitness::improves(b, a))
    }

    // ---- deploying (prompt, configuration, workflow and tool changes)

    /// The evolvable state with a change applied (nothing written).
    pub fn apply(&self, snapshot: &Snapshot, change: &Change) -> Result<Snapshot, String> {
        let mut next = snapshot.clone();
        match change {
            Change::Prompt { add, remove } => {
                let mut b = parse_behavior(&snapshot.behavior)?;
                b.change_guidelines(add, remove)?;
                next.behavior = b.render();
            }
            Change::Configuration { key, from, to } => {
                let mut b = parse_behavior(&snapshot.behavior)?;
                if b.get(key).as_ref() != Some(from) {
                    return Err(format!("{key} has changed since this was proposed"));
                }
                b.set(key, to)?;
                next.behavior = b.render();
            }
            Change::Workflow { workflow } => {
                workflow.validate()?;
                next.workflows.insert(workflow.name.clone(), workflow.render());
            }
            Change::Tool { tool } => {
                next.tools.insert(tool.name.clone(), tool.render());
            }
            Change::Skill { .. } | Change::Code { .. } => return Err("skill and code changes deploy through their own systems".into()),
        }
        Ok(next)
    }

    /// Apply an approved (or auto-approved) change: write it, and record a
    /// new generation whose parent is the current one.
    pub fn deploy(&self, c: Candidate, why: &str) -> Result<Generation, String> {
        if matches!(c.status, CandidateStatus::Rejected | CandidateStatus::Deployed | CandidateStatus::RolledBack) {
            return Err(format!("candidate {} is {}", c.short(), c.status));
        }
        let current = self.generation()?;
        let next = self.apply(&self.snapshot(), &c.change)?;
        self.write_snapshot(&next)?;
        let generation = self.add_generation(next, Some(&current), Some(c.id), &format!("{} — {why}", c.change.summary()))?;
        let v = c.validation.clone().unwrap_or_default();
        let fb = v.fitness_before.map(|f| f.total);
        let fa = v.fitness_after.map(|f| f.total);
        let mut c = c;
        c.status = CandidateStatus::Deployed;
        c.updated_at = Utc::now();
        self.save(&c)?;
        self.event(Some(&c), "deployed", format!("{} — {why}", c.change.summary()), Some(current.number), fb, fa)?;
        // Other candidates for the same problem lose the selection.
        for other in self.candidates(500)?.into_iter().filter(|o| o.group == c.group && o.id != c.id) {
            if matches!(other.status, CandidateStatus::Proposed | CandidateStatus::Testing) {
                self.set_status(other, CandidateStatus::Rejected, &format!("{} was selected instead", c.short()))?;
            }
        }
        Ok(generation)
    }

    /// A code change handled outside the agent (a branch); recorded here.
    pub fn mark_deployed_elsewhere(&self, c: Candidate, note: &str) -> Result<Candidate, String> {
        self.set_status(c, CandidateStatus::Deployed, note)
    }

    /// A skill revision the skill system applied: it becomes a generation of
    /// its own (same files, plus the revision), so the monitor attributes
    /// regressions to it and a rollback can undo it.
    pub fn deploy_skill(&self, c: Candidate, revision: SkillRevision, why: &str) -> Result<Generation, String> {
        let current = self.generation()?;
        let reason = format!("{} (v{} → v{}) — {why}", c.change.summary(), revision.from_version, revision.to_version);
        let mut g = Generation {
            id: Uuid::new_v4(),
            number: current.number + 1,
            parent: Some(current.id),
            snapshot: current.snapshot.clone(),
            candidate: Some(c.id),
            reason: reason.clone(),
            created_at: Utc::now(),
            skill: Some(revision),
        };
        // The files may have been edited since; snapshot what's there now.
        g.snapshot = self.snapshot();
        self.db(self.store.add_generation(&g))?;
        let v = c.validation.clone().unwrap_or_default();
        let (fb, fa) = (v.fitness_before.map(|f| f.total), v.fitness_after.map(|f| f.total));
        let mut c = c;
        c.status = CandidateStatus::Deployed;
        c.updated_at = Utc::now();
        self.save(&c)?;
        self.event(Some(&c), "deployed", reason, Some(current.number), fb, fa)?;
        for other in self.candidates(500)?.into_iter().filter(|o| o.group == c.group && o.id != c.id) {
            if matches!(other.status, CandidateStatus::Proposed | CandidateStatus::Testing) {
                self.set_status(other, CandidateStatus::Rejected, &format!("{} was selected instead", c.short()))?;
            }
        }
        Ok(g)
    }

    pub fn reject(&self, c: Candidate, why: &str) -> Result<Candidate, String> {
        self.set_status(c, CandidateStatus::Rejected, why)
    }

    /// Restore an earlier generation's state (the parent of the current one by
    /// default). The rollback is itself a new generation. Skill revisions made
    /// after the target are returned, newest first, for the skill system to
    /// undo (see [`Generation::skill`]).
    pub fn rollback(&self, to: Option<u32>, why: &str) -> Result<(Generation, Vec<SkillRevision>), String> {
        let generations = self.generations()?;
        let current = generations.first().ok_or("no generations")?.clone();
        let target = match to {
            Some(n) => generations.iter().find(|g| g.number == n).ok_or(format!("no generation {n}"))?,
            None => {
                let parent = current.parent.ok_or("the first generation has nothing to roll back to")?;
                generations.iter().find(|g| g.id == parent).ok_or("the parent generation is missing")?
            }
        };
        if target.number == current.number {
            return Err(format!("generation {} is already current", current.number));
        }
        self.write_snapshot(&target.snapshot)?;
        let reason = format!("rollback to generation {}: {why}", target.number);
        let generation = self.add_generation(target.snapshot.clone(), Some(&current), None, &reason)?;
        // Changes made after the target are undone. `generations` is newest first.
        let after: Vec<&Generation> = generations.iter().filter(|g| g.number > target.number).collect();
        let skills: Vec<SkillRevision> = after.iter().filter_map(|g| g.skill.clone()).collect();
        let undone: Vec<Uuid> = after.iter().filter_map(|g| g.candidate).collect();
        for c in self.candidates(500)?.into_iter().filter(|c| undone.contains(&c.id) && c.status == CandidateStatus::Deployed) {
            let note = format!("{} undone by the {reason}", c.change.summary());
            self.set_status(c, CandidateStatus::RolledBack, &note)?;
        }
        self.event(None, "rolled_back", reason, Some(current.number), None, None)?;
        Ok((generation, skills))
    }

    // ---- monitoring

    /// Whether the current generation is doing clearly worse than its parent.
    /// Judged runs compare success rates; all runs compare how often the user
    /// corrected the answer and how many errors runs hit, so a regression shows
    /// even when few runs get an explicit outcome. Returns the reason.
    pub fn regression(&self) -> Result<Option<String>, String> {
        let generations = self.generations()?;
        let Some(current) = generations.first() else { return Ok(None) };
        let Some(parent) = current.parent.and_then(|p| generations.iter().find(|g| g.id == p)) else { return Ok(None) };
        if current.candidate.is_none() {
            return Ok(None);
        }
        let runs = self.runs(1000)?;
        let of = |n: u32| runs.iter().filter(|r| r.generation == n).collect::<Vec<_>>();
        let (now_all, before_all) = (of(current.number), of(parent.number));
        fn judged<'a>(rs: &[&'a RunRecord]) -> Vec<&'a RunRecord> {
            rs.iter().copied().filter(|r| r.outcome != RunOutcome::Unknown).collect()
        }
        let (now, before) = (judged(&now_all), judged(&before_all));
        let n = self.settings.monitor_runs;
        let drop = self.settings.monitor_drop;
        let (cur, par) = (current.number, parent.number);
        if now.len() >= n
            && before.len() >= n
            && let (Some(a), Some(b)) = (success_rate(&now), success_rate(&before))
            && b - a > drop
        {
            return Ok(Some(format!("generation {cur} succeeds {:.0}% of the time vs {:.0}% for generation {par}", a * 100.0, b * 100.0)));
        }
        if now_all.len() < n || before_all.len() < n {
            return Ok(None);
        }
        let rate = |rs: &[&RunRecord], f: &dyn Fn(&RunRecord) -> f32| rs.iter().map(|r| f(r)).sum::<f32>() / rs.len() as f32;
        let corrected = |r: &RunRecord| if r.corrected { 1.0 } else { 0.0 };
        let (ca, cb) = (rate(&now_all, &corrected), rate(&before_all, &corrected));
        if ca - cb > drop {
            return Ok(Some(format!("generation {cur} gets corrected {:.0}% of the time vs {:.0}% for generation {par}", ca * 100.0, cb * 100.0)));
        }
        let errors = |r: &RunRecord| r.errors.len() as f32;
        let (ea, eb) = (rate(&now_all, &errors), rate(&before_all, &errors));
        if ea > eb * 2.0 + 0.25 {
            return Ok(Some(format!("generation {cur} hits {ea:.1} errors per run vs {eb:.1} for generation {par}")));
        }
        Ok(None)
    }

    // ---- history

    fn event(
        &self,
        candidate: Option<&Candidate>,
        kind: &str,
        description: String,
        old_generation: Option<u32>,
        fitness_before: Option<f32>,
        fitness_after: Option<f32>,
    ) -> Result<(), String> {
        let new_generation = self.generations()?.first().map(|g| g.number);
        let e = EvolutionEvent {
            id: Uuid::new_v4(),
            candidate: candidate.map(|c| c.id),
            kind: kind.to_string(),
            description,
            old_generation,
            new_generation,
            fitness_before,
            fitness_after,
            category: candidate.map(|c| c.change.category()),
            evidence: candidate.map(|c| c.evidence.clone()).unwrap_or_default(),
            created_at: Utc::now(),
        };
        self.db(self.store.record(&e))
    }

    /// Note that a review ran (for scheduling).
    pub fn note_review(&self, summary: &str) -> Result<(), String> {
        self.event(None, "review", summary.to_string(), None, None, None)
    }

    pub fn events(&self, limit: usize) -> Result<Vec<EvolutionEvent>, String> {
        self.db(self.store.events(limit))
    }

    pub fn last_review(&self) -> Result<Option<chrono::DateTime<Utc>>, String> {
        Ok(self.events(500)?.into_iter().find(|e| e.kind == "review").map(|e| e.created_at))
    }
}

pub fn success_rate(runs: &[&RunRecord]) -> Option<f32> {
    if runs.is_empty() {
        return None;
    }
    let score: f32 = runs
        .iter()
        .map(|r| match r.outcome {
            RunOutcome::Success => 1.0,
            RunOutcome::Partial => 0.5,
            _ => 0.0,
        })
        .sum();
    Some(score / runs.len() as f32)
}

fn parse_behavior(text: &str) -> Result<Behavior, String> {
    if text.trim().is_empty() { Ok(Behavior::default()) } else { Behavior::parse(text) }
}

fn read_texts(dir: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "toml")
            && let (Some(stem), Ok(text)) = (path.file_stem().and_then(|s| s.to_str()), std::fs::read_to_string(&path))
        {
            out.insert(stem.to_string(), text);
        }
    }
    out
}

fn read_dir<T>(dir: &Path, parse: impl Fn(&str) -> Result<T, String>) -> (Vec<T>, Vec<String>) {
    let mut ok = Vec::new();
    let mut errors = Vec::new();
    for (name, text) in read_texts(dir) {
        match parse(&text) {
            Ok(v) => ok.push(v),
            Err(e) => errors.push(format!("{name}: {e}")),
        }
    }
    (ok, errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composite::{Input, ToolCall};
    use crate::workflow::Phase;
    use serde_json::json;

    fn manager(mode: Mode) -> (tokio::runtime::Runtime, EvolutionManager, PathBuf) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let home = std::env::temp_dir().join(format!("lyra-evolution-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let store = rt.block_on(EvolutionStore::in_memory()).unwrap();
        let m = EvolutionManager::with_store(store, &home, rt.handle().clone(), Settings { mode, ..Settings::default() }).unwrap();
        (rt, m, home)
    }

    fn op() -> Opportunity {
        Opportunity { kind: "frequent_corrections".into(), problem: "answers too long".into(), categories: vec![Category::Prompt], evidence: vec![], details: json!({}) }
    }

    fn proposal(change: Change) -> Proposal {
        Proposal { change, rationale: "r".into(), confidence: 0.8 }
    }

    #[test]
    fn deploying_writes_files_and_makes_a_generation() {
        let (_rt, m, home) = manager(Mode::Propose);
        assert_eq!(m.generation().unwrap().number, 1);
        let cands = m
            .propose(
                &op(),
                vec![
                    proposal(Change::Prompt { add: vec!["Answer in at most three sentences.".into()], remove: vec![] }),
                    proposal(Change::Configuration { key: "recall_before_answering".into(), from: json!(false), to: json!(true) }),
                ],
            )
            .unwrap();
        let g = m.deploy(cands[0].clone(), "approved").unwrap();
        assert_eq!(g.number, 2);
        assert_eq!(m.behavior().unwrap().guidelines, ["Answer in at most three sentences."]);
        assert!(std::fs::read_to_string(home.join("config/behavior.toml")).unwrap().contains("three sentences"));
        let statuses: Vec<CandidateStatus> = m.candidates(10).unwrap().iter().map(|c| c.status).collect();
        assert!(statuses.contains(&CandidateStatus::Deployed) && statuses.contains(&CandidateStatus::Rejected), "the sibling loses");
        assert!(m.deploy(cands[0].clone(), "again").is_err() || m.find(&cands[0].short()).unwrap().status == CandidateStatus::Deployed);
    }

    #[test]
    fn workflows_and_tools_are_files_and_rollback_restores_everything() {
        let (_rt, m, home) = manager(Mode::Propose);
        let wf = WorkflowDef {
            name: "infra".into(),
            description: "server changes".into(),
            triggers: vec!["server".into()],
            phases: vec![Phase { name: "gather".into(), instruction: "inspect first".into() }],
        };
        let tool = CompositeTool {
            name: "memory_overview".into(),
            description: "recall and list".into(),
            inputs: vec![Input { name: "q".into(), description: "query".into(), required: true }],
            steps: vec![ToolCall { tool: "memory_recall".into(), arguments: json!({"query": "{{q}}"}) }, ToolCall { tool: "memory_list".into(), arguments: json!({}) }],
        };
        let a = m.propose(&op(), vec![proposal(Change::Workflow { workflow: wf.clone() })]).unwrap();
        m.deploy(a[0].clone(), "ok").unwrap();
        let b = m.propose(&op(), vec![proposal(Change::Tool { tool: tool.clone() })]).unwrap();
        m.deploy(b[0].clone(), "ok").unwrap();
        assert_eq!(m.workflows().0, [wf]);
        assert_eq!(m.composites().0, [tool]);
        assert_eq!(m.generation().unwrap().number, 3);

        let (g, skills) = m.rollback(Some(1), "test").unwrap();
        assert_eq!(g.number, 4, "a rollback is a new generation");
        assert!(skills.is_empty());
        assert!(m.workflows().0.is_empty() && m.composites().0.is_empty());
        assert!(!home.join("tools/memory_overview.toml").exists());
        let rolled: Vec<CandidateStatus> = m.candidates(10).unwrap().iter().map(|c| c.status).collect();
        assert_eq!(rolled, [CandidateStatus::RolledBack, CandidateStatus::RolledBack]);
        assert!(m.events(50).unwrap().iter().any(|e| e.kind == "rolled_back"));
    }

    #[test]
    fn skill_revisions_are_generations_and_rollback_reports_them() {
        let (_rt, m, _home) = manager(Mode::Propose);
        let c = m
            .propose(&op(), vec![proposal(Change::Skill { skill: "rust-commit".into(), instructions: "run clippy first".into() })])
            .unwrap()
            .remove(0);
        let revision = SkillRevision { name: "rust-commit".into(), from_version: 2, to_version: 3 };
        let g = m.deploy_skill(c, revision.clone(), "approved").unwrap();
        assert_eq!((g.number, g.skill.clone()), (2, Some(revision.clone())));
        assert_eq!(m.candidates(5).unwrap()[0].status, CandidateStatus::Deployed);
        let (g, skills) = m.rollback(None, "worse").unwrap();
        assert_eq!(g.number, 3);
        assert_eq!(skills, [revision], "the caller restores v2");
        assert_eq!(m.candidates(5).unwrap()[0].status, CandidateStatus::RolledBack);
    }

    #[test]
    fn stale_configuration_changes_are_refused() {
        let (_rt, m, _home) = manager(Mode::Propose);
        let c = m.propose(&op(), vec![proposal(Change::Configuration { key: "max_tool_rounds".into(), from: json!(5), to: json!(4) })]).unwrap();
        assert!(m.deploy(c[0].clone(), "x").unwrap_err().contains("changed since"));
    }

    #[test]
    fn only_low_risk_improvements_auto_deploy() {
        let (_rt, auto, _h) = manager(Mode::Auto);
        let better = Validation {
            valid: true,
            fitness_before: Some(Fitness { total: 0.5, success_rate: 0.5, safety: 1.0, ..Default::default() }),
            fitness_after: Some(Fitness { total: 0.7, success_rate: 0.7, safety: 1.0, ..Default::default() }),
            ..Default::default()
        };
        let prompt = auto.propose(&op(), vec![proposal(Change::Prompt { add: vec!["Be brief.".into()], remove: vec![] })]).unwrap().remove(0);
        let prompt = auto.save_validation(prompt, better.clone()).unwrap();
        assert!(auto.may_auto_deploy(&prompt));
        let config = auto
            .propose(&op(), vec![proposal(Change::Configuration { key: "max_tool_rounds".into(), from: json!(8), to: json!(4) })])
            .unwrap()
            .remove(0);
        let config = auto.save_validation(config, better.clone()).unwrap();
        assert!(!auto.may_auto_deploy(&config), "configuration changes are proposed");
        let worse = Validation { fitness_after: Some(Fitness { total: 0.4, ..Default::default() }), ..better };
        let p2 = auto.propose(&op(), vec![proposal(Change::Prompt { add: vec!["Be terse.".into()], remove: vec![] })]).unwrap().remove(0);
        let p2 = auto.save_validation(p2, worse).unwrap();
        assert!(!auto.may_auto_deploy(&p2), "must beat the baseline");
        let (_rt2, propose, _h2) = manager(Mode::Propose);
        assert!(!propose.may_auto_deploy(&prompt), "propose mode never auto-deploys");
    }

    #[test]
    fn telemetry_and_regressions() {
        let (_rt, m, _home) = manager(Mode::Auto);
        let c = m.propose(&op(), vec![proposal(Change::Prompt { add: vec!["Be brief.".into()], remove: vec![] })]).unwrap().remove(0);
        for i in 0..10 {
            let mut r = RunRecord::new(RunKind::Chat, &format!("t{i}"), 1);
            r.outcome = RunOutcome::Success;
            m.record_run(&r).unwrap();
        }
        m.deploy(c, "ok").unwrap();
        assert_eq!(m.regression().unwrap(), None, "not enough runs yet");
        for i in 0..10 {
            let r = RunRecord::new(RunKind::Chat, &format!("u{i}"), 2);
            m.record_run(&r).unwrap();
            m.set_outcome(r.id, if i < 3 { RunOutcome::Success } else { RunOutcome::Failure }, i >= 3, None).unwrap();
        }
        let why = m.regression().unwrap().unwrap();
        assert!(why.contains("30%") && why.contains("100%"), "{why}");
        let stats = m.stats().unwrap();
        assert_eq!((stats.runs, stats.corrections, stats.generation), (20, 7, 2));
    }

    #[test]
    fn regressions_show_in_corrections_and_errors_without_outcomes() {
        let (_rt, m, _home) = manager(Mode::Propose);
        let c = m.propose(&op(), vec![proposal(Change::Prompt { add: vec!["Be brief.".into()], remove: vec![] })]).unwrap().remove(0);
        for i in 0..10 {
            m.record_run(&RunRecord::new(RunKind::Chat, &format!("t{i}"), 1)).unwrap();
        }
        m.deploy(c, "ok").unwrap();
        for i in 0..10 {
            let mut r = RunRecord::new(RunKind::Chat, &format!("u{i}"), 2);
            r.errors = vec!["connection refused".into()];
            m.record_run(&r).unwrap();
        }
        let why = m.regression().unwrap().unwrap();
        assert!(why.contains("errors per run"), "{why}");
    }
}

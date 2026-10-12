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
    /// A deployed prompt, setting or workflow change is tried on this share
    /// of the turns first (0 turns trials off).
    pub trial_share: f32,
    /// Turns with an outcome the trial needs before it's decided.
    pub trial_runs: usize,
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
            trial_share: 0.3,
            trial_runs: 20,
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
            write_atomic(&self.behavior_path(), &s.behavior)?;
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
                write_atomic(&dir.join(format!("{name}.toml")), text)?;
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
            trial: None,
            edited_by: None,
            edited_keys: Vec::new(),
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
    pub fn set_outcome(&self, id: Uuid, outcome: RunOutcome, corrected: bool, feedback: Option<&str>, source: Option<&str>) -> Result<(), String> {
        let Some(mut run) = self.db(self.store.run(id))? else { return Ok(()) };
        run.outcome = outcome;
        if let Some(s) = source {
            run.outcome_source = Some(s.to_string());
        }
        // The latest verdict says (a 👍 after a correction takes it back).
        run.corrected = corrected;
        if let Some(f) = feedback {
            run.feedback = Some(f.chars().take(500).collect());
        }
        self.db(self.store.save_run(&run))
    }

    /// The run stopped at the tool-call limit (`rounds`), part done.
    pub fn mark_limited(&self, id: Uuid, rounds: u32) -> Result<(), String> {
        let Some(mut run) = self.db(self.store.run(id))? else { return Ok(()) };
        run.limited = true;
        let note = format!("stopped at the tool-call limit ({rounds} rounds)");
        if !run.errors.contains(&note) {
            run.errors.push(note);
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
        agents: &[detect::AgentRecord],
    ) -> Result<Vec<Opportunity>, String> {
        let runs = self.runs(self.settings.window)?;
        let mut ops = detect::detect(&runs, skills, &self.settings.thresholds);
        ops.extend(detect::detect_capabilities(&runs, capabilities, &self.settings.thresholds));
        ops.extend(detect::detect_goals(goals, &self.settings.thresholds));
        ops.extend(detect::detect_agents(agents, &self.settings.thresholds));
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
        // Only through a trial: what can be tried on part of the turns.
        self.settings.mode == Mode::Auto && c.level.policy() == Policy::AutoEventually && v.valid && v.improves() && self.trials(&c.change)
    }

    /// Whether this change is deployed on trial: a prompt, setting or
    /// workflow change that shows in chats (planner-only ones can't be told
    /// apart turn by turn), with trials on.
    pub fn trials(&self, change: &Change) -> bool {
        let chat = match change {
            Change::Prompt { .. } | Change::Workflow { .. } => true,
            Change::Configuration { key, .. } => matches!(key.as_str(), "max_tool_rounds" | "recall_before_answering"),
            _ => false,
        };
        chat && self.settings.trial_share > 0.0 && self.settings.trial_runs > 0
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
            Change::Skill { .. } | Change::Code { .. } | Change::Agent { .. } | Change::Api { .. } => {
                return Err("skill, agent, code and API changes deploy through their own systems".into());
            }
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
        // A running trial ends first: the one before it stays the agent.
        if current.trial.as_ref().is_some_and(|t| t.status == TrialStatus::Running) {
            return Err(format!("generation {} is still on trial: /evolve keep or /evolve rollback first", current.number));
        }
        self.write_snapshot(&next)?;
        let mut generation = self.add_generation(next, Some(&current), Some(c.id), &format!("{} — {why}", c.change.summary()))?;
        if self.trials(&c.change) {
            generation.trial = Some(Trial { share: self.settings.trial_share.clamp(0.05, 0.95), runs: self.settings.trial_runs, started: Utc::now(), status: TrialStatus::Running, note: String::new() });
            self.db(self.store.update_generation(&generation))?;
        }
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
            trial: None,
            edited_by: None,
            edited_keys: Vec::new(),
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
        // What people set by hand since the target stays as they set it.
        let mut snapshot = target.snapshot.clone();
        let kept: Vec<String> = generations.iter().filter(|g| g.number > target.number).flat_map(|g| g.edited_keys.iter().cloned()).collect();
        let mut reason = format!("rollback to generation {}: {why}", target.number);
        if !kept.is_empty() {
            let now = parse_behavior(&self.snapshot().behavior)?;
            let mut b = parse_behavior(&snapshot.behavior)?;
            let mut names = Vec::new();
            for k in &kept {
                if let Some(v) = now.get(k)
                    && b.set(k, &v).is_ok()
                    && !names.contains(k)
                {
                    names.push(k.clone());
                }
            }
            snapshot.behavior = b.render();
            if !names.is_empty() {
                reason += &format!(" (kept your {})", names.join(", "));
            }
        }
        self.write_snapshot(&snapshot)?;
        let generation = self.add_generation(snapshot.clone(), Some(&current), None, &reason)?;
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
        if current.candidate.is_none() || current.trial.is_some() {
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

    /// Someone changed behavior settings by hand (the Settings page): it
    /// becomes a generation, so a rollback keeps it. During a trial it's only
    /// noted (a generation would end the trial's comparison).
    pub fn record_edit(&self, who: &str, keys: &[String]) -> Result<Option<Generation>, String> {
        if keys.is_empty() {
            return Ok(None);
        }
        let note = format!("{} set by {who}", keys.join(", "));
        if self.trial()?.is_some() {
            self.event(None, "edited", note, None, None, None)?;
            return Ok(None);
        }
        let current = self.generation()?;
        let mut g = self.add_generation(self.snapshot(), Some(&current), None, &format!("settings: {note}"))?;
        g.edited_by = Some(who.to_string());
        g.edited_keys = keys.to_vec();
        self.db(self.store.update_generation(&g))?;
        self.event(None, "edited", note, Some(current.number), None, None)?;
        Ok(Some(g))
    }

    // ---- how lyra is doing

    /// Outcomes over the last `weeks` weeks, how they came in, each recent
    /// generation's record and each skill's effect: counts only (no one's words).
    pub fn health(&self, weeks: usize) -> Result<Health, String> {
        let now = Utc::now();
        let since = now - chrono::Duration::weeks(weeks as i64);
        let runs: Vec<RunRecord> = self.runs(5000)?.into_iter().filter(|r| r.created_at >= since && r.kind == RunKind::Chat).collect();
        let mut buckets: Vec<Week> = (0..weeks)
            .rev()
            .map(|i| Week { start: (now - chrono::Duration::weeks(i as i64 + 1)).date_naive(), ..Default::default() })
            .collect();
        let mut sources: BTreeMap<String, usize> = BTreeMap::new();
        for r in &runs {
            let age = (now - r.created_at).num_days().max(0) as usize / 7;
            if let Some(w) = buckets.len().checked_sub(age + 1).and_then(|i| buckets.get_mut(i)) {
                w.add(r);
            }
            if r.outcome != RunOutcome::Unknown {
                *sources.entry(r.outcome_source.clone().unwrap_or_else(|| "earlier".into())).or_default() += 1;
            }
        }
        let all = Arm::of(&runs);
        let generations = self
            .generations()?
            .into_iter()
            .take(10)
            .map(|g| {
                let mine: Vec<RunRecord> = runs.iter().filter(|r| r.generation == g.number && r.arm.as_deref() != Some("control")).cloned().collect();
                GenerationHealth { number: g.number, reason: g.reason, created_at: g.created_at, trial: g.trial, record: Arm::of(&mine) }
            })
            .collect();
        // A skill's effect: how replies that used it went vs those that didn't.
        let mut names: Vec<String> = runs.iter().flat_map(|r| r.skills_used.iter().cloned()).collect();
        names.sort();
        names.dedup();
        let skills = names
            .into_iter()
            .map(|name| {
                let (with, without): (Vec<RunRecord>, Vec<RunRecord>) = runs.iter().cloned().partition(|r| r.skills_used.contains(&name));
                SkillEffect { name, with: Arm::of(&with), without: Arm::of(&without) }
            })
            .collect();
        let others = runs.iter().filter(|r| r.person.is_some()).count();
        Ok(Health { weeks: buckets, sources, all, generations, skills, trial: self.trial_standing()?, owner_runs: runs.len() - others, other_runs: others })
    }

    // ---- trials (E8)

    /// The generation on trial now, and the one it's compared with.
    pub fn trial(&self) -> Result<Option<(Generation, Generation)>, String> {
        let generations = self.generations()?;
        let Some(current) = generations.first().filter(|g| g.trial.as_ref().is_some_and(|t| t.status == TrialStatus::Running)) else { return Ok(None) };
        let parent = current.parent.and_then(|p| generations.iter().find(|g| g.id == p)).ok_or("the generation before the trial is missing")?;
        Ok(Some((current.clone(), parent.clone())))
    }

    /// How the trial stands: its turns and the control's since it started.
    pub fn trial_standing(&self) -> Result<Option<TrialStanding>, String> {
        let Some((g, parent)) = self.trial()? else { return Ok(None) };
        let t = g.trial.clone().expect("a running trial");
        let runs = self.runs(2000)?;
        let side = |arm: &str| runs.iter().filter(|r| r.arm.as_deref() == Some(arm) && r.created_at >= t.started).cloned().collect::<Vec<_>>();
        let (trial, control) = (side("trial"), side("control"));
        Ok(Some(TrialStanding { generation: g.number, parent: parent.number, trial: Arm::of(&trial), control: Arm::of(&control), needed: t.runs, share: t.share, started: t.started }))
    }

    /// Decide the trial once it has enough: `Some(true)` keep it, `Some(false)`
    /// it lost (roll it back), `None` not yet. With the reason.
    pub fn trial_verdict(&self) -> Result<Option<(bool, String)>, String> {
        let Some(s) = self.trial_standing()? else { return Ok(None) };
        let (t, c) = (&s.trial, &s.control);
        let enough_control = c.judged >= (s.needed / 2).max(5);
        let (Some(ts), Some(cs)) = (t.success_rate(), c.success_rate()) else { return Ok(None) };
        let say = |verdict: &str| {
            format!(
                "generation {} {verdict} its trial: {:.0}% good of {} turns vs {:.0}% of {} for generation {}; corrected {:.0}% vs {:.0}%",
                s.generation, ts * 100.0, t.judged, cs * 100.0, c.judged, s.parent, t.correction_rate() * 100.0, c.correction_rate() * 100.0
            )
        };
        // Clearly worse half way: stop early.
        if t.judged >= s.needed / 2 && enough_control && cs - ts > self.settings.monitor_drop {
            return Ok(Some((false, say("lost"))));
        }
        if t.judged < s.needed || !enough_control {
            return Ok(None);
        }
        let keep = ts + 1e-6 >= cs && t.correction_rate() <= c.correction_rate() + 0.1;
        Ok(Some((keep, say(if keep { "won" } else { "lost" }))))
    }

    /// End the running trial: kept (it's the agent now) or lost (the caller rolls it back).
    pub fn end_trial(&self, status: TrialStatus, note: &str) -> Result<Generation, String> {
        let (mut g, _) = self.trial()?.ok_or("nothing is on trial")?;
        if let Some(t) = g.trial.as_mut() {
            t.status = status;
            t.note = note.to_string();
        }
        self.db(self.store.update_generation(&g))?;
        let kind = if status == TrialStatus::Kept { "trial_kept" } else { "trial_lost" };
        self.event(None, kind, note.to_string(), Some(g.number), None, None)?;
        Ok(g)
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

/// Write a file atomically: a temp file of its own, flushed, then renamed over
/// it (a snapshot cut off part way never leaves a half-written behavior.toml).
fn write_atomic(path: &std::path::Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.tmp-{}-{}", std::process::id(), N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)));
    let done = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if done.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    done.map_err(|e| format!("{}: {e}", path.display()))
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
        // Trials have their own test; these deploy straight away.
        let m = EvolutionManager::with_store(store, &home, rt.handle().clone(), Settings { mode, trial_share: 0.0, ..Settings::default() }).unwrap();
        (rt, m, home)
    }

    fn trial_manager() -> (tokio::runtime::Runtime, EvolutionManager, PathBuf) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let home = std::env::temp_dir().join(format!("lyra-evolution-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let store = rt.block_on(EvolutionStore::in_memory()).unwrap();
        let m = EvolutionManager::with_store(store, &home, rt.handle().clone(), Settings { trial_runs: 10, ..Settings::default() }).unwrap();
        (rt, m, home)
    }

    /// `n` runs on one side of a trial, `good` of them good.
    fn arm_runs(m: &EvolutionManager, generation: u32, arm: &str, n: usize, good: usize) {
        for i in 0..n {
            let mut r = RunRecord::new(RunKind::Chat, "a task", generation);
            r.arm = Some(arm.into());
            r.outcome = if i < good { RunOutcome::Success } else { RunOutcome::Failure };
            m.record_run(&r).unwrap();
        }
    }

    #[test]
    fn a_rollback_keeps_what_someone_set_by_hand() {
        let (_rt, m, _home) = manager(Mode::Propose);
        let c = m.propose(&op(), vec![proposal(Change::Prompt { add: vec!["Be brief.".into()], remove: vec![] })]).unwrap().remove(0);
        m.deploy(c, "approved").unwrap();
        // The Settings page: 20 tool-call rounds, written to the file and recorded.
        let mut b = m.behavior().unwrap();
        b.set("max_tool_rounds", &json!(20)).unwrap();
        std::fs::write(m.home().join("config/behavior.toml"), b.render()).unwrap();
        let g = m.record_edit("owner", &["max_tool_rounds".into()]).unwrap().unwrap();
        assert_eq!((g.number, g.edited_keys.clone()), (3, vec!["max_tool_rounds".to_string()]));
        let (back, _) = m.rollback(Some(1), "test").unwrap();
        assert!(back.reason.contains("kept your max_tool_rounds"), "{}", back.reason);
        let now = m.behavior().unwrap();
        assert_eq!((now.max_tool_rounds, now.guidelines.len()), (20, 0), "the guideline went, the hand edit stayed");
        assert!(m.record_edit("owner", &[]).unwrap().is_none());
    }

    #[test]
    fn a_change_is_tried_on_part_of_the_turns_and_kept_only_if_it_wins() {
        let (_rt, m, _home) = trial_manager();
        let c = m.propose(&op(), vec![proposal(Change::Prompt { add: vec!["Say which source each fact came from.".into()], remove: vec![] })]).unwrap().remove(0);
        let g = m.deploy(c, "approved").unwrap();
        assert_eq!(g.trial.as_ref().map(|t| (t.status, t.runs)), Some((TrialStatus::Running, 10)));
        let (trial, parent) = m.trial().unwrap().unwrap();
        assert_eq!((trial.number, parent.number), (2, 1));
        // Not yet: too few turns either side.
        arm_runs(&m, 2, "trial", 4, 3);
        arm_runs(&m, 1, "control", 4, 2);
        assert_eq!(m.trial_verdict().unwrap(), None);
        // Another change waits for the trial.
        let next = m.propose(&op(), vec![proposal(Change::Prompt { add: vec!["Be brief.".into()], remove: vec![] })]).unwrap().remove(0);
        assert!(m.deploy(next, "x").unwrap_err().contains("on trial"));
        // Enough, and it did better: kept.
        arm_runs(&m, 2, "trial", 6, 6);
        arm_runs(&m, 1, "control", 6, 3);
        let (keep, why) = m.trial_verdict().unwrap().unwrap();
        assert!(keep, "{why}");
        assert!(why.contains("won its trial"), "{why}");
        m.end_trial(TrialStatus::Kept, &why).unwrap();
        assert!(m.trial().unwrap().is_none());
        assert_eq!(m.generation().unwrap().trial.unwrap().status, TrialStatus::Kept);
    }

    #[test]
    fn a_trial_clearly_losing_half_way_stops_early_and_planner_changes_skip_trials() {
        let (_rt, m, _home) = trial_manager();
        let c = m.propose(&op(), vec![proposal(Change::Prompt { add: vec!["Answer in one line.".into()], remove: vec![] })]).unwrap().remove(0);
        m.deploy(c, "approved").unwrap();
        arm_runs(&m, 2, "trial", 5, 1);
        arm_runs(&m, 1, "control", 6, 5);
        let (keep, why) = m.trial_verdict().unwrap().unwrap();
        assert!(!keep && why.contains("lost"), "{why}");
        assert!(!m.trials(&Change::Configuration { key: "plan_step_rounds".into(), from: json!(6), to: json!(4) }));
        assert!(!m.trials(&Change::Agent { agent: "writer".into(), instructions: "x".into() }));
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
        let revision = SkillRevision { name: "rust-commit".into(), from_version: 2, to_version: 3, agent: false };
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
        let (rt, _m, home) = trial_manager();
        let store = rt.block_on(EvolutionStore::in_memory()).unwrap();
        let auto = EvolutionManager::with_store(store, &home, rt.handle().clone(), Settings { mode: Mode::Auto, ..Settings::default() }).unwrap();
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
        let worse = Validation { fitness_after: Some(Fitness { total: 0.4, ..Default::default() }), ..better.clone() };
        let p2 = auto.propose(&op(), vec![proposal(Change::Prompt { add: vec!["Be terse.".into()], remove: vec![] })]).unwrap().remove(0);
        let p2 = auto.save_validation(p2, worse).unwrap();
        assert!(!auto.may_auto_deploy(&p2), "must beat the baseline");
        let (_rt2, propose, _h2) = manager(Mode::Propose);
        assert!(!propose.may_auto_deploy(&prompt), "propose mode never auto-deploys");
        let (_rt3, no_trials, _h3) = manager(Mode::Auto);
        assert!(!no_trials.may_auto_deploy(&prompt), "only through a trial");
        let agent = auto.propose(&op(), vec![proposal(Change::Agent { agent: "writer".into(), instructions: "Write plainly.".into() })]).unwrap().remove(0);
        let agent = auto.save_validation(agent, better.clone()).unwrap();
        assert!(!auto.may_auto_deploy(&agent), "an agent's instructions always wait for a person");
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
            m.set_outcome(r.id, if i < 3 { RunOutcome::Success } else { RunOutcome::Failure }, i >= 3, None, None).unwrap();
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

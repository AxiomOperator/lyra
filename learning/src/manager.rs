//! The agent's interface to skills. The model proposes; this owns policy and
//! persistence: what gets created or changed, when, and the history of it.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Result, anyhow, bail};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;

use crate::curator::{self, Health, Plan};
use crate::evaluator::{Candidate, Decision};
use crate::ledger::{Event, Ledger, Version};
use crate::lifecycle::{self, Policy, Transition};
use crate::proposal::{Change, NewSkill, Proposal, ProposalStatus};
use crate::scoring::{self, SkillScore, Weights};
use crate::{FileSkillStore, Relationship, Skill, SkillOutcome, SkillStatus, SkillStore};

/// How much the system may change on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// No reviews, no automatic changes.
    Off,
    /// Lessons, refinements and lifecycle changes wait for approval (the default).
    #[default]
    Propose,
    /// Safe changes apply on their own: refinements, promotion and deprecation
    /// on evidence, and trial use of confident new skills. New skills still
    /// start proposed, and merges and splits still need approval.
    Auto,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Propose => "propose",
            Mode::Auto => "auto",
        }
    }
}

/// Learning policy, from `[learning]` in the config.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub mode: Mode,
    /// Lessons the reviewer is less sure of than this are dropped.
    pub min_confidence: f32,
    /// Most skills added to a prompt.
    pub max_skills: usize,
    /// Word overlap at which two skills count as duplicate candidates.
    pub duplicate_threshold: f32,
    pub scoring: Weights,
    pub lifecycle: Policy,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            mode: Mode::Propose,
            min_confidence: 0.6,
            max_skills: 3,
            duplicate_threshold: 0.6,
            scoring: Weights::default(),
            lifecycle: Policy::default(),
        }
    }
}

/// A skill picked for a task, and why.
#[derive(Debug, Clone)]
pub struct Ranked {
    pub skill: Skill,
    pub score: SkillScore,
    /// A proposed skill used on trial (auto mode) to gather evidence.
    pub trial: bool,
}

/// What came of a review decision.
#[derive(Debug)]
pub enum Applied {
    Ignored(String),
    Created(Skill),
    Updated { skill: Skill, version: i64 },
    Proposed(Proposal),
}

/// Something the curator's local checks found.
#[derive(Debug, Default)]
pub struct Report {
    /// `(name, name, similarity)`.
    pub duplicates: Vec<(String, String, f32)>,
    pub stale: Vec<String>,
}

pub struct SkillManager<S: SkillStore = FileSkillStore> {
    store: S,
    ledger: Ledger,
    settings: Settings,
}

impl SkillManager<FileSkillStore> {
    /// Skill files in `dir`, with the ledger alongside them in `dir/ledger.db`.
    pub async fn open(dir: &Path, settings: Settings) -> Result<Self> {
        let store = FileSkillStore::open(dir)?;
        let ledger = Ledger::open(&dir.join("ledger.db")).await?;
        Ok(Self::new(store, ledger, settings))
    }

    /// Skill files that couldn't be read, with the reason.
    pub fn load_errors(&self) -> Vec<String> {
        self.store.load_errors()
    }
}

impl<S: SkillStore> SkillManager<S> {
    pub fn new(store: S, ledger: Ledger, settings: Settings) -> Self {
        Self { store, ledger, settings }
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    // ---- reading

    /// Every skill, with usage numbers filled in.
    pub async fn list(&self, status: Option<SkillStatus>) -> Result<Vec<Skill>> {
        let usage = self.ledger.usage().await?;
        let mut skills = self.store.list(status).await?;
        for s in &mut skills {
            s.usage = usage.get(&s.id).copied().unwrap_or_default();
        }
        Ok(skills)
    }

    pub async fn get(&self, id: Uuid) -> Result<Option<Skill>> {
        Ok(self.list(None).await?.into_iter().find(|s| s.id == id))
    }

    /// A skill by exact name or id prefix.
    pub async fn find(&self, key: &str) -> Result<Skill> {
        self.find_in(key, |_| true).await
    }

    /// A skill `viewer` may see (shared, or theirs), by name or id prefix.
    pub async fn find_as(&self, key: &str, viewer: Option<&str>) -> Result<Skill> {
        let viewer = viewer.map(str::to_string);
        self.find_in(key, move |s| s.visible_to(viewer.as_deref())).await
    }

    async fn find_in(&self, key: &str, keep: impl Fn(&Skill) -> bool) -> Result<Skill> {
        let key = key.trim();
        if key.is_empty() {
            bail!("give a skill name or id");
        }
        let matches: Vec<Skill> = self
            .list(None)
            .await?
            .into_iter()
            .filter(|s| keep(s) && (s.name == key || s.id.to_string().starts_with(key)))
            .collect();
        match matches.len() {
            0 => bail!("no skill matches {key:?}"),
            1 => Ok(matches.into_iter().next().unwrap()),
            n => bail!("{n} skills match {key:?}; use more of the id"),
        }
    }

    /// Skills to use for a task: active ones (and, in auto mode, confident
    /// proposals on trial), ranked by relevance, reliability, confidence and
    /// freshness.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<Ranked>> {
        let matches = self.store.search(query, 50).await?;
        let Some(best) = matches.first().map(|(_, r)| *r).filter(|r| *r > 0.0) else {
            return Ok(Vec::new());
        };
        let usage = self.ledger.usage().await?;
        let (now, s) = (Utc::now(), &self.settings);
        let mut ranked: Vec<Ranked> = matches
            .into_iter()
            .filter_map(|(mut skill, relevance)| {
                let trial = skill.status == SkillStatus::Proposed
                    && s.mode == Mode::Auto
                    && skill.confidence >= s.lifecycle.promote_confidence;
                if skill.status != SkillStatus::Active && !trial {
                    return None;
                }
                skill.usage = usage.get(&skill.id).copied().unwrap_or_default();
                let score = scoring::score(&skill, relevance / best, &s.scoring, now);
                Some(Ranked { skill, score, trial })
            })
            .collect();
        ranked.sort_by(|a, b| b.score.final_score.total_cmp(&a.score.final_score));
        ranked.truncate(limit);
        Ok(ranked)
    }

    /// Existing skills (proposed, active or deprecated) most like `text`, for
    /// the reviewer to refine instead of duplicating.
    pub async fn related(&self, text: &str, limit: usize) -> Result<Vec<Skill>> {
        Ok(self
            .store
            .search(text, limit * 3)
            .await?
            .into_iter()
            .map(|(s, _)| s)
            .filter(|s| s.status != SkillStatus::Rejected)
            .take(limit)
            .collect())
    }

    pub async fn proposals(&self) -> Result<Vec<Proposal>> {
        self.ledger.proposals(Some(ProposalStatus::Pending)).await
    }

    /// A pending proposal by id prefix.
    pub async fn find_proposal(&self, key: &str) -> Result<Option<Proposal>> {
        let key = key.trim();
        let matches: Vec<Proposal> =
            self.proposals().await?.into_iter().filter(|p| p.id.to_string().starts_with(key)).collect();
        match matches.len() {
            0 => Ok(None),
            1 => Ok(matches.into_iter().next()),
            n => bail!("{n} proposals match {key:?}; use more of the id"),
        }
    }

    /// A skill's versions and audit trail, newest first.
    pub async fn history(&self, id: Uuid) -> Result<(Vec<Version>, Vec<Event>)> {
        Ok((self.ledger.versions(id).await?, self.ledger.events(Some(id), 50).await?))
    }

    /// Relationships involving a skill, as readable lines
    /// (`supersedes old-name: merged`, `superseded by new-name: merged`).
    pub async fn relationships(&self, id: Uuid) -> Result<Vec<String>> {
        let skills = self.store.list(None).await?;
        let name = |id: Uuid| skills.iter().find(|s| s.id == id).map_or(id.to_string(), |s| s.name.clone());
        Ok(self
            .ledger
            .relationships()
            .await?
            .into_iter()
            .filter_map(|(from, to, kind, reason)| {
                let (verb, other) = match (from == id, to == id, kind) {
                    (true, _, Relationship::Supersedes) => ("supersedes", to),
                    (_, true, Relationship::Supersedes) => ("superseded by", from),
                    (true, _, Relationship::Extends) => ("extends", to),
                    (_, true, Relationship::Extends) => ("extended by", from),
                    (true, _, Relationship::ConflictsWith) => ("conflicts with", to),
                    (_, true, Relationship::ConflictsWith) => ("conflicts with", from),
                    (true, _, Relationship::RelatedTo) => ("related to", to),
                    (_, true, Relationship::RelatedTo) => ("related to", from),
                    _ => return None,
                };
                Some(format!("{verb} {}: {reason}", name(other)))
            })
            .collect())
    }

    /// The most recent audit entries across all skills.
    pub async fn recent_events(&self, limit: usize) -> Result<Vec<Event>> {
        self.ledger.events(None, limit).await
    }

    // ---- learning (V1, V2, V4)

    /// Act on a reviewer's decision: create a proposed skill, refine one (or
    /// propose the refinement), or record why nothing was learned.
    pub async fn apply(&self, decision: Decision, run: Option<Uuid>, evidence: &str) -> Result<Applied> {
        self.apply_as(decision, run, evidence, None).await
    }

    /// Act on a review of `owner`'s conversation (`None`: lyra's owner, whose
    /// skills are shared): new skills are theirs; only their own are refined.
    pub async fn apply_as(&self, decision: Decision, run: Option<Uuid>, evidence: &str, owner: Option<&str>) -> Result<Applied> {
        match decision {
            Decision::Ignore(why) => Ok(Applied::Ignored(why)),
            Decision::Create(c) => Ok(Applied::Created(self.learn_as(c, "conversation", run, owner).await?)),
            Decision::Update { skill, description, instructions, confidence, reason } => {
                let target = self.find_as(&skill, owner).await?;
                if target.owner.as_deref() != owner {
                    return Ok(Applied::Ignored(format!("{} is shared: only an admin changes it", target.name)));
                }
                if target.status == SkillStatus::Rejected {
                    return Ok(Applied::Ignored(format!("{} was rejected before", target.name)));
                }
                let description = if description.is_empty() { target.description.clone() } else { description };
                if target.description == description && target.instructions == instructions {
                    return Ok(Applied::Ignored(format!("{} already says this", target.name)));
                }
                let live = matches!(target.status, SkillStatus::Active | SkillStatus::Proposed);
                if self.settings.mode == Mode::Auto && live {
                    let version = self.update(target.id, &description, &instructions, &reason, run).await?;
                    let skill = self.get(target.id).await?.ok_or_else(|| anyhow!("skill vanished"))?;
                    return Ok(Applied::Updated { skill, version });
                }
                let change = Change::Update { skill: target.id, description, instructions };
                let proposal = Proposal::new(change, reason, evidence.to_string(), confidence, run);
                self.ledger.add_proposal(&proposal).await?;
                Ok(Applied::Proposed(proposal))
            }
        }
    }

    /// Save a new skill. It starts proposed; approval or evidence makes it active.
    pub async fn learn(&self, c: Candidate, source: &str, run: Option<Uuid>) -> Result<Skill> {
        self.learn_as(c, source, run, None).await
    }

    /// The same, for one person: theirs alone (`owner`), or shared with `None`.
    /// A name someone else already has gets a number.
    pub async fn learn_as(&self, c: Candidate, source: &str, run: Option<Uuid>, owner: Option<&str>) -> Result<Skill> {
        let now = Utc::now();
        let mut name = c.name.clone();
        if owner.is_some() {
            let taken: Vec<String> = self.list(None).await?.into_iter().map(|s| s.name).collect();
            let mut n = 2;
            while taken.contains(&name) {
                name = format!("{}-{n}", c.name);
                n += 1;
            }
        }
        let skill = Skill {
            id: Uuid::new_v4(),
            name,
            description: c.description,
            instructions: c.instructions,
            source: source.to_string(),
            confidence: c.confidence,
            status: SkillStatus::Proposed,
            created_at: now,
            updated_at: now,
            agent: None,
            owner: owner.map(str::to_string),
            usage: Default::default(),
        };
        let skill = self.store.create(skill).await?;
        let reason = if c.reason.is_empty() { "learned".to_string() } else { c.reason };
        self.ledger
            .add_version(skill.id, &skill.name, &skill.description, &skill.instructions, &reason, run)
            .await?;
        self.ledger
            .record(&Event::new("created", Some(skill.id), reason).states("none", skill.status).run(run))
            .await?;
        Ok(skill)
    }

    /// Change a skill's text. The current text is snapshotted first, so every
    /// update can be rolled back. Returns the new version number.
    pub async fn update(
        &self,
        id: Uuid,
        description: &str,
        instructions: &str,
        reason: &str,
        run: Option<Uuid>,
    ) -> Result<i64> {
        let mut skill = self.get(id).await?.ok_or_else(|| anyhow!("no skill with id {id}"))?;
        self.snapshot(&skill, "before update", run).await?;
        skill.description = description.trim().to_string();
        skill.instructions = instructions.trim().to_string();
        skill.updated_at = Utc::now();
        self.store.save(&skill).await?;
        let version = self
            .ledger
            .add_version(id, &skill.name, &skill.description, &skill.instructions, reason, run)
            .await?;
        self.ledger
            .record(&Event::new("updated", Some(id), reason).states(format!("v{}", version - 1), format!("v{version}")).run(run))
            .await?;
        Ok(version)
    }

    /// Make a skill belong to one subagent (`None`: global, for every agent).
    pub async fn set_agent(&self, id: Uuid, agent: Option<&str>) -> Result<Skill> {
        let mut skill = self.get(id).await?.ok_or_else(|| anyhow!("no skill with id {id}"))?;
        skill.agent = agent.map(str::to_string);
        skill.updated_at = Utc::now();
        self.store.save(&skill).await?;
        let reason = agent.map_or("now a global skill".to_string(), |a| format!("now belongs to the {a} agent"));
        self.ledger.record(&Event::new("assigned", Some(id), &reason)).await?;
        Ok(skill)
    }

    /// Restore an earlier version's text (the previous one by default). The
    /// rollback is itself a new version, so it can be undone too.
    pub async fn rollback(&self, id: Uuid, to: Option<i64>, run: Option<Uuid>) -> Result<i64> {
        let skill = self.get(id).await?.ok_or_else(|| anyhow!("no skill with id {id}"))?;
        self.snapshot(&skill, "before rollback", run).await?;
        let versions = self.ledger.versions(id).await?;
        let current = versions.first().map_or(0, |v| v.version);
        let target_version = to.unwrap_or(current - 1);
        let target = versions
            .iter()
            .find(|v| v.version == target_version)
            .ok_or_else(|| anyhow!("{} has no version {target_version}", skill.name))?;
        self.update(id, &target.description, &target.instructions, &format!("rolled back to v{target_version}"), run)
            .await
    }

    /// Record the current text as a version if it isn't the latest one
    /// (new skill, or edited by hand outside lyra).
    async fn snapshot(&self, skill: &Skill, reason: &str, run: Option<Uuid>) -> Result<bool> {
        let latest = self.ledger.latest_version(skill.id).await?;
        let same = latest
            .as_ref()
            .is_some_and(|v| v.description == skill.description && v.instructions == skill.instructions);
        if same {
            return Ok(false);
        }
        let reason = if latest.is_none() { "first seen" } else { reason };
        self.ledger
            .add_version(skill.id, &skill.name, &skill.description, &skill.instructions, reason, run)
            .await?;
        Ok(true)
    }

    /// Bring the version history up to date with the files: new files get a
    /// first version, files edited by hand get a new one. Returns notes.
    pub async fn sync(&self) -> Result<Vec<String>> {
        let mut notes = Vec::new();
        for skill in self.store.list(None).await? {
            let known = self.ledger.latest_version(skill.id).await?.is_some();
            if self.snapshot(&skill, "edited outside lyra", None).await? {
                let what = if known { "edited outside lyra" } else { "added" };
                self.ledger.record(&Event::new(what, Some(skill.id), what)).await?;
                notes.push(format!("{}: {what}", skill.name));
            }
        }
        Ok(notes)
    }

    // ---- lifecycle (V1, V6)

    /// Approve a proposed skill or a pending proposal; also reactivates a
    /// rejected or deprecated skill. Returns a description of what happened.
    pub async fn approve(&self, key: &str) -> Result<String> {
        if let Some(p) = self.find_proposal(key).await? {
            let note = self.apply_change(&p.change, &format!("approved: {}", p.reason), p.run_id).await?;
            self.ledger.set_proposal_status(p.id, ProposalStatus::Applied).await?;
            return Ok(note);
        }
        let skill = self.find(key).await?;
        if skill.status == SkillStatus::Active {
            bail!("{} is already active", skill.name);
        }
        self.set_status(&skill, SkillStatus::Active, "approved", "approved by the user", "", None).await?;
        Ok(format!("approved {} — it will be used from now on", skill.name))
    }

    /// Approve, reject or deprecate as `viewer`: their own skills (and
    /// proposals about them); shared ones only when `admin`.
    pub async fn decide_as(&self, what: &str, key: &str, viewer: Option<&str>, admin: bool) -> Result<String> {
        let editable = |s: &Skill| s.editable_by(viewer, admin);
        if let Some(p) = self.find_proposal(key).await? {
            let subject = match p.change.subject() {
                Some(id) => self.get(id).await?,
                None => None,
            };
            let ok = match &subject {
                Some(s) => editable(s),
                // Merges and splits of the collection: admins.
                None => admin,
            };
            if !ok {
                bail!("no proposal or skill matches {key:?}");
            }
        } else {
            let skill = self.find_as(key, viewer).await?;
            if !editable(&skill) {
                bail!("{} is shared: only an admin can {what} it", skill.name);
            }
        }
        match what {
            "approve" => self.approve(key).await,
            "reject" => self.reject(key).await,
            "deprecate" => self.deprecate(key, "deprecated by the user").await,
            "forget" => self.forget(key).await,
            other => bail!("can't {other} a skill"),
        }
    }

    /// Reject a proposed skill (for good) or a pending proposal.
    pub async fn reject(&self, key: &str) -> Result<String> {
        if let Some(p) = self.find_proposal(key).await? {
            self.ledger.set_proposal_status(p.id, ProposalStatus::Rejected).await?;
            let subject = match p.change.subject() {
                Some(id) => self.get(id).await?.map_or(id.to_string(), |s| s.name),
                None => "-".into(),
            };
            self.ledger
                .record(&Event::new("proposal_rejected", p.change.subject(), format!("{} of {subject}", p.change.kind())))
                .await?;
            return Ok(format!("rejected the proposed {} of {subject}", p.change.kind()));
        }
        let skill = self.find(key).await?;
        if skill.status != SkillStatus::Proposed {
            bail!("{} is {}; only proposed skills can be rejected (try /deprecate)", skill.name, skill.status);
        }
        self.set_status(&skill, SkillStatus::Rejected, "rejected", "rejected by the user", "", None).await?;
        Ok(format!("rejected {} — it won't be proposed again", skill.name))
    }

    /// Stop using a skill without deleting it.
    pub async fn deprecate(&self, key: &str, reason: &str) -> Result<String> {
        let skill = self.find(key).await?;
        if skill.status == SkillStatus::Deprecated {
            bail!("{} is already deprecated", skill.name);
        }
        self.set_status(&skill, SkillStatus::Deprecated, "deprecated", reason, "", None).await?;
        Ok(format!("deprecated {} — /approve {} brings it back", skill.name, &skill.id.to_string()[..8]))
    }

    /// Delete a skill's file. Its history stays in the ledger.
    pub async fn forget(&self, key: &str) -> Result<String> {
        let skill = self.find(key).await?;
        self.snapshot(&skill, "before delete", None).await?;
        self.store.delete(skill.id).await?;
        self.ledger
            .record(&Event::new("deleted", Some(skill.id), "deleted by the user").states(skill.status, "none"))
            .await?;
        Ok(format!("deleted {}", skill.name))
    }

    async fn set_status(
        &self,
        skill: &Skill,
        status: SkillStatus,
        kind: &str,
        reason: &str,
        evidence: &str,
        run: Option<Uuid>,
    ) -> Result<()> {
        let mut updated = skill.clone();
        updated.status = status;
        updated.updated_at = Utc::now();
        self.store.save(&updated).await?;
        let event = Event::new(kind, Some(skill.id), reason).states(skill.status, status).evidence(evidence).run(run);
        self.ledger.record(&event).await
    }

    /// Carry out an approved (or automatic) change.
    async fn apply_change(&self, change: &Change, reason: &str, run: Option<Uuid>) -> Result<String> {
        let name_of = async |id: Uuid| -> Result<Skill> {
            self.get(id).await?.ok_or_else(|| anyhow!("skill {id} no longer exists"))
        };
        match change {
            Change::Update { skill, description, instructions } => {
                let s = name_of(*skill).await?;
                let v = self.update(*skill, description, instructions, reason, run).await?;
                Ok(format!("updated {} to v{v}", s.name))
            }
            Change::Promote { skill } => {
                let s = name_of(*skill).await?;
                self.set_status(&s, SkillStatus::Active, "promoted", reason, "", run).await?;
                Ok(format!("promoted {}", s.name))
            }
            Change::Deprecate { skill } => {
                let s = name_of(*skill).await?;
                self.set_status(&s, SkillStatus::Deprecated, "deprecated", reason, "", run).await?;
                Ok(format!("deprecated {}", s.name))
            }
            Change::Merge { skills, into } => {
                let new = self.create_active(into, reason, run).await?;
                for id in skills {
                    let old = name_of(*id).await?;
                    self.set_status(&old, SkillStatus::Deprecated, "deprecated", &format!("merged into {}", new.name), "", run)
                        .await?;
                    self.ledger.relate(new.id, old.id, Relationship::Supersedes, reason).await?;
                }
                Ok(format!("merged {} skills into {}", skills.len(), new.name))
            }
            Change::Split { skill, parts } => {
                let old = name_of(*skill).await?;
                let mut names = Vec::new();
                for part in parts {
                    let new = self.create_active(part, reason, run).await?;
                    self.ledger.relate(new.id, old.id, Relationship::Supersedes, reason).await?;
                    names.push(new.name);
                }
                self.set_status(&old, SkillStatus::Deprecated, "deprecated", &format!("split into {}", names.join(", ")), "", run)
                    .await?;
                Ok(format!("split {} into {}", old.name, names.join(", ")))
            }
        }
    }

    /// A skill created by an approved merge or split: active straight away.
    async fn create_active(&self, new: &NewSkill, reason: &str, run: Option<Uuid>) -> Result<Skill> {
        let candidate = Candidate {
            name: crate::evaluator::kebab(&new.name),
            description: new.description.clone(),
            instructions: new.instructions.clone(),
            confidence: 1.0,
            reason: reason.to_string(),
        };
        let skill = self.learn(candidate, "curator", run).await?;
        self.set_status(&skill, SkillStatus::Active, "approved", reason, "", run).await?;
        Ok(Skill { status: SkillStatus::Active, ..skill })
    }

    /// Apply the lifecycle policy to the evidence: in auto mode promotions and
    /// deprecations happen (and are audited); in propose mode they become
    /// proposals. Returns notes for the log.
    pub async fn enforce(&self, run: Option<Uuid>) -> Result<Vec<String>> {
        if self.settings.mode == Mode::Off {
            return Ok(Vec::new());
        }
        let skills = self.list(None).await?;
        let pending = self.proposals().await?;
        let mut notes = Vec::new();
        for s in lifecycle::review(&skills, &self.settings.lifecycle) {
            let change = match s.transition {
                Transition::Promote => Change::Promote { skill: s.skill },
                Transition::Deprecate => Change::Deprecate { skill: s.skill },
            };
            if self.settings.mode == Mode::Auto {
                let skill = self.get(s.skill).await?.ok_or_else(|| anyhow!("skill vanished"))?;
                let (status, kind) = match s.transition {
                    Transition::Promote => (SkillStatus::Active, "promoted"),
                    Transition::Deprecate => (SkillStatus::Deprecated, "deprecated"),
                };
                self.set_status(&skill, status, kind, &format!("automatic: {}", s.reason), &s.evidence, run)
                    .await?;
                notes.push(format!("{kind} {} ({})", s.name, s.reason));
            } else if !pending.iter().any(|p| p.change == change) {
                let p = Proposal::new(change, s.reason.clone(), s.evidence.clone(), 1.0, run);
                self.ledger.add_proposal(&p).await?;
                notes.push(format!("proposed: {} {} ({})", p.change.kind(), s.name, s.reason));
            }
        }
        Ok(notes)
    }

    // ---- usage (V3)

    /// Note which skills were in the prompt for a run.
    pub async fn record_usage(&self, run: Uuid, skills: &[Uuid]) -> Result<()> {
        self.ledger.record_usage(run, skills).await
    }

    /// Record how a run went for the skills it used, then apply the lifecycle
    /// policy to the new evidence. `explicit` outcomes (from the user) replace
    /// inferred ones. Returns notes for the log.
    pub async fn record_outcome(&self, run: Uuid, outcome: SkillOutcome, explicit: bool) -> Result<Vec<String>> {
        let affected = self.ledger.set_outcome(run, outcome, explicit).await?;
        if affected.is_empty() {
            return Ok(Vec::new());
        }
        let skills = self.list(None).await?;
        let names: Vec<&str> =
            affected.iter().filter_map(|id| skills.iter().find(|s| s.id == *id).map(|s| s.name.as_str())).collect();
        let mut notes = vec![format!("{outcome}: {}", names.join(", "))];
        notes.extend(self.enforce(Some(run)).await?);
        Ok(notes)
    }

    // ---- curation (V7)

    /// The checks that need no model: near-duplicates and stale skills.
    pub async fn review_collection(&self) -> Result<Report> {
        let skills = self.list(None).await?;
        let name = |id: Uuid| skills.iter().find(|s| s.id == id).map_or("?".into(), |s| s.name.clone());
        Ok(Report {
            duplicates: curator::duplicates(&skills, self.settings.duplicate_threshold)
                .into_iter()
                .map(|(a, b, sim)| (name(a), name(b), sim))
                .collect(),
            stale: lifecycle::stale(&skills, &self.settings.lifecycle, Utc::now())
                .into_iter()
                .map(|s| s.name.clone())
                .collect(),
        })
    }

    /// Turn a curator plan into proposals (merges and splits always need
    /// approval) and flagged conflicts. Returns notes for the log.
    pub async fn apply_plan(&self, plan: Plan, run: Option<Uuid>) -> Result<Vec<String>> {
        let skills = self.list(None).await?;
        let by_name: HashMap<&str, &Skill> = skills.iter().map(|s| (s.name.as_str(), s)).collect();
        let ids = |names: &[String]| -> Option<Vec<Uuid>> {
            names.iter().map(|n| by_name.get(n.as_str()).map(|s| s.id)).collect()
        };
        let mut notes = Vec::new();
        for m in plan.merges {
            let Some(skills) = ids(&m.skills).filter(|ids| ids.len() >= 2) else {
                notes.push(format!("skipped merge of {}: unknown skill", m.skills.join(", ")));
                continue;
            };
            let into = NewSkill { name: m.name, description: m.description, instructions: m.instructions };
            if into.instructions.trim().is_empty() || crate::evaluator::looks_secret(&into.instructions) {
                continue;
            }
            let p = Proposal::new(Change::Merge { skills, into }, m.reason, "curator review".into(), 0.8, run);
            self.ledger.add_proposal(&p).await?;
            notes.push(format!("proposed merging {}", m.skills.join(" + ")));
        }
        for sp in plan.splits {
            let Some(skill) = by_name.get(sp.skill.as_str()) else { continue };
            if sp.parts.len() < 2 {
                continue;
            }
            let p = Proposal::new(
                Change::Split { skill: skill.id, parts: sp.parts },
                sp.reason,
                "curator review".into(),
                0.8,
                run,
            );
            self.ledger.add_proposal(&p).await?;
            notes.push(format!("proposed splitting {}", sp.skill));
        }
        for c in plan.conflicts {
            let Some(found) = ids(&c.skills).filter(|ids| ids.len() == 2) else { continue };
            self.ledger.relate(found[0], found[1], Relationship::ConflictsWith, &c.reason).await?;
            self.ledger
                .record(&Event::new("conflict_flagged", Some(found[0]), c.reason.clone()).run(run))
                .await?;
            notes.push(format!("conflict: {} — {}", c.skills.join(" vs "), c.reason));
        }
        self.ledger.record(&Event::new("curated", None, format!("{} changes suggested", notes.len())).run(run)).await?;
        Ok(notes)
    }

    /// When the collection was last curated.
    pub async fn last_curated(&self) -> Result<Option<DateTime<Utc>>> {
        Ok(self.ledger.last_event("curated").await?.map(|e| e.created_at))
    }

    pub async fn health(&self) -> Result<Health> {
        let skills = self.list(None).await?;
        let count = |status| skills.iter().filter(|s| s.status == status).count();
        let active: Vec<&Skill> = skills.iter().filter(|s| s.status == SkillStatus::Active).collect();
        let known: Vec<f32> =
            active.iter().filter(|s| s.usage.completed() > 0).map(|s| s.usage.reliability()).collect();
        let report = self.review_collection().await?;
        let conflicts = self
            .ledger
            .relationships()
            .await?
            .iter()
            .filter(|(_, _, kind, _)| *kind == Relationship::ConflictsWith)
            .count();
        Ok(Health {
            total: skills.len(),
            active: active.len(),
            proposed: count(SkillStatus::Proposed),
            deprecated: count(SkillStatus::Deprecated),
            rejected: count(SkillStatus::Rejected),
            pending_proposals: self.proposals().await?.len(),
            duplicate_candidates: report.duplicates.len(),
            conflicts,
            average_reliability: (!known.is_empty()).then(|| known.iter().sum::<f32>() / known.len() as f32),
            never_used: active.iter().filter(|s| s.usage.use_count == 0).count(),
            failing: active
                .iter()
                .filter(|s| s.usage.completed() >= 2 && s.usage.failure_count > s.usage.success_count)
                .count(),
            stale: report.stale.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curator::{ConflictPlan, MergePlan};
    use crate::evaluator::{Decision, parse};
    use crate::files::tests::temp_dir;

    async fn manager(mode: Mode) -> SkillManager {
        let settings = Settings { mode, ..Settings::default() };
        SkillManager::open(&temp_dir("manager"), settings).await.unwrap()
    }

    fn candidate(name: &str, confidence: f32) -> Candidate {
        Candidate {
            name: name.into(),
            description: "When committing Rust code".into(),
            instructions: "Run cargo fmt --check, cargo clippy and cargo test before every commit.".into(),
            confidence,
            reason: "user correction".into(),
        }
    }

    fn decision(json: &str, existing: &[&str]) -> Decision {
        let existing: Vec<String> = existing.iter().map(|s| s.to_string()).collect();
        parse(json).unwrap().decide(0.6, &existing)
    }

    /// The doc's scenario: correction → skill → approval → retrieval → success.
    #[tokio::test]
    async fn learn_approve_use_and_track() {
        let m = manager(Mode::Propose).await;
        let skill = m.learn(candidate("rust-commit", 0.9), "conversation", None).await.unwrap();
        assert_eq!(skill.status, SkillStatus::Proposed);
        assert!(m.search("commit my rust code", 3).await.unwrap().is_empty(), "proposals aren't used");

        m.approve("rust-commit").await.unwrap();
        let found = m.search("commit my rust code", 3).await.unwrap();
        assert_eq!(found[0].skill.name, "rust-commit");
        assert!(!found[0].trial);

        let run = Uuid::new_v4();
        m.record_usage(run, &[skill.id]).await.unwrap();
        let notes = m.record_outcome(run, SkillOutcome::Success, false).await.unwrap();
        assert_eq!(notes[0], "success: rust-commit");
        let s = m.get(skill.id).await.unwrap().unwrap();
        assert_eq!((s.usage.use_count, s.usage.success_count), (1, 1));

        let (versions, events) = m.history(skill.id).await.unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(events.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(), ["approved", "created"]);
    }

    #[tokio::test]
    async fn personal_skills_are_their_owners_alone() {
        let m = manager(Mode::Propose).await;
        let shared = m.learn(candidate("rust-commit", 0.9), "conversation", None).await.unwrap();
        let mine = m.learn_as(candidate("rust-commit", 0.9), "conversation", None, Some("dana")).await.unwrap();
        assert_eq!((mine.name.as_str(), mine.owner.as_deref()), ("rust-commit-2", Some("dana")), "a name already taken gets a number");
        assert!(shared.visible_to(Some("dana")) && mine.visible_to(Some("dana")));
        assert!(!mine.visible_to(None) && !mine.visible_to(Some("juan")), "nobody else sees it, not even the owner");
        // Dana approves her own; not the shared one, and Juan can't touch hers.
        assert!(m.decide_as("approve", "rust-commit", Some("dana"), false).await.unwrap_err().to_string().contains("only an admin"));
        assert!(m.decide_as("approve", "rust-commit-2", Some("juan"), false).await.is_err());
        assert!(m.decide_as("approve", "rust-commit-2", None, true).await.is_err(), "an admin can't approve someone's personal skill");
        m.decide_as("approve", "rust-commit-2", Some("dana"), false).await.unwrap();
        assert_eq!(m.get(mine.id).await.unwrap().unwrap().status, SkillStatus::Active);
        m.decide_as("approve", "rust-commit", None, true).await.unwrap();
        // Her conversations' reviews may refine hers, not the shared one.
        let d = decision(r#"{"action":"update","skill":"rust-commit","description":"x","instructions":"Always run cargo test.","confidence":0.8,"reason":"r"}"#, &["rust-commit"]);
        assert!(matches!(m.apply_as(d, None, "e", Some("dana")).await.unwrap(), Applied::Ignored(_)));
    }

    #[tokio::test]
    async fn updates_are_versioned_and_reversible() {
        let m = manager(Mode::Auto).await;
        let skill = m.learn(candidate("rust-commit", 0.9), "conversation", None).await.unwrap();
        let d = decision(
            r#"{"action":"update","skill":"rust-commit","description":"When committing Rust code","instructions":"Run cargo fmt --check, cargo clippy, cargo test and cargo doc before every commit.","confidence":0.8,"reason":"added cargo doc"}"#,
            &["rust-commit"],
        );
        let Applied::Updated { skill: updated, version } = m.apply(d, None, "").await.unwrap() else {
            panic!("auto mode applies updates")
        };
        assert_eq!(version, 2);
        assert!(updated.instructions.contains("cargo doc"));

        let v = m.rollback(skill.id, None, None).await.unwrap();
        assert_eq!(v, 3, "a rollback is a new version");
        let back = m.get(skill.id).await.unwrap().unwrap();
        assert!(!back.instructions.contains("cargo doc"));
        let (versions, _) = m.history(skill.id).await.unwrap();
        assert_eq!(versions[0].change_reason, "rolled back to v1");
    }

    #[tokio::test]
    async fn propose_mode_turns_updates_into_proposals() {
        let m = manager(Mode::Propose).await;
        m.learn(candidate("rust-commit", 0.9), "conversation", None).await.unwrap();
        let d = decision(
            r#"{"action":"update","skill":"rust-commit","description":"d","instructions":"new steps","confidence":0.8,"reason":"r"}"#,
            &["rust-commit"],
        );
        let Applied::Proposed(p) = m.apply(d, None, "evidence").await.unwrap() else { panic!("expected proposal") };
        assert!(!m.find("rust-commit").await.unwrap().instructions.contains("new steps"), "not applied yet");

        let note = m.approve(&p.id.to_string()[..8]).await.unwrap();
        assert_eq!(note, "updated rust-commit to v2");
        assert_eq!(m.find("rust-commit").await.unwrap().instructions, "new steps");
        assert!(m.proposals().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn auto_mode_trials_and_promotes_on_evidence() {
        let m = manager(Mode::Auto).await;
        let confident = m.learn(candidate("rust-commit", 0.9), "conversation", None).await.unwrap();
        m.learn(candidate("unsure-commit", 0.7), "conversation", None).await.unwrap();

        let found = m.search("commit rust code", 3).await.unwrap();
        assert_eq!(found.len(), 1, "only confident proposals go on trial");
        assert!(found[0].trial);

        for _ in 0..2 {
            let run = Uuid::new_v4();
            m.record_usage(run, &[confident.id]).await.unwrap();
            m.record_outcome(run, SkillOutcome::Success, false).await.unwrap();
        }
        assert_eq!(m.get(confident.id).await.unwrap().unwrap().status, SkillStatus::Active);
        let (_, events) = m.history(confident.id).await.unwrap();
        assert_eq!(events[0].kind, "promoted");
        assert!(events[0].evidence.contains("2 succeeded"), "{}", events[0].evidence);
    }

    #[tokio::test]
    async fn failing_skills_are_deprecated_not_deleted() {
        let m = manager(Mode::Auto).await;
        let s = m.learn(candidate("flaky", 0.9), "conversation", None).await.unwrap();
        m.approve("flaky").await.unwrap();
        for outcome in [SkillOutcome::Failure; 6] {
            let run = Uuid::new_v4();
            m.record_usage(run, &[s.id]).await.unwrap();
            m.record_outcome(run, outcome, false).await.unwrap();
        }
        let s = m.get(s.id).await.unwrap().unwrap();
        assert_eq!(s.status, SkillStatus::Deprecated);
        assert!(m.search("commit rust", 3).await.unwrap().is_empty());
        m.approve("flaky").await.unwrap();
        assert_eq!(m.get(s.id).await.unwrap().unwrap().status, SkillStatus::Active, "reversible");
    }

    #[tokio::test]
    async fn propose_mode_proposes_lifecycle_changes_once() {
        let m = manager(Mode::Propose).await;
        let s = m.learn(candidate("rust-commit", 0.9), "conversation", None).await.unwrap();
        for _ in 0..3 {
            let run = Uuid::new_v4();
            m.record_usage(run, &[s.id]).await.unwrap();
            m.record_outcome(run, SkillOutcome::Success, true).await.unwrap();
        }
        let pending = m.proposals().await.unwrap();
        assert_eq!(pending.len(), 1, "one promotion proposal, not one per outcome");
        assert_eq!(pending[0].change, Change::Promote { skill: s.id });
        assert_eq!(m.get(s.id).await.unwrap().unwrap().status, SkillStatus::Proposed);
    }

    #[tokio::test]
    async fn hand_edits_are_versioned_by_sync() {
        let dir = temp_dir("sync");
        let m = SkillManager::open(&dir, Settings::default()).await.unwrap();
        std::fs::write(dir.join("tea.md"), "Brew at 80°C.").unwrap();
        assert_eq!(m.sync().await.unwrap(), ["tea: added"]);
        assert!(m.sync().await.unwrap().is_empty());
        std::fs::write(dir.join("tea.md"), "Brew at 75°C.").unwrap();
        assert_eq!(m.sync().await.unwrap(), ["tea: edited outside lyra"]);
        let tea = m.find("tea").await.unwrap();
        assert_eq!(m.history(tea.id).await.unwrap().0.len(), 2);
    }

    #[tokio::test]
    async fn curation_proposes_merges_and_flags_conflicts() {
        let m = manager(Mode::Auto).await;
        let a = m.learn(candidate("commit-a", 0.9), "conversation", None).await.unwrap();
        let b = m.learn(candidate("commit-b", 0.9), "conversation", None).await.unwrap();
        let report = m.review_collection().await.unwrap();
        assert_eq!(report.duplicates.len(), 1, "same wording");

        let plan = Plan {
            merges: vec![MergePlan {
                skills: vec!["commit-a".into(), "commit-b".into()],
                name: "rust-commit".into(),
                description: "When committing".into(),
                instructions: "fmt, clippy, test".into(),
                reason: "duplicates".into(),
            }],
            conflicts: vec![ConflictPlan { skills: vec!["commit-a".into(), "commit-b".into()], reason: "differ".into() }],
            ..Plan::default()
        };
        let notes = m.apply_plan(plan, None).await.unwrap();
        assert_eq!(notes.len(), 2);
        assert_eq!(m.get(a.id).await.unwrap().unwrap().status, SkillStatus::Proposed, "merges wait for approval");

        let merge = m.proposals().await.unwrap().remove(0);
        m.approve(&merge.id.to_string()).await.unwrap();
        assert_eq!(m.find("rust-commit").await.unwrap().status, SkillStatus::Active);
        assert_eq!(m.get(a.id).await.unwrap().unwrap().status, SkillStatus::Deprecated);
        assert_eq!(m.get(b.id).await.unwrap().unwrap().status, SkillStatus::Deprecated);

        let rels = m.relationships(a.id).await.unwrap();
        assert!(rels.iter().any(|r| r.starts_with("superseded by rust-commit")), "{rels:?}");
        assert!(rels.iter().any(|r| r.starts_with("conflicts with commit-b")), "{rels:?}");

        let health = m.health().await.unwrap();
        assert_eq!((health.active, health.deprecated, health.conflicts), (1, 2, 1));
        assert!(m.last_curated().await.unwrap().is_some());
    }
}

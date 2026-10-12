//! Self-learning glue between the chat and the `SkillManager`: picks skills for
//! each message and records their use, reads the user's reactions as outcomes,
//! asks the chat model to review turns and curate the collection, and backs
//! the skill commands. Policy and persistence belong to the manager.

use std::sync::Arc;
use std::time::Duration;

use lyra_learning::curator::{self, Health};
use lyra_learning::evaluator::{self, Turn};
use lyra_learning::proposal::Change;
use lyra_learning::{Applied, Mode, Ranked, Skill, SkillManager, SkillOutcome, SkillStatus, Uuid};
use serde_json::{Value, json};
use tokio::runtime::Handle;

use crate::stats::Usage;

/// What the skills panel shows.
pub struct SkillsSnapshot {
    pub mode: Mode,
    pub active: Vec<Skill>,
    pub proposed: Vec<Skill>,
    /// Pending changes, one line each: `abcd1234 update rust-commit`.
    pub proposals: Vec<String>,
    pub health: Health,
    /// Skill files that couldn't be read.
    pub errors: Vec<String>,
}

pub struct Learning {
    manager: Arc<SkillManager>,
    runtime: Handle,
    /// The skills folder, for messages.
    dir: String,
    /// The skills' meaning as vectors (4.1), when an embedding model is set up.
    index: std::sync::RwLock<Option<Arc<SkillIndex>>>,
}

/// Every skill's text embedded and kept in LanceDB (`skills/index/`), so a
/// request finds the skills that mean the same thing, not only those sharing
/// its words. The skill files stay what they are; this is only an index.
pub struct SkillIndex {
    index: lyra_capabilities::index::CapabilityIndex,
    embedder: Arc<dyn lyra_memory::EmbeddingProvider>,
    /// What was indexed last (so unchanged skills aren't embedded again).
    synced: std::sync::Mutex<u64>,
}

impl SkillIndex {
    pub async fn open(dir: &std::path::Path, embedder: Arc<dyn lyra_memory::EmbeddingProvider>) -> anyhow::Result<Self> {
        Ok(Self { index: lyra_capabilities::index::CapabilityIndex::open(dir).await?, embedder, synced: Default::default() })
    }
}

/// A finished model call (review or curation): what came of it, and the
/// tokens it used (kept even when the reply couldn't be used).
pub struct Review<T> {
    pub outcome: Result<T, String>,
    pub usage: Option<Usage>,
}

impl Learning {
    pub fn new(manager: Arc<SkillManager>, runtime: Handle, dir: String) -> Self {
        Self { manager, runtime, dir, index: Default::default() }
    }

    /// Use meaning (vectors) to find skills from now on.
    pub fn set_index(&self, index: Option<Arc<SkillIndex>>) {
        *self.index.write().unwrap_or_else(|e| e.into_inner()) = index;
    }

    /// Each skill's similarity to `message` (indexing what changed first);
    /// `None` without an index or when the embedding model can't be reached.
    fn similarity(&self, message: &str) -> Option<std::collections::HashMap<Uuid, f32>> {
        let idx = self.index.read().unwrap_or_else(|e| e.into_inner()).clone()?;
        let skills = self.run(self.manager.list(None)).ok()?;
        let rows: Vec<(String, String)> = skills
            .iter()
            .filter(|s| matches!(s.status, SkillStatus::Active | SkillStatus::Proposed))
            .map(|s| (s.id.to_string(), format!("{}: {}\n{}", s.name, s.description, s.instructions.chars().take(600).collect::<String>())))
            .collect();
        let hash = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            rows.hash(&mut h);
            h.finish()
        };
        let e = idx.embedder.clone();
        let run = async {
            let mut synced = idx.synced.lock().unwrap_or_else(|e| e.into_inner()).to_owned();
            if synced != hash {
                let missing = idx.index.sync(&rows, Some((e.model(), e.dimensions()))).await?;
                for chunk in missing.chunks(32) {
                    let texts: Vec<String> = chunk.iter().map(|(_, t)| t.clone()).collect();
                    let vectors = e.embed(&texts).await?;
                    let pairs: Vec<(String, Vec<f32>)> = chunk.iter().map(|(id, _)| id.clone()).zip(vectors).collect();
                    idx.index.set_vectors(e.model(), &pairs).await?;
                }
                synced = hash;
                *idx.synced.lock().unwrap_or_else(|e| e.into_inner()) = synced;
            }
            let v = e.embed_query(message).await?;
            let found = idx.index.search_vector(e.model(), &v, 50).await?;
            anyhow::Ok(found.into_iter().filter_map(|(id, sim)| Some((Uuid::parse_str(&id).ok()?, sim))).collect())
        };
        self.runtime.block_on(run).ok()
    }

    pub fn mode(&self) -> Mode {
        self.manager.settings().mode
    }

    fn run<T, E: std::fmt::Display>(&self, f: impl Future<Output = Result<T, E>>) -> Result<T, String> {
        self.runtime.block_on(f).map_err(|e| format!("{e:#}"))
    }

    /// Skills to add to the main agent's prompt for `message`, best first:
    /// global skills only (subagents' own skills stay theirs).
    pub fn relevant(&self, message: &str) -> Result<Vec<Ranked>, String> {
        self.relevant_for(None, message)
    }

    /// Skills for an agent: global ones and its own (A11, A15); and only the
    /// shared ones and the own of whoever this turn is for.
    ///
    /// With an embedding model, skills are found by meaning and only those
    /// above the relevance floor go in; without one, by their words. Of two
    /// skills known to conflict, only the better goes in, and all of them
    /// together stay within `max_skill_tokens` (4.1).
    pub fn relevant_for(&self, agent: Option<&str>, message: &str) -> Result<Vec<Ranked>, String> {
        let settings = self.manager.settings();
        let limit = settings.max_skills;
        let found = match self.similarity(message) {
            Some(rel) => self.run(self.manager.search_by(&rel, settings.relevance_floor, limit * 6))?,
            None => self.run(self.manager.search(message, limit * 6))?,
        };
        let viewer = viewer();
        let conflicts = self.run(self.manager.conflicts()).unwrap_or_default();
        let mut out: Vec<Ranked> = Vec::new();
        let mut tokens = 0usize;
        for r in found.into_iter().filter(|r| (r.skill.agent.is_none() || r.skill.agent.as_deref() == agent) && r.skill.visible_to(viewer.as_deref())) {
            // The loser of a known conflict stays out (the list is best first).
            if out.iter().any(|w| conflicts.iter().any(|(a, b)| (*a == w.skill.id && *b == r.skill.id) || (*b == w.skill.id && *a == r.skill.id))) {
                continue;
            }
            let size = approx_tokens(&format!("{} {} {}", r.skill.name, r.skill.description, r.skill.instructions)) as usize;
            if !out.is_empty() && tokens + size > settings.max_skill_tokens {
                continue;
            }
            tokens += size;
            out.push(r);
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    /// Make a skill an agent's own (or global again with `None`). Agents are
    /// everyone's: only shared skills (never someone's personal one).
    pub fn assign(&self, key: &str, agent: Option<&str>) -> Result<Skill, String> {
        let skill = self.run(self.manager.find_as(key, None))?;
        self.run(self.manager.set_agent(skill.id, agent))
    }

    /// Active shared skills (the capability registry is everyone's: a
    /// person's own skills stay out of it).
    pub fn active_skills(&self) -> Result<Vec<Skill>, String> {
        Ok(self.run(self.manager.list(Some(SkillStatus::Active)))?.into_iter().filter(|s| s.owner.is_none()).collect())
    }

    /// A skill by name or id prefix.
    pub fn find(&self, key: &str) -> Result<Skill, String> {
        self.run(self.manager.find(key))
    }

    /// Replace a skill's instructions (a new version, so it can be rolled back).
    pub fn refine(&self, key: &str, instructions: &str, reason: &str) -> Result<i64, String> {
        let skill = self.find(key)?;
        self.run(self.manager.update(skill.id, &skill.description, instructions, reason, None))
    }

    pub fn record_usage(&self, run: Uuid, skills: &[Uuid]) -> Result<(), String> {
        self.run(self.manager.record_usage(run, skills))
    }

    /// Record how a run went for the skills it used; returns notes (including
    /// any promotions or deprecations that follow).
    pub fn record_outcome(&self, run: Uuid, outcome: SkillOutcome, explicit: bool) -> Result<Vec<String>, String> {
        self.run(self.manager.record_outcome(run, outcome, explicit))
    }

    /// The same, for only the skills the reply followed (by name); the
    /// other skills in its prompt get no credit for it.
    pub fn record_outcome_for(&self, run: Uuid, followed: &[String], outcome: SkillOutcome, explicit: bool) -> Result<Vec<String>, String> {
        let ids: Vec<Uuid> = followed.iter().filter_map(|n| self.find(n.trim_end_matches(" (trial)")).ok().map(|s| s.id)).collect();
        self.run(self.manager.record_outcome_for(run, &ids, outcome, explicit))
    }

    /// Version skill files that are new or were edited by hand.
    pub fn sync(&self) -> Result<Vec<String>, String> {
        self.run(self.manager.sync())
    }

    /// Ask the chat model whether the conversation taught (or refined) a
    /// skill, and act on its answer.
    pub fn review(
        &self,
        url: &str,
        model: &str,
        trigger: &str,
        transcript: &str,
        run: Option<Uuid>,
        owner: Option<&str>,
    ) -> Review<Applied> {
        // The reviewer sees the shared skills and this person's own: nobody else's.
        let prepared = (|| -> Result<(String, Vec<String>), String> {
            let all: Vec<Skill> = self.run(self.manager.list(None))?.into_iter().filter(|s| s.visible_to(owner)).collect();
            let related: Vec<Skill> = self.run(self.manager.related(transcript, 9))?.into_iter().filter(|s| s.visible_to(owner)).take(3).collect();
            let others: Vec<String> = all
                .iter()
                .filter(|s| !related.iter().any(|r| r.id == s.id))
                .map(|s| s.name.clone())
                .collect();
            let names: Vec<String> = all.into_iter().map(|s| s.name).collect();
            Ok((evaluator::prompt(trigger, transcript, &related, &others), names))
        })();
        let (prompt, names) = match prepared {
            Ok(p) => p,
            Err(e) => return Review { outcome: Err(e), usage: None },
        };
        let (reply, usage) = match complete_light(url, model, evaluator::SYSTEM_PROMPT, &prompt) {
            Ok(r) => r,
            Err(e) => return Review { outcome: Err(e), usage: None },
        };
        let outcome = evaluator::parse(&reply).and_then(|verdict| {
            let decision = verdict.decide(self.manager.settings().min_confidence, &names);
            self.run(self.manager.apply_as(decision, run, trigger, owner))
        });
        Review { outcome, usage }
    }

    /// Review the whole collection: local checks, then the model's plan for
    /// merges, splits and conflicts. Returns notes for the log.
    pub fn curate(&self, url: &str, model: &str, run: Option<Uuid>) -> Review<Vec<String>> {
        let local = (|| -> Result<(lyra_learning::Report, Vec<Skill>), String> {
            let mut report = self.run(self.manager.review_collection())?;
            // The shared collection only: a person's own skills are theirs to
            // keep, and their names don't reach the shared curator either.
            let skills: Vec<Skill> = self.run(self.manager.list(None))?.into_iter().filter(|s| s.owner.is_none()).collect();
            let shared = |n: &String| skills.iter().any(|s| &s.name == n);
            report.duplicates.retain(|(a, b, _)| shared(a) && shared(b));
            report.stale.retain(|n| shared(n));
            Ok((report, skills))
        })();
        let (report, skills) = match local {
            Ok(x) => x,
            Err(e) => return Review { outcome: Err(e), usage: None },
        };
        let mut notes: Vec<String> = report.stale.iter().map(|n| format!("stale: {n} hasn't been used lately")).collect();
        let live = skills.iter().filter(|s| matches!(s.status, SkillStatus::Active | SkillStatus::Proposed)).count();
        if live < 2 {
            notes.push("fewer than 2 skills in use; nothing to merge or compare".into());
            let done = self.run(self.manager.apply_plan(Default::default(), run));
            return Review { outcome: done.map(|n| [notes, n].concat()), usage: None };
        }
        let prompt = curator::prompt(&skills, &report.duplicates);
        let (reply, usage) = match complete_light(url, model, curator::SYSTEM_PROMPT, &prompt) {
            Ok(r) => r,
            Err(e) => return Review { outcome: Err(e), usage: None },
        };
        let shared = self.share_lessons(url, model);
        let outcome = curator::parse(&reply)
            .and_then(|plan| self.run(self.manager.apply_plan(plan, run)))
            .map(|applied| [notes, applied, shared].concat());
        Review { outcome, usage }
    }

    /// Lessons two or more people learned on their own, and that worked for
    /// each, offered as one shared skill (4.3): written in general terms by
    /// the model (nobody's details), proposed, for an admin to approve. The
    /// personal ones stay theirs.
    pub fn share_lessons(&self, url: &str, model: &str) -> Vec<String> {
        let Ok(groups) = self.run(self.manager.sharing_candidates()) else { return Vec::new() };
        let mut notes = Vec::new();
        for group in groups.into_iter().take(3) {
            let texts: Vec<String> = group.iter().map(|s| format!("- When: {}\n  How: {}", s.description, s.instructions)).collect();
            let prompt = format!("{} people each learned this on their own, and it worked for them:\n{}", group.len(), texts.join("\n"));
            let Ok((reply, _)) = complete_light(url, model, SHARE_PROMPT, &prompt) else { continue };
            let Some(v) = reply.find('{').zip(reply.rfind('}')).and_then(|(a, b)| serde_json::from_str::<Value>(reply.get(a..=b)?).ok()) else { continue };
            let (Some(name), Some(when), Some(how)) = (v["name"].as_str(), v["description"].as_str(), v["instructions"].as_str()) else { continue };
            if lyra_evolution::safety_scan(how).is_some() {
                continue;
            }
            let c = evaluator::Candidate { name: name.trim().to_string(), description: when.trim().to_string(), instructions: how.trim().to_string(), confidence: 0.8, reason: format!("learned on their own by {} people, and it worked for each", group.len()) };
            match self.run(self.manager.learn_as(c, "shared from people's own skills", None, None)) {
                Ok(skill) => notes.push(format!("offered for sharing: {} (learned by {} people) · an admin approves it: /approve {}", skill.name, group.len(), skill.name)),
                Err(e) => notes.push(format!("couldn't offer a shared skill: {e}")),
            }
        }
        notes
    }

    /// Whether a scheduled curation is due.
    pub fn curation_due(&self, every_days: Option<i64>) -> bool {
        let Some(days) = every_days else { return false };
        match self.run(self.manager.last_curated()) {
            Ok(Some(last)) => chrono::Utc::now() - last >= chrono::Duration::days(days),
            Ok(None) => self.run(self.manager.list(None)).is_ok_and(|s| s.len() >= 2),
            Err(_) => false,
        }
    }

    pub fn snapshot(&self) -> Result<SkillsSnapshot, String> {
        // The owner's TUI: the shared skills.
        let all: Vec<Skill> = self.run(self.manager.list(None))?.into_iter().filter(|s| s.owner.is_none()).collect();
        let of = |status| all.iter().filter(|s| s.status == status).cloned().collect::<Vec<_>>();
        Ok(SkillsSnapshot {
            mode: self.mode(),
            active: of(SkillStatus::Active),
            proposed: of(SkillStatus::Proposed),
            proposals: self
                .run(self.manager.proposals())?
                .iter()
                .map(|p| format!("{} {}", short_id(p.id), self.describe_change(&p.change, &all)))
                .collect(),
            health: self.run(self.manager.health())?,
            errors: self.manager.load_errors(),
        })
    }

    /// The app's Skills page: every skill and the changes waiting for review.
    /// The Skills page for `viewer`: the shared skills and their own, and
    /// which they may approve (`mine`: theirs alone; `can_decide`).
    pub fn page_for(&self, viewer: Option<&str>, admin: bool) -> Result<Value, String> {
        let all: Vec<Skill> = self.run(self.manager.list(None))?.into_iter().filter(|s| s.visible_to(viewer)).collect();
        let proposals = self.visible_proposals(viewer, admin, &all)?;
        Ok(json!({
            "mode": self.mode().as_str(),
            "skills": all.iter().map(|s| json!({
                "id": short_id(s.id), "name": s.name, "description": s.description, "instructions": s.instructions,
                "status": s.status.as_str(), "confidence": s.confidence, "agent": s.agent, "record": track_record(s),
                "updated": s.updated_at.to_rfc3339(), "mine": s.owner.is_some(), "can_decide": s.editable_by(viewer, admin),
            })).collect::<Vec<_>>(),
            "proposals": proposals.iter().map(|p| json!({
                "id": short_id(p.id), "change": self.describe_change(&p.change, &all), "reason": p.reason,
                "detail": match &p.change { Change::Update { instructions, .. } => instructions.clone(), _ => String::new() },
            })).collect::<Vec<_>>(),
        }))
    }

    /// Pending proposals `viewer` may decide: about their own skills, or (admins) shared ones.
    fn visible_proposals(&self, viewer: Option<&str>, admin: bool, visible: &[Skill]) -> Result<Vec<lyra_learning::proposal::Proposal>, String> {
        Ok(self
            .run(self.manager.proposals())?
            .into_iter()
            .filter(|p| match p.change.subject() {
                Some(id) => visible.iter().any(|s| s.id == id && s.editable_by(viewer, admin)),
                None => admin && viewer.is_none(),
            })
            .collect())
    }

    /// Text for `/skills` for `viewer`: the shared skills and their own; what
    /// needs review in full, the rest briefly.
    pub fn describe_for(&self, viewer: Option<&str>, admin: bool) -> Result<String, String> {
        let all: Vec<Skill> = self.run(self.manager.list(None))?.into_iter().filter(|s| s.visible_to(viewer)).collect();
        let proposals = self.visible_proposals(viewer, admin, &all)?;
        let mut out = vec![format!(
            "Skill files: {} (one <name>.md per skill; edit them or add your own) · mode {}",
            self.dir,
            self.mode().as_str()
        )];
        for error in self.manager.load_errors() {
            out.push(format!("  unreadable: {error}"));
        }
        if all.is_empty() {
            out.push(
                "No skills yet. Lessons from corrections and multi-step work show up here as \
                 proposals; /learn reviews the conversation now."
                    .into(),
            );
            return Ok(out.join("\n"));
        }
        if !proposals.is_empty() {
            out.push(format!("changes to review ({}):", proposals.len()));
            for p in &proposals {
                out.push(format!("  {} {}", short_id(p.id), self.describe_change(&p.change, &all)));
                out.push(format!("      why: {}", p.reason));
                if let Change::Update { instructions, .. } = &p.change {
                    out.extend(instructions.lines().map(|l| format!("      + {l}")));
                }
            }
        }
        for status in [SkillStatus::Proposed, SkillStatus::Active, SkillStatus::Deprecated, SkillStatus::Rejected] {
            let group: Vec<&Skill> = all.iter().filter(|s| s.status == status).collect();
            if group.is_empty() {
                continue;
            }
            out.push(format!("{status} ({}):", group.len()));
            for s in group {
                out.push(format!("  {} {} — {}", short_id(s.id), s.name, s.description));
                out.push(format!("      {}", track_record(s)));
                if status == SkillStatus::Proposed {
                    out.extend(s.instructions.lines().map(|l| format!("      {l}")));
                }
            }
        }
        out.push(
            "/approve <id> · /reject <id> · /deprecate <id> · /history <id> · /rollback <id> [version]".into(),
        );
        Ok(out.join("\n"))
    }

    fn describe_change(&self, change: &Change, all: &[Skill]) -> String {
        let name = |id: &Uuid| all.iter().find(|s| s.id == *id).map_or("?".to_string(), |s| s.name.clone());
        match change {
            Change::Update { skill, .. } => format!("update {}", name(skill)),
            Change::Promote { skill } => format!("promote {}", name(skill)),
            Change::Deprecate { skill } => format!("deprecate {}", name(skill)),
            Change::Merge { skills, into } => {
                format!("merge {} into {}", skills.iter().map(name).collect::<Vec<_>>().join(" + "), into.name)
            }
            Change::Split { skill, parts } => format!(
                "split {} into {}",
                name(skill),
                parts.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
            ),
        }
    }

    /// Text for `/history`: versions and audit trail, of a skill `viewer` may
    /// see (shared, or theirs; `None`: lyra's owner, the shared ones). Someone
    /// else's personal skill isn't found: its evidence is from their conversations.
    pub fn history(&self, key: &str, viewer: Option<&str>) -> Result<String, String> {
        let skill = self.run(self.manager.find_as(key, viewer))?;
        let (versions, events) = self.run(self.manager.history(skill.id))?;
        let mut out = vec![format!("{} ({}) — {}", skill.name, skill.status, track_record(&skill))];
        let relationships = self.run(self.manager.relationships(skill.id))?;
        if !relationships.is_empty() {
            out.push("relationships:".into());
            out.extend(relationships.iter().map(|r| format!("  {r}")));
        }
        out.push("versions:".into());
        for v in &versions {
            out.push(format!("  v{} {} — {}", v.version, v.created_at.format("%Y-%m-%d %H:%M"), v.change_reason));
        }
        out.push("events:".into());
        for e in &events {
            let states = match (&e.from_state, &e.to_state) {
                (Some(a), Some(b)) => format!(" ({a} → {b})"),
                _ => String::new(),
            };
            out.push(format!("  {} {}{states}: {}", e.created_at.format("%Y-%m-%d %H:%M"), e.kind, e.reason));
            if !e.evidence.is_empty() {
                out.push(format!("      evidence: {}", e.evidence));
            }
        }
        Ok(out.join("\n"))
    }

    /// `/rollback <skill> [version]`: a skill `viewer` may change (their own;
    /// a shared one only an admin).
    pub fn rollback(&self, args: &str, viewer: Option<&str>, admin: bool) -> Result<String, String> {
        let mut parts = args.split_whitespace();
        let key = parts.next().ok_or("usage: /rollback <skill> [version]")?;
        let to = parts
            .next()
            .map(|v| v.trim_start_matches('v').parse::<i64>().map_err(|_| format!("bad version {v:?}")))
            .transpose()?;
        let skill = self.run(self.manager.find_as(key, viewer))?;
        if !skill.editable_by(viewer, admin) {
            return Err(format!("{} is a shared skill: only an admin can roll it back", skill.name));
        }
        let v = self.run(self.manager.rollback(skill.id, to, None))?;
        Ok(format!("rolled {} back; now at v{v} (see /history {})", skill.name, skill.name))
    }

    /// A skill's procedure, for workflow steps in plans. Only active or
    /// proposed skills; rejected and deprecated ones aren't offered.
    pub fn instructions(&self, name: &str) -> Option<String> {
        let skill = self.run(self.manager.find_as(name, viewer().as_deref())).ok()?;
        matches!(skill.status, SkillStatus::Active | SkillStatus::Proposed).then_some(skill.instructions)
    }

    /// Approve, reject, deprecate or forget as `viewer` (`None`: lyra's
    /// owner): their own skills; shared ones only when `admin`.
    pub fn decide(&self, what: &str, key: &str, viewer: Option<&str>, admin: bool) -> Result<String, String> {
        self.run(self.manager.decide_as(what, key, viewer, admin))
    }
}

/// Whose skills a thread may use: `None` for lyra's owner (the shared ones),
/// else the person it works for (theirs too).
fn viewer() -> Option<String> {
    let u = crate::acting::current();
    (!crate::acting::is_owner(&u)).then_some(u)
}

/// `5 uses · 80% success · reliability 0.71 · confidence 0.85`.
const SHARE_PROMPT: &str = "\
You turn lessons several people learned on their own into one skill everyone can use. \
Write it in general terms: no names, addresses, accounts, file paths or other details of any one person. \
Reply with JSON only: {\"name\": \"kebab-case-name\", \"description\": \"when it applies\", \"instructions\": \"how to do it, as steps\"}.";

pub fn track_record(s: &Skill) -> String {
    let u = &s.usage;
    let rate = u.success_rate().map_or("no outcomes yet".into(), |r| format!("{:.0}% success", r * 100.0));
    format!(
        "{} use{} · {rate} · reliability {:.2} · confidence {:.2}",
        u.use_count,
        if u.use_count == 1 { "" } else { "s" },
        u.reliability(),
        s.confidence
    )
}

/// Short id shown in the UI and accepted by the commands.
pub fn short_id(id: Uuid) -> String {
    id.to_string()[..8].to_string()
}

/// Rough token count (about 4 characters per token), for display.
pub fn approx_tokens(text: &str) -> u64 {
    text.len() as u64 / 4
}

/// System prompt section listing the skills chosen for this message.
pub fn prompt_section(skills: &[Ranked]) -> String {
    let mut out = String::from(
        "# Learned skills\n\nProcedures learned in earlier sessions that look relevant to this \
         message. Follow them when they apply.",
    );
    for r in skills {
        let trial = if r.trial { " (trial: learned recently, not yet verified)" } else { "" };
        out += &format!(
            "\n\n## {}{trial}\n\nWhen: {}\n\n{}",
            r.skill.name, r.skill.description, r.skill.instructions
        );
    }
    out
}

/// Pick out the turn that just finished from the chat history (role, content)
/// pairs. `skills_used` is how many skills the reply before it used.
pub fn last_turn<'a>(history: &[(&'a str, &'a str)], skills_used: usize) -> Option<Turn<'a>> {
    let start = history.iter().rposition(|(role, _)| *role == "user")?;
    let after = &history[start + 1..];
    let tools: Vec<&str> =
        after.iter().filter(|(role, _)| *role == "tool").map(|(_, c)| *c).collect();
    Some(Turn {
        user: history[start].1,
        follows_reply: history[..start].iter().any(|(role, _)| *role == "assistant"),
        tool_calls: tools.len(),
        // Failed by their structure (`ok: false`, an `error`), not a mention of one.
        tool_errors: tools.iter().filter(|c| crate::outcomes::failure(c).is_some()).count(),
        skills_used,
    })
}

/// The recent conversation as plain text for the reviewer.
pub fn transcript(history: &[(&str, &str)], max_messages: usize) -> String {
    let start = history.len().saturating_sub(max_messages);
    history[start..]
        .iter()
        .map(|(role, content)| {
            let content: String = content.chars().take(1500).collect();
            format!("[{role}] {content}")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// How lyra's internal JSON calls are made (from the config).
#[derive(Clone, Copy)]
pub struct Structured {
    pub max_tokens: u32,
    pub thinking: bool,
}

static STRUCTURED: std::sync::RwLock<Structured> = std::sync::RwLock::new(Structured { max_tokens: 8192, thinking: true });

/// Set at startup and on reload.
pub fn configure(s: Structured) {
    *STRUCTURED.write().unwrap_or_else(|e| e.into_inner()) = s;
}

/// One non-streaming chat completion; returns the reply text and its usage.
/// The chat model's chat completions URL and name, for work done outside a
/// conversation that has no App at hand (the web reader). Set at start, on
/// `/reload` and `/model`.
static CHAT: std::sync::RwLock<Option<(String, String)>> = std::sync::RwLock::new(None);

pub(crate) fn set_chat(base_url: &str, model: &str) {
    *CHAT.write().unwrap_or_else(|e| e.into_inner()) = Some((format!("{}/chat/completions", base_url.trim_end_matches('/')), model.to_string()));
}

pub(crate) fn chat() -> Option<(String, String)> {
    CHAT.read().unwrap_or_else(|e| e.into_inner()).clone()
}

pub(crate) fn complete(
    url: &str,
    model: &str,
    system: &str,
    user: &str,
) -> Result<(String, Option<Usage>), String> {
    call(url, model, system, user, false, 0)
}

/// A small background job (memory capture, a skill review, mail triage…):
/// the first fallback that takes them (`background = true`) does it while the
/// main model is up, so the main one is left to the chats; if it fails, the
/// main model does it after all.
pub(crate) fn complete_light(
    url: &str,
    model: &str,
    system: &str,
    user: &str,
) -> Result<(String, Option<Usage>), String> {
    call(url, model, system, user, true, 0)
}

/// A small background job with a short answer: at most `max_tokens` (a
/// page's summary), so the fallback doesn't hold up the round writing more.
pub(crate) fn complete_light_short(url: &str, model: &str, system: &str, user: &str, max_tokens: u64) -> Result<(String, Option<Usage>), String> {
    call(url, model, system, user, true, max_tokens)
}

fn call(
    url: &str,
    model: &str,
    system: &str,
    user: &str,
    light: bool,
    cap: u64,
) -> Result<(String, Option<Usage>), String> {
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(600))
        .build()
        .map_err(|e| e.to_string())?;
    let mut body = json!({
        "model": model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
    });
    let s = *STRUCTURED.read().unwrap_or_else(|e| e.into_inner());
    if s.max_tokens > 0 {
        body["max_tokens"] = json!(s.max_tokens);
    }
    if cap > 0 {
        body["max_tokens"] = json!(cap);
    }
    if !s.thinking {
        body["chat_template_kwargs"] = json!({ "enable_thinking": false });
    }
    let started = std::time::Instant::now();
    let send = |url: &str, body: &Value| -> Result<Value, String> {
        let resp = client.post(url).json(body).send().map_err(|e| e.to_string())?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("{status}: {}", resp.text().unwrap_or_default().chars().take(500).collect::<String>()));
        }
        resp.json().map_err(|e| e.to_string())
    };
    // The main chat model down (or failing just now): the next in line does it.
    let (reply, used) = crate::fallback::call(url, model, light, |u, m| {
        body["model"] = json!(m);
        send(u, &body)
    })?;
    // lyra's own work for whoever this thread works for (capture, triage, briefings, reviews…).
    crate::usage::record_usage("background", &used, &reply["usage"], started.elapsed().as_millis() as u64);
    let text = reply["choices"][0]["message"]["content"]
        .as_str()
        .ok_or("model returned no content")?
        .to_string();
    Ok((text, serde_json::from_value(reply["usage"].clone()).ok()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_turn_counts_tools_and_errors() {
        let history = [
            ("user", "set it up"),
            ("assistant", "done"),
            ("user", "No, use the staging server instead"),
            ("assistant", ""),
            ("tool", r#"{"error":"connection refused"}"#),
            ("tool", r#"{"ok":true}"#),
            ("assistant", "fixed"),
        ];
        let turn = last_turn(&history, 2).unwrap();
        assert_eq!(turn.user, "No, use the staging server instead");
        assert!(turn.follows_reply);
        assert_eq!((turn.tool_calls, turn.tool_errors, turn.skills_used), (2, 1, 2));
    }

    #[test]
    fn skills_are_found_by_meaning_above_a_floor_without_a_conflicts_loser_and_within_a_budget() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let dir = std::env::temp_dir().join(format!("lyra-learn-meaning-{}", Uuid::new_v4()));
        let settings = lyra_learning::Settings { relevance_floor: 0.3, ..Default::default() };
        let manager = Arc::new(rt.block_on(SkillManager::open(&dir, settings)).unwrap());
        let skill = |name: &str, when: &str, how: &str, confidence: f32| {
            let c = lyra_learning::evaluator::Candidate { name: name.into(), description: when.into(), instructions: how.into(), confidence, reason: "r".into() };
            let s = rt.block_on(manager.learn_as(c, "conversation", None, None)).unwrap();
            rt.block_on(manager.approve(&s.name)).unwrap();
            s
        };
        let printer = skill("restart-printer", "When the office printer stops printing", "Power-cycle the printer, then clear the printer queue.", 0.9);
        let vpn = skill("vpn-reconnect", "When the VPN keeps dropping", "Re-import the VPN profile and reconnect.", 0.9);
        let other = skill("printer-queue-purge", "When printer jobs pile up", "Purge the printer queue from the print server.", 0.6);
        let l = Learning::new(manager.clone(), rt.handle().clone(), dir.display().to_string());
        let names = |l: &Learning, q: &str| l.relevant(q).unwrap().into_iter().map(|r| r.skill.name).collect::<Vec<_>>();
        // Words only (no index): whatever shares a word comes up.
        assert!(names(&l, "printer stopped printing").contains(&"restart-printer".to_string()));
        // By meaning: the printer skills, not the VPN one (under the floor).
        let fake = lyra_memory::embedding::FakeEmbedder::new("fake", 64);
        l.set_index(Some(Arc::new(rt.block_on(SkillIndex::open(&dir.join("index"), Arc::new(fake))).unwrap())));
        let found = names(&l, "the printer stopped printing again");
        assert!(found.contains(&printer.name) && !found.contains(&vpn.name), "{found:?}");
        assert!(found.contains(&other.name), "{found:?}");
        // Known to conflict: only the better one goes in.
        rt.block_on(manager.mark_conflict(printer.id, other.id, "one restarts, one purges")).unwrap();
        let found = names(&l, "the printer stopped printing again");
        assert_eq!(found, vec![printer.name.clone()], "the conflict's loser stays out");
        assert!(dir.join("index").exists(), "the vectors are kept in LanceDB beside the files");
        // A small budget: only the best fits.
        let tight = Arc::new(rt.block_on(SkillManager::open(&dir, lyra_learning::Settings { relevance_floor: 0.3, max_skill_tokens: 5, ..Default::default() })).unwrap());
        let l2 = Learning::new(tight, rt.handle().clone(), dir.display().to_string());
        assert_eq!(names(&l2, "printer stopped printing vpn dropping").len(), 1, "within max_skill_tokens");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn history_and_rollback_keep_to_whose_skill_it_is() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let dir = std::env::temp_dir().join(format!("lyra-learn-history-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let manager = Arc::new(rt.block_on(SkillManager::open(&dir, lyra_learning::Settings::default())).unwrap());
        let l = Learning::new(manager.clone(), rt.handle().clone(), dir.display().to_string());
        let c = |name: &str| lyra_learning::evaluator::Candidate { name: name.into(), description: "When committing Rust code".into(), instructions: "Run cargo test first.".into(), confidence: 0.9, reason: "a correction".into() };
        rt.block_on(manager.learn_as(c("shared-one"), "conversation", None, None)).unwrap();
        rt.block_on(manager.learn_as(c("danas-own"), "conversation", None, Some("dana"))).unwrap();
        // Dana sees hers and the shared one; Juan (and the owner) not hers.
        assert!(l.history("danas-own", Some("dana")).unwrap().contains("danas-own"));
        assert!(l.history("shared-one", Some("juan")).is_ok());
        assert!(l.history("danas-own", Some("juan")).is_err(), "another member's skill");
        assert!(l.history("danas-own", None).is_err(), "not the owner either: its evidence is Dana's");
        // Rollback: only what one may change.
        assert!(l.rollback("danas-own", Some("juan"), true).is_err());
        assert!(l.rollback("shared-one", Some("juan"), false).unwrap_err().contains("only an admin"));
        assert!(l.assign("danas-own", Some("ops")).is_err(), "agents get shared skills only");
        drop(l);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn transcript_keeps_the_most_recent_messages() {
        let history = [("user", "one"), ("assistant", "two"), ("user", "three")];
        assert_eq!(transcript(&history, 2), "[assistant] two\n\n[user] three");
    }
}

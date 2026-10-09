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
}

/// A finished model call (review or curation): what came of it, and the
/// tokens it used (kept even when the reply couldn't be used).
pub struct Review<T> {
    pub outcome: Result<T, String>,
    pub usage: Option<Usage>,
}

impl Learning {
    pub fn new(manager: Arc<SkillManager>, runtime: Handle, dir: String) -> Self {
        Self { manager, runtime, dir }
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
    pub fn relevant_for(&self, agent: Option<&str>, message: &str) -> Result<Vec<Ranked>, String> {
        let limit = self.manager.settings().max_skills;
        let found = self.run(self.manager.search(message, limit * 6))?;
        let viewer = viewer();
        Ok(found.into_iter().filter(|r| (r.skill.agent.is_none() || r.skill.agent.as_deref() == agent) && r.skill.visible_to(viewer.as_deref())).take(limit).collect())
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
            let report = self.run(self.manager.review_collection())?;
            // The shared collection only: a person's own skills are theirs to keep.
            let skills: Vec<Skill> = self.run(self.manager.list(None))?.into_iter().filter(|s| s.owner.is_none()).collect();
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
        let outcome = curator::parse(&reply)
            .and_then(|plan| self.run(self.manager.apply_plan(plan, run)))
            .map(|applied| [notes, applied].concat());
        Review { outcome, usage }
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
        tool_errors: tools.iter().filter(|c| c.contains("\"error\"")).count(),
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
pub(crate) fn complete(
    url: &str,
    model: &str,
    system: &str,
    user: &str,
) -> Result<(String, Option<Usage>), String> {
    call(url, model, system, user, false)
}

/// A small background job (memory capture, a skill review, mail triage…):
/// the fallback model does it first when it takes them (`[fallback_model]
/// background = true`) and the main model is up, so the main one is left to
/// the chats; if the fallback fails, the main model does it after all.
pub(crate) fn complete_light(
    url: &str,
    model: &str,
    system: &str,
    user: &str,
) -> Result<(String, Option<Usage>), String> {
    call(url, model, system, user, true)
}

fn call(
    url: &str,
    model: &str,
    system: &str,
    user: &str,
    light: bool,
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
    // The main chat model down (or failing just now): the fallback does it.
    if let Some(why) = crate::fallback::blocked() {
        return Err(why);
    }
    let fallback = crate::fallback::target().filter(|(u, _)| u != url);
    let mut used = model.to_string();
    let first = light.then(crate::fallback::light).flatten().filter(|(u, _)| u != url);
    let reply = match &fallback {
        Some((fb_url, fb_model)) if first.is_some() => {
            body["model"] = json!(fb_model);
            match send(fb_url, &body) {
                Ok(r) => {
                    used = fb_model.clone();
                    r
                }
                Err(_) => {
                    body["model"] = json!(model);
                    send(url, &body)?
                }
            }
        }
        Some((fb_url, fb_model)) if crate::fallback::skip_main() => {
            body["model"] = json!(fb_model);
            used = fb_model.clone();
            send(fb_url, &body)?
        }
        _ => match send(url, &body) {
            Ok(r) => r,
            Err(e) if crate::fallback::unreachable(&e) && fallback.is_some() => {
                let (fb_url, fb_model) = fallback.clone().unwrap_or_default();
                crate::fallback::main_failed();
                body["model"] = json!(fb_model);
                used = fb_model;
                send(&fb_url, &body)?
            }
            Err(e) => return Err(e),
        },
    };
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

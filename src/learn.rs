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

    /// Skills to add to the prompt for `message`, best first.
    pub fn relevant(&self, message: &str) -> Result<Vec<Ranked>, String> {
        self.run(self.manager.search(message, self.manager.settings().max_skills))
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
    ) -> Review<Applied> {
        let prepared = (|| -> Result<(String, Vec<String>), String> {
            let all = self.run(self.manager.list(None))?;
            let related = self.run(self.manager.related(transcript, 3))?;
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
        let (reply, usage) = match complete(url, model, evaluator::SYSTEM_PROMPT, &prompt) {
            Ok(r) => r,
            Err(e) => return Review { outcome: Err(e), usage: None },
        };
        let outcome = evaluator::parse(&reply).and_then(|verdict| {
            let decision = verdict.decide(self.manager.settings().min_confidence, &names);
            self.run(self.manager.apply(decision, run, trigger))
        });
        Review { outcome, usage }
    }

    /// Review the whole collection: local checks, then the model's plan for
    /// merges, splits and conflicts. Returns notes for the log.
    pub fn curate(&self, url: &str, model: &str, run: Option<Uuid>) -> Review<Vec<String>> {
        let local = (|| -> Result<(lyra_learning::Report, Vec<Skill>), String> {
            let report = self.run(self.manager.review_collection())?;
            let skills = self.run(self.manager.list(None))?;
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
        let (reply, usage) = match complete(url, model, curator::SYSTEM_PROMPT, &prompt) {
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
        let all = self.run(self.manager.list(None))?;
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

    /// Text for `/skills`: what needs review in full, the rest briefly.
    pub fn describe(&self) -> Result<String, String> {
        let all = self.run(self.manager.list(None))?;
        let proposals = self.run(self.manager.proposals())?;
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

    /// Text for `/history`: versions and audit trail.
    pub fn history(&self, key: &str) -> Result<String, String> {
        let skill = self.run(self.manager.find(key))?;
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

    /// `/rollback <skill> [version]`.
    pub fn rollback(&self, args: &str) -> Result<String, String> {
        let mut parts = args.split_whitespace();
        let key = parts.next().ok_or("usage: /rollback <skill> [version]")?;
        let to = parts
            .next()
            .map(|v| v.trim_start_matches('v').parse::<i64>().map_err(|_| format!("bad version {v:?}")))
            .transpose()?;
        let skill = self.run(self.manager.find(key))?;
        let v = self.run(self.manager.rollback(skill.id, to, None))?;
        Ok(format!("rolled {} back; now at v{v} (see /history {})", skill.name, skill.name))
    }

    /// A skill's procedure, for workflow steps in plans. Only active or
    /// proposed skills; rejected and deprecated ones aren't offered.
    pub fn instructions(&self, name: &str) -> Option<String> {
        let skill = self.run(self.manager.find(name)).ok()?;
        matches!(skill.status, SkillStatus::Active | SkillStatus::Proposed).then_some(skill.instructions)
    }

    pub fn approve(&self, key: &str) -> Result<String, String> {
        self.run(self.manager.approve(key))
    }

    pub fn reject(&self, key: &str) -> Result<String, String> {
        self.run(self.manager.reject(key))
    }

    pub fn deprecate(&self, key: &str) -> Result<String, String> {
        self.run(self.manager.deprecate(key, "deprecated by the user"))
    }

    pub fn forget(&self, key: &str) -> Result<String, String> {
        self.run(self.manager.forget(key))
    }
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
    let resp = client.post(url).json(&body).send().map_err(|e| e.to_string())?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("{status}: {}", resp.text().unwrap_or_default()));
    }
    let reply: Value = resp.json().map_err(|e| e.to_string())?;
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
    fn transcript_keeps_the_most_recent_messages() {
        let history = [("user", "one"), ("assistant", "two"), ("user", "three")];
        assert_eq!(transcript(&history, 2), "[assistant] two\n\n[user] three");
    }
}

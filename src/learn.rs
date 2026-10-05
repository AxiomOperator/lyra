//! Self-learning glue: finds skills relevant to each message, reviews finished
//! turns for lessons, and backs the /skills commands. Talks to the
//! `LearningManager`; the store underneath is its business.

use std::sync::Arc;
use std::time::Duration;

use lyra_learning::evaluator::{self, Turn};
use lyra_learning::{Learned, LearningManager, Mode, Skill, SkillStatus};
use serde_json::{Value, json};
use tokio::runtime::Handle;

use crate::stats::Usage;

/// What the skills panel shows.
pub struct SkillsSnapshot {
    pub mode: Mode,
    pub active: Vec<Skill>,
    pub proposed: Vec<Skill>,
    pub rejected: usize,
    /// Skill files that couldn't be read.
    pub errors: Vec<String>,
}

pub struct Learning {
    manager: Arc<LearningManager>,
    runtime: Handle,
    /// The skills folder, for messages.
    dir: String,
    pub mode: Mode,
    min_confidence: f32,
    max_skills: usize,
}

impl Learning {
    pub fn new(
        manager: Arc<LearningManager>,
        runtime: Handle,
        dir: String,
        mode: Mode,
        min_confidence: f32,
        max_skills: usize,
    ) -> Self {
        Self { manager, runtime, dir, mode, min_confidence, max_skills }
    }

    /// Active skills that match `message`, best first.
    pub fn relevant(&self, message: &str) -> Result<Vec<Skill>, String> {
        self.runtime
            .block_on(self.manager.search(message, self.max_skills))
            .map_err(|e| e.to_string())
    }

    /// Review the conversation for a reusable lesson with the chat model, and
    /// save one if found. The usage is kept even if the reply can't be used.
    pub fn review(&self, url: &str, model: &str, trigger: &str, transcript: &str) -> Review {
        let existing = match self.all() {
            Ok(all) => all.into_iter().map(|s| s.name).collect::<Vec<_>>(),
            Err(e) => return Review { outcome: Err(e), usage: None },
        };
        let user = evaluator::prompt(trigger, transcript, &existing);
        match complete(url, model, evaluator::SYSTEM_PROMPT, &user) {
            Ok((reply, usage)) => Review { outcome: self.judge(&reply), usage },
            Err(e) => Review { outcome: Err(e), usage: None },
        }
    }

    /// Validate the reviewer's reply and save the skill it describes, if any.
    fn judge(&self, reply: &str) -> Result<Outcome, String> {
        let candidate = match evaluator::parse(reply)?.into_candidate(self.min_confidence) {
            Ok(c) => c,
            Err(why) => return Ok(Outcome::Nothing(why)),
        };
        let learned = self
            .runtime
            .block_on(self.manager.learn(candidate, "conversation", self.mode))
            .map_err(|e| e.to_string())?;
        Ok(match learned {
            Learned::Saved(skill) => Outcome::Learned(skill),
            Learned::Duplicate(skill) => {
                Outcome::Nothing(format!("{} already exists ({})", skill.name, skill.status))
            }
        })
    }

    pub fn snapshot(&self) -> Result<SkillsSnapshot, String> {
        let all = self.all()?;
        let of = |status| all.iter().filter(|s| s.status == status).cloned().collect::<Vec<_>>();
        Ok(SkillsSnapshot {
            mode: self.mode,
            active: of(SkillStatus::Active),
            proposed: of(SkillStatus::Proposed),
            rejected: all.iter().filter(|s| s.status == SkillStatus::Rejected).count(),
            errors: self.manager.load_errors(),
        })
    }

    /// Text for `/skills`: proposals in full (to review), the rest briefly.
    pub fn describe(&self) -> Result<String, String> {
        let all = self.all()?;
        let mut out = vec![format!(
            "Skill files: {} (one <name>.md per skill; edit them or add your own)",
            self.dir
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
        for status in [SkillStatus::Proposed, SkillStatus::Active, SkillStatus::Rejected] {
            let group: Vec<&Skill> = all.iter().filter(|s| s.status == status).collect();
            if group.is_empty() {
                continue;
            }
            out.push(format!("{status} ({}):", group.len()));
            for s in group {
                out.push(format!("  {} {} ({:.2}) — {}", short(s), s.name, s.confidence, s.description));
                if status == SkillStatus::Proposed {
                    out.extend(s.instructions.lines().map(|l| format!("      {l}")));
                }
            }
        }
        out.push("/approve <id> · /reject <id> · /forget-skill <id>".into());
        Ok(out.join("\n"))
    }

    pub fn approve(&self, id: &str) -> Result<String, String> {
        let skill = self.resolve(id)?;
        self.runtime.block_on(self.manager.approve(skill.id)).map_err(|e| e.to_string())?;
        Ok(format!("approved {} — it will be used from now on", skill.name))
    }

    pub fn reject(&self, id: &str) -> Result<String, String> {
        let skill = self.resolve(id)?;
        self.runtime.block_on(self.manager.reject(skill.id)).map_err(|e| e.to_string())?;
        Ok(format!("rejected {} — it won't be proposed again", skill.name))
    }

    pub fn forget(&self, id: &str) -> Result<String, String> {
        let skill = self.resolve(id)?;
        self.runtime.block_on(self.manager.forget(skill.id)).map_err(|e| e.to_string())?;
        Ok(format!("deleted {}", skill.name))
    }

    fn all(&self) -> Result<Vec<Skill>, String> {
        self.runtime.block_on(self.manager.list(None, 1000)).map_err(|e| e.to_string())
    }

    /// A skill by id prefix or exact name.
    fn resolve(&self, key: &str) -> Result<Skill, String> {
        let key = key.trim();
        if key.is_empty() {
            return Err("give a skill id (from /skills) or name".into());
        }
        let matches: Vec<Skill> = self
            .all()?
            .into_iter()
            .filter(|s| s.name == key || s.id.to_string().starts_with(key))
            .collect();
        match matches.len() {
            0 => Err(format!("no skill matches {key:?}")),
            1 => Ok(matches.into_iter().next().unwrap()),
            n => Err(format!("{n} skills match {key:?}; use more of the id")),
        }
    }
}

/// A finished review: what came of it, and the tokens the request used.
pub struct Review {
    pub outcome: Result<Outcome, String>,
    pub usage: Option<Usage>,
}

pub enum Outcome {
    Learned(Skill),
    Nothing(String),
}

/// Short id shown in the UI and accepted by the commands.
pub fn short(skill: &Skill) -> String {
    skill.id.to_string()[..8].to_string()
}

/// Rough token count (about 4 characters per token), for display.
pub fn approx_tokens(text: &str) -> u64 {
    text.len() as u64 / 4
}

/// System prompt section listing the skills that apply to this message.
pub fn prompt_section(skills: &[Skill]) -> String {
    let mut out = String::from(
        "# Learned skills\n\nProcedures learned in earlier sessions that look relevant to this \
         message. Follow them when they apply.",
    );
    for s in skills {
        out += &format!("\n\n## {}\n\nWhen: {}\n\n{}", s.name, s.description, s.instructions);
    }
    out
}

/// Pick out the turn that just finished from the chat history (role, content) pairs.
pub fn last_turn<'a>(history: &[(&'a str, &'a str)]) -> Option<Turn<'a>> {
    let start = history.iter().rposition(|(role, _)| *role == "user")?;
    let after = &history[start + 1..];
    let tools: Vec<&str> =
        after.iter().filter(|(role, _)| *role == "tool").map(|(_, c)| *c).collect();
    Some(Turn {
        user: history[start].1,
        follows_reply: history[..start].iter().any(|(role, _)| *role == "assistant"),
        tool_calls: tools.len(),
        tool_errors: tools.iter().filter(|c| c.contains("\"error\"")).count(),
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

/// One non-streaming chat completion; returns the reply text and its usage.
fn complete(
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
    let body = json!({
        "model": model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
    });
    let resp = client.post(url).json(&body).send().map_err(|e| e.to_string())?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("{status}: {}", resp.text().unwrap_or_default()));
    }
    let reply: Value = resp.json().map_err(|e| e.to_string())?;
    let text = reply["choices"][0]["message"]["content"]
        .as_str()
        .ok_or("reviewer returned no content")?
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
        let turn = last_turn(&history).unwrap();
        assert_eq!(turn.user, "No, use the staging server instead");
        assert!(turn.follows_reply);
        assert_eq!((turn.tool_calls, turn.tool_errors), (2, 1));
    }

    #[test]
    fn transcript_keeps_the_most_recent_messages() {
        let history = [("user", "one"), ("assistant", "two"), ("user", "three")];
        assert_eq!(transcript(&history, 2), "[assistant] two\n\n[user] three");
    }
}

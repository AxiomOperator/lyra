//! Self-learning glue: finds skills relevant to each message, reviews finished
//! turns for lessons, and backs the /skills commands. Talks to the
//! `LearningManager`; the store underneath is its business.

use std::sync::Arc;
use std::time::Duration;

use lyra_learning::evaluator::{self, Turn};
use lyra_learning::{Learned, LearningManager, Mode, Skill, SkillStatus};
use serde_json::{Value, json};
use tokio::runtime::Handle;

/// What the skills panel shows.
pub struct SkillsSnapshot {
    pub mode: Mode,
    pub active: Vec<Skill>,
    pub proposed: Vec<Skill>,
    pub rejected: usize,
}

pub struct Learning {
    manager: Arc<LearningManager>,
    runtime: Handle,
    pub mode: Mode,
    min_confidence: f32,
    max_skills: usize,
}

impl Learning {
    pub fn new(
        manager: Arc<LearningManager>,
        runtime: Handle,
        mode: Mode,
        min_confidence: f32,
        max_skills: usize,
    ) -> Self {
        Self { manager, runtime, mode, min_confidence, max_skills }
    }

    /// Active skills that match `message`, best first.
    pub fn relevant(&self, message: &str) -> Result<Vec<Skill>, String> {
        self.runtime
            .block_on(self.manager.search(message, self.max_skills))
            .map_err(|e| e.to_string())
    }

    /// Review the conversation for a reusable lesson with the chat model, and
    /// save one if found. Returns a line for the activity log.
    pub fn review(
        &self,
        url: &str,
        model: &str,
        trigger: &str,
        transcript: &str,
    ) -> Result<Review, String> {
        let existing: Vec<String> = self.all()?.into_iter().map(|s| s.name).collect();
        let user = evaluator::prompt(trigger, transcript, &existing);
        let (reply, tokens) = complete(url, model, evaluator::SYSTEM_PROMPT, &user)?;
        let verdict = evaluator::parse(&reply)?;
        let candidate = match verdict.into_candidate(self.min_confidence) {
            Ok(c) => c,
            Err(why) => return Ok(Review::Nothing { why, tokens }),
        };
        let learned = self
            .runtime
            .block_on(self.manager.learn(candidate, "conversation", self.mode))
            .map_err(|e| e.to_string())?;
        Ok(match learned {
            Learned::Saved(skill) => Review::Learned { skill, tokens },
            Learned::Duplicate(skill) => Review::Nothing {
                why: format!("{} already exists ({})", skill.name, skill.status),
                tokens,
            },
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
        })
    }

    /// Text for `/skills`: proposals in full (to review), the rest briefly.
    pub fn describe(&self) -> Result<String, String> {
        let all = self.all()?;
        if all.is_empty() {
            return Ok("No skills yet. Lessons from corrections and multi-step work show up \
                       here as proposals; /learn reviews the conversation now."
                .into());
        }
        let mut out = Vec::new();
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

pub enum Review {
    Learned { skill: Skill, tokens: u64 },
    Nothing { why: String, tokens: u64 },
}

/// Short id shown in the UI and accepted by the commands.
pub fn short(skill: &Skill) -> String {
    skill.id.to_string()[..8].to_string()
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

/// One non-streaming chat completion; returns the reply text and tokens used.
fn complete(url: &str, model: &str, system: &str, user: &str) -> Result<(String, u64), String> {
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
    Ok((text, reply["usage"]["total_tokens"].as_u64().unwrap_or(0)))
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

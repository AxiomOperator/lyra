use std::path::Path;
use std::str::FromStr;

use anyhow::Result;
use chrono::Utc;
use uuid::Uuid;

use crate::evaluator::Candidate;
use crate::{FileSkillStore, Skill, SkillStatus, SkillStore};

/// `list` limit meaning "everything"; V1 expects skills in the tens, not thousands.
const ALL: usize = 100_000;

/// How learned skills are handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Don't evaluate interactions for lessons.
    Off,
    /// Save lessons as proposals for a person to approve (the default).
    Propose,
    /// Save lessons as active skills straight away.
    Auto,
}

impl FromStr for Mode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "off" => Ok(Mode::Off),
            "propose" => Ok(Mode::Propose),
            "auto" => Ok(Mode::Auto),
            _ => Err(format!("unknown learning mode {s:?} (off, propose or auto)")),
        }
    }
}

/// Result of saving a candidate.
pub enum Learned {
    Saved(Skill),
    /// A skill with this name already exists (in any status, so rejections stick).
    Duplicate(Skill),
}

/// The agent's interface to learned skills.
pub struct LearningManager<S: SkillStore = FileSkillStore> {
    store: S,
}

impl LearningManager<FileSkillStore> {
    /// Use the skill files in `dir`, creating it if needed.
    pub fn open(dir: &Path) -> Result<Self> {
        Ok(Self::new(FileSkillStore::open(dir)?))
    }

    /// Skill files that couldn't be read, with the reason.
    pub fn load_errors(&self) -> Vec<String> {
        self.store.load_errors()
    }
}

impl<S: SkillStore> LearningManager<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    /// Save a validated candidate: proposed, or active in [`Mode::Auto`].
    pub async fn learn(&self, candidate: Candidate, source: &str, mode: Mode) -> Result<Learned> {
        if let Some(existing) = self.find(&candidate.name).await? {
            return Ok(Learned::Duplicate(existing));
        }
        let now = Utc::now();
        let status = if mode == Mode::Auto { SkillStatus::Active } else { SkillStatus::Proposed };
        let skill = Skill {
            id: Uuid::new_v4(),
            name: candidate.name,
            description: candidate.description,
            instructions: candidate.instructions,
            source: source.to_string(),
            confidence: candidate.confidence,
            status,
            created_at: now,
            updated_at: now,
        };
        Ok(Learned::Saved(self.store.learn(skill).await?))
    }

    /// Active skills relevant to `query`, best first.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<Skill>> {
        self.store.search(query, limit).await
    }

    pub async fn update(&self, id: Uuid, instructions: &str) -> Result<()> {
        self.store.update(id, instructions).await
    }

    pub async fn forget(&self, id: Uuid) -> Result<()> {
        self.store.forget(id).await
    }

    pub async fn approve(&self, id: Uuid) -> Result<()> {
        self.store.set_status(id, SkillStatus::Active).await
    }

    pub async fn reject(&self, id: Uuid) -> Result<()> {
        self.store.set_status(id, SkillStatus::Rejected).await
    }

    /// Newest first, optionally only one status.
    pub async fn list(&self, status: Option<SkillStatus>, limit: usize) -> Result<Vec<Skill>> {
        self.store.list(status, limit).await
    }

    async fn find(&self, name: &str) -> Result<Option<Skill>> {
        Ok(self.store.list(None, ALL).await?.into_iter().find(|s| s.name == name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(name: &str) -> Candidate {
        Candidate {
            name: name.into(),
            description: "When committing code".into(),
            instructions: "Run the test suite before every commit.".into(),
            confidence: 0.85,
        }
    }

    async fn manager() -> LearningManager {
        LearningManager::open(&crate::files::tests::temp_dir("manager")).unwrap()
    }

    #[tokio::test]
    async fn proposals_are_not_used_until_approved() {
        let m = manager().await;
        let Learned::Saved(skill) = m.learn(candidate("test-before-commit"), "conversation", Mode::Propose).await.unwrap() else {
            panic!("expected a new skill");
        };
        assert_eq!(skill.status, SkillStatus::Proposed);
        assert!(m.search("commit code", 3).await.unwrap().is_empty());

        m.approve(skill.id).await.unwrap();
        let found = m.search("commit code", 3).await.unwrap();
        assert_eq!(found[0].name, "test-before-commit");
    }

    #[tokio::test]
    async fn auto_mode_activates_immediately() {
        let m = manager().await;
        m.learn(candidate("test-before-commit"), "conversation", Mode::Auto).await.unwrap();
        assert_eq!(m.search("commit", 3).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn rejected_names_are_not_proposed_again() {
        let m = manager().await;
        let Learned::Saved(skill) = m.learn(candidate("x"), "conversation", Mode::Propose).await.unwrap() else {
            panic!("expected a new skill");
        };
        m.reject(skill.id).await.unwrap();
        assert!(matches!(
            m.learn(candidate("x"), "conversation", Mode::Propose).await.unwrap(),
            Learned::Duplicate(s) if s.status == SkillStatus::Rejected
        ));
    }
}

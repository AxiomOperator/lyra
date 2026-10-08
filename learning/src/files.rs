//! Skills as Markdown files, one per skill, so people can read, edit, version
//! and share them:
//!
//! ```text
//! ~/.lyra/skills/rust-pre-commit-check-order.md
//! ---
//! description: When checking a Rust project before committing
//! status: active
//! confidence: 0.90
//! source: conversation
//! created: 2026-10-05T00:06:24Z
//! updated: 2026-10-05T00:06:40Z
//! id: 2c9ca857-1f0e-4d0e-9a51-6f4c0f3e8a11
//! ---
//! 1. cargo fmt --check
//! 2. cargo clippy -- -D warnings
//! 3. cargo test
//! ```
//!
//! The file name is the skill's name and the body is its instructions. Every
//! header line is optional, so a hand-written file works: it is active, fully
//! trusted, and gets a stable id derived from its name. Files are read on every
//! call, so edits take effect immediately.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use uuid::Uuid;

use crate::{Skill, SkillStatus, SkillStore};

pub struct FileSkillStore {
    dir: PathBuf,
}

impl FileSkillStore {
    /// Use `dir` for skill files, creating it if needed.
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        Ok(Self { dir: dir.to_path_buf() })
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.md"))
    }

    /// Every readable skill file. Files that fail to parse are skipped rather
    /// than breaking the agent; `load_errors` reports them.
    fn load(&self) -> Result<Vec<Skill>> {
        let mut skills: Vec<Skill> = self
            .files()?
            .iter()
            .filter_map(|path| read(path).ok())
            .collect();
        skills.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(a.name.cmp(&b.name)));
        Ok(skills)
    }

    fn files(&self) -> Result<Vec<PathBuf>> {
        let entries = std::fs::read_dir(&self.dir)
            .with_context(|| format!("reading {}", self.dir.display()))?;
        Ok(entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|ext| ext == "md"))
            .collect())
    }

    /// Skill files that couldn't be read, with the reason, for the UI.
    pub fn load_errors(&self) -> Vec<String> {
        self.files()
            .unwrap_or_default()
            .iter()
            .filter_map(|p| read(p).err().map(|e| format!("{}: {e:#}", p.display())))
            .collect()
    }

    fn find(&self, id: Uuid) -> Result<Skill> {
        self.load()?.into_iter().find(|s| s.id == id).ok_or_else(|| anyhow!("no skill with id {id}"))
    }

    fn write(&self, skill: &Skill) -> Result<()> {
        // Write then rename, so a crash never leaves a half-written skill.
        let path = self.path(&skill.name);
        let tmp = self.dir.join(format!(".{}.md.tmp", skill.name));
        std::fs::write(&tmp, render(skill)).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl SkillStore for FileSkillStore {
    async fn create(&self, skill: Skill) -> Result<Skill> {
        check_name(&skill.name)?;
        if self.path(&skill.name).exists() {
            bail!("a skill named {:?} already exists", skill.name);
        }
        self.write(&skill)?;
        Ok(skill)
    }

    async fn get(&self, id: Uuid) -> Result<Option<Skill>> {
        Ok(self.load()?.into_iter().find(|s| s.id == id))
    }

    async fn search(&self, query: &str, limit: usize) -> Result<Vec<(Skill, f32)>> {
        Ok(rank(query, self.load()?, limit))
    }

    async fn list(&self, status: Option<SkillStatus>) -> Result<Vec<Skill>> {
        Ok(self
            .load()?
            .into_iter()
            .filter(|s| status.is_none_or(|status| s.status == status))
            .collect())
    }

    async fn save(&self, skill: &Skill) -> Result<()> {
        let existing = self.find(skill.id)?;
        if existing.name != skill.name {
            bail!("skills can't be renamed ({} → {})", existing.name, skill.name);
        }
        self.write(skill)
    }

    async fn delete(&self, id: Uuid) -> Result<()> {
        let skill = self.find(id)?;
        let path = self.path(&skill.name);
        std::fs::remove_file(&path).with_context(|| format!("deleting {}", path.display()))
    }
}

/// Names become file names, so keep them to lowercase letters, digits and dashes.
fn check_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && !name.starts_with('-')
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !ok {
        bail!("bad skill name {name:?}: use lowercase letters, digits and dashes");
    }
    Ok(())
}

fn read(path: &Path) -> Result<Skill> {
    let text = std::fs::read_to_string(path)?;
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("file name isn't UTF-8"))?
        .to_string();
    // A hand-written file has no `created:`; when it was written is the next best thing.
    let modified = std::fs::metadata(path)?.modified().map(DateTime::<Utc>::from).unwrap_or_else(|_| Utc::now());
    parse(&name, &text, modified)
}

/// Parse a skill file. Header lines are `key: value`; all are optional.
/// `fallback_time` stands in for a missing `created:`.
fn parse(name: &str, text: &str, fallback_time: DateTime<Utc>) -> Result<Skill> {
    let (header, body) = match text.strip_prefix("---\n") {
        Some(rest) => match rest.split_once("\n---") {
            Some((header, body)) => (header, body.strip_prefix('\n').unwrap_or(body)),
            None => bail!("header starts with --- but never ends"),
        },
        None => ("", text),
    };
    let field = |key: &str| {
        header.lines().find_map(|line| {
            let (k, v) = line.split_once(':')?;
            (k.trim() == key).then(|| v.trim().to_string())
        })
    };
    let time = |key: &str| -> Result<Option<DateTime<Utc>>> {
        field(key)
            .map(|t| {
                DateTime::parse_from_rfc3339(&t)
                    .map(|t| t.with_timezone(&Utc))
                    .map_err(|e| anyhow!("bad {key} {t:?}: {e}"))
            })
            .transpose()
    };
    let instructions = body.trim().to_string();
    if instructions.is_empty() {
        bail!("no instructions");
    }
    let created_at = time("created")?.unwrap_or(fallback_time);
    Ok(Skill {
        // Hand-written files have no id; derive a stable one from the name.
        id: match field("id") {
            Some(id) => Uuid::parse_str(&id).map_err(|e| anyhow!("bad id: {e}"))?,
            None => Uuid::new_v5(&Uuid::NAMESPACE_URL, format!("lyra-skill:{name}").as_bytes()),
        },
        name: name.to_string(),
        // Hand-written files may skip the description; the first line stands in.
        description: field("description").filter(|d| !d.is_empty()).unwrap_or_else(|| {
            let first = instructions.lines().next().unwrap_or_default();
            first.chars().take(100).collect()
        }),
        instructions,
        source: field("source").unwrap_or_else(|| "user".into()),
        confidence: match field("confidence") {
            Some(c) => c.parse().map_err(|e| anyhow!("bad confidence {c:?}: {e}"))?,
            None => 1.0,
        },
        status: match field("status") {
            Some(s) => s.parse()?,
            None => SkillStatus::Active,
        },
        created_at,
        updated_at: time("updated")?.unwrap_or(created_at),
        agent: field("agent").filter(|a| !a.is_empty()),
        owner: field("owner").filter(|a| !a.is_empty()),
        usage: Default::default(),
    })
}

fn render(skill: &Skill) -> String {
    let time = |t: DateTime<Utc>| t.to_rfc3339_opts(SecondsFormat::Secs, true);
    // Header values are single lines.
    let description = skill.description.split_whitespace().collect::<Vec<_>>().join(" ");
    let agent = skill.agent.as_ref().map_or(String::new(), |a| format!("agent: {a}\n"));
    let agent = format!("{agent}{}", skill.owner.as_ref().map_or(String::new(), |o| format!("owner: {o}\n")));
    format!(
        "---\n\
         description: {description}\n\
         {agent}\
         status: {}\n\
         confidence: {:.2}\n\
         source: {}\n\
         created: {}\n\
         updated: {}\n\
         id: {}\n\
         ---\n\
         {}\n",
        skill.status,
        skill.confidence,
        skill.source,
        time(skill.created_at),
        time(skill.updated_at),
        skill.id,
        skill.instructions.trim(),
    )
}

/// Words too common to say anything about which skill applies.
const STOPWORDS: &[&str] = &[
    "a", "about", "an", "and", "are", "as", "at", "be", "but", "by", "can", "could", "do",
    "does", "for", "from", "have", "how", "i", "if", "in", "is", "it", "its", "just", "me", "my",
    "of", "on", "or", "please", "should", "so", "that", "the", "then", "this", "to", "us", "was",
    "we", "what", "when", "where", "which", "will", "with", "would", "you", "your",
];

pub(crate) fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|w| !w.is_empty() && !STOPWORDS.contains(&w.as_str()))
        .collect()
}

/// Same word, allowing for endings: "commit" matches "commits" and "committing".
fn matches(a: &str, b: &str) -> bool {
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    short == long || (short.len() >= 4 && long.starts_with(short))
}

/// Score skills by how well they match the query: each query word found in a
/// skill scores by its rarity across skills (idf), double in the name.
fn rank(query: &str, skills: Vec<Skill>, limit: usize) -> Vec<(Skill, f32)> {
    let terms: HashSet<String> = words(query).into_iter().collect();
    if terms.is_empty() || skills.is_empty() {
        return Vec::new();
    }
    let docs: Vec<(Vec<String>, Vec<String>)> = skills
        .iter()
        .map(|s| {
            let body = format!("{} {}", s.description, s.instructions);
            (words(&s.name.replace('-', " ")), words(&body))
        })
        .collect();
    let has = |doc: &(Vec<String>, Vec<String>), term: &str| {
        doc.0.iter().chain(&doc.1).any(|w| matches(w, term))
    };
    let n = skills.len() as f64;
    let mut scored: Vec<(f64, Skill)> = skills
        .into_iter()
        .zip(&docs)
        .map(|(skill, doc)| {
            let score: f64 = terms
                .iter()
                .filter(|t| has(doc, t))
                .map(|t| {
                    let df = docs.iter().filter(|d| has(d, t)).count() as f64;
                    let idf = (1.0 + n / df).ln();
                    if doc.0.iter().any(|w| matches(w, t)) { 2.0 * idf } else { idf }
                })
                .sum();
            (score, skill)
        })
        .filter(|(score, _)| *score > 0.0)
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    scored.into_iter().take(limit).map(|(score, s)| (s, score as f32)).collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A fresh, empty directory under the system temp dir.
    pub(crate) fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lyra-skills-{}-{name}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub(crate) fn skill(name: &str, instructions: &str, status: SkillStatus) -> Skill {
        let now = Utc::now();
        Skill {
            id: Uuid::new_v4(),
            name: name.into(),
            description: format!("about {name}"),
            instructions: instructions.into(),
            source: "conversation".into(),
            confidence: 0.8,
            status,
            created_at: now,
            updated_at: now,
            agent: None,
            owner: None,
            usage: Default::default(),
        }
    }

    fn names(found: &[(Skill, f32)]) -> Vec<&str> {
        found.iter().map(|(s, _)| s.name.as_str()).collect()
    }

    #[tokio::test]
    async fn create_writes_a_readable_markdown_file() {
        let dir = temp_dir("write");
        let store = FileSkillStore::open(&dir).unwrap();
        let s = skill("deploy-steps", "1. build\n2. ship", SkillStatus::Proposed);
        store.create(s.clone()).await.unwrap();

        let text = std::fs::read_to_string(dir.join("deploy-steps.md")).unwrap();
        assert!(text.starts_with("---\ndescription: about deploy-steps\nstatus: proposed\n"), "{text}");
        assert!(text.ends_with("---\n1. build\n2. ship\n"), "{text}");

        let back = store.get(s.id).await.unwrap().unwrap();
        assert_eq!(back.instructions, "1. build\n2. ship");
        assert_eq!(back.status, SkillStatus::Proposed);
    }

    #[tokio::test]
    async fn hand_written_files_are_active_skills_with_stable_ids() {
        let dir = temp_dir("hand");
        std::fs::write(dir.join("tea.md"), "Brew green tea at 80°C for 2 minutes.\n").unwrap();
        let store = FileSkillStore::open(&dir).unwrap();
        let found = store.search("how do I brew green tea", 5).await.unwrap();
        assert_eq!(names(&found), ["tea"]);
        let tea = &found[0].0;
        assert_eq!((tea.status, tea.confidence), (SkillStatus::Active, 1.0));
        let age = Utc::now() - tea.created_at;
        assert!(age.num_minutes() < 5, "created defaults to when the file was written, not 1970");
        assert_eq!(tea.description, "Brew green tea at 80°C for 2 minutes.");
        let again = store.list(None).await.unwrap();
        assert_eq!(tea.id, again[0].id, "id is derived from the name, so it's stable");

        // Saving gives the file a header.
        let mut tea = tea.clone();
        tea.status = SkillStatus::Rejected;
        store.save(&tea).await.unwrap();
        let text = std::fs::read_to_string(dir.join("tea.md")).unwrap();
        assert!(text.contains("status: rejected"), "{text}");
    }

    #[tokio::test]
    async fn search_scores_matches_of_any_status() {
        let dir = temp_dir("search");
        let store = FileSkillStore::open(&dir).unwrap();
        let steps = "Implement ModelProvider, register it with ModelRouter, add config validation.";
        store.create(skill("add-model-provider", steps, SkillStatus::Active)).await.unwrap();
        store.create(skill("provider-draft", steps, SkillStatus::Proposed)).await.unwrap();
        store
            .create(skill("rust-commit-checks", "Run cargo fmt, clippy and test before committing.", SkillStatus::Active))
            .await
            .unwrap();

        let found = store.search("Add another model provider please", 5).await.unwrap();
        assert_eq!(names(&found)[0], "add-model-provider");
        assert!(names(&found).contains(&"provider-draft"));
        assert!(found[0].1 > 0.0);

        let found = store.search("I'm committing my changes", 5).await.unwrap();
        assert_eq!(names(&found), ["rust-commit-checks"], "commit matches committing");

        assert!(store.search("what is the capital of France", 5).await.unwrap().is_empty());
        assert!(store.search("what is the", 5).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn save_and_delete() {
        let dir = temp_dir("lifecycle");
        let store = FileSkillStore::open(&dir).unwrap();
        let mut s = store.create(skill("deploy", "run deploy.sh", SkillStatus::Proposed)).await.unwrap();
        s.instructions = "run deploy.sh --dry-run first".into();
        store.save(&s).await.unwrap();
        assert_eq!(store.get(s.id).await.unwrap().unwrap().instructions, "run deploy.sh --dry-run first");

        let mut renamed = s.clone();
        renamed.name = "other".into();
        assert!(store.save(&renamed).await.is_err());

        store.delete(s.id).await.unwrap();
        assert!(!dir.join("deploy.md").exists());
        assert!(store.get(s.id).await.unwrap().is_none());
        assert!(store.delete(s.id).await.is_err());
        assert!(store.save(&s).await.is_err(), "can't save a skill that doesn't exist");
    }

    #[tokio::test]
    async fn names_are_unique_and_safe() {
        let dir = temp_dir("names");
        let store = FileSkillStore::open(&dir).unwrap();
        store.create(skill("dup", "a", SkillStatus::Proposed)).await.unwrap();
        let err = store.create(skill("dup", "b", SkillStatus::Proposed)).await.unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
        for bad in ["../escape", "Upper", "", "a/b", "-x"] {
            assert!(store.create(skill(bad, "x", SkillStatus::Proposed)).await.is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn broken_files_are_skipped_and_reported() {
        let dir = temp_dir("broken");
        std::fs::write(dir.join("ok.md"), "do the thing").unwrap();
        std::fs::write(dir.join("bad.md"), "---\nstatus: maybe\n---\nx").unwrap();
        std::fs::write(dir.join("empty.md"), "---\nstatus: active\n---\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "not a skill").unwrap();
        let store = FileSkillStore::open(&dir).unwrap();
        assert_eq!(store.list(None).await.unwrap().len(), 1);
        assert_eq!(store.load_errors().len(), 2);
    }
}

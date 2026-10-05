//! Agent templates (A4): starting points the wizard customizes, so "create a
//! writer agent" needs only a few questions. `researcher` and `archivist`
//! are installed on first run: plans use them as helpers.

use crate::model::*;

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// Every template name, for the wizard.
pub const NAMES: &[&str] = &[
    "writer",
    "developer",
    "researcher",
    "analyst",
    "project-manager",
    "assistant",
    "reviewer",
    "data-analyst",
    "archivist",
    "custom",
];

/// The templates installed when there are no agents yet.
pub const DEFAULTS: &[&str] = &["researcher", "archivist"];

pub fn template(name: &str) -> Option<AgentProfile> {
    let t = |title: &str, description: &str| {
        let mut p = AgentProfile::new(&AgentProfile::slug(title), title, description);
        p.template = Some(AgentProfile::slug(title));
        p
    };
    let p = match AgentProfile::slug(name).as_str() {
        "writer" => {
            let mut p = t("Writer", "Specialist for drafting, rewriting, editing, proofreading and improving written communication.");
            p.role = "Professional writing assistant.".into();
            p.instructions = "Write clearly and naturally for the reader. Keep the author's meaning and any facts, names, \
                dates and numbers exactly. Match the requested tone; without one, be professional and concise. \
                Return only the finished text unless asked for options or explanations."
                .into();
            p.delegation = DelegationProfile {
                auto_delegate: true,
                intents: strs(&["rewrite_text", "draft_email", "edit_text", "proofread", "change_tone", "shorten_text"]),
                keywords: strs(&["rewrite", "reword", "rephrase", "proofread", "draft", "polish", "tone", "email", "letter", "paragraph", "shorten"]),
                examples: strs(&[
                    "Rewrite this email",
                    "Make this sound more professional",
                    "Draft a response to this message",
                    "Improve this paragraph",
                    "Proofread this for me",
                    "Shorten this announcement",
                ]),
                priority: 10,
                exclusions: strs(&["write source code", "generate sql", "write a function", "fix this code"]),
            };
            p.memory_policy = MemoryPolicy { mode: MemoryMode::Scoped, read: strs(&["user"]), write: Vec::new() };
            p.tools = strs(&["memory_recall"]);
            p.test_task = Some("Rewrite this message professionally: \"hey the server is broke again, cant log in, pls fix asap\"".into());
            p
        }
        "developer" => {
            let mut p = t("Developer", "Specialist for writing, reviewing and debugging code and explaining technical problems.");
            p.role = "Senior software engineer.".into();
            p.instructions = "Give correct, minimal, idiomatic code that fits the existing style. Explain the cause before \
                the fix when debugging. Say what you'd test. Never invent APIs; say when you're unsure."
                .into();
            p.delegation = DelegationProfile {
                auto_delegate: false,
                intents: strs(&["write_code", "debug", "review_code", "explain_code"]),
                keywords: strs(&["code", "function", "bug", "compile", "error", "rust", "python", "sql", "refactor", "stack trace"]),
                examples: strs(&["Write a function that parses this", "Why does this code panic", "Review this diff", "Fix this compile error"]),
                priority: 5,
                exclusions: strs(&["rewrite this email"]),
            };
            p.memory_policy = MemoryPolicy { mode: MemoryMode::Scoped, read: strs(&["project:*", "agent"]), write: strs(&["project:*"]) };
            p.tools = strs(&["memory_recall", "memory_remember"]);
            p.permission_policy.max_risk = "low_write".into();
            p.test_task = Some("Explain what this Rust does and one way it can panic: `let n: u32 = s.parse().unwrap();`".into());
            p
        }
        "researcher" => {
            let mut p = t("Researcher", "Looks things up in memory and the available read-only sources and reports what it finds, with where it came from.");
            p.role = "Careful researcher.".into();
            p.instructions = "Find what's actually known about the question. Report findings with their source, separate \
                facts from guesses, and say plainly what you couldn't find."
                .into();
            p.delegation = DelegationProfile {
                auto_delegate: false,
                intents: strs(&["research", "find_information", "summarize_sources"]),
                keywords: strs(&["research", "look up", "find out", "what do we know"]),
                examples: strs(&["Find out what we know about the backup setup", "Research the options for this"]),
                priority: 3,
                exclusions: Vec::new(),
            };
            p.memory_policy = MemoryPolicy { mode: MemoryMode::SharedReadOnly, ..Default::default() };
            p.tools = strs(&["memory_recall", "memory_list", "memory_inspect"]);
            p.test_task = Some("What do we know about the user's preferences?".into());
            p
        }
        "archivist" => {
            let mut p = t("Archivist", "Records findings and decisions in memory (agent and project scopes).");
            p.role = "Keeper of records.".into();
            p.instructions = "Record each finding or decision as one clear, self-contained memory in the right scope. \
                Correct or supersede what changed instead of duplicating it."
                .into();
            p.delegation = DelegationProfile {
                intents: strs(&["record", "remember_findings"]),
                keywords: strs(&["record", "note down", "keep track"]),
                ..Default::default()
            };
            p.memory_policy = MemoryPolicy { mode: MemoryMode::Scoped, read: strs(&["*"]), write: strs(&["agent", "project:*"]) };
            p.tools = strs(&["memory_recall", "memory_list", "memory_inspect", "memory_remember", "memory_correct", "memory_supersede"]);
            p.permission_policy.max_risk = "low_write".into();
            p
        }
        "analyst" => {
            let mut p = t("Analyst", "Breaks problems down, compares options and recommends with reasons.");
            p.role = "Analyst.".into();
            p.instructions = "Lay out the options, the criteria that matter, the trade-offs and a recommendation with \
                your reasoning. Quantify where you can; say what would change your mind."
                .into();
            p.delegation = DelegationProfile {
                auto_delegate: false,
                intents: strs(&["compare_options", "analyze", "recommend"]),
                keywords: strs(&["compare", "pros and cons", "trade-off", "which is better", "analyze", "evaluate"]),
                examples: strs(&["Compare these two approaches", "What are the pros and cons of", "Which option should we pick"]),
                priority: 4,
                exclusions: Vec::new(),
            };
            p.memory_policy = MemoryPolicy { mode: MemoryMode::SharedReadOnly, ..Default::default() };
            p.tools = strs(&["memory_recall"]);
            p
        }
        "project-manager" => {
            let mut p = t("Project Manager", "Turns goals into tasks, tracks progress and coordinates other agents.");
            p.role = "Project manager.".into();
            p.instructions = "Break work into concrete tasks with owners and order, keep track of status, call out risks \
                and blockers early. Hand specialist work to the right agent."
                .into();
            p.delegation = DelegationProfile {
                auto_delegate: false,
                intents: strs(&["plan_project", "track_tasks", "status_report"]),
                keywords: strs(&["project", "milestone", "status update", "roadmap", "tasks", "deadline"]),
                examples: strs(&["Make a project plan for this", "Give me a status update on the project"]),
                priority: 4,
                exclusions: Vec::new(),
            };
            p.memory_policy = MemoryPolicy { mode: MemoryMode::Scoped, read: strs(&["project:*", "user"]), write: strs(&["project:*"]) };
            p.tools = strs(&["memory_recall", "memory_remember", "goal_list", "goal_get", "goal_note"]);
            p.permission_policy = PermissionPolicy { max_risk: "write".into(), deny: Vec::new(), can_delegate: true };
            p
        }
        "assistant" => {
            let mut p = t("Assistant", "A general helper for everyday questions and small tasks.");
            p.role = "Helpful general assistant.".into();
            p.instructions = "Answer directly and helpfully; ask when something important is unclear.".into();
            p.memory_policy = MemoryPolicy { mode: MemoryMode::SharedReadOnly, ..Default::default() };
            p.tools = strs(&["memory_recall"]);
            p
        }
        "reviewer" => {
            let mut p = t("Reviewer", "Checks work for mistakes, gaps and unclear parts, and suggests fixes.");
            p.role = "Thorough reviewer.".into();
            p.instructions = "Find concrete problems: errors, missing pieces, inconsistencies, unclear wording. Rank them by \
                importance and suggest a fix for each. Don't nitpick style unless asked."
                .into();
            p.delegation = DelegationProfile {
                auto_delegate: false,
                intents: strs(&["review", "check", "critique"]),
                keywords: strs(&["review", "check this", "critique", "feedback on", "any mistakes"]),
                examples: strs(&["Review this document", "Check this for mistakes", "Give me feedback on this plan"]),
                priority: 4,
                exclusions: strs(&["rewrite this"]),
            };
            p.memory_policy = MemoryPolicy { mode: MemoryMode::None, ..Default::default() };
            p
        }
        "data-analyst" => {
            let mut p = t("Data Analyst", "Works with numbers and tables: summaries, trends, simple statistics, charts in words.");
            p.role = "Data analyst.".into();
            p.instructions = "Work from the data given. Show how you computed things, call out outliers and data quality \
                problems, and don't overstate what the data shows."
                .into();
            p.delegation = DelegationProfile {
                auto_delegate: false,
                intents: strs(&["analyze_data", "summarize_table", "compute_statistics"]),
                keywords: strs(&["data", "table", "csv", "average", "trend", "statistics", "numbers"]),
                examples: strs(&["Summarize this table", "What's the trend in these numbers", "Compute the averages"]),
                priority: 4,
                exclusions: Vec::new(),
            };
            p.memory_policy = MemoryPolicy { mode: MemoryMode::None, ..Default::default() };
            p
        }
        "custom" => t("Custom", "A new specialist."),
        _ => return None,
    };
    Some(p)
}

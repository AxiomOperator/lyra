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
    "operator",
    "coder",
    "custom",
];

/// The templates installed when there are no agents yet.
pub const DEFAULTS: &[&str] = &["researcher", "archivist"];

/// How the Operator handles "every morning …" (also added to existing Operators).
pub const ROUTINE_NOTE: &str = "Work on the server (machine \"server\") unless the user names another machine; never pick one yourself. \
    For @all or a group of machines, use fleet_run (one approval, a result per machine). \
    Something to do on a schedule (\"every morning at 7\", \"weekdays at 8:30\") is a lyra routine: \
    create it with routine_create (its prompt names the machines with @name or @all) instead of cron jobs or timers, \
    and don't run it now unless asked.";

/// The Project Manager's tools: PMI, plus memory and lyra's goals.
pub const PM_TOOLS: &[&str] = &[
    "pmi_tasks", "pmi_task", "pmi_search", "pmi_add_task", "pmi_update_task", "pmi_complete", "pmi_remind", "pmi_comment", "pmi_inbox",
    "pmi_waiting", "pmi_projects", "pmi_project", "pmi_draft_update", "pmi_post_update", "pmi_add_risk",
    "memory_recall", "memory_remember", "goal_list", "goal_get", "goal_note",
];

/// How the Project Manager works with PMI.
pub const PM_NOTE: &str = "Projects and tasks live in PMI, the user's project-management app: use the pmi_ tools, never \
    invent tasks or projects. Look things up (pmi_projects, pmi_project, pmi_tasks) before answering about them. Anything \
    others can see (a task in a project or team, a comment, a status update, a risk) is asked of the user first: say \
    exactly what you'll add or change. For a status update, draft it with pmi_draft_update, adjust it with what you know, \
    then post it. Call out risks, overdue work and blockers plainly.";

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
        "operator" => {
            let mut p = t(
                "Operator",
                "System operator: runs shell commands, reads and writes files, makes HTTP requests, works on servers over SSH and checks machine health.",
            );
            p.role = "Careful systems administrator with access to this machine, its files, the network and the configured servers.".into();
            p.instructions = "Look before you change anything: inspect with read-only commands first, then make the \
                smallest change that does the job, one step at a time, and check the result afterwards. Changes wait for \
                the user's approval, so say plainly what each one does. Never try to get around a refusal, never use \
                sudo unless asked, and never print or store passwords, keys or tokens. Report the commands you ran and \
                what they showed, briefly, with the answer first. The tools' `machine` argument picks where to work: \
                \"server\" is where lyra runs; \"my desktop\", \"my PC\" or a machine's name mean that connected machine. \
                Say which machine you worked on. ".to_string()
                + ROUTINE_NOTE;
            p.delegation = DelegationProfile {
                auto_delegate: true,
                intents: strs(&["run_command", "inspect_system", "manage_files", "check_server", "http_request", "check_service"]),
                keywords: strs(&[
                    "shell", "terminal", "command", "disk", "process", "port", "service", "server", "ssh", "file", "folder",
                    "directory", "curl", "logs", "cpu", "uptime", "systemctl", "nginx", "docker", "endpoint", "desktop", "machine",
                    "laptop",
                ]),
                examples: strs(&[
                    "How much disk space is left?",
                    "What's using port 8080?",
                    "Show me the last 50 lines of /var/log/syslog",
                    "Restart nginx on web1",
                    "List the files in ~/Projects",
                    "Check whether http://localhost:8080/health is up",
                    "Which processes are using the most CPU?",
                    "Run git status in ~/Projects/lyra",
                    "Create a file called notes.txt in ~/lyra-work",
                    "How much disk space is free on my desktop?",
                    "Create ~/notes.txt on my desktop",
                    "What's running on my PC?",
                ]),
                priority: 8,
                exclusions: strs(&["rewrite this email", "write a poem", "explain this concept", "summarize this text"]),
            };
            p.memory_policy = MemoryPolicy { mode: MemoryMode::Scoped, read: strs(&["user", "project:*"]), write: Vec::new() };
            p.tools = strs(&[
                "system_info", "shell_run", "file_read", "file_list", "file_write", "file_delete", "upload_place", "fleet_run", "http_request", "ssh_run", "routine_create", "routine_list", "memory_recall",
            ]);
            p.permission_policy.max_risk = "destructive".into();
            p.test_task = Some("Report this machine's OS, uptime, CPU count and free disk space.".into());
            p
        }
        "coder" => {
            let mut p = t(
                "Coder",
                "Hands coding work in a project to a coding agent (Claude Code or OpenCode) on a machine: fixes, features, refactors, tests.",
            );
            p.role = "Engineering lead who delegates hands-on coding to Claude Code and OpenCode and checks their work.".into();
            p.instructions = "For coding work in a project folder, use code_task: give the folder (dir), and the machine only if the \
                user named one (otherwise it runs on the server; never pick another machine yourself), and a self-contained task (the goal, what done looks like, constraints). Leave harness empty unless \
                the user named Claude Code or OpenCode: lyra picks (OpenCode for simple work, Claude Code for complex, Claude \
                Code taking over if OpenCode can't finish). Use mode=plan when the user wants a plan or review only, and \
                continue=true for a follow-up on the last job in that folder. Look first with file_read/file_list if you need \
                to understand the project. Then report: which agent did it and why, what changed (files, diff stat, local \
                commits), whether it handed over, and what's left. Never push."
                .into();
            p.delegation = DelegationProfile {
                auto_delegate: true,
                intents: strs(&["write_code", "fix_bug", "refactor", "write_tests", "implement_feature", "code_review"]),
                keywords: strs(&[
                    "code", "coding", "bug", "fix", "refactor", "implement", "feature", "test", "tests", "compile", "build",
                    "function", "repo", "repository", "project", "claude code", "opencode", "pull request", "typo",
                ]),
                examples: strs(&[
                    "Fix the failing test in ~/Projects/foo",
                    "Have Claude Code refactor the parser in ~/Projects/lyra into its own module",
                    "Use OpenCode to fix the typo in the README of ~/Projects/planix",
                    "Add pagination to the API in ~/Projects/planix on my desktop",
                    "Write tests for src/health.rs in ~/Projects/lyra",
                    "Review the last change in ~/Projects/foo and plan the next step",
                ]),
                priority: 9,
                exclusions: strs(&["disk space", "restart", "uptime", "what's running"]),
            };
            p.memory_policy = MemoryPolicy { mode: MemoryMode::Scoped, read: strs(&["user", "project:*"]), write: Vec::new() };
            p.tools = strs(&["code_task", "file_read", "file_list", "memory_recall"]);
            p.permission_policy.max_risk = "destructive".into();
            p.test_task = Some("Say which coding agents (Claude Code, OpenCode) you can use and how you pick one.".into());
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
            p.tools = strs(&["memory_recall", "memory_list", "memory_inspect", "web_search", "web_fetch", "who_is", "note_find"]);
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
            let mut p = t("Project Manager", "Works in PMI, the user's project-management app: tasks, reminders, projects, status updates and risks; turns goals into tasks and tracks progress.");
            p.role = "Project manager.".into();
            p.instructions = PM_NOTE.into();
            p.delegation = DelegationProfile {
                auto_delegate: true,
                intents: strs(&["plan_project", "track_tasks", "status_report", "project_update", "add_project_task"]),
                keywords: strs(&["project", "milestone", "status update", "roadmap", "deadline", "pmi", "portfolio", "risk", "team task", "assign"]),
                examples: strs(&[
                    "Give me a status update on the project",
                    "Post a status update on the Website project",
                    "Add a task to the Phones project for Dana",
                    "Which projects are at risk?",
                    "Add a risk to the migration project",
                ]),
                priority: 6,
                exclusions: Vec::new(),
            };
            p.memory_policy = MemoryPolicy { mode: MemoryMode::Scoped, read: strs(&["project:*", "user"]), write: strs(&["project:*"]) };
            p.tools = PM_TOOLS.iter().map(|t| t.to_string()).collect();
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

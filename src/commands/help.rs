//! `/help`: the command list (the palette reads it too).

/// A command line as it may be shown or logged: tokens hidden.
pub(crate) fn shown(line: &str) -> String {
    match line.trim_start().strip_prefix("/pmi token") {
        Some(rest) if !rest.trim().is_empty() => "/pmi token ••••".into(),
        _ => line.to_string(),
    }
}

pub(crate) const COMMANDS: &str = "\
/skills                      skills and changes waiting for review
/approve <id>                apply a proposal, or (re)activate a skill
/reject <id>                 discard a proposal or proposed skill for good
/deprecate <id>              stop using a skill without deleting it
/forget-skill <id>           delete a skill's file (its history is kept)
/stop                        stop the reply being written (also Ctrl-X)
/new                         start a new conversation (this one is saved)
/retry [other]               send your last message again (other: the other model answers); its reply is replaced
/edit <text>                 change your last message and send it again; its reply is replaced
/chat-only [on|off]          just talk in this conversation: no tools (nothing looked up, sent or changed)
/steps [on|off]              when lyra does several things at once, show them first to skip any (in the app)
/machines [update|remove <name>]  machines lyra works on (lyra-node): online, version, update, remove
/machines health [name|server]  disks, memory, load, failed units and updates (alerts: [health])
/machines rules <name|server> [on|off | allow|write|deny|ssh add|remove <value>]  what runs without asking there
/devices [approve|deny <code>]    paired phones, browsers, terminals and machines; pairing requests
/devices remove <name>       unpair a device or machine
/sessions                    saved conversations (lyra -c continues the latest)
/sessions search <words>     find a conversation by what was said in it
/routine [list]              scheduled things to ask lyra; runs tell you only when something needs you
/routine new <name> | <schedule> | <what to do> [| notify problems|always|never] [| changes]
/routine run|pause|resume|delete|show <name> · /routine edit <name> schedule|prompt|notify|changes <value>
/coding                      coding jobs handed to Claude Code / OpenCode (ask: 'fix … in ~/Projects/x on @desktop')
/diagnose [<machine> <problem>]  problems researched (read-only); look into one now
/tasks [today|overdue|week|project <name>]   your PMI tasks (personal and assigned), numbered
/task add <what> [when] · /task done <n> [comment] · /task snooze <n> [1h|tomorrow]
/users [approve|admin|member|disable <who>]   the people who use lyra serve (admins)
/users rounds <who> <n|default>   their own limit on tool calls in one reply (1–64; admins)
/users add <username> <name> [admin]   an account without Microsoft: a one-time password, shown once (admins)
/users password <who> [username]   a new one-time password (or a first username) for someone (admins)
/whoami                      who this conversation belongs to
/settings [<key> <value>]    the common settings (models, working hours, briefing, recap, notifications); change one (admins)
/templates                   your saved prompts and the shared ones (save them in the app)
/usage [days]                AI usage: everyone's and each person's (admins), your own (members)
/recap                       your end-of-day recap now (it also comes at the end of the working day)
/watches [cancel <id>]       what lyra watches for you (tell me when Jeremy replies)
/feedback                    bug reports and feature requests: yours (admins: everyone's)
/pmi [token <token>]         the PMI connection (your project-management app)
/calendar [today|tomorrow|week|<day>|disconnect]   your Outlook calendar (connect it from More in the app)
/today [plan]                plan my day: meetings, focus blocks for tasks due soon, mail to answer first (plan: re-plan now)
/notes [words] · /note <title>: <text> · /list <name> [add <a, b>|done|undone|remove <item>]   your notes and lists
/style [learn|note <text>|clear notes]   how you write, learned from your sent mail (used for drafts)
/mail [all|search <words>]   your Outlook inbox: new mail from people (all: newsletters too)
/briefing [now]              the daily briefing: what happened and what needs a look ([briefing] schedule)
/status [now]                everything lyra depends on: models, search, APIs, address, storage, backups, machines
/backup [now|list]           back up lyra (memory, skills, goals, sessions, config); nightly by itself
/model [name]                the model in use and the ones on offer; switch (saved to config.toml)
/resume <id>                 switch to a saved conversation
/history <id>                a skill's versions and audit trail
/rollback <id> [version]     restore an earlier version (the previous one by default)
/outcome good|bad|partial    how the last reply's skills worked out
/learn                       review the conversation for a lesson now
/curate                      look for duplicates, conflicts and stale skills now
/memory                      memory stats and what's waiting for approval
/memory search <query>       recall with scores · /memory list [scope]
/memory inspect <id>         a memory's details, versions, links and history
/memory correct <id> <text>  fix a memory (keeps the old version)
/memory forget|archive|restore|purge <id>
/memory approve|reject <id>  act on a proposed consolidation or archive
/memory working [clear]      show or clear working memory
/memory events               what happened to memories lately (created, superseded, linked, …)
/memory reembed              give every memory a vector from the current embedding model
/memory backup               copy the memory store to ~/.lyra/backup (restore: lyra --restore-memory <dir>)
/memory curate               consolidate duplicates, flag contradictions now
/memory episode              record this conversation as an episode
/memory project [name|none]  the current project (its memories are recalled, others' aren't)
/plan <request>              turn a request into a goal and a structured plan
/plan [id] · /plans          show the current (or a) plan · list plans
/plan run|resume [id]        execute it (pauses for approvals and budgets)
/plan approve <step>         approve a step's action (needed again if it changes)
/plan retry|skip <step>      after a failure or an interrupted step
/plan cancel · /plan events  stop it · what happened, with metrics
/plan run anyway             run despite the goal's open questions
/plan budget [raise]         the plan's budget · add the configured budget again
/plan checkpoints|revisions  recovery points · how the plan changed
/caps [search|show|allow|health]  what lyra can do, which capability fits, what's allowed
/outcome good|bad|partial    (also) how the last reply went, for evolution";

pub(crate) const HELP_END: &str = "/help                        this list";

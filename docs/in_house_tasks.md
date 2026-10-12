# In-house task management (replacing PMI)

**Goal:** lyra keeps tasks, reminders and projects itself, for everyone who
uses lyra, so it no longer depends on PMI.

**Why:**
- Not everyone has a PMI account, so members get nothing from today's task
  features.
- PMI is being sunset.

This is a guide (CLAUDE.md rule 3). It fits lyra's established decisions:
- **Where things live:** everything in `~/.lyra`.
- **Storage:** state other than memory in SQLite, with portable SQL (rule 5).
- **Approvals:** they stay in Rust, and anything other people see waits for an
  Allow.

## 1. What PMI does for lyra today

PMI reaches about 15 places. Each one needs an in-house replacement before
PMI can be turned off.

| Where | What it uses PMI for |
|---|---|
| **Chat tools** (`src/pmi.rs`) | 15 tools: `pmi_tasks`, `pmi_task`, `pmi_search`, `pmi_add_task`, `pmi_update_task`, `pmi_complete`, `pmi_remind`, `pmi_comment`, `pmi_inbox`, `pmi_waiting`, `pmi_projects`, `pmi_project`, `pmi_draft_update`, `pmi_post_update`, `pmi_add_risk` |
| **Tasks page** (`web/ui/src/lyra/tasks.tsx`) | Open tasks (personal and assigned), what waits on you, projects' health, the inbox. Live through PMI's event stream. |
| **Reminders** (`serve/assistant.rs`, `pmi::Nags`) | A reminder that went off is pushed with Done / In 1 hour / Tomorrow buttons (`push_action`), then pushed again every 30 min, up to 3 times, while it's open. |
| **Briefing** (`briefing.rs`) | Overdue and today's tasks, what waits on you, projects off track, unread inbox items. |
| **Plan my day** (`planner.rs`) | Focus blocks in Outlook for tasks due this week. |
| **Proactive help** (`proactive.rs`) | Meeting prep: open tasks about the meeting. Mail triage: a mail that asks something becomes a personal task. |
| **Recap** (`recap.rs`) | The day's tasks done and due. |
| **People** (`people.rs`) | "Who is …": the tasks you share with someone, and their PMI user. |
| **Watches** (`watches.rs`) | "Tell me when this task changes." |
| **Meetings** (`meetings.rs`) | Action items from a meeting offered as your own tasks. |
| **Project Manager agent** (`agents/src/templates.rs`) | `PM_TOOLS` and `PM_NOTE`: all its work is in PMI. |
| **Goals** (`goals.rs`) | Only test text mentions PMI (no live link). |
| **Status, Settings, secrets** | The PMI row, `[pmi]` (url, org, nag settings), `pmi` tokens per person. |
| **The loop** (`serve/loop.rs`) | `pmi_tick`: an SSE thread per person with a token, re-reads every 5 min, the per-person view sent to devices. |

What's already in lyra and stays as it is:
- **Notes' checklists** (`notes.rs`, `- [ ]` lists): shopping lists and the
  like.
- **The per-conversation task board** (`board.rs`): it's for lyra and its
  agents.
- **Goals** (`lyra-goals`): long-lived aims that plans work toward.

Tasks are a new, separate thing that the others can link to. A note's list
item can be made into a task, and a goal can have tasks.

## 2. The design

### 2.1 One tasks module, two backends during the change

- **`src/tasks.rs`** is the only thing the rest of lyra calls, for listing,
  getting, adding, updating, completing, reminding, searching, projects and the
  live view.
- **Behind it, two backends:**
  - **lyra** (new): the in-house store.
  - **pmi** (today's `pmi.rs`): kept only while people move over.
- **The setting:** `[tasks] backend = "lyra" | "pmi"`.
  - It starts as `pmi` on production so nothing changes on day one.
  - It becomes `lyra` after the import, and then `pmi` is removed.
- **What this buys:** every consumer in §1 (briefing, planner, proactive,
  recap, people, watches, meetings, nags, the page) switches once, to
  `tasks::`, and needs no change when PMI goes.

### 2.2 Storage: `~/.lyra/tasks/tasks.db` (SQLite, portable SQL)

**Why SQLite rather than JSON files:** tasks are shared, assigned between
people and queried by due date and project. That's what lyra already uses
SQLite for (plans, the skills ledger, evolution).

**Tables:**
- **`tasks`:**
  - Identity: `id` TEXT (UUID).
  - Who: `owner` (lyra user id), `created_by`.
  - Content: `title`, `notes`, `checklist` (JSON text).
  - State: `status` (`open`, `done`, `cancelled`), with a `CHECK`. `priority`
    (`low`, `normal`, `high`, `urgent`).
  - Dates: `due` (date, TEXT `YYYY-MM-DD`), `remind_at` (RFC 3339), `repeat`
    (a rule: "every weekday", "monthly on the 1st").
  - Links: `project_id` (FK, nullable), `parent_id` (subtasks, FK), `goal_id`
    (nullable).
  - Origin: `source` (`chat`, `mail`, `meeting`, `routine`, `import`, `page`)
    and `source_ref` (a mail id, a meeting id).
  - Times: `created`, `updated`, `completed`.
  - Import: `pmi_id` (unique, to make the import repeatable).
- **`task_assignees`** (`task_id`, `user`, `accepted`): a task given to someone
  else, accepted or not yet. This replaces PMI's "transfers".
- **`projects`:** `id`, `name`, `description`, `owner`, `status` (`active`,
  `on_hold`, `done`), `health` (`on_track`, `at_risk`, `off_track`), `due`,
  `pmi_id`.
- **`project_members`** (`project_id`, `user`, `role`: `owner`, `member`).
- **`task_events`:** what changed, by whom and when: comments, status changes,
  reassignments. This is the task's history, and what watches and the recap
  read.
- **`project_updates`:** status updates (text, health, by, at) and risks
  (text, likelihood, impact, owner, status).

**SQL rules that apply:**
- `sqlx` with bound parameters.
- Times passed in from Rust.
- `ON CONFLICT` for upserts.
- No `datetime('now')`.

**Search:** plain `LIKE` on title and notes. Optionally, tasks can go into
lyra's existing one-search.

### 2.3 Who sees what (and what waits for an Allow)

- **Everyone gets personal tasks,** members included. That's the gap PMI left.
- **A project's tasks** are visible to its members. Admins see every project.
- **Assigning a task to someone else:**
  - It's sent to them as a request (`accepted = false`): they see it on their
    Tasks page with Accept / Decline.
  - Assigning, commenting on a shared task and posting a project update all
    change what someone else sees. So in the chat they're `RiskLevel::Write`
    and wait for an Allow, as `pmi_comment` and `pmi_post_update` do today.
  - Done by hand on the page, they act at once: the click is the person's
    own.
- **One person's tasks never show in another's** briefing, recap or search,
  except shared ones. Each turn already works on its person's behalf
  (`acting::current()`), and the store filters on it.

### 2.4 Tools for the chat and agents

The tools are the same set, renamed from `pmi_*` to `task_*` and `project_*`:
- `task_list`, `task_get`, `task_search`, `task_add`, `task_update`,
  `task_complete`, `task_remind`, `task_comment`, `task_assign`.
- `tasks_waiting`: requests for you to accept.
- `project_list`, `project_get`, `project_add`, `project_update_draft`,
  `project_update_post`, `project_add_risk`.

**How they behave:**
- The argument shapes stay as they are today (`title`, `where`, `due` as said,
  `when` for reminders), so the model's habits carry over.
- Natural dates go through `when.rs`, as now.
- **Approvals:** `approval()` keeps PMI's rules. Your own task is `LowWrite`.
  Anything another person sees is `Write` and waits for an Allow.
- **Old names:** while the change is under way, the old `pmi_*` names are
  accepted as aliases, so saved workflows, skills and evolved tools don't
  break. They're logged when used, so they can be cleaned up.
- **The Project Manager agent** gets the new tools, and `PM_NOTE` is rewritten
  for lyra's own tasks.

### 2.5 Reminders, without PMI's inbox

- **Who sends them:** the serve loop, from `remind_at`, through a small due
  queue rebuilt when tasks change (no polling of the database every tick).
- **What's sent:** a push with the same buttons (Done / In 1 hour / Tomorrow).
  `push_action` calls `tasks::` instead of PMI.
- **Nags stay:** pushed again every `nag_minutes`, up to `nag_max`, while the
  task is open and the reminder hasn't been answered. Their state moves into
  the reminder row, so `pmi/nags.json` isn't needed.
- **Repeating tasks:** finishing one makes the next, with its due date and
  reminder moved forward by the rule.
- **Quiet time** (`planner.rs`) is respected, as it is now.

### 2.6 The live view and the Tasks page

- **The live view:** no SSE thread. Each write bumps a revision. The loop
  rebuilds the view (`tasks::State`, the same shape as `pmi::State`) only for
  people whose tasks changed, and it goes out as a status patch (0.83).
- **The Tasks page** (rewritten on the same data):
  - **Lists:** Today, Upcoming, Overdue, Waiting on me (requests to accept),
    Assigned by me, Done.
  - **Quick add** with natural dates ("call Dana friday 3pm").
  - **Editing:** a dialog for title, notes, due, reminder, repeat, priority,
    project, assignees and checklist.
  - **Projects:** a list with health and progress. Each project's page shows
    its tasks, updates, risks and members.
  - **Members get it too:** it moves to the members' pages (`MEMBER_PAGES`).
- **Notes:** a list item gets "Make a task".
- **Today page / briefing:** read `tasks::`.

### 2.7 Moving off PMI: the import

`/tasks import pmi` (admin), run per person who has a PMI token:
1. **Preview:** how many tasks, projects and comments would come over, and who
   they'd belong to. Nothing is written.
2. **Users:** PMI users are matched to lyra users by email. Anyone with no
   match is listed, and their tasks go to the importer or are skipped,
   whichever the admin chooses.
3. **What comes over:**
   - Open tasks, and the last 90 days of done ones, with due dates, reminders,
     repeat rules, priority, checklist, subtasks and assignees.
   - Projects the person is in, with their members, last status update and
     open risks.
   - Comments, as task events.
4. **Repeatable:** `pmi_id` makes a second run update rather than duplicate.
   So the import can run once early, PMI can be used for a while, and then the
   import can run again just before switching.
5. **After the switch:** `[tasks] backend = "lyra"`. PMI becomes read-only
   from lyra's side (its tools gone, the import still possible). Later, `[pmi]`,
   the `pmi` tokens and `pmi.rs` are removed, and the changelog says so.

`src/migrate.rs` does nothing here: the import is a deliberate admin step, not
something automatic at startup.

## 3. Phases

Each phase is its own commits (rule 1), with changelog entries, tests and a
TUI panel review.

**Phase 1: the store and personal tasks** (members included)
- `src/tasks.rs` facade and backend switch; `tasks.db` schema.
- Personal tasks: add, update, complete, due dates, priority, checklist,
  repeat.
- Reminders with push buttons and nags.
- The `task_*` tools (own tasks only).
- The Tasks page for personal tasks.
- `[tasks]` in `config.example.toml`; a Status row.
- **TUI:** a "Tasks" line in the Goals panel (open · overdue · today), and
  Activity lines for reminders sent.

**Phase 2: every consumer through `tasks::`**
- The briefing, plan my day, proactive (meeting prep, mail → task), the recap,
  people, watches (task changed), meetings (action items → tasks), and the
  Today page.
- The loop's per-person view without SSE.
- With `backend = "pmi"`, everything still works on PMI.

**Phase 3: sharing and projects**
- Assigning with Accept / Decline; "waiting on me".
- Projects, members, status updates (draft, then post), risks and comments.
- Approvals for anything another person sees.
- The Project Manager agent on the new tools.

**Phase 4: the import from PMI**
- Preview, user matching, repeatable runs, and a report.
- Tried on a copy of production's data first: the server's PMI is read, and a
  demo home is written to.

**Phase 5: the switch and sunset**
- `backend = "lyra"` on production.
- The old `pmi_*` names stay as aliases for a release, then go.
- `pmi.rs`, `[pmi]`, the PMI Status row and the PMI tokens are removed.
- README and CLAUDE.md updated, and this doc moved to `docs/done/`.

## 4. Decisions for you

1. **Projects:** do you want projects with status updates and risks (what PMI
   has), or only tasks with a project label to start? Projects are Phase 3, so
   the label is a smaller start.
2. **Assigning between people:** with Accept / Decline as proposed, or simply
   assigned?
3. **Import:** bring over open tasks and projects (proposed), the done history
   too, or start fresh?
4. **The switch:** one date for everyone, or person by person? The backend
   setting can be per person if needed.
5. **Outlook / Microsoft To Do:** lyra already talks to Microsoft 365. Should
   tasks also show in To Do (a one-way sync later), or stay in lyra only?
   This plan keeps them in lyra only.

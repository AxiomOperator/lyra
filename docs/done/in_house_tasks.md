# In-house task management (replacing PMI)

> **Status (2026-10-12): done.** Phases 1–4 in 0.87.0.247: lyra keeps tasks itself and PMI is
> gone. Since nothing was imported and everyone switched at once (decisions 3 and 4), the
> two-backend switch of §2.1 wasn't needed: the code moved straight to lyra's tasks and
> production was updated once. Phase 5 in 0.93.0.256 (`src/tasks/todo.rs`): each person's own
> tasks and a To Do list ("lyra" by default), both ways, through Graph's delta query and
> `task_links`. The newer of two changes wins, and the other goes in the task's history. It's
> opt-in: `[web.entra] todo` asks new connections for Tasks.ReadWrite, and `[tasks] todo_sync`
> is on by default. Reminders aren't synced.

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
- **The setting:** `[tasks] backend = "lyra" | "pmi"`, one for everyone.
  - It stays `pmi` on production while the in-house side is built, so nothing
    changes for anyone until then.
  - It becomes `lyra` once the whole feature is done (decision 4), and then
    `pmi` is removed.
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
  - Origin: `source` (`chat`, `mail`, `meeting`, `routine`, `page`) and
    `source_ref` (a mail id, a meeting id).
  - Times: `created`, `updated`, `completed`, `deleted` (a deleted task is
    kept, marked, so a later sync can tell another side; see §2.8).
- **`task_assignees`** (`task_id`, `user`, `accepted`): a task given to someone
  else, accepted or not yet. This replaces PMI's "transfers".
- **`projects`:** `id`, `name`, `description`, `owner`, `status` (`active`,
  `on_hold`, `done`), `health` (`on_track`, `at_risk`, `off_track`), `due`.
- **`project_members`** (`project_id`, `user`, `role`: `owner`, `member`).
- **`task_events`:** what changed, by whom and when: comments, status changes,
  reassignments. This is the task's history, and what watches and the recap
  read.
- **`project_updates`:** status updates (text, health, by, at). No risks for
  now (decision 1).
- **`task_links`** (`task_id`, `service`, `external_id`, `etag`, `synced`):
  empty until the Microsoft To Do sync (§2.8). It's in the schema from the
  start so the sync needs no migration of tasks.

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
  `project_update_post`.

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
    its tasks, updates and members.
  - **Members get it too:** it moves to the members' pages (`MEMBER_PAGES`).
- **Notes:** a list item gets "Make a task".
- **Today page / briefing:** read `tasks::`.

### 2.7 Moving off PMI: start fresh

Nothing is imported from PMI (decision 3).
- **Until the switch:** PMI keeps working as it does now. The in-house side is
  built and tried on demo servers and by admins.
- **Before the switch:** everyone with a PMI token gets a notice a week ahead,
  in the briefing and as a push, saying that tasks move into lyra on that
  date, PMI's stay in PMI, and they should re-add what they still need.
- **The switch:** `[tasks] backend = "lyra"` for everyone at once, once every
  phase is done (decision 4). From then on, lyra's tools and pages show only
  lyra's own tasks.
- **Afterwards:** `[pmi]`, the `pmi` tokens and `pmi.rs` are removed, and the
  changelog says so.

`src/migrate.rs` has nothing to do: there's no old data to bring over.

### 2.8 Later: two-way sync with Microsoft To Do

Not built now (decision 5), but the design leaves room for it:
- **Matching:** `task_links` maps a lyra task to its To Do task (`service =
  "todo"`, the Graph id and its etag).
- **Comparing:** `updated` on each task and the `deleted` mark tell what
  changed on lyra's side. Graph's delta queries tell what changed in To Do.
- **Conflicts:** when both sides changed the same task, the newer change wins,
  and the other version is kept in `task_events` so nothing is lost.
- **Whose list:** each person's own Outlook connection (`graph.rs`), with a
  To Do list of their choosing. Only their own tasks sync; a shared project's
  tasks stay in lyra.

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
- Projects, members, status updates (draft, then post) and comments. No
  risks.
- Approvals for anything another person sees.
- The Project Manager agent on the new tools.

**Phase 4: the switch and sunset**
- The week-ahead notice, then `backend = "lyra"` for everyone.
- The old `pmi_*` names stay as aliases for a release, then go.
- `pmi.rs`, `[pmi]`, the PMI Status row and the PMI tokens are removed.
- README and CLAUDE.md updated.

**Phase 5 (later): two-way Microsoft To Do sync** (§2.8)
- This doc moves to `docs/done/` after this phase (or when the sync is
  dropped).

## 4. Decisions (2026-10-12)

1. **Projects:** yes, with members, health and status updates. No risks for
   now.
2. **Assigning between people:** a task given to someone waits for their
   Accept / Decline.
3. **Import:** none. Everyone starts fresh in lyra; PMI's tasks stay in PMI.
4. **The switch:** one date for everyone, once the whole feature (Phases 1–3)
   is complete.
5. **Microsoft To Do:** a two-way sync, later (Phase 5). The schema is ready
   for it from the start.

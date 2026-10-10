# lyra

Rust TUI chat client for local OpenAI-compatible LLMs, built up step by step.

## Layout

- `src/` — the TUI binary: `main.rs` (app state: `App`, its events and helpers, the TUI loop), `turn.rs` (a chat turn: sending, the prompt's parts, the tool loop, streaming), `startup.rs` (the command line, opening each subsystem, `configure()` shared by start and `/reload`), `commands/` (`palette.rs` the `/` palette, `dispatch.rs` the dispatcher and page/member rules, `help.rs`, `ops.rs`, `work.rs` plans/goals/caps/evolve, `admin.rs` the server's machines/users/devices commands), `ui.rs` (all drawing), `markdown.rs` (replies as styled lines), `sessions.rs` (saved conversations, `-c`/`-r`), `serve/` (`lyra serve`: `mod.rs` mirroring and helpers, `loop.rs` the loop, `data.rs` the app's pages, `pushes.rs` notifications), `connect.rs` (`lyra connect`: the TUI as a client of a server),
  `config.rs`, `context.rs` (SOUL/USER/AGENT.md), `tools.rs` (memory tools + composite tools), `caps.rs` (capabilities glue), `goals.rs` (goals glue), `mem.rs` (memory glue), `plan.rs` (planning glue), `learn.rs`
  (self-learning glue), `evolve.rs` (evolution glue: evolved state, benchmark, `/evolve`), `retrieval.rs` (embedding/reranker clients), `briefing.rs` (the daily briefing), `acting.rs` (whose behalf a thread works on), `board.rs` (each conversation's task board, shared by lyra and its agents: tasks, notes between agents, results; `boards/<session>.json`), `actions.rs` (what lyra did for each person and why: the About me page), `asks.rs` (what lyra asks in the chat while a reply runs: a missing piece as a form, a round's steps to skip), `graph.rs` (Microsoft Graph: the sign-in, each person's connection and tokens, the requests every Microsoft 365 module makes), `pmi.rs` (PMI: tasks, reminders, projects), `calendar.rs` (each person's Outlook connection and calendar, Microsoft Graph), `mail.rs` (their Outlook mail), `mailout.rs` (email from lyra, only ever to the person themselves: routine results, briefing, recap, `email_me`; lyra's mailbox is the services an admin adds (`Account`s of `KINDS`: Postmark, Resend, SendGrid; `email/providers.json`, keys in secrets.toml as `email-<id>`, tried in order) or their own Outlook), `teams.rs` (their Teams chats), `fallback.rs` (the fallback chat models, `[fallback_model]` then `[second_fallback_model]`), `known_down.rs` (models an admin marked known down: not tried until cleared), `files.rs` (their OneDrive/SharePoint files), `planner.rs` (plan my day: focus blocks, working hours, quiet time), `proactive.rs` (meeting prep, mail triage, follow-ups), `recap.rs` (the end-of-day recap), `meetings.rs` (the Meetings page: prep, notes and follow-ups, from Teams transcripts or the notes), `watches.rs` ("tell me when …": mail, replies, PMI tasks, Teams), `search.rs` (one search across conversations, notes, memories, mail, Teams, files), `style.rs` (each person's writing style, STYLE.md), `notes.rs` (each person's notes, lists and documents, one Markdown file each), `people.rs` ("who is …" across memory, mail, calendar, PMI), `projects.rs` (project folders on a person's own PC, lent by their open page: File System Access, `Hub::call_folder`), `vision.rs` (PDFs, scans and pictures as text: `[vision_model]`), `changelog.rs` (`CHANGELOG.json`: What's new and the version), `feedback.rs` (bug reports, feature requests and questions: submit, review, comments), `qa.rs` (Q&A: answered questions everyone reads), `limits.rs` (tool calls per reply: a person's own limit, else the shared one in behavior.toml), `templates.rs` (saved prompts: each person's and shared ones), `documents.rs` (the note editor on the Notes page: drafts with lyra, Word to OneDrive or a mail; a new title renames the file), `docx.rs` (Markdown as a Word file), `settings.rs` (the Settings page and `/settings`: the common settings, checked and written to config.toml), `secrets.rs` (`config/secrets.toml`), `usage.rs` (AI usage per person: `usage/<YYYY-MM>.jsonl`, `/usage`), `when.rs` (natural dates), `store.rs` (lyra's own JSON/text files: atomic writes, a lock per file, `JsonStore<T>`), `text.rs` (HTML, Office XML and web pages as plain text), `alerts.rs` (what's been told and not cleared: `alerts/<name>.json`), `stats.rs`.
- `memory/` — `lyra-memory` crate: memory behind `MemoryManager` (LanceDB by default: typed memory rows with
  their embedding, FTS + vector search, history tables; SQLite backend kept as an alternative; the
  `EmbeddingProvider` makes vectors outside the store; schema/embedding versioning, re-embed, backup;
  kinds, provenance, supersede/versions, hybrid ranking, context compiler, capture, curator,
  safety scan, working memory). `src/mem.rs` is lyra's side (embeddings, model calls, /memory).
- `learning/` — `lyra-learning` crate: skills behind `SkillManager` (Markdown files + SQLite ledger
  of versions, usage, relationships, proposals, audit log). Design: `docs/done/skill_learning.md`.
- `agents/` — `lyra-agents` crate: user-defined subagents (profiles as TOML files, registry with
  versions and delegation log, rule/semantic/model routing, the creation wizard, the delegation
  contract and permission checks). `src/agents.rs` is lyra's side (delegation, wizard, /agent).
- `system/` — `lyra-system` crate: system access for agents (shell with a Rust command classifier,
  files, HTTP, SSH, system info), every call checked (`System::check`: auto / ask / forbidden). Only
  agents whose profile lists the tools (the Operator) may call them; changes wait for the user's y/n.
- `node/` — `lyra-node` crate (lib + static binary): lends a machine's system tools to a server
  (outbound WebSocket), headless pairing, self-update and self-uninstall; its pairing/connection
  helpers are shared with `src/connect.rs`. Built static with musl (`.cargo/config.toml`).
- `web/` — `lyra-web` crate: `lyra serve`'s HTTP + WebSocket server (axum), device pairing (token
  hashes in `~/.lyra/web/devices.json`), Web Push (VAPID + RFC 8291 with RustCrypto), and the PWA
  in `web/ui/` (React + Vite + Tailwind + shadcn/ui + Vercel AI Elements; `npm run build` → `web/ui/dist/`,
  committed and embedded in the binary — rebuild and commit `dist/` after changing the app). `src/serve/` mirrors the `App` to devices as small
  updates and feeds their input back; TLS comes from the user's reverse proxy (Zoraxy).
- `execution/` — `lyra-execution` crate: goals, plans and the execution `Engine` (task graph,
  verification, retry, replanning, approvals, budgets, checkpoints, events) behind a `Runtime`
  trait; `src/plan.rs` is lyra's runtime and `/plan` text.
- `evolution/` — `lyra-evolution` crate: run telemetry, detectors, evolver prompts, candidates,
  fitness, generations with snapshots and rollback (`EvolutionManager`, SQLite), behavior settings,
  workflows and composite tools as TOML data, and the code lab (git worktree sandbox; approval only
  creates a local `evolution/<id>` branch — never merge, push or touch the running binary).
- `capabilities/` — `lyra-capabilities` crate: every capability (native, composite, OpenAPI, MCP,
  workflow, skill, subagent) in one model; `CapabilityManager` (registry, FTS + semantic discovery in
  LanceDB, scoring, Rust-enforced policy, usage in SQLite, health); OpenAPI and MCP (stdio) providers.
  `src/caps.rs` is lyra's side: builds the registry and is the one way the chat and plans call tools.
- `goals/` — `lyra-goals` crate: long-lived goals (`GoalManager`, SQLite): decomposition, goal ↔ plan
  attempts, progress, blockers/dependencies, priority scoring, triggers and the autonomy policy.
  `src/goals.rs` is lyra's side: commands, plan outcomes → progress, the autonomy session; the
  goal loop runs from `App::goals_tick`.
- `docs/` — design guides and examples (not binding; see rule 3). Fully implemented ones move to
  `docs/done/`.
- Runtime files live in `~/.lyra` (`$LYRA_HOME`): `config/config.toml`, `config/secrets.toml` (tokens, 0600, not backed up), `notes/<slug>.md` (the owner's notes and lists), `users/<id>/` (anyone but the owner's own files: USER.md, STYLE.md, secrets.toml, notes/, goals/, routines/, briefing/, pmi/; the owner's stay where they are), `context/`
  (SOUL/USER/AGENT.md), `memory/lance/` (LanceDB), `plans/plans.db`, `skills/<name>.md` (one Markdown file per skill) + `skills/ledger.db`,
  `evolution/evolution.db`, `config/behavior.toml`, `workflows/<name>.toml`, `tools/<name>.toml` (evolved state),
  `sessions/<id>.json` (saved conversations, `lyra -c` / `-r`), `boards/<id>.json` (each conversation's task board), `briefing/last.json` (the last daily briefing), `email/providers.json` (lyra's email services, in order), `routines/results/<name>/<stamp>.md` (each run's whole result: the next run sees the last two; Past results), `status/resume.json` (routine runs a restart cut off, run again at start), `email/prefs.json` (how each person's email from lyra goes, and what was sent), `usage/` (every model call: who, kind, model, tokens), `web/` (paired devices, users, VAPID key),
  `agents/<name>.toml` (one file per subagent) + `agents/agents.db` + `agents/index/` (routing, LanceDB).
  `src/migrate.rs` brings older layouts up to date on startup.

## Checks

```sh
cargo clippy --all-targets --workspace   # must be warning-free
cargo test --workspace
```

## Rules

1. **Commit and push after every completed feature or fix.** Once the checks pass, commit
   straight to `main` and `git push origin main`. Do not open a pull request.
2. **After every feature, review the TUI panels.** Decide whether what was added should show
   up in the side panels (Session, Agent, Memory, Skills, Goals, Plan, Evolution, Activity in `src/ui.rs`) or be
   logged to the Activity panel, and update them when it makes sense. Say in the summary
   what was changed in the UI, or why nothing needed to be.
3. **`docs/` files are guides and examples, not instructions to change what's established.**
   Use them for ideas, features and structure, but adapt them to the project as it is. If a
   doc assumes something different from an established decision (e.g. it says skills live in
   SQLite, but skills are Markdown files), keep the established decision and fit the doc's
   idea around it; never undo or migrate away from it because a doc says so. Established
   decisions are what the code and this file already do, for example:
   - everything lyra keeps lives in `~/.lyra` (`config/`, `context/`, `memory/`, `skills/`, `plans/`,
     `evolution/`, `workflows/`, `tools/`, `capabilities/`, `goals/`, `agents/`, `sessions/`, `web/`)
   - skills are Markdown files, one per skill; their history/evidence is in the ledger
   - subagents are TOML files, one per agent; versions and delegations are in `agents.db`
   - memory is LanceDB behind `MemoryManager` (vectors + FTS); other state (plans, skills ledger,
     evolution) stays SQLite
   Only the user changes an established decision. If a doc's approach seems clearly better,
   say so and ask; don't switch on your own.
4. **Every change updates `CHANGELOG.json` and the version, in the same commit.** No commit goes in without a new entry at the top.
   - **Version:** `major.minor.fix.build`.
     - **Build:** +1 on every commit, whatever the change. It goes back to 0 only when the major number changes.
     - **Minor:** +1 for a new feature or a visible change; the fix number goes back to 0.
     - **Fix:** +1 for a bug fix.
     - **Major:** only when the user says so.
   - **Entry:** `version`, `date`, a short `title`, and `new` / `improved` / `fixed` lines. Write the lines for users, saying what they can now do or what's better, not file or function names.
   - **Where it shows:** lyra builds the file in. It's the app's What's new page, the version in the menu under your initials, the "lyra was updated" note, the TUI's Session title and `lyra --version`.
   - **Check:** `changelog::tests` enforces the format and the numbering.
5. **Write SQL that would also run on PostgreSQL.** lyra uses SQLite today. Keep new tables, migrations and queries portable so a later move to PostgreSQL is mostly a driver change.
   - **Library:** use `sqlx` with bound parameters (`?` placeholders, never formatted values).
   - **Ids:** UUIDs or explicit ids stored as `TEXT`, not `AUTOINCREMENT` or `rowid`.
   - **Times:** RFC 3339 `TEXT` written and parsed in Rust (chrono). Don't use SQLite's `datetime('now')`, `strftime` or `julianday`; pass the time in as a parameter.
   - **Types:** standard ones only: `TEXT`, `INTEGER`, `BIGINT`, `REAL`, `BOOLEAN`, `BLOB`. Declare a column's type and keep to it, since PostgreSQL won't put a string in an integer column.
   - **Inserts:** for insert-or-update, use `INSERT … ON CONFLICT (…) DO UPDATE` / `DO NOTHING`, never `INSERT OR REPLACE` or `REPLACE INTO`.
   - **Constraints:** foreign keys and `CHECK`s declared in the schema, not only in Rust.
   - **SQLite-only features stay in one place:** `PRAGMA`s, `VACUUM INTO`, `json_extract`, `GLOB` and `FTS5` live only in the connection setup or the backup code (`src/backup.rs`), never in feature queries. Search across text goes through LanceDB or plain `LIKE`.
   - **Inside SQL:** plain `CASE`, `COALESCE` and joins; no `IFNULL`, `||` string tricks or rowid tricks.
   - **Existing SQLite-only code** is fixed when it's next touched; don't rewrite working code just for this.
6. **Move a doc to `docs/done/` once it's completely implemented**, so `docs/` only holds work
   still to do. Update any references to its path (README, CLAUDE.md, code comments).

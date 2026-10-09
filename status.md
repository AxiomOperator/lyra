# lyra: status and roadmap

*As of 2026-10-08, **lyra 0.25.0.141** (`ad55b2e`), running on 172.99.99.102 (lyra.fbcad.org).*

The project's overview: where lyra stands, what works, what's wrong or missing, and what to do next. It draws on four sources:
1. a read-only survey of the code;
2. a read-only health check of the production server;
3. a careful review of the newest features;
4. what's known from building them.

Finding IDs (**I-n** issues, **G-n** gaps, **D-n** debt) are referenced in the roadmap.

---

## 1. Summary

lyra is a working, self-hosted AI assistant for a small team. Its pieces:
- a Rust server (`lyra serve`) behind Zoraxy;
- a React PWA for phones and desktops;
- a terminal client;
- lyra-node for Linux and Windows machines.

It runs entirely on local models:

| Role | Model |
|---|---|
| Chat | Qwen3.8 27B |
| Vision | Gemma 4 31B |
| Decisions | clef-flash |
| Embedding and reranking | the embedding and reranker models |

| | |
|---|---|
| **Health** | Good. Production is stable: no panics, every model answering, backups nightly, disk 31%. |
| **Breadth** | Very wide for its age (5 days, 141 commits, about 57k lines of Rust and 16.5k of TypeScript). |
| **Biggest risks** | 2 verified cross-user problems (I-1, I-2); the server loop doing slow network work inline (I-5, I-6); a web app with no automated tests (G-1). |
| **Biggest opportunities** | Hardening and test coverage before more users join; test coverage and the remaining technical debt (§6); a few high-value assistant features (§8). |

---

## 2. Current state: what lyra does today

### Core assistant
- Streaming chat with tool calls, reasoning, Markdown, and a context meter with cost.
- Several conversations at once.
- Conversation list with pins, folders (lyra suggests one), date groups and archive.
- Search across conversations, plus **Ctrl-K** to search everything.
- **Memory** in LanceDB: typed memories, hybrid recall, a context compiler, automatic capture, a curator, and per-person scopes.
- **Skills** as Markdown files with a ledger: learned from corrections, approved, scored, curated. Skills are **shared** (the owner's) or **personal** (each member's own).
- **Agents:** Operator, Coder, Researcher, Assistant, Project Manager and others, with routing, delegation and permissions.
- **Plans and goals:** structured plans with verification, retries, checkpoints and budgets; long-lived goals with an autonomy policy.
- **Self-evolution:** telemetry, proposals, benchmarks, and versions that can be rolled back. A code lab works in a git worktree and never pushes.
- **Capabilities:** a registry of native, OpenAPI, MCP, workflow and skill tools, with discovery and policy.

### Machines and operations
- lyra-node for Linux and Windows: shell, files, HTTP and SSH, every change approved.
- `@machine`, `@all` and groups to target machines.
- Machine health and alerts, routines, self-researched diagnoses, a Status page (uptime and latency of everything lyra uses), nightly backups.
- Coding agents (Claude Code and OpenCode) on the server and on machines.

### Personal assistant (Microsoft 365 and PMI)
- Sign in with Microsoft. Users and roles (owner, admin, member), with per-person data everywhere.
- **Outlook:**
  - calendar, including find a time with free/busy and invites after approval;
  - mail: triage, HTML drafts, sending after approval;
  - Teams chats and OneDrive/SharePoint files, read-only.
- **PMI** tasks, reminders and projects, with live updates.
- **Each day:**
  - daily briefing;
  - plan my day (focus blocks);
  - proactive help (meeting prep, mail triage, follow-ups);
  - end-of-day recap;
  - "tell me when…" watches.
- Notes and lists, "who is…", a learned writing style.
- Meeting follow-up from Teams transcripts. This is **built but off** until the permissions are granted (§9).

### Files, pictures and voice
- **Project folders:** folders on a person's own PC, lent through the browser. lyra can read and search them, and write after approval, with a diff and an "Always allow" option.
- PDFs read as text; scans and pictures read by the vision model, including those attached in chat. Transparent PNGs are flattened before reading.
- Dictation in the browser.
- Read replies aloud with the device's voices, auto-read and speed.

### Team and administration
- **AI usage** per person and per model, with per-model prices.
- **Feedback:**
  - bug reports, feature requests and questions;
  - lyra's write-up of each (summary, cause and fixes, or implementation);
  - **Enhance** to rewrite a draft more clearly;
  - status, priority, comments and pushes.
- **Q&A**, readable by everyone: questions with answers drafted by lyra and approved by an admin. lyra also searches it in chat.
- What's new and version numbers (`CHANGELOG.json`, `major.minor.fix.build`).
- `scripts/lyra-check-config`.

### The app (PWA)
- Built from AI Elements: attachments, Confirmation, Task, Sources, Context, Suggestion, Plan, Queue, Checkpoint, Terminal, File tree, Code block, Model selector, Voice selector.
- An icon rail with names, and a conversation panel beside it.
- Teal accent, a phone layout, push notifications, installable.

---

## 3. Production health (live check, read-only)

| Area | State |
|---|---|
| Service | Active. About 380 MB RSS (peak about 500 MB), 1.3% CPU. **107 restarts in 4 days, all clean deploys, no crashes.** |
| Errors (3 days) | 130 of 3,731 journal lines; **no panics**. Recurring ones are listed below. |
| Host | Fedora 43, up 117 days. Load 0.1, 26 GiB RAM free, `/` 31% used. |
| Data | `~/.lyra` is 105 MB: capabilities index 75 MB, memory 25 MB. 16 sessions, 6 skills, 2 users, 11 devices, 3 feedback items, Q&A empty. |
| Models | Every check passes: chat, embedding (4096-d), reranker, decide, vision, SearXNG, PMI. |
| Backups | Nightly at 03:30, keep 7; newest about 8 hours old. **Size jumped 8.8 → 39.6 MB in one night** (I-12). |
| Usage (about 20 hours) | 4,295 model calls; 1.49M input and 130k output tokens. The small models make up 97% of calls but a small share of tokens. Chat uses 21 calls for 520k input tokens, so prompts are large (I-14). |
| Host logging | `/var/log/messages` (68 MB) hasn't rotated since Jul 28; the journal is 1 GB. This is the host, not lyra (O-1). |

Recurring errors:

| Count | Error | Ref |
|---|---|---|
| 13× | The desktop node reports `mnt-dbr2-repo.mount failed` (a real NFS problem on that PC) | O-2 |
| 8× | Operator "program not found" | I-15 |
| 5× | Memory capture: "bad capture JSON … null" | I-13 |
| 2× | PMI 422: a comment is required when closing a task | I-16 |
| 2× | "no open lyra page has the folder ".""  | I-17 |
| 1 | A short decide-model outage on Oct 7, when its port was briefly given to Gemma (resolved) | |

---

## 4. Issues (verified)

### High: fix before more people use it

| ID | Issue | Where | Fix |
|---|---|---|---|
| **I-1** ✅ | **Fixed in 0.29.5.158:** folders are kept per person in IndexedDB (`folders:<user id>`), only the signed-in person's are announced (none until lyra says who that is), and the announcement says whose they are: the server takes folders only when that matches the connection's person, so an older cached app lends none. The old browser-wide list is dropped once (whose it was can't be known); the Projects page says to add them again. *Was:* **Lent project folders belong to the browser, not the person.** Folder handles live in IndexedDB under one key and are re-announced as whoever signs in next. On a shared PC or browser profile, user B's chat can read, search and (after approval) write user A's folders. | `web/ui/src/lyra/folders.ts:57-78`, `web/src/lib.rs` "folders" | Key the store by user (`folders:<id>`), and only announce the signed-in person's folders. |
| **I-2** ✅ | **Fixed in 0.29.5.158:** `/history` and `/rollback` look skills up with `find_as(key, viewer)` (shared, or the person's own), `/rollback` also needs `editable_by`, and `/agent`'s skill assignment takes shared skills only. *Was:* **`/history <skill>` shows another member's personal skill:** its versions, evidence (from their conversations) and related skills. Members are allowed `/history`, and it looks the skill up without checking whose it is. | `src/learn.rs:308`, `src/commands/dispatch.rs` (`/history`) | `find_as(key, viewer)` in `history` (and `rollback`). |

### Medium

| ID | Issue | Where | Fix |
|---|---|---|---|
| I-3 | **The curator can pull personal skills into shared ones.** `review_collection` and `apply_plan` look at all skills, so duplicates or merges can include someone's own skill and an admin's approval makes it shared. | `learning/src/manager.rs:665-686`, `src/learn.rs:161-168` | Filter `owner.is_none()` in both. |
| I-4 ✅ | **Fixed in 0.25.3.147** (`store.rs`; `pass` now takes out only the watches that fired). *Was:* **The watches file can lose updates.** No lock, no atomic write, and `pass` saves an old copy after slow network checks, so a watch added or cancelled meanwhile is lost or comes back. | `src/watches.rs:40-46, 147-167` | Lock, write to a temp file and rename, then re-load and merge before saving. |
| I-5 ✅ | **Fixed in 0.30.1.161:** each Get is answered in its own task through a channel, so a slow one doesn't stop the connection's live updates or folder requests. *Was:* **One app connection freezes while a slow request runs** (search everything, Enhance, models). The WebSocket loop awaits the answer inside `select!`, so the page gets no live updates and its folder requests stall. | `web/src/lib.rs` "get" arm | Answer each Get in its own task, through a channel. |
| I-6 ✅ | **Fixed in 0.30.1.161:** the conversation list, search, Mail, Calendar and PMI pages are made on threads (`serve::data::slow`), and page commands that only call out (`/recap`, `/calendar`, `/mail`, `/today`, `/pmi`, `/tasks`, `/task`, `/model`) run on threads (`App::off_loop`); a `/model` switch is checked off the loop and applied on it. *Was:* **Some requests block every conversation** while they make network or model calls on the serve loop: the app's Mail, Calendar and PMI pages; `/recap`, `/mail`, `/today`, `/pmi` and `/model` from page buttons; reading all sessions for the list and search. | `src/serve/data.rs` `data()`, `quiet_command` (`src/commands/dispatch.rs`) | Move them to threads, as `everything`, `models` and `feedback_enhance` already are. |
| I-7 ✅ | **Fixed in 0.30.1.161:** attachments are read on a thread and the message comes back to the loop to be sent (`deliver`); the pictures are set only where `send()` runs and cleared after, so nothing rides along on the next message. *Was:* **Attachments are parsed on the loop, and leftovers carry over.** PDF text and image base64 are done inline. `attach_images` and `attach_looks` are set before the `/new`, `/resume` and busy branches, so they can ride along on the next message. | `src/serve/loop.rs` (the `send` arm) | Set them only where `send()` runs; do the work in the turn thread. |
| I-8 ✅ | **Fixed in 0.30.1.161:** conversations are marked in progress before the thread starts, and one filed or archived meanwhile gets no suggestion. *Was:* **Folder suggestions start twice.** `looked` is set only after the decision model answers, so each refresh of the list starts another thread for the same conversations. | `src/serve/data.rs` (`"sessions"`) | Mark them in progress before spawning; skip ones the user has filed meanwhile. |
| I-9 ✅ | **Fixed in 0.30.1.161:** `calendar::events` follows `@odata.nextLink` (up to 2,000 events), and free-text spans are capped at 21 days. *Was:* **Find a time can miss busy time.** The calendar reads at most 100 events with no paging; free-text spans aren't capped. | `src/calendar.rs:260-266, 312` | Follow `@odata.nextLink`; cap spans at about 21 days. |
| I-10 ✅ | **Fixed in 0.30.1.161:** `usage::since` walks the month files in UTC, as they're named. *Was:* **Usage undercounts at month end.** Files are named by UTC month but read by local month (Texas is behind UTC). | `src/usage.rs:103, 120-126` | Walk months in UTC. |
| I-11 ✅ | **Fixed in 0.30.1.161:** trust is decided by the same page pick as the request (`Hub::folder_trusted`), and a write that went ahead without asking goes only to a page where that folder is trusted (`call_trusted_folder`). *Was:* **A trusted folder on one PC can skip approval for a same-named folder on another PC.** Trust and routing are decided separately. | `src/projects.rs:95`, `web/src/lib.rs` `pick_page` | Decide trust and route by the same page. |
| I-12 ✅ | **Fixed in 0.30.1.161:** confirmed: `capabilities/index` went 14 → 59 MB in a night (94 MB now, 1,244 LanceDB versions: every sync made one and none were dropped), and memory 1 → 14 MB (real data). The capabilities and agent-routing indexes are left out of backups (rebuilt on every start), and the index is compacted and its versions older than an hour pruned after each change. *Was:* **Backups grew 4× overnight** (8.8 → 39.6 MB), probably the 75 MB capabilities index. It can be rebuilt, so it needn't be backed up. | `src/backup.rs` | Leave out `capabilities/index` (and other rebuildable LanceDB indexes); confirm what grew. |

### Low

| ID | Issue | Fix |
|---|---|---|
| I-13 | Memory capture fails when the model returns `null` for a text field (5× in 3 days). | Accept null as missing. |
| I-14 | Chat prompts are large: about 25k input tokens per chat call on average (system prompt, memories, skills, tools). | Measure each part, trim the tool list, cache. |
| I-15 | The Operator runs commands that don't exist on the target ("program not found", 8×), mostly PowerShell and Linux mix-ups. | Tell it the target's OS and shell in its prompt; a better error. |
| I-16 | PMI 422 "a closing comment goes with completing the task". | Ask for or add a closing comment in `pmi_complete`. |
| I-17 | "no open lyra page has the folder "."": the model passes "." as a folder name. | Treat "."/"" as "which folder?" and list the folders. |
| I-18 ✅ | **Fixed in 0.25.3.147.** *Was:* `watches::cancel ""` removes the first watch. | Reject an empty id. |
| I-19 | `teams_from` watches can fire on messages from before the watch existed. | Compare against `created`. |
| I-20 | Promoting a question to Q&A isn't atomic, and the same question can be promoted twice. | Check `source` first; link both steps. |
| I-21 | Feedback: `analyzing` can stick if its thread panics; the badge re-reads the file for every status update; Q&A saves don't bump the refresh counter. | Small fixes in `feedback.rs`, `qa.rs` and `serve/mod.rs` `status()`. |
| I-22 ✅ | **Fixed in 0.25.3.147.** *Was:* `recap` saves aren't atomic. | Write to a temp file and rename. |
| I-23 ✅ | **Fixed in 0.29.1.154:** `briefing` is on the members' page list and `/briefing` on their command list. `/briefing` showed the owner's briefing (`briefing::last()`) and is now each person's own (`last_for`); "Brief me now" makes only that person's briefing (`request_for`; the schedule still makes everyone's), and the batch counts what's left instead of waiting for the owner's, so a member-only one can't leave it busy. *Was:* members got "that's for admins" for their own briefing page. | Add `briefing` to the member list. |
| I-24 | "Search everything" threads keep running after its 8 s cutoff. | One search at a time per person. |
| I-25 ✅ | **Fixed in 0.29.2.155:** secrets files were wiped down to the Microsoft tokens on 2026-10-08 15:05 (the owner's and a member's: lyra's Microsoft app secret and PMI tokens lost). `secrets::set_token_for` read the file, changed one token and rewrote it with a truncating write and no lock, so concurrent Microsoft token refreshes read an empty file and wrote back only their own token. Now one lock, a private temp file renamed into place, and a file that can't be read is an error instead of a fresh start. | Re-enter the lost secrets (`lyra secret entra`, `/pmi token`). |

### Checked and fine
- Every `data()` handler takes identity from the connection, never from the client.
- Background threads run as the right person.
- Feedback and Q&A permissions are checked on the server.
- Personal skills are filtered for use, listing and decisions.
- Memory search keeps people's scopes apart.
- Downloads are limited to their sender, admins and machines.
- Project paths can't escape their folder.
- Usage is filtered per person.

---

## 5. Gaps (missing rather than broken)

| ID | Gap |
|---|---|
| **G-1** | **The web app has no automated tests.** That's 16.5k lines of TypeScript, and every change is checked by hand or with ad-hoc headless screenshots. A Playwright smoke suite against a demo lyra with a fake model (the same setup used during development) would catch regressions like the production-only Terminal crash. |
| **G-2** | **Thin tests on the server's riskiest parts:** `serve/` (2 tests, about 2,000 lines), `plan.rs` (0 / 514), `search.rs` (0), `acting.rs` (0), `connect.rs` (2), `evolve.rs` (2), and the Graph modules (`calendar.rs` 4, `mail.rs` 4, `teams.rs` 1, `files.rs` 1), whose network paths are untested. |
| G-3 | **Features nobody has tried with live data yet:** find a time, watches, meeting follow-up, search everything over mail, Teams and files, and personal skills for a real member. They're built and unit-tested but not exercised against Microsoft 365 or PMI. |
| G-4 | **No usage budgets or alerts** (proposed earlier as item 8): no per-person monthly limits, no warning at 80%. |
| G-5 | **No in-app admin view of server health** beyond Status: logs, restart history and error rates (§3 needed SSH). |
| G-6 | **No end-to-end encryption or secret rotation story** for the Entra client secret, PMI tokens and device tokens. `secrets.toml` is 0600 and left out of backups, but nothing rotates or expires them. |
| **G-7** ✅ | **Done in 0.28.0.152:** an outbox (`web/ui/src/lyra/outbox.ts`, kept in localStorage): plain messages are queued and sent one at a time, only when connected and lyra isn't answering, and leave the outbox only once the conversation shows them (re-sent if the connection dropped first, or after 20 s unseen), so nothing is lost or doubled; Edit and Cancel while waiting. Drafts are kept per conversation; the last conversation is kept on the device so the app opens with it offline; all of it is wiped when the device is unpaired. Files and commands still need lyra there. *Was:* offline and poor-network behaviour was basic: the shell was cached, but no queued sending or offline drafts. |
| G-8 | **Accessibility** isn't audited: keyboard navigation of the rail and dialogs, contrast of the teal on dark, screen-reader labels (many icon buttons have them, not all). |
| G-9 | **Paging and limits:** Graph lists cap at fixed `$top` values (calendar 100, mail 50) without paging; the conversation list caps at 300 plus pinned and filed ones. |
| G-10 | **Meeting transcripts** wait on the permissions (§9). **Teams messages and files are read-only:** lyra can't post to Teams or upload to OneDrive. |
| G-11 | **The Q&A page is empty.** Promote a few early questions so it's useful from the start. |

---

## 6. Technical debt

| ID | Debt | Why it matters |
|---|---|---|
| **D-1** ✅ | **Done in 0.25.0.144:** `main.rs` 4,405 → 2,140 lines and `serve.rs` split into `serve/{mod,loop,data,pushes}.rs`, with `turn.rs`, `startup.rs` (one `configure()` for start and reload) and `commands/` alongside. *Was:* **`src/main.rs` is 4,405 lines and `src/serve.rs` 2,241.** `main.rs` holds the app state, the command dispatcher (40 commands), the tool loop, the startup and reload config and more. `serve.rs`'s `data()` has 28 arms. | These are the slowest files to change and review, and the source of bugs like startup config only applied on reload (since fixed). Split them into `commands/`, `turn.rs`, `startup.rs`, and `serve/{loop,data,pushes}.rs`. |
| **D-2** ✅ | **Done in 0.25.3.147:** `src/store.rs` (`JsonStore<T>` with a lock per file, `write_atomic`/`write_json`/`read_json`/`write_text`; an unreadable file is set aside as `<name>.bad-<time>` and logged) used by watches, recap, feedback, Q&A, proactive, routines, health and status alerts, diagnoses, briefings, coding jobs, PMI nags, style, notes, sessions and their meta, the OpenAPI spec cache and the MCP files. *Was:* **About 12 hand-rolled JSON file stores** (watches, recap, feedback, qa, proactive, routines, sessions meta, style, notes, health and status alerts…), most writing with a plain `fs::write` and ignoring errors. | Races and torn writes (I-4, I-22). One small `JsonStore<T>` helper (lock, temp file, rename, error logging) used everywhere. |
| **D-3** ✅ | **Done in 0.25.4.148:** `text.rs` (`html_text`, `xml_text`, `page_text`: one tag stripper and one entity reader), `alerts.rs` (`Ledger`: told once, cleared once, kept in `alerts/<name>.json`, older files still read) under both `health::Alerts` and `status::Alerts`, and `graph.rs` (the Microsoft sign-in, each person's connection and access tokens, `graph`/`graph_with`/`graph_bytes`, Graph times) used by calendar, mail, Teams, files, meetings, planner and proactive; no module builds a Graph URL itself. *Was:* **Duplicated helpers:** two HTML/XML-to-text strippers (`teams.rs`, `files.rs`) besides `html2text`; two near-identical `Alerts` types (`health.rs`, `status.rs`); Graph HTTP helpers spread across modules. | Pull them into `text.rs`, `alerts.rs` and `graph.rs`. |
| D-4 | **`execution/src/engine.rs` has about 25 `unwrap()`s** outside tests, e.g. `plan.step_mut(id).unwrap()`. | A bad plan state would panic the plan thread. Replace them with errors. |
| **D-5** ✅ | **Done in 0.26.0.149:** the first load is 1.1 MB of JavaScript (345 KB gzipped), was 1.7 MB (480 KB). Every page but the chat loads when first opened (`page()` in `App.tsx`, reloading once if an update removed its chunk); Shiki's core, engine and themes load with the first code block; the unused tokenlens price catalogue and motion (Shimmer is CSS now) are gone; `chunkSizeWarningLimit` is 1100 KB so growth shows in the build. **0.26.1.150:** lyra-web now sends the app gzipped (it went out uncompressed before): on an emulated phone (1.6 Mbit/s, 150 ms, CPU ×4) the first screen takes 2.5 s and 392 KB, was 9.9 s and 1,849 KB. What's left is React, Streamdown with its Markdown/HTML parsers (parse5 for raw HTML in replies) and the chat. *Was:* **The app's main bundle is 1.7 MB** (Shiki grammars add about 1 MB more on demand). | A slow first load on phones. Split the routes (Feedback, Q&A, Usage, manage pages) and lazy-load Shiki. |
| **D-6** ✅ | **Done in 0.27.0.151:** a Settings page (admins; the menu under your initials and More) and `/settings` for the common ones: models (chat model, server, prices, vision and decision models), working hours, briefing and recap, notifications. `src/settings.rs` holds one list of fields (key, label, help, kind with limits, getter) that drives the page, the checks and the command; a save checks every value first, writes only what changed with `config::update` (comments, even after a value, kept; never a file lyra cannot read back) and reloads every conversation. *Was:* **The config has 40 sections** (401-line example). | Powerful but daunting. The check script helps; a **Settings page** for the common ones (models, working hours, recap, notifications) would help more. |
| D-7 | `lancedb` is pinned at `=0.37.1` (0.38+ needs its `remote` feature). | Watch for a fix upstream, then upgrade. |
| D-8 | **Changelog discipline relies on a rule.** The test checks format and numbering, but not that each commit has an entry. | A git pre-commit hook could enforce it. |
| D-9 | **Help text is spread over 5 `COMMANDS` constants**, with `PAGE_COMMANDS` (32) and `member_may` kept by hand. | One table with each command's help, page permission and member permission. |

---

## 7. Operations

| ID | Item |
|---|---|
| O-1 | **Host log rotation:** `/var/log/messages` isn't rotating and the journal is 1 GB. Check logrotate and rsyslog, and set `SystemMaxUse=` for journald. |
| O-2 | **The desktop's NFS mount** (`mnt-dbr2-repo.mount`) keeps failing and alerting (13×). Fix the mount, or quiet that alert. |
| O-3 | **Restart churn:** 107 restarts in 4 days from deploying every change. Fine during heavy development, but each one drops connections and in-flight turns. Consider batching deploys or a graceful restart that waits for answers to finish. |
| O-4 | **Deploys build on the production host** (`cargo install` there). This works, but it builds as root on the server, takes minutes and uses its CPU. A CI build (or building on the desktop and copying the binary), with a quick rollback to the previous binary, would be safer. |
| O-5 | **Backups:** check what grew (I-12), and do a **test restore** to a scratch directory once, so restoring is known to work. |
| O-6 | **Monitoring from outside:** lyra checks itself, but nothing tells you if lyra itself is down. Add a simple external check (Zoraxy, Uptime Kuma) on `/health`. |

---

## 8. Roadmap

### Now: hardening (about a week)
1. ~~I-1, I-2~~ (done), **I-3:** the cross-user problems.
2. ~~I-5, I-6, I-7: nothing slow on the serve loop or a connection's loop~~ (done).
3. ~~D-2: one atomic JSON store, which also fixes I-4 and I-22~~ (done).
4. ~~I-9, I-10, I-12, I-23~~ (done), **I-13** and the small ones (I-17 to I-21).
5. **G-1:** a Playwright smoke suite (sign in, chat, approval, feedback, Q&A, projects) against a demo lyra with a fake model, run before each deploy.
6. **O-1, O-5, O-6:** log rotation, a test restore, an external uptime check.

### Next: make it easier to live with (2–4 weeks)
1. ~~D-1: split `main.rs` and `serve.rs`~~ (done). **D-9:** one command table.
2. ~~D-5: a smaller app bundle, faster on phones~~ (done).
3. **G-4:** usage budgets and alerts.
4. ~~D-6: a Settings page for the everyday options~~ (done).
5. **G-3:** try every Microsoft 365 feature with real accounts and members; fix what turns up.
6. **G-10:** turn on meeting transcripts once the permissions are granted.
7. **I-14:** trim the chat prompt (faster and cheaper replies).

### Now: user features (asked for 2026-10-08)
1. ✅ **Home-screen shortcuts and notification buttons** (0.31.0.162): long-press the icon for New chat, Add task, Quick note (manifest `shortcuts`, `lyra/intent.ts`). Approvals (Allow / Deny) and reminders (Done / In 1 hour / Tomorrow) were already answerable from the notification.
2. **Saved prompts (templates):** one-tap starters; each person's own, plus shared ones an admin publishes.
3. **Meeting workspace:** a page per meeting: prep before, notes during, the follow-up after (summary, decisions, your action items as tasks, a draft email to attendees).
4. **Document workspace:** draft a letter, memo or one-pager side by side with lyra; save it to OneDrive or attach it to an email.
5. **"What lyra knows about me":** memories, writing style, connected accounts and recent actions on one page, each correctable, deletable or exportable.
6. **"Why?" on any action:** what prompted it and which rule or skill applied.

### Next major release (1.0)
- **Voice conversation mode:** hands-free back-and-forth (driving, walking between offices), best with a local Whisper model for speech to text (and a small TTS model for the voice).

### Later: grow
See §9 for the ideas in more detail.

---

## 9. Feature ideas

Grouped by who benefits; ⭐ marks the most useful.

**Everyone**
- ⭐ **Mobile quick actions:** answer approvals and reminders straight from the notification, and an "ask lyra" home-screen shortcut.
- ⭐ **Shared spaces:** share a note, list, folder of conversations or Q&A entry with named coworkers.
- **Send Teams messages** (after approval) and post meeting follow-ups to the meeting's chat.
- **Email digests** for people who live in Outlook: the briefing and recap by mail.
- **Templates:** saved prompts or workflows ("weekly status", "incident write-up") anyone can run.
- **A Deeper look for feedback:** the Coder reads the source code (read-only, on the server) for exact fixes.
- **A voice conversation mode:** hands-free, combining dictation and read-back in a loop (the AI Elements Transcription and Audio player components fit here).
- **A document workspace:** draft a Word document or one-pager with lyra and save it to OneDrive (after approval).

**Admins**
- ⭐ ~~**A Settings page** (D-6)~~ (done) and **an admin dashboard**: errors, restarts, model latency, usage trends and feedback counts on one screen.
- **Per-person policies:** which tools, models and machines each member may use; spending limits (G-4).
- **Audit log:** who approved what, when (from the existing approval records), searchable and exportable.
- **A fleet view for machines:** patch status, last backup and disk trends across all nodes.

**lyra itself**
- **Smarter routing:** learn from wrong delegations (the Operator searching for a project folder) to stop detours.
- **Evaluations:** a fixed set of everyday questions run after each model or prompt change, scored, with results on What's new for admins.
- **Model choice per task:** the small model for quick answers, the big one for plans, chosen automatically.

---

## 10. Decisions and actions needed from you

| # | What | Why |
|---|---|---|
| 1 | **Grant `OnlineMeetings.Read` and `OnlineMeetingTranscript.Read.All`** (admin consent) in Entra, then set `[web.entra] meetings = true`. | Turns on meeting follow-ups (G-10). |
| 2 | **Approve the hardening sprint** (§8 Now) before inviting more coworkers. | I-1 and I-2 matter as soon as two people share a device or have personal skills. |
| 3 | **Deploy approach** (O-3, O-4): keep deploying on every change, or batch them and build elsewhere? | Fewer restarts and safer rollbacks. |
| 4 | **Host logging** (O-1) and **the desktop NFS mount** (O-2). | Both are outside lyra; they need you or a decision to fix. |
| 5 | **Seed Q&A** with a few answered questions (G-11). | Makes the page useful from day one. |
| 6 | **Usage budgets** (G-4): set limits per person? | Only matters once cost or capacity does. |

---

## 11. By the numbers

| | |
|---|---|
| Version | 0.25.0.141 (141 commits, 2026-10-04 → 10-08) |
| Rust | about 56,900 lines in 11 crates; the main binary has 54 modules (29,900 lines) |
| TypeScript (app) | about 16,550 lines, 77 files |
| Tests | 329 Rust tests (`src` 130, memory 52, learning 38, execution 34, evolution 27, web 14, …); app: none |
| clippy | warning-free (required on every commit) |
| Code markers | 0 TODO/FIXME; 5 `#[allow]`s (4 too-many-arguments, 1 dead-code) |
| Slash commands | 40 top-level |
| App bundle | 1.7 MB main JS, plus Shiki grammars on demand |
| Production data | 105 MB in `~/.lyra`; about 4.3k model calls a day |
| Design docs pending | none (all 9 in `docs/done/`; `docs/api.json` is the PMI API spec) |

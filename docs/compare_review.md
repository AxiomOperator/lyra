# lyra compared: Grok Bot, Meta Muse and OpenClaw

A review of `docs/compare.md` (the feature inventory of Grok Bot, Meta Muse and
OpenClaw, researched 2026-10-10) against lyra as it is in version 0.61.0.207.

How to read it:

- **The three products** are as `compare.md` describes them. They weren't
  checked separately.
- **lyra's side** was checked in the code; each row names the module that does
  it.
- **The four verdicts:**
  - **Lyra leads:** lyra does more, or does it more safely.
  - **Same:** comparable.
  - **Behind:** lyra has it, but it's narrower.
  - **Full gap:** lyra has nothing for it.
- **"By design":** a gap that comes from one of lyra's own rules (approvals stay
  in Rust, nothing runs off the host unless named, email only to yourself,
  members never get machines or system tools). These aren't missing work; they
  stay unless the owner decides otherwise.

## At a glance

| Area | Grok Bot | Meta Muse | OpenClaw | lyra |
|---|---|---|---|---|
| Agent identities and lifecycle | strong | — | strong | **Same** (no avatars, no sharing) |
| Long-term memory | yes | yes | strong (LanceDB, dreaming) | **Leads** (typed, versioned, curated, per person) |
| Skills and self-improvement | from demonstrations | built-in skills | SKILL.md, workshop | **Leads** on self-evolution; **behind** on learning from demonstrations |
| Multi-agent orchestration | strong | — | strong (A2A, ACP, swarms) | **Same**: parallel and nested delegation, a shared task board, direct questions between agents, group huddles, agents starting each other's work; **gap** only for outside protocols (A2A, ACP) |
| Computer and browser use | cloud computer | secure VM | browser automation | **Gap** for the browser; **leads** for real machines (nodes) |
| Goals | — | strong | standing instructions | **Same / leads** (goal manager, plans, autonomy policy) |
| Proactivity and routines | routines | monitoring | cron, heartbeat, webhooks | **Same** (routines, briefing, watches, proactive); **gap** for webhooks |
| Channels | desktop, mobile, voice | app, WhatsApp | 7 chat apps, voice calls | **Behind** (PWA, TUI, push, email to self) |
| Governance and security | approval cards, rules, audit | VM, payment cards | policies, sandboxes | **Leads** (Rust-enforced policy, per-action approvals, per-person isolation) |
| Work integrations | apps | email, calendar, Instagram | many | **Leads** for Microsoft 365 and PMI depth |
| Purchases and payments | secure forms | one-time cards | — | **Gap (by design)** |

## Where lyra leads

| What | Compared with | lyra |
|---|---|---|
| **Memory with structure.** Typed memories (semantic, episodic, working) with provenance, versions and supersede; hybrid keyword and vector search with reranking; a context compiler; a curator that proposes merges and retirements for approval; a safety scan; re-embedding and backups. | OpenClaw's LanceDB memory and "dreaming"; Grok's and Muse's persistent memory | `memory/` crate (`MemoryManager`, `curator`, `capture`); `src/mem.rs`; Memory page by scope and kind |
| **Per-person isolation on one server.** Each person has their own memories (`user:<id>`), notes, routines, goals, briefing, PMI and Microsoft account. Every background job carries whose behalf it works on (`acting::spawn`). Members never get machines, system tools or coding. | Grok's team accounts share one cloud computer; OpenClaw is single-user | `acting.rs`, `MEMBER_PAGES`, `member_may` |
| **Policy in Rust, not in the prompt.** Every capability has a risk level, and the policy (auto / approval / deny) is enforced in Rust. Shell commands go through a command classifier (auto / ask / forbidden). Anything other people would see waits for an Allow. | Grok's approval cards and rules; OpenClaw's tool policies | `capabilities/` (policy), `system/` (`System::check`), `asks.rs` |
| **Self-evolution with rollback.** lyra watches its own runs, detects problems, proposes candidates (guidelines, behavior settings, workflows, composite tools, skill refinements, code patches in a sandboxed worktree), benchmarks them against the baseline, and deploys or rolls back by generation. | None of the three describe anything like this; OpenClaw's "self-learning" is the closest | `evolution/` crate, `src/evolve.rs` |
| **Skills learned from corrections.** Reusable procedures picked up from corrections and successful runs: one Markdown file each, with a ledger of versions, usage, outcomes and an audit log, and proposals waiting for approval. | OpenClaw's SKILL.md and Skill Workshop; Grok's skills from demonstrations | `learning/` crate, `src/learn.rs` |
| **Real machines, not a sandbox VM.** lyra-node lends a machine's tools to the server over an outbound WebSocket (Linux and Windows), with health, self-update, per-machine rules and read-only diagnoses that write up the cause and the fix. | Grok's cloud computer; Muse's secure VM | `node/`, `src/diagnose.rs`, Machines page |
| **Microsoft 365 and PMI in depth.** Calendar, mail and Teams; plan-my-day focus blocks; meeting prep, notes and follow-ups from transcripts; mail triage into PMI tasks with drafted replies (never sent); follow-ups on mail that got no answer; "Who is …" across sources; OneDrive and SharePoint files. | Muse's connected email and calendar; Grok's integrations | `calendar.rs`, `mail.rs`, `teams.rs`, `planner.rs`, `proactive.rs`, `meetings.rs`, `people.rs`, `pmi.rs` |
| **Transparency to each person.** The "What lyra knows about me" page lists every change a tool made for them, why (their message, triage, a routine), which skills were in play and whether they approved it. Their AI usage is per person, per kind and per conversation. | Muse's activity history; Grok's audit logs | `actions.rs` (About me), `usage.rs` (Usage page) |
| **Local models with a fallback chain.** Any OpenAI-compatible server; a first and second fallback; background jobs go to the first fallback that takes them; models can be marked known down; separate embedding, reranker, decision and vision models. | OpenClaw's multi-provider support and model fallback | `fallback.rs`, `known_down.rs`, `decide.rs`, `vision.rs` |
| **Coding hand-off with guard rails.** Coding work goes to Claude Code or OpenCode in a project folder on a named machine: one approval, full auto inside, never pushes, resumable. | OpenClaw's code execution and ACP | `coding.rs` |

## Where lyra is the same

| What | lyra |
|---|---|
| Agent-to-agent coordination (since 0.62–0.63): delegations side by side and nested; direct questions between agents; group huddles of two to four agents; a task board per conversation where agents post tasks and notes for each other, and posted tasks start the other agent's work by themselves | `agents.rs` (`delegate_many`, `ask_agent`, `huddle`, `hand_on`), `board.rs`; `[agents] max_parallel`, `board_autostart` |
| Named, persistent agents with roles, instructions, routing and a conversational creation wizard with templates | `agents/` (profiles as TOML, versions, rule / semantic / model routing, `builder`, `templates`) |
| Separate memory per agent (its own `agent:<name>` scope, or none, read-only, or scoped) | `agents/src/model.rs` memory modes |
| Persistent sessions: saved conversations, resume (`-c` / `-r`), folders, pins, archive, search across them, age limits | `sessions.rs`, `retention.rs` |
| SOUL / USER / AGENT context files | `context.rs` |
| Streaming replies with progress, steps to skip, forms for a missing piece, a Running now page, stop | `turn.rs`, `asks.rs`, Running now page |
| Long-running work that goes on after the person disconnects | `lyra serve` (the loop runs on the server) |
| Scheduled routines that remember their last two results, report "all clear" or what needs you, can email the result to you, resume after a restart; paused and run on demand | `routines.rs` (`enabled`, `/routine run`) |
| A daily briefing and an end-of-day recap | `briefing.rs`, `recap.rs` |
| Goals in plain language: decomposition, plans, progress, blockers, priority, triggers and an autonomy policy | `goals/` crate, `execution/` crate |
| "Tell me when …" monitoring (mail from someone, replies, PMI task changes, Teams messages) | `watches.rs` |
| Push notifications to phones and browsers, and email (to the person only) | `web/` (Web Push), `mailout.rs` |
| Dictation and read-aloud (the device's own voice) | `web/ui/src/lyra/dictation.ts`, `voice.tsx` |
| Web search and page reading, with a reader that condenses long pages | `websearch.rs` |
| Documents: drafted and revised with lyra, to Word, OneDrive or an email draft | `notes.rs`, `documents.rs`, `docx.rs` |
| PDFs, scans and pictures read as text | `vision.rs` |
| MCP as client (stdio providers) and as server (lyra's own tools to other harnesses) | `capabilities/` MCP provider, `mcp_server.rs` |
| Team administration: admins and members, pending sign-ins, usage per person, devices by person | Users, Usage and Devices pages |
| Backups, versioned state, diagnostics, status checks with history | `backup.rs`, `status.rs`, Status page |
| Secrets kept out of the model's context (`secrets.toml`, 0600, not backed up) | `secrets.rs` |

## Where lyra falls behind

| What | The others | lyra today | What closing it would take |
|---|---|---|---|
| **Learning from demonstrations** | Watch a person do it, turn it into a skill (Grok) | Skills come from corrections and successful runs, not from watching | A "show me" recording in the app (steps typed or picked) that the skill evaluator turns into a proposed skill |
| **Dynamic tool building** | Builds tools when integrations fall short (Muse) | Evolution proposes composite tools and workflows, benchmarked and approved; OpenAPI and MCP providers are added by an admin | Let the evolver propose an OpenAPI provider from a URL; still behind approval |
| **Channels** | WhatsApp, Telegram, Discord, Slack, Signal, iMessage, Teams (OpenClaw); WhatsApp (Muse) | The PWA, the TUI, push and email to yourself; Teams is read (and watched), not answered | A Teams bot or chat reply path first (it's already connected); other channels would need their own sign-in and isolation per person |
| **Voice** | Voice calls and spoken conversation (Grok, OpenClaw) | Dictation in and read-aloud out, per device | A hands-free mode that chains the two (listen, send, speak, listen) |
| **Context compaction** | Session compaction and pruning (OpenClaw) | Long tool results are shortened to fit; older turns aren't summarized | Summarize the oldest turns into one note when a conversation nears the model's context |
| **Agent sharing and look** | Avatars, colors, duplicate and share templates, Team Bots (Grok) | Agents are the owner's, set up from templates; no avatar or color; no sharing between people | A colour and icon per agent profile; "duplicate" in the wizard; shared agents for members stay an admin's choice |
| **Reasoning effort** | Configurable per run (OpenClaw) | Not exposed; the model's defaults apply | A per-conversation setting passed to servers that support it |
| **Monitoring the web** | Prices and activities (Muse) | Watches cover mail, replies, PMI and Teams; a routine with web search can do it, without a "tell me when the price drops" | A web watch kind (a page or a search, checked on a schedule, told once on change) |

## Full gaps

| What | Who has it | Note |
|---|---|---|
| **Browser automation and computer use** (driving websites without APIs, forms, bookings, customer service) | Grok, Muse, OpenClaw | lyra reads pages (`web_fetch`) but doesn't drive them. The safest fit would be a headless browser on a named node, every submit behind an Allow. |
| **Inbound webhooks and event triggers from outside** | OpenClaw | Nothing receives events from other systems. Goals have triggers, and watches poll, but no URL accepts a push. It would need per-person tokens and an Allow before acting. |
| **Plugin marketplace** | OpenClaw (ClawHub) | Capabilities, skills, agents and MCP providers are added locally; there is no catalogue to install from. |
| **Agent Client Protocol and Agent-to-Agent protocols** | OpenClaw | lyra speaks MCP (both ways) only. |
| **Image generation and presentations** | Muse (images), Grok (presentations) | Documents go to Word; no images or slides are made. |
| **Purchases, payments, one-time cards, secure payment forms** | Muse, Grok | By design: lyra doesn't spend money or collect payment details. |
| **A sandboxed computer per agent** (VM or container) | Muse (secure VM), Grok (shared cloud computer), OpenClaw (sandbox options) | lyra works on real machines through nodes, with rules and approvals instead of a sandbox. A container per job would add isolation for coding and diagnoses. |
| **Mobile device control** | OpenClaw | The PWA runs on phones, but lyra doesn't control them. |

## What would matter most next

In order of value for lyra's users, keeping its rules:

1. ~~**Agent-to-agent coordination.**~~ Done in 0.62–0.63: delegations side
   by side, a task board per conversation, direct questions between agents,
   group huddles, and agents starting each other's work.
2. **Context compaction** for long conversations. It's cheap and helps every
   chat, especially on local models with small contexts.
3. **Answering in Teams.** lyra already reads and watches Teams per person.
   Replies would be drafts or behind an Allow, like mail.
4. **A web watch** ("tell me when this page or price changes"). It fits the
   existing watches and routines.
5. **A browser on a named node**, read-first, with every submit behind an
   Allow. It closes the largest functional gap while keeping approvals in Rust
   and nothing off the host unless named.
6. **Hands-free voice**, by joining dictation and read-aloud.

Left out on purpose: payments and purchases, a marketplace, and channels that
would send as the person without an Allow.

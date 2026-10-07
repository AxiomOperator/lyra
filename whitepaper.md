# Lyra: A Self-Hosted, Self-Improving Personal and Team Assistant on Local Models

**Whitepaper · October 2026**

---

## Abstract

Lyra is an AI assistant written in Rust. It runs on hardware its owner controls and talks to any
OpenAI-compatible model endpoint (llama.cpp, vLLM, Ollama, LM Studio and others). It began as a
terminal chat client. It now provides:

- long-term memory backed by a vector database
- procedural learning ("skills")
- planning with verification and checkpoints
- long-lived goals with bounded autonomy
- user-defined subagents with permission checks
- one capability registry that unifies native, OpenAPI and MCP tools
- a self-evolution system that benchmarks changes to its own behavior before deploying them, and
  can roll them back

On top of that core, `lyra serve` makes lyra an always-on service. It has an installable web app
(PWA) with push notifications, remote terminals, a fleet of managed machines (Linux and Windows),
scheduled routines, health monitoring, and a daily briefing. Each user can also connect their own
Microsoft 365 account (calendar, mail, Teams, files) and project-management tool (PMI).

Two ideas run through the whole design. First, **the model proposes and Rust decides**: every
action that matters (storing a memory, running a command, sending an email, deploying a change to
lyra itself) passes a deterministic check in compiled code that the model cannot talk its way
around. Second, **everything is evidence-driven and reversible**: memories keep their history,
skills keep their versions, plans keep checkpoints, and evolution keeps generations with full
snapshots and rollback.

This paper describes the motivation, architecture, subsystems, safety model and operational
characteristics of lyra as it currently stands.

---

## Table of Contents

1. [Motivation](#1-motivation)
2. [Design Principles](#2-design-principles)
3. [System Overview](#3-system-overview)
4. [The Conversation Loop](#4-the-conversation-loop)
5. [Memory](#5-memory)
6. [Skills: Procedural Learning](#6-skills-procedural-learning)
7. [Planning and Execution](#7-planning-and-execution)
8. [Goals and Bounded Autonomy](#8-goals-and-bounded-autonomy)
9. [Subagents](#9-subagents)
10. [Capabilities: One Model for Every Tool](#10-capabilities-one-model-for-every-tool)
11. [Self-Evolution](#11-self-evolution)
12. [System Access and the Security Model](#12-system-access-and-the-security-model)
13. [The Decision Model](#13-the-decision-model)
14. [Distribution: Server, App, Terminals and Nodes](#14-distribution-server-app-terminals-and-nodes)
15. [Operations: Routines, Health, Diagnosis, Status, Briefing](#15-operations-routines-health-diagnosis-status-briefing)
16. [Personal Assistant: Microsoft 365, PMI, Planning the Day](#16-personal-assistant-microsoft-365-pmi-planning-the-day)
17. [Coding Agents](#17-coding-agents)
18. [Multi-User Operation](#18-multi-user-operation)
19. [Observability and Cost](#19-observability-and-cost)
20. [Data, Storage and Durability](#20-data-storage-and-durability)
21. [Deployment](#21-deployment)
22. [Limitations and Future Work](#22-limitations-and-future-work)
23. [Conclusion](#23-conclusion)
24. [Appendix A: Codebase at a Glance](#appendix-a-codebase-at-a-glance)
25. [Appendix B: Safety Guarantees Summary](#appendix-b-safety-guarantees-summary)
26. [Appendix C: Glossary](#appendix-c-glossary)

---

## 1. Motivation

Hosted assistants are capable, but they have three properties that matter to a small organization
or an individual who runs their own infrastructure:

1. **Data leaves the building.** Conversations, mail, calendars, server state and memories end up
   on someone else's systems.
2. **They forget, or they remember opaquely.** Memory is either missing or a black box that can't
   be inspected, corrected or rolled back.
3. **They don't operate.** A chat window can't watch servers at 3 a.m., triage the inbox before
   the working day, or run a check on twenty machines and report only what's wrong.

Local models have become good enough to do real work, but a raw model endpoint is only a
component. What's missing is the system around it: memory, procedures, planning, tools,
permissions, scheduling, a way to reach it from a phone, and a way to get better over time
without anyone hand-tuning prompts.

Lyra is that system. It is built for one owner and their coworkers on a single server. It assumes
that the model is **fallible**: it will sometimes hallucinate, misuse tools, or confidently
propose the wrong thing. So lyra puts guardrails in code rather than in prompts.

---

## 2. Design Principles

**The model proposes; Rust decides.**
Model output is treated as a suggestion. Command classification, path deny-lists, permission
checks, memory safety scanning, capability policy, approval gating and budget enforcement are all
written in Rust and run on every call, whatever the model asked for.

**Nothing is lost silently.**
A changed fact supersedes the old one, which is kept and linked. A skill update snapshots the
previous version first. An evolution deployment is a generation with a full snapshot. A restore
keeps what it replaced. The history tables are append-only.

**Evidence over intuition.**
Skills are promoted or deprecated by their measured reliability. Capabilities are ranked by their
success rate. Evolution candidates must beat the current agent on a replayed benchmark before
they can be approved without an override.

**Local and self-contained.**
Everything lyra keeps lives in one directory, `~/.lyra` (`$LYRA_HOME`). It needs no external
database server: LanceDB and SQLite are embedded. The PWA is compiled into the binary. TLS is left
to the user's existing reverse proxy.

**Progressive autonomy.**
Each autonomous feature (memory maintenance, skill learning, goal pursuit, evolution) has an
explicit mode, typically `off | propose | auto`, and defaults to the conservative one. Higher
autonomy still runs inside hard, runtime-enforced budgets.

**Private things happen; shared things ask.**
Lyra acts at once on anything only the user can see: their personal tasks, their own calendar
events, drafts in their own mailbox. It waits for an explicit **Allow** before anything other
people can see: sending mail, inviting attendees, posting a project update, changing a machine.

**Graceful degradation.**
Without an embedding model, recall falls back to keywords. Without a decision model, the chat
model decides. Without the summary model, the briefing goes out without its one-liner. When a
provider is down, it drops out of discovery rather than failing the conversation.

---

## 3. System Overview

### 3.1 Workspace structure

Lyra is a Cargo workspace. One binary crate (`lyra`) glues ten library crates together. Each
library owns one domain and knows nothing about the TUI, the web server, or the chat model's wire
format:

```text
                           ┌──────────────────────────────────────────┐
                           │                 lyra (src/)              │
                           │ App state · streaming · tool loop · TUI  │
                           │ serve · connect · mcp · M365 · PMI · ... │
                           └───────────────┬──────────────────────────┘
       ┌──────────────┬──────────────┬─────┴───────┬──────────────┬──────────────┐
       ▼              ▼              ▼             ▼              ▼              ▼
 lyra-memory    lyra-learning   lyra-execution  lyra-goals   lyra-agents   lyra-evolution
 (LanceDB)      (skills, MD +   (plans, engine, (goals,      (subagents,   (telemetry,
                 SQLite ledger)  verification)   autonomy)    routing)      candidates,
                                                                            generations)
       ┌──────────────┬──────────────┬─────────────┐
       ▼              ▼              ▼             ▼
 lyra-capabilities lyra-system   lyra-web       lyra-node
 (registry, policy (shell/file/  (axum HTTP+WS, (outbound agent on
  OpenAPI, MCP)     http/ssh     pairing, push,  other machines;
                    classifier)  embedded PWA)   static musl / Windows)
```

The glue modules in `src/` (`mem.rs`, `learn.rs`, `plan.rs`, `goals.rs`, `agents.rs`,
`caps.rs`, `evolve.rs`) connect each crate to lyra's model client, commands and UI. This keeps
each crate testable on its own. For example, the execution `Engine` runs against a `Runtime`
trait, and `src/plan.rs` supplies lyra's implementation of it.

### 3.2 Front ends

The same core can be reached in four ways:

| Front end | Command | Description |
|---|---|---|
| Local TUI | `lyra` | ratatui terminal app with live side panels; runs everything in-process |
| Server | `lyra serve` | headless; serves the PWA and WebSocket API, runs routines and background work |
| Remote terminal | `lyra connect` | the same TUI as a thin client of a server |
| MCP server | `lyra mcp` | exposes lyra to Claude Code and OpenCode as tools |

There is also `lyra-node`, which is not a front end. It is a small agent that lends a machine's
shell and files to the server.

### 3.3 State on disk

```text
~/.lyra/
├── config/       config.toml, behavior.toml (evolved), secrets.toml (0600, never backed up)
├── context/      SOUL.md, USER.md, AGENT.md, STYLE.md
├── memory/lance/ LanceDB: memories + vectors + history tables
├── skills/       <name>.md (one per skill) + ledger.db
├── plans/        plans.db
├── goals/        goals.db
├── agents/       <name>.toml (one per subagent) + agents.db + index/ (routing vectors)
├── capabilities/ capabilities.db (usage) + index/ (discovery vectors) + specs/
├── evolution/    evolution.db
├── workflows/    <name>.toml        (evolved)
├── tools/        <name>.toml        (evolved composite tools)
├── routines/     <name>.toml + runs.json
├── sessions/     <id>.json          (saved conversations)
├── notes/        <slug>.md          (owner's notes and lists)
├── users/<id>/   per-person USER.md, STYLE.md, secrets, notes, goals, routines, briefing, pmi
├── usage/        <YYYY-MM>.jsonl    (every model call)
├── status/       status.db          (8 days of check history)
├── briefing/     last.json
└── web/          devices.json (token hashes), vapid.key, users
```

Human-editable state is plain text: skills and notes are Markdown, and agents, routines,
workflows, tools and behavior are TOML. Editing a skill or agent file by hand is supported, and an
edit becomes a new version. Machine state (histories, ledgers, telemetry) is SQLite. Anything that
needs vector search is LanceDB. `src/migrate.rs` brings older layouts up to date on startup.

---

## 4. The Conversation Loop

A single user message goes through these stages:

1. **Context assembly.** The system prompt is built from:
   - **SOUL.md** (identity and tone; nearest file wins)
   - **USER.md** (who the user is; nearest wins)
   - **AGENT.md** (operating rules; *all* files stack from general to specific)

   These are looked up in `~/.lyra/context/` and then in every directory from `/` down to the
   working directory, so a project can override or extend the global persona.

   Lyra then adds:
   - the evolved behavior guidelines
   - the open goals
   - working memory
   - the person's writing style, when writing as them
2. **Routing.** The message may be handed to a subagent (§9) before the main agent sees it.
3. **Recall.** The context compiler (§5.4) picks the few memories relevant to this message, within
   a token budget. The best-matching skills (§6) are added too.
4. **Tool offer.** If more capabilities are callable than `max_tools`, only the ones discovered
   for this message are offered, plus `capability_search` (§10).
5. **Streaming and tool rounds.** The reply streams token by token. Tool calls are executed
   through the capability layer. Each call is policy-checked, recorded and, for writes, verified.
   The loop runs for up to `max_tool_rounds`, which is itself an evolvable setting.
6. **Post-turn work.** The turn is recorded as an evolution *run*. Durable-sounding statements
   trigger memory capture. Corrections or multi-step successes trigger a skill review. The
   user's *next* message is read as feedback on this one ("thanks" counts as success, a correction
   as failure), and that outcome flows into memory ranking, skill reliability, agent statistics
   and evolution telemetry.

Lyra treats every reply as an experiment whose outcome is measured. This feedback loop is what
makes the learning systems work.

---

## 5. Memory

*Crate: `lyra-memory`. Design: `docs/done/memory_system.md`, `docs/done/lancedb_migration.md`.*

### 5.1 Model

Memory holds three kinds of things:

- **Facts** (semantic): what lyra knows
- **Episodes**: what happened, such as a finished plan or a multi-step task, with its outcome
- **Working memory**: the current goal, plan and scratch values; never persisted

Each stored memory records its scope (`user`, `agent`, `project:<name>`, `user:<id>`), kind,
source, the run and tool call it came from, the session it was saved in, importance and
confidence. Something the user stated outright is trusted more than something inferred.

### 5.2 Storage

The default backend is **LanceDB**, an embedded columnar store in `~/.lyra/memory/lance`.
Each memory is a typed row together with its embedding: a fixed-size vector tagged with the
model and generation that produced it. Keyword search uses LanceDB's full-text index. Semantic
search is exact vector search until the collection passes `vector_index_threshold`, after which
an ANN index is built. History (versions, links, events, usage, episodes, proposals) lives in
small side tables. A SQLite backend remains available.

The `EmbeddingProvider` lives *outside* the store, so the store never talks to a model. When
the embedding model changes, old vectors stop being used and memories are re-embedded at startup
or on `/memory reembed`.

### 5.3 Writes: the manager decides

The model proposes memories with `memory_remember`. `MemoryManager` then decides what happens:

- **Safety scan first.** Passwords, tokens, keys and other credentials are refused before
  anything is stored.
- **No duplicates.** Restating a fact reconfirms the existing memory and raises its confidence
  slightly. Similarity is judged by word overlap (`same_wording`) and vector similarity
  (`same_meaning`).
- **No silent overwrites.**
  - A changed fact *supersedes* the old one, which is kept and linked.
  - A fixed typo becomes a new *version*.
- **Relating.** Similar existing memories are shown to the model, which classifies the relation
  as *update*, *contradiction*, *support* or *related*. Contradictions are flagged for review.
  Support links raise ranking and contradictions lower it.
- **Scope enforcement.** `allowed_scopes` limits where lyra may write. Subagents and plan
  helpers can only write to their own permitted scopes.

### 5.4 Reads: the context compiler

Before each message, candidate memories are scored on six weighted signals:

| Signal | Default weight |
|---|---|
| semantic (vector) similarity | 0.40 |
| lexical (FTS) match | 0.20 |
| importance | 0.15 |
| confidence | 0.10 |
| recency (per-kind half-life decay) | 0.10 |
| relationship (support/contradiction links) | 0.05 |

Near-duplicates are dropped, and so is anything below a relevance floor. The best few memories
that fit a token budget (600 tokens and 8 memories by default) are injected into the prompt.
Replies show which memories they used. The user's reaction marks those memories helpful or not,
and that feeds back into ranking.

Recency decay uses a half-life per kind: working memory 1 day, episodic 30, semantic 365. Facts
tagged by category get their own: preferences 730, decisions 365, configuration 90.

### 5.5 Projects

Memories about a project live in `project:<name>`. The current project defaults to the git
checkout lyra was started in. Recall draws from the user's memories, the agent's, and the current
project's, and never another project's unless that scope is asked for explicitly.

### 5.6 Upkeep

- Expired memories are archived.
- A curator (manual, daily or weekly) proposes consolidating duplicates and flags
  contradictions and stale memories.
- `/memory backup` and `lyra --restore-memory` give point-in-time copies.
- `/memory` reports the store's size and P50/P95 latencies for each operation.

---

## 6. Skills: Procedural Learning

*Crate: `lyra-learning`. Design: `docs/done/skill_learning.md`.*

Memory holds *facts*. Skills hold *procedures*: how lyra did something that worked.

### 6.1 Representation

Each skill is one Markdown file in `~/.lyra/skills/`, with optional front matter (description,
status, confidence, source, timestamps, id). Users can edit them or drop in their own. The
**ledger** (`ledger.db`) holds the evidence:

- every version
- which replies used which skills, and how they went
- relationships (supersedes, conflicts with)
- pending proposals
- an audit log of every change

### 6.2 The learning cycle

1. **Use.** Skills are ranked by relevance (0.50), observed reliability (0.25), learned
   confidence (0.15) and freshness (0.10, 90-day half-life). The best are added to the prompt and
   recorded on the run.
2. **Outcome.** The next message, or an explicit `/outcome`, scores the skills used. Reliability
   is Laplace-smoothed: `(successes + 1) / (outcomes + 2)`, so a new skill starts at 0.5.
3. **Learn.** A review is triggered by a correction, a failed step that was then fixed, a
   multi-step success, or "remember how we did this". The model is shown the most similar
   existing skills and chooses one of three actions:
   - *ignore*
   - *create* a skill
   - *update* one (with a snapshot first, so `/rollback` can undo it)
4. **Lifecycle.**
   - *Promote:* a confident proposal with 2 successes and no failures.
   - *Deprecate:* an active skill whose reliability falls below 0.4 after 5 uses. Skills are
     never deleted automatically.
5. **Curation.** Finds near-duplicates, stale skills, and merge, split and contradiction
   candidates. Merges and splits are always proposed, never applied automatically.

### 6.3 Autonomy modes

| | `propose` (default) | `auto` |
|---|---|---|
| new skills | proposed | proposed; confident ones used on trial |
| refinements | proposed | applied, versioned |
| promotion / deprecation | proposed | applied, audited |
| merges / splits | proposed | proposed |

---

## 7. Planning and Execution

*Crate: `lyra-execution`. Design: `docs/done/planning_execution_system.md`.*

For multi-step work, `/plan <request>` produces a **goal**, with success criteria, constraints
and open questions, and a **structured plan**: a DAG of steps, each with an action, an expected
outcome, and a way to verify it.

### 7.1 Step types

- **tool**: one tool call. Arguments may reference earlier results (`{{s3}}`), which are filled
  in just before the step runs.
- **reasoning**: the model works on it with tools.
- **workflow**: follow a learned skill.
- **subagent**: delegate to a helper agent with scoped tools. Its report must end with a status,
  so a helper that couldn't do the task fails the step instead of passing it on.

### 7.2 Execution guarantees

- **Parallelism with safety.**
  - Independent steps run concurrently (`max_parallel`).
  - Steps that share a resource lock, or that are unsafe to repeat, are serialized.
- **Verification is mandatory.** "It ran" is not taken as "it worked". Each step is verified by
  its tool result, a follow-up read-only tool, a deterministic text check, or the model judging
  the evidence.
- **Retry policy by error class.** Transient errors (timeouts, rate limits, connection failures)
  are retried with backoff. Permission and input errors are not.
- **Localized replanning.** A step that still fails is replanned. Only the broken subgraph
  changes, completed work is kept, and the plan's version goes up.
- **Idempotency.**
  - Every step declares whether it is safe to repeat.
  - Mutating tool steps get an *operation id*, and completed operations are recorded.
  - A retry, or a resume after a crash, reuses the recorded result instead of acting twice.
- **Approval for destructive actions.** Destructive tools may only run as their own tool steps,
  and the plan pauses for approval of the *exact* call. If the arguments change, approval is
  needed again. When the goal itself is destructive or external, every mutating step needs
  approval.
- **Open questions block execution.** A plan with open questions doesn't run until they are
  answered or the user says `/plan run anyway`.
- **Budgets.** Model calls, tool calls, replans, tokens and wall-clock time are all enforced.
  Running out pauses the plan rather than failing it.
- **Checkpoints** are saved before anything that changes state.
- **Goal evaluation.** At the end, the result is judged against the success criteria. A plan
  whose steps all finished can still be only *partial*.

### 7.3 Crash recovery and integration

If lyra stops mid-run, the plan is found again on the next start. A step that was running is
rerun only if it is safe to repeat or its operation is on record. Otherwise it waits for
`/plan retry` or `/plan skip`.

A finished plan feeds the other systems:

- it becomes a memory episode
- its results go through memory capture
- its recoveries are offered to the skill reviewer
- its full telemetry goes to evolution

---

## 8. Goals and Bounded Autonomy

*Crate: `lyra-goals`. Design: `docs/done/goal_manager.md`.*

A plan is one attempt. A **goal** is something lyra pursues over days or weeks.

### 8.1 Goal model

A goal has:

- a title and description
- success criteria
- priority (0–10) and importance
- a deadline
- a parent and subgoals
- dependencies on other goals

Its status is one of proposed, active, blocked, paused, completed, failed or cancelled. Goals the
model suggests start as *proposed*.

- **Decomposition** breaks a goal into ordered subgoals.
- **Plans as attempts.** `/goal work` plans the next useful piece of a goal, telling the planner
  what's already done and what earlier plans found. The plan's evaluation updates the goal's
  progress.
- **Blockers are typed:** missing information, missing permission, an external or failed
  dependency, approval required, or a capability unavailable. A blocked goal is not retried. It
  waits for a state change, such as an approval, a capability becoming healthy again, or a
  trigger.
- **Priority scoring** combines several weighted signals:

  | Signal | Weight |
  |---|---|
  | explicit priority | 0.35 |
  | deadline urgency | 0.25 |
  | goals waiting on it | 0.15 |
  | importance | 0.10 |
  | progress | 0.10 |
  | cost so far (subtracted) | 0.05 |

  So a goal due tomorrow that blocks three others can outrank a nominally higher-priority one.
- **Triggers** wake a goal:
  - at a time, or every so often (recurring goals reopen each period)
  - after another goal completes
  - when a condition becomes true: an environment variable is set, a file exists, or a
    capability is healthy

### 8.2 Autonomy levels

| Mode | Behavior |
|---|---|
| `reactive` (default) | works only when asked |
| `assisted` | continues goals already in progress; every write waits for approval |
| `autonomous` | picks the top-priority goal that can progress, and only while the user is idle |

Assisted and autonomous sessions run inside **runtime-enforced** limits: minutes, plans, tool and
model calls, replans, cost, and the riskiest capability class allowed (`max_risk`). When a limit
is reached the session stops, and a cooldown applies before another can start. After
`max_failures` consecutive failed plans, the goal is blocked.

Only the owner's goals are ever worked on unattended. Plans run tools with full rights, so
unattended work for other users is refused (§18).

---

## 9. Subagents

*Crate: `lyra-agents`. Design: `docs/done/sub_agents.md`.*

The main agent owns the conversation. **Subagents** own specialties.

### 9.1 Profiles

Each agent is a TOML file. A profile contains:

- instructions
- allowed tools and capability patterns, a deny list, and a risk ceiling
- a memory policy (which scopes it may read and write)
- its own skills
- a model policy: it can use a different model, endpoint, temperature or thinking setting
- routing hints: intents, keywords, example requests, and requests it must *not* get

Versions and every delegation are recorded in `agents.db`.

Several agents are built in:

| Agent | Role |
|---|---|
| Researcher | read-only research, with web search and people lookup |
| Archivist | can also record in memory, in the agent and project scopes only |
| Operator | the only agent with system access (§12) |
| Coder | hands work to coding agents (§17) |
| Project Manager | PMI changes others can see, with approval |
| Assistant | everyday tasks |

### 9.2 Creating agents

`/agent new` runs an interview, one question at a time. It can start from nine templates (writer,
developer, researcher, analyst, project-manager, assistant, reviewer, data-analyst, archivist) or
import a TOML or YAML profile. The model writes the instructions and routing examples, and the
agent is **tried on a test task before it exists**. The user then activates it, modifies it,
tests it again, or cancels.

### 9.3 Routing

Routing is a cascade from cheap to expensive:

1. **Explicit:** `@writer …` or "ask the writer".
2. **Rules:** examples, keywords and intents score the message; exclusions veto. Above
   `rule_threshold` (0.75), the message routes.
3. **Semantic:** embedding similarity to what an agent handles, stored in LanceDB. Above
   `semantic_threshold` (0.85), the message routes.
4. **Model:** in the band between `semantic_floor` and the thresholds, the decision or chat model
   is asked.
5. Otherwise the main agent keeps the message. It can still hand work over with the `delegate`
   tool.

### 9.4 The delegation contract

A specialist receives a structured request:

- the task
- only the context it needs: the input, plus the memories its policy lets it read
- its skills
- an output contract
- a budget (default 6 model calls and 12 tool calls)

It returns a result, a confidence, or a refusal with a reason. The main agent checks the result
before answering the user. Delegation depth is capped (`max_depth`, default 2).

Every tool call an agent makes is checked again against its profile, and its memory reads and
writes stay inside its scopes. Corrections create agent-specific skills. Agents that keep failing
become evolution problems. Revised instructions are benchmarked on the agent's own tasks and
deployed as a new, reversible agent version.

---

## 10. Capabilities: One Model for Every Tool

*Crate: `lyra-capabilities`. Design: `docs/done/capabilities.md`.*

Lyra has many kinds of tools:

- native tools (memory, PMI, mail, notes, ...)
- composite tools produced by evolution
- OpenAPI operations
- MCP tools
- workflows
- skills
- subagents

The capability layer describes all of them the same way, and `src/caps.rs` is the *only* path by
which the chat or a plan calls a tool.

### 10.1 The capability record

Each capability has:

- an input schema
- a **risk level**: read-only, low write, write, destructive or privileged
- permissions and tags
- **requirements**: identifiers it needs, and which capability provides them (a task operation
  needs a `projectId`, which `projects.list` provides)
- a **verification rule**: how to confirm a write worked, such as reading a memory back or
  fetching a created resource

### 10.2 Discovery

Offering a model hundreds of tools hurts accuracy. When more than `max_tools` (16) capabilities
are callable, the model is offered only the ones that fit the message, plus a
`capability_search` tool to find more. Discovery is hybrid: full-text search plus semantic
vectors in LanceDB.

Candidates are scored on:

| Signal | Weight |
|---|---|
| relevance | 0.55 |
| reliability (observed success rate) | 0.20 |
| permission (whether policy lets it run) | 0.10 |
| efficiency (speed) | 0.10 |
| usage history | 0.05 |

So a tool that usually works ranks above one that often fails.

### 10.3 Policy in Rust

`[capabilities.policy]` maps each risk level to `auto`, `approval` or `deny`. The defaults are:

| Risk | Policy |
|---|---|
| read-only, low write, write | auto |
| destructive | approval |
| privileged | deny |

Per-capability glob overrides are supported. Denied capabilities are never offered. Capabilities
that need approval run as approved plan steps, or after a session-scoped `/caps allow`.

### 10.4 Providers

- **OpenAPI.** Each operation in a spec (JSON or YAML, a file or a URL, cached daily) becomes a
  capability.
  - Names come from the `operationId`, or are derived from the path.
  - Risk comes from the HTTP method, or from an `x-lyra-risk` extension.
  - Path identifiers become requirements.
  - Creates and updates are verified by fetching the result.
  - `$ref`s are inlined, and OpenAPI 3.1 is supported.
  - Tag, path and method filters keep a large API manageable.
  - `defaults` fills in fixed values such as an organization id, so the model never has to.
  - Credentials come from an environment variable or `secrets.toml`, never from the config file.
- **MCP.** Stdio servers are supported. Risk comes from the `readOnlyHint` and
  `destructiveHint` annotations.

### 10.5 Tracking and health

Every call is recorded with its success, latency, retries and error class. Providers are
health-checked. An unreachable provider is left out of discovery and planning, and one that keeps
failing is marked degraded and ranked lower.

---

## 11. Self-Evolution

*Crate: `lyra-evolution`. Design: `docs/done/self_evolution.md`.*

Skills are *what* lyra learns. Evolution changes *how lyra works*: its prompts, settings,
workflows and tools. Each change is driven by evidence, tested before it is deployed, and can be
rolled back.

### 11.1 Pipeline

```text
 telemetry ──► detectors ──► evolver ──► validate & benchmark ──► approve ──► generation ──► monitor
  (runs)      (problems)    (1–3         (replay recent tasks,     (policy     (snapshot)     (regression?
                             candidates)  judge, fitness score)     gated)                     → rollback)
```

1. **Telemetry.** Every chat turn and plan run is recorded with its model and tool calls,
   errors, retries, replans, tokens, time, the skills used, and the generation that did the work.
   The user's next message becomes the run's outcome, and their words are kept with it.
2. **Detectors** are deterministic, not model-based. They find problems backed by evidence:
   - heavy runs
   - the same error again and again
   - frequent corrections
   - the same chain of tools in many runs
   - failing skills
   - plans that need rework
   - failing or slow capabilities
   - goals that keep getting stuck
   - agents that keep getting corrected
3. **Evolver.** A separate model role proposes 1–3 competing **candidates** for each problem.
   Each candidate is one of five kinds of small, structured change:

   | Kind | What changes |
   |---|---|
   | *prompt* | a behavior guideline added to the system prompt. It may not rewrite the prompt, weaken safety, or contain secrets. |
   | *configuration* | one whitelisted behavior setting (`max_tool_rounds`, `plan_step_rounds`, `recall_before_answering`, `search_skills_before_planning`, `verify_reasoning_steps`) |
   | *workflow* | phases the planner follows for requests with certain trigger words, as TOML |
   | *tool* | a composite tool that chains existing, non-destructive tools. It is data, not code. |
   | *skill* | revised skill instructions, deployed as a new skill version |

4. **Validation and benchmark.** A candidate must first apply cleanly, be safe, and reference
   only tools that exist. Then recent tasks, starting with the evidence runs, are **replayed
   headless** with both the current agent and the candidate:
   - Only read-only tools really run.
   - Writes are simulated.
   - Destructive calls count as safety violations.

   Changes that only affect planning are measured by planning and running the tasks against a
   throwaway plan store. The model judges each answer against the task and, if the user had
   corrected the original answer, against that correction. Fitness is weighted:

   | Component | Weight |
   |---|---|
   | success | 0.40 |
   | accuracy | 0.25 |
   | efficiency | 0.15 |
   | reliability | 0.10 |
   | safety | 0.10 |

5. **Deploy.** Only a tested candidate that passed its checks can be approved. One that didn't
   beat the baseline needs `force`. Each deployment is a new **generation** with a full snapshot
   of `behavior.toml`, the workflows, the tools, and the skill versions. Deploying one candidate
   rejects its competitors.
6. **Monitor.** Once a generation has enough judged runs, it is compared with its parent on
   success rate, correction rate and error rate. A clear drop is flagged. In `auto` mode it is
   rolled back automatically. A rollback is itself a generation.

### 11.2 Policy levels

| Change | In `auto` mode |
|---|---|
| guidelines, skills | deploy automatically if they beat the baseline |
| configuration, workflows, tools | always wait for `/evolve approve` |
| code | manual only |
| architecture | never touched |

The event history (proposals, tests, approvals, deployments, rejections, rollbacks) is
**append-only, enforced by the database**.

### 11.3 Code evolution

When `source_repo` points to a git checkout of lyra, `/evolve code <problem>` has the model pick
files and write a patch. The patch is tested in a throwaway **git worktree** with clippy and the
full test suite.

Several limits are hard-coded:

- Patches may not touch the evolution system, the safety scanner, or anything outside the
  repository.
- Approving a patch only creates a local branch `evolution/<id>`.
- Nothing is merged or pushed, and the running binary is never modified.

A human reviews the branch and merges it.

---

## 12. System Access and the Security Model

*Crate: `lyra-system` (and `lyra-node` for remote machines).*

### 12.1 Who can touch the system

The main agent is **not** offered shell, file or SSH tools, and its calls to them are refused.
These tools belong to the **Operator** subagent, and to any agent the user explicitly grants
them. Plans reach them only through an Operator step.

The system tools are:

- `system_info`
- `shell_run`
- `file_read`, `file_list`, `file_write`, `file_delete`
- `http_request`
- `ssh_run`
- `fleet_run`
- `upload_place`

### 12.2 The command classifier

Every call passes `System::check` in Rust, which returns one of three verdicts:

| Verdict | Examples |
|---|---|
| **auto** (runs at once) | `system_info`, file reads and listings, GET/HEAD, read-only commands (`ls`, `df`, `ps`, `cat`, `grep`, `git status`, `systemctl status`, `docker ps`, `ping -c`, …), prefixes in `allow_commands`, writes inside `write_roots` |
| **ask** | any other command, `>` redirects, `$(…)` substitution, writes elsewhere, deletes, mutating HTTP methods |
| **forbidden** | `rm -rf /` or `~`, `mkfs`, `dd` to a disk, fork bombs, anything under `deny_paths` (keys, credentials, lyra's config), hosts not in `ssh_hosts` |

On Windows nodes, the same three-way verdict applies to PowerShell verbs. Get-, Test- and Select-
run at once. Set-, New-, Start- and unknown verbs ask. Remove-Item, Stop-Process,
Invoke-Expression and Restart-Computer are flagged dangerous. Wiping drives, `C:\Windows`, user
profiles and Program Files are never allowed.

### 12.3 Approvals

An "ask" verdict opens an approval in the TUI, on the PWA (as a card), and as a push notification
with **Allow** / **Deny** buttons on Android. The approval shows:

- which agent is asking
- what kind of action it is
- exactly what will happen (the command and where it runs, the path, or the URL)
- why it needs approval

Risky operations (delete, kill, `sudo`, `git push`, stopping services) are highlighted in red.
The answers are:

- allow once
- deny
- allow this exact action for the session

No answer within the timeout counts as a deny.

### 12.4 Defense in depth

- **Rules live on the target.** A remote machine checks every request against its *own*
  `node.toml` rules. Credentials are refused there even if the server "approved" the request.
- **Narrow tokens.** A node's token can only lend tools. It cannot chat or approve, and it can
  read uploads but never the conversation.
- **Unsafe rule changes are refused.** A rule that would open everything (`/` as a write root,
  allowing `sudo` or `*`) is rejected. Only a person on a paired device can change rules; the
  model has no tool for it.
- **Process limits.** Commands have timeouts that kill the whole process group, and their output
  is capped.
- **Look-only routines and diagnoses.** Any change requested during a look-only routine or a
  diagnosis is declined at once, so an unattended run never sits waiting for an approval.
- **Secrets.**
  - Kept in `secrets.toml`, with mode 0600 and never backed up.
  - Read from stdin without echo.
  - Never shown in chat or logs.
  - Refused by the memory safety scanner.
- **Auditing.** Every call, approved or not, is recorded in capability usage and in the agent's
  delegation log.

---

## 13. The Decision Model

Lyra makes many small classification decisions:

- which agent should take a message
- how a new memory relates to existing ones
- whether a plan step worked
- how a benchmark answer scored
- whether a turn deserves a memory capture or a skill review

Each of these costs a full chat-model completion lasting several seconds.

An optional **decision model** (for example Cloudflare's *clef-flash*, served by llama-server's
`/v1/systemone`) returns a *probability for each option* in tens of milliseconds. Lyra uses its
answer only when confidence is at least `min_confidence` (0.75). Otherwise, or when the input
doesn't fit in one batch, or when the endpoint is down, the chat model decides. A down endpoint
is retried after a minute.

Because a decision is now cheap, the per-turn memory and lesson checks can run even when the
keyword heuristics don't fire. This catches more turns that deserve capture, and the expensive
chat model is called only on a "yes". Approvals and system checks **never** depend on the
decision model.

---

## 14. Distribution: Server, App, Terminals and Nodes

### 14.1 `lyra serve`

The server runs the full lyra core headless and serves HTTP and WebSocket with axum (crate
`lyra-web`).

- It listens on plain HTTP (default `127.0.0.1:8484`). TLS comes from the user's reverse proxy
  (Zoraxy in the reference deployment).
- `src/serve.rs` mirrors the application state to connected devices as small incremental updates
  and feeds their input back in.
- **Conversations run in parallel.** Each device has its own conversation, several can be
  generating at once, and two devices on the same conversation see it live.
- Background work (schedules, goals, routines) runs once, not once per device.
- A conversation nobody has open is saved and put away after 15 minutes.
- `GET /health` returns 200 with uptime and a busy flag, or 503 if the main loop is stuck, so an
  external uptime monitor can watch it.
- A pid lock keeps a second lyra from running against the same `~/.lyra`.

### 14.2 The PWA

The app is built with React, Vite, Tailwind, shadcn/ui and Vercel AI Elements (`web/ui/`). It is
compiled to `web/ui/dist/` and **embedded in the binary**, so the server has nothing else to
deploy. It has pages for:

- Chat
- Status
- Machines
- Devices
- Activity
- Tasks and Projects
- Notes
- Routines
- Coding
- Memory, Skills, Goals, Model
- Usage
- Users

Management pages run lyra's own slash commands, so they behave exactly as the terminal does.

It supports:

- attachments up to 25 MB (text files are inlined; images go to the model if it has vision)
- voice dictation
- the Android share target
- update prompts when a new version is deployed

**Pairing.** `lyra pair` prints a one-time code, a link and a QR code. Each device gets its own
token, and the server stores only its hash. Five wrong attempts cancel a code. Devices send their
token as a WebSocket subprotocol, which keeps it out of URLs and proxy logs.

**Push notifications** use Web Push with lyra's own VAPID key, encrypted end-to-end per RFC 8291,
implemented with RustCrypto. The push service sees only that *something* arrived. Notifications
are sent only when no device has lyra open.

**Single sign-on.** Coworkers can sign in with Microsoft Entra ID. The flow uses OIDC with PKCE
and a client secret, and requests only the `openid profile email` scopes. Lyra checks the
token's tenant, audience, issuer, expiry and nonce. The sign-in is bound to the browser that
started it by a cookie, and the device token is collected with a one-time code. Guest accounts
can never become the owner, and new users wait for an admin's approval.

### 14.3 `lyra connect`

`lyra connect` is the full TUI (chat, Markdown, command and machine palettes, approvals, side
panel) as a thin client of a server. It stores nothing locally except its token (0600). On the
server itself, running `lyra` while `lyra serve` is up opens this client instead of starting a
second instance.

### 14.4 `lyra-node`

`lyra-node` is a small, statically linked (musl) binary, also built for Windows. It lends a
machine's tools to the server over an **outbound** WebSocket, so the machine needs no open ports
and no SSH.

- **Install** with one line: `curl … /install.sh | sh` or `irm … /install.ps1 | iex`. The
  installer verifies a checksum.
- **Headless pairing.** The machine shows a short code, and an admin approves it in the app, from
  a push, or with `/devices approve`.
- **Self-management.** `/machines update` makes the node download the server's build, verify it
  and restart into it. `/machines remove` makes it uninstall its service, settings and binary.
- **Fleet operations.** `@all` and named groups run one command on many machines at once with
  `fleet_run`. Each machine checks the command against its own rules. Those that need approval
  are combined into **one** approval, and the result is reported per machine.
- **Health.** Each node reports disks, memory, load, failed units or stopped services, and
  pending updates every 5 minutes.

### 14.5 `lyra mcp`

`lyra mcp` makes lyra itself an MCP server for Claude Code and OpenCode, using the pairing made by
`lyra connect`. Its tools are `lyra_ask`, `lyra_memory_recall`, `lyra_status`, `lyra_machines`,
`lyra_routines` and `lyra_run_routine`. With this, a coding agent can ask lyra what it remembers
or check on the fleet.

---

## 15. Operations: Routines, Health, Diagnosis, Status, Briefing

These features turn lyra from a chat application into an operations assistant.

### 15.1 Routines

A routine is a scheduled prompt that reports only when something matters. It can be created in
natural language ("every morning at 7, check disk space, pending updates and failed services on
@all and tell me only if something's wrong") or with `/routine new`.

- **Schedules** use plain words: `every day at 07:00`, `weekdays at 8:30`,
  `monday and friday at 9pm`, `every 30m`, and so on. A run missed while lyra was down happens
  once when it's back.
- **Each run** gets its own conversation. The reply starts with a verdict, and the decision model
  then answers "does this need the user?".
- **Notifications.** `notify = problems` (the default) pushes only on a "yes". `always` pushes
  every run, and `never` only logs it.
- **Look-only by default.** A routine only looks unless `changes` is turned on, so a 7 a.m. run
  never waits on an approval.

### 15.2 Machine health and alerts

Health reports are checked against thresholds:

- disk 90% full
- memory 95% used
- 15-minute load above 2 per CPU
- any failed unit
- a machine offline for 10 minutes

When a threshold is crossed, lyra logs it and sends a push, once. It sends another when the
problem clears.

### 15.3 Automatic diagnosis

When a problem appears on the **server**, the Operator investigates without being asked. Problems
on *other* machines wait for the user, because nothing runs elsewhere unless the user asks.

The investigation uses read-only checks: `systemctl status`, `journalctl`, unit and config files,
and port probes. It produces a write-up of what's wrong, the likely cause, and the exact fix,
with whether the fix is safe. Nothing is changed. The headline is pushed ("🔎 desktop: … mount
failed — the NFS mount hangs"), and **Fix it** sends the write-up to chat, where changes go
through normal approval. Only one diagnosis runs at a time, and the same problem is not
re-diagnosed for a day.

### 15.4 Status

Every minute, lyra checks everything it depends on:

- the chat, embedding, reranker and decision models
- web search
- each OpenAPI and MCP provider
- itself and its public URL
- notifications, storage and backups
- routines
- machines

Each check records up/degraded/down/off, latency, an hour-long sparkline, and 24-hour and 7-day
uptime, kept for 8 days. A check that is down twice in a row triggers a push, and so does its
recovery.

### 15.5 Daily briefing

Each morning, lyra compiles one briefing from what it already tracks. It does not contact any
machine to make it. It covers:

- machines: problems, updates, offline
- lyra's checks
- routines
- new diagnoses
- coding jobs
- goals
- the person's calendar, mail, Teams and PMI tasks

The chat model adds a one-line takeaway, and the briefing goes out without it if the model is
unavailable. Briefings survive restarts. A briefing missed at the scheduled time is made at
startup, unless that is more than 3 hours late.

---

## 16. Personal Assistant: Microsoft 365, PMI, Planning the Day

### 16.1 Microsoft 365 (Graph)

Each person connects their *own* Outlook account. Lyra stores only a refresh token, in that
person's own secrets file. It uses delegated permissions, so lyra can never see more than the
person can.

| Area | Happens at once | Waits for Allow |
|---|---|---|
| Calendar | reading; own events with no attendees | invites, declines, moving or cancelling meetings with others |
| Mail | reading, search, drafts into Drafts, mark/flag/archive/delete | sending |
| Teams | reading chats | (read-only) |
| Files | reading text of Word/Excel/PowerPoint/CSV; attaching (≤3 MB, else a link) | (read-only) |

Drafts are HTML in Outlook's usual style. The model writes Markdown, and a reply goes above the
quoted original.

### 16.2 PMI (tasks, reminders, projects)

Lyra uses the organization's project-management system as its task system. It follows PMI's live
event stream, so its view is current within seconds.

- **Personal tasks and reminders** are created at once, from plain-language dates ("friday 3pm",
  "in 2h", "next monday").
- **Changes others can see** go through the Project Manager agent and wait for Allow: project
  tasks, comments, status updates and risks.
- **Reminder follow-ups.** PMI sends the reminders itself. If a task is still open 30 minutes
  after its reminder, lyra pushes a follow-up with **Done / In 1 hour / Tomorrow** buttons, at
  most 3 times.

### 16.3 Plan my day

For users with Outlook and PMI connected, lyra puts private **focus blocks** on their calendar for
tasks due in the next 3 days.

- **Order:** overdue first, then by due date and priority.
- **Length:** 90, 60 or 30 minutes by priority.
- **Limits:** at most 3 blocks a day, never more than half the day's free time, and lunch is kept
  free.

The plan is redone every 15 minutes during working hours. A block that a meeting lands on moves
to the next free slot, and a block whose task is done is removed. During **quiet time** (outside
working hours, or during a meeting), planning pushes and reminder follow-ups are held. Server
alerts still come through.

### 16.4 Proactive help

- **Meeting prep.** 15 minutes before a meeting with other people, lyra pushes who's attending,
  their latest emails, and related open tasks.
- **Mail triage.** Every 10 minutes, new mail from people is read. If an email asks the user to
  do or answer something, lyra:
  - creates a personal task, with the due date if the email gives one
  - flags the email
  - drafts a reply, which is never sent

  These are all private actions, so lyra does them itself and reports them in a summary push.
- **Follow-ups.** Once a day, a sent question with no answer after 3 days gets one nudge.

A per-person record of what has been done makes sure nothing happens twice.

### 16.5 Writing style, notes and people

- **Style.** Lyra learns each person's writing style from their own sent mail, with quotes,
  forwards and signatures stripped. The result is kept in `STYLE.md`, which has two parts:
  *Learned* (refreshed weekly) and *Your notes* (the person's corrections, which always win).
  The style is applied whenever lyra writes as that person.
- **Notes and lists** are per-person Markdown files that can be edited by chat, in the app, or
  with commands.
- **"Who is …"** combines memory, recent mail, meetings in the last and next three weeks, and
  shared PMI tasks. It uses only the asking person's own accounts.

---

## 17. Coding Agents

Lyra doesn't try to be a coding agent itself. It **orchestrates** the coding agents the user
already has.

- The **Coder** agent turns a request into a self-contained task for a folder and calls
  `code_task`.
- **Routing by complexity.** The decision or chat model rates the task:
  - *simple* work (one file, a rename, a small fix) goes to **OpenCode**
  - *complex* work (several files, architecture, an unknown bug) goes to **Claude Code**

  If OpenCode fails, Claude Code takes over and is told what was tried. Naming an agent in the
  request overrides the rating.
- **Execution.**
  - Full auto after one approval.
  - A plan-only mode that changes nothing.
  - It never pushes, and commits only if the task asks for it.
  - Runs on the server unless the user names another machine, in which case it runs through
    that machine's node.
- **Results:**
  - live steps while it works
  - a summary, changed files and diff stat
  - commits, time and cost
  - **Show diff**, **Continue** (the same session) and **Copy resume command**

---

## 18. Multi-User Operation

`lyra serve` can be shared by coworkers. Isolation is enforced in Rust, not by the prompt.

- **Roles.**
  - *Admins* can do everything.
  - *Members* chat and use the shared models, skills and agents, but never get the Operator or
    the Coder. The tools themselves refuse members.
- **Per-person data.**
  - Conversations: members see and resume only their own.
  - Memories: scope `user:<id>`. The owner never sees them, not even in counts.
  - Each person also has their own `USER.md`, `STYLE.md`, secrets, notes, goals, routines,
    briefings, and PMI and Microsoft connections.
- **Approvals.** A person can answer only their own approvals.
- **Learning stays clean.** Skills and evolution learn only from the owner's conversations.
  Memory capture works for everyone, but each person's capture compares against and writes into
  their own scope only, whatever the model suggests.
- **Unattended work.** Only the owner's goals run unattended. A member's routines run with that
  member's rights, and only that member is notified.
- **Changes are immediate.** Disabling a user, removing a device or changing a role applies at
  once, even to connections already open.

---

## 19. Observability and Cost

- **Per-reply metrics:** time to first token, generation speed, tokens in and out, prompt-cache
  hits and savings, cost, and total time. Token counts come from the server's `usage` report, or
  are estimated (marked `~`) if the server doesn't send one.
- **AI usage ledger.** Every model call is appended to `usage/<YYYY-MM>.jsonl`, credited to the
  person it was for and classified as `chat`, `agent` or `background` (capture, triage,
  briefings, reviews). `/usage` and the Usage page show admins everyone's totals; members see
  only their own.
- **TUI side panels:** Session, Agent, Memory, Skills, Agents, Goals, Plan, Evolution and
  Activity. Each shows the live state of its subsystem, and Activity is a timestamped log of
  every request, tool call and event.
- **Plan event logs**, **memory events**, **evolution history** and **capability usage** are all
  stored and can be queried.

---

## 20. Data, Storage and Durability

| Store | Technology | Why |
|---|---|---|
| Memory, routing and discovery indexes | LanceDB (embedded) | vectors and FTS in one local directory, no server |
| Plans, goals, skill ledger, agents, evolution, capabilities, status | SQLite (sqlx) | transactional history and ledgers |
| Skills, notes, context, style | Markdown | human-editable, diffable |
| Agents, routines, workflows, composite tools, behavior, config | TOML | human-editable configuration |
| Usage | JSON Lines | append-only, easy to process |

**Backups** run nightly. Each backup is a `tar.gz` in `~/.lyra-backups`, outside the data
directory, and it can be pointed at a NAS. SQLite databases are copied with `VACUUM INTO`, and
memory goes through its own backup path, so a backup taken while lyra runs is consistent. Seven
backups are kept by default. Secrets are excluded.

`lyra restore` keeps whatever it replaces as `~/.lyra.before-restore-<time>`. The app can trigger
a backup and download the latest one, so a copy can be kept off the server.

---

## 21. Deployment

The reference deployment is a single Fedora server:

1. Build on the server: `cargo install --path . --locked --root /usr/local`. LanceDB needs
   `protoc`, and building on the target avoids glibc mismatches.
2. Build `lyra-node` static for musl and install it next to `lyra`, so the server can hand it out
   to machines.
3. Run `lyra service` to write a systemd unit (a system unit when run as root, otherwise a user
   unit with lingering enabled).
4. Point a reverse proxy at `[web] listen`, with TLS and WebSocket proxying on and no response
   buffering.
5. Pair devices, install the PWA, and turn on notifications.

Hardware needs depend on the model. Lyra itself is a single process. Inference runs on whatever
serves the OpenAI-compatible endpoints: one chat model, plus optional embedding, reranker and
decision models.

---

## 22. Limitations and Future Work

- **Model quality bounds everything.** The guardrails make a weak model *safe*, not *good*.
  Planning, verification and evolution judging all depend on the model's competence. The
  decision model and `structured_thinking = false` reduce the cost of the many internal JSON
  calls, but don't make them smarter.
- **Single server, single process.** Lyra scales to a team on one host, not to an organization.
  Background work runs exactly once because there is exactly one server.
- **The reranker** is health-checked but not yet used in ranking.
- **Concurrency between the TUI and the server.** Running the local TUI and `lyra serve` on the
  same conversation means the last save wins. The pid lock and the `lyra connect` hand-off make
  this hard to do by accident.
- **Windows nodes** run as LocalSystem and can't see coding agents logged in under a user
  account.
- **Code evolution** stops at a local branch by design. Shortening the loop between a merged
  improvement and a rebuilt binary is deliberately left to a human.
- **Benchmarks are small.** By default a candidate is replayed on 3 tasks. That's enough to catch
  regressions, but not enough for strong statistical claims. The post-deploy monitor is the
  second line of defense.

Possible directions include using the reranker in recall and discovery, richer verification
rules for OpenAPI capabilities, and broader benchmarks for evolution.

---

## 23. Conclusion

Lyra shows that a self-hosted assistant on local models can do much more than chat. It can
remember with provenance, learn procedures from outcomes, plan with verification, pursue goals
within budgets, delegate to permissioned specialists, operate a fleet of machines, manage a
working day, and improve its own behavior. It does this without giving the model authority it
can't be trusted with.

The architecture rests on a few commitments: model output is advisory and Rust is
authoritative; every change keeps its history and can be undone; autonomy is earned through
measured evidence and capped by hard budgets; and actions that affect other people always wait
for a human.

---

## Appendix A: Codebase at a Glance

As of October 2026:

| Component | Files | Lines of Rust |
|---|---:|---:|
| `src/` (binary: app, TUI, serve, connect, integrations) | 45 | ~27,000 |
| `memory/` (lyra-memory) | 19 | ~5,400 |
| `execution/` (lyra-execution) | 9 | ~3,700 |
| `evolution/` (lyra-evolution) | 11 | ~3,100 |
| `learning/` (lyra-learning) | 11 | ~3,000 |
| `web/` (lyra-web) | 8 | ~2,800 |
| `agents/` (lyra-agents) | 8 | ~2,000 |
| `capabilities/` (lyra-capabilities) | 9 | ~2,000 |
| `node/` (lyra-node) | 5 | ~1,700 |
| `goals/` (lyra-goals) | 6 | ~1,600 |
| `system/` (lyra-system) | 3 | ~1,500 |
| **Total Rust** | **134** | **~53,700** |
| PWA (`web/ui/src`, TypeScript/React) | | ~10,800 |

Other facts:

- About 300 unit and integration tests.
- Rust edition 2024.
- `cargo clippy --all-targets --workspace` is kept warning-free.
- Key dependencies:
  - ratatui (TUI)
  - tokio (async runtime)
  - axum (HTTP and WebSocket)
  - LanceDB (vectors and FTS)
  - sqlx/SQLite (ledgers and history)
  - reqwest (HTTP client)
  - pulldown-cmark (Markdown)
  - RustCrypto (Web Push encryption)
  - tokio-tungstenite (node and connect WebSockets)

**Model-facing tools (selection):**

| Area | Tools |
|---|---|
| memory | `memory_remember`, `memory_recall`, `memory_list`, `memory_inspect`, `memory_correct`, `memory_supersede`, `memory_archive`, `memory_forget`, `working_memory` |
| goals | `goal_list`, `goal_get`, `goal_create`, `goal_note` |
| system (Operator) | `system_info`, `shell_run`, `file_*`, `http_request`, `ssh_run`, `fleet_run`, `upload_place` |
| web | `web_search` (SearXNG), `web_fetch` |
| PMI | `pmi_tasks`, `pmi_add_task`, `pmi_complete`, `pmi_remind`, `pmi_projects`, `pmi_project`, `pmi_draft_update`, `pmi_post_update`, `pmi_add_risk`, `pmi_comment`, `pmi_inbox`, `pmi_waiting`, … |
| Microsoft 365 | `mail_inbox`, `mail_read`, `mail_thread`, `mail_search`, `mail_draft`, `mail_send`, `mail_tidy`, `teams_chats`, `teams_read`, `files_search`, `files_read`, `files_attach`, calendar tools |
| personal | `note_save`, `note_find`, `note_read`, `note_delete`, `list_add`, `list_mark`, `who_is`, `style_note`, `plan_my_day` |
| orchestration | `delegate`, `capability_search`, `code_task`, `routine_create`, `routine_list` |

---

## Appendix B: Safety Guarantees Summary

| Guarantee | Enforced by |
|---|---|
| Credentials are never stored as memories | `lyra-memory` safety scan, before any write |
| Main agent cannot run shell, file or SSH tools | capability layer and `System::check`; calls refused |
| Every system call is classified auto / ask / forbidden | `lyra-system` classifier (Rust), and again on the node |
| Forbidden operations cannot be approved | node-side and server-side deny rules |
| Destructive plan steps need approval of the exact call | `lyra-execution` engine |
| Mutating plan steps are not repeated after a crash | operation ids and checkpoints |
| Autonomy cannot exceed its budget or risk ceiling | `lyra-goals` autonomy policy at runtime |
| Subagents stay inside their tools and memory scopes | `lyra-agents` permission checks on every call |
| Members never reach system or coding tools | role checks inside the tools |
| Evolution cannot modify safety or evolution code | path guard on code-evolution patches |
| Evolution never merges, pushes or replaces the binary | code lab creates a local branch only |
| Evolution history cannot be rewritten | append-only constraint in the database |
| Actions visible to others wait for a human | approval flow (TUI, PWA, push) |
| Device and node tokens cannot be replayed from storage | only hashes are stored; node tokens are tool-only |
| Secrets stay off disk backups and out of logs | `secrets.toml` (0600), backup exclusion, redaction |

---

## Appendix C: Glossary

| Term | Meaning |
|---|---|
| **Capability** | Anything lyra can call (a tool, operation, workflow, skill or agent), described by one shared record |
| **Candidate** | A proposed evolution change, tested before it can be deployed |
| **Context compiler** | The component that chooses which memories go into a prompt |
| **Decision model** | A small classifier model for fast multiple-choice decisions |
| **Episode** | A memory of something that happened, with its outcome |
| **Generation** | A deployed evolution state with a full snapshot; it can be rolled back |
| **Node** | A machine that lends its tools to the server through `lyra-node` |
| **Operator** | The subagent that holds system access |
| **Owner** | The first user; the only one whose conversations train skills and evolution |
| **Routine** | A scheduled prompt that reports only when something needs attention |
| **Run** | One recorded chat turn or plan execution, with its telemetry and outcome |
| **Skill** | A learned procedure, stored as Markdown, with reliability measured from outcomes |
| **Supersede** | Replacing a memory with a newer fact while keeping the old one linked |

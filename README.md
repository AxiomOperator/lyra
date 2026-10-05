# lyra

A dead simple terminal chat client for a local LLM.

Talks to any OpenAI-compatible `/chat/completions` endpoint (Ollama, llama.cpp server, LM Studio, vLLM, ...).

## Files

Everything lyra keeps lives in one folder, `~/.lyra` (set `LYRA_HOME` to use another):

```text
~/.lyra/
├── config/    config.toml
├── context/   SOUL.md, USER.md, AGENT.md
├── memory/    memory.db
├── plans/     plans.db
└── skills/    <name>.md, one file per skill
```

Upgrading from an older version moves things here automatically: on first run,
files from `~/.config/lyra` and `~/.local/share/lyra/data` are copied in (the
originals are left in place, so delete them once you're happy), and context
files in `~/.lyra/config` move to `~/.lyra/context`.

## Config

`~/.lyra/config/config.toml`:

```toml
url = "http://localhost:8080/v1"
model = "qwen3"
```

All keys are optional; defaults are `http://localhost:11434/v1` and `llama3.2`.

Cost tracking uses prices per million tokens (default 0):

```toml
input_cost_per_mtok = 0.50
cached_input_cost_per_mtok = 0.05   # defaults to the input price
output_cost_per_mtok = 1.50
currency = "$"
```
`LYRA_URL` / `LYRA_MODEL` env vars override the file. See `config.example.toml`.

### Embedding and reranker models

Optional; not used by the chat yet. When configured, lyra sends each a tiny
test request at startup and on `Ctrl-L` and reports in the chat whether it's ready.

```toml
[embedding]           # OpenAI-style /embeddings
url = "http://localhost:8082/v1"
model = "embedding"

[reranker]            # /rerank (vLLM, llama.cpp, Jina/Cohere style)
url = "http://localhost:8081/v1"
model = "reranker"
```

These tables must come after the top-level keys.

## Personality and context files

Three Markdown files make up the system prompt sent with every request:

| File | Purpose | Layering |
|---|---|---|
| `SOUL.md` | Identity: personality, tone, boundaries, communication style | nearest file wins |
| `USER.md` | Who you are, your projects and preferences | nearest file wins |
| `AGENT.md` | Operating rules, purpose, codebase instructions | all files stack, general to specific |

Each is looked up in `~/.lyra/context/`, then in every directory from `/` down to
the one lyra is started in. So a project can override the global `SOUL.md`, and
a project `AGENT.md` adds to the global one. The first line in the chat shows
which files were loaded. Templates are in `examples/`.

## Memory

What lyra knows (facts), what happened (episodes) and what it's working on
(working memory), built following `docs/memory_system.md` in the `memory/`
crate (`lyra-memory`). Everything goes through `MemoryManager`: the model
proposes memories, the manager decides. Storage is one SQLite file,
`~/.lyra/memory/memory.db` (FTS5 keyword search, vectors in a table).

- **Remembering.** The model saves facts with `memory_remember`. After a turn
  where you shared something that sounds durable ("we use…", "I prefer…", "we
  moved to…"), lyra also asks the model to review it and capture what it missed.
  Each memory records its scope, kind, source, the run it came from, importance
  and confidence (what you say outright is trusted more than what's inferred).
- **No duplicates, no silent overwrites.** Saying something again reconfirms the
  existing memory. A changed fact *supersedes* the old one, which is kept and
  linked; a fixed typo is a new *version*. Nothing is overwritten without history.
- **Recall.** Before each message the *context compiler* ranks candidate memories
  by meaning (vectors from the `[embedding]` model), keywords, importance,
  confidence, recency and relationships, drops near-duplicates and anything that
  doesn't actually match the message, and adds the best few within a token
  budget. Replies show `used memories: …`; your reaction (thanks, or a
  correction) marks them helpful or not, which feeds back into ranking.
  Without an embedding model, recall uses keywords only.
- **Working memory.** The model keeps the current goal, plan and scratch values
  with `working_memory`; identifiers you mention are tracked too. It's shown in
  the prompt and the panel, and never saved.
- **Episodes.** Multi-step tasks (and `/memory episode`) are summarized as
  episodes: what happened and how it ended, linked to the run.
- **Upkeep.** Expired memories are archived, old ones fade in ranking
  (half-lives per kind), and `/memory curate` (or `curate = "daily"`/`"weekly"`)
  proposes consolidating duplicates and flags contradictions.
- **Safety.** Passwords, tokens, keys and other credentials are refused before
  anything is stored. `allowed_scopes` limits which scopes lyra may use.

Model tools: `memory_remember`, `memory_recall`, `memory_list`, `memory_correct`,
`memory_supersede`, `memory_archive`, `memory_forget`, `working_memory`.

Commands: `/memory` (stats and what's waiting for approval), `/memory search <q>`
(with each ranking signal), `/memory list [scope]`, `/memory inspect <id>`
(provenance, links, versions, history), `/memory correct <id> <text>`,
`/memory forget|archive|restore|purge <id>`, `/memory approve|reject <id>`,
`/memory working [clear]`, `/memory curate`, `/memory episode`.

```toml
[memory]
enabled = true
default_scope = "user"
allowed_scopes = ["*"]       # e.g. ["user", "agent", "project:*"]
capture = "auto"             # auto | off
maintenance = "propose"      # off | propose | auto
curate = "manual"            # manual | daily | weekly
inject = true                # add relevant memories to each prompt

[memory.context]             # the context compiler
max_tokens = 600
max_memories = 8
min_score = 0.35
min_relevance = 0.45

[memory.ranking]
semantic = 0.40
lexical = 0.20
importance = 0.15
confidence = 0.10
recency = 0.10
relationship = 0.05

[memory.half_life_days]
working = 1
episodic = 30
semantic = 365
```

## Plans (planning and execution)

For work with several steps, `/plan <request>` turns the request into a **goal**
with explicit success criteria (plus constraints and open questions) and a
**structured plan**: steps with dependencies, an action each, an expected outcome
and how to verify it. Built following `docs/planning_execution_system.md` in the
`execution/` crate (`lyra-execution`); plans persist in `~/.lyra/plans/plans.db`.

A step's action is one of:

- **tool**: one tool call. Arguments that depend on an earlier result are
  written as `{{s3}}` and filled in from that result just before the step runs.
- **reasoning**: the model works on it, using tools as needed.
- **workflow**: follow one of your learned skills.
- **subagent**: a helper with its own scoped task and tools (`researcher` is
  read-only; `archivist` can also record in memory).

`/plan run` executes it:

- independent steps run in parallel; steps that share a resource, or that are
  unsafe to repeat, don't;
- every attempt and result is stored; each step is **verified** (tool result,
  a follow-up read-only tool, a deterministic text check, or the model judging
  the evidence), so "it ran" isn't taken for "it worked";
- transient failures (timeouts, rate limits, connection errors) are **retried**
  with backoff; permission and input errors aren't;
- a step that still fails is **replanned**: only the broken part changes,
  completed work is kept, and the plan's version goes up;
- **destructive tools** (e.g. `memory_forget`) only run as their own tool steps
  and **pause the plan for approval** of the exact call; changed arguments need
  approval again. Reasoning steps can't use them.
- **budgets** (model calls, tool calls, replans, tokens, time) are enforced;
  running out pauses the plan;
- a **checkpoint** is saved before anything that changes things;
- at the end the **goal** is judged against its success criteria: a plan whose
  steps all finished can still be only *partial*.

If lyra stops mid-run, the plan is found on the next start; a step that was
running is only rerun if repeating it is safe, otherwise it waits for
`/plan retry` or `/plan skip`. A finished plan is recorded as a memory episode,
and recoveries (retries, replans) are offered to the skill reviewer.

Planning, verification, memory capture, skill reviews and curation all make
internal JSON calls to the chat model. With a reasoning model these can think for
a long time, so two top-level settings bound them: `structured_max_tokens`
(default 8192) caps each call, and `structured_thinking = false` asks the server
to skip thinking for them (`chat_template_kwargs.enable_thinking`, understood by
llama.cpp and vLLM for Qwen3-style models), several times faster.

Commands: `/plan <request>`, `/plan` (show), `/plans`, `/plan run|resume [id]`,
`/plan approve <step>`, `/plan retry|skip <step>`, `/plan cancel`,
`/plan events` (the full event log with metrics).

```toml
[planning]
enabled = true
max_parallel = 3
forbidden_tools = []        # tools plan steps may never use

[planning.budget]
max_model_calls = 40
max_tool_calls = 100
max_replans = 3
max_minutes = 30
max_tokens = 300000
```

## Self-learning (skills)

Memory holds facts; **skills** hold procedures lyra learned. They're built
following `docs/skill_learning.md` (V1–V7) and live in their own crate
(`learning/`, `lyra-learning`):

- **Skill files** in `~/.lyra/skills/<name>.md` hold each skill's current text
  and status. Edit them freely, or drop in your own (every header line is
  optional; a file without one is an active skill).
- **The ledger** (`~/.lyra/skills/ledger.db`) holds the evidence and history:
  every version, which replies used which skills and how that went,
  relationships (supersedes, conflicts with, …), pending proposals and an
  audit log of every change.

```markdown
---
description: When checking a Rust project before committing
status: active
confidence: 0.90
source: conversation
created: 2026-10-05T00:06:24Z
updated: 2026-10-05T00:06:40Z
id: 2c9ca857-1f0e-4d0e-9a51-6f4c0f3e8a11
---
1. cargo fmt --check
2. cargo clippy -- -D warnings
3. cargo test
```

How it works:

1. **Use.** Before each message, matching skills are ranked by relevance,
   observed reliability, learned confidence and freshness (weights in
   `[learning.scoring]`) and the best are added to the prompt. Each reply is a
   *run*; the skills it used are recorded and shown under the reply.
2. **Outcomes.** Your next message is read as feedback on those skills: thanks
   or "that worked" is a success, a correction a failure, anything else no
   signal. `/outcome good|bad|partial` says so explicitly. Reliability is
   smoothed, `(successes + 1) / (outcomes + 2)`, so new skills start at 0.5.
3. **Learning.** After a correction, a failed step that was fixed, a multi-step
   success or "remember how we did this", the model reviews the turn, seeing
   the existing skills most like it, and decides to *ignore*, *create* a new
   skill or *update* an existing one. New skills start proposed. Updates
   snapshot the old text first, so `/rollback` can undo them.
4. **Lifecycle.** Evidence moves skills along: a confident proposal with
   2 successes and no failures is promoted; an active skill whose reliability
   falls below 0.4 after 5 outcomes is deprecated (never deleted). Thresholds
   are in `[learning.lifecycle]`.
5. **Curation.** `/curate` (or `curate = "daily"`/`"weekly"`) looks for
   near-duplicates, stale skills and, with the model's help, merges, splits and
   contradictions. Merges and splits are proposed; conflicts are flagged.

`mode` decides how much happens on its own:

| | `propose` (default) | `auto` |
|---|---|---|
| new skills | proposed; `/approve` to use | proposed; confident ones used on trial |
| refinements | proposed | applied (versioned) |
| promotion / deprecation | proposed | applied (audited) |
| merges / splits | proposed | proposed |

`off` turns reviews and automatic changes off.

Commands: `/skills` (what to review, and every skill's track record),
`/approve <id>`, `/reject <id>`, `/deprecate <id>`, `/forget-skill <id>`,
`/history <id>`, `/rollback <id> [version]`, `/outcome good|bad|partial`,
`/learn`, `/curate`, `/help`. Ids are skill names or id prefixes.

```toml
[learning]
mode = "propose"          # off | propose | auto
curate = "manual"         # manual | daily | weekly
min_confidence = 0.6
max_skills = 3

[learning.scoring]
relevance = 0.50
reliability = 0.25
confidence = 0.15
freshness = 0.10
half_life_days = 90

[learning.lifecycle]
promote_confidence = 0.85
promote_successes = 2
deprecate_reliability = 0.4
deprecate_min_uses = 5
stale_days = 90
```

## Metrics

Each reply shows time to first token, generation speed, tokens in/out, cost and
total time. The status bar shows session totals (replies, tokens in/out/total,
cost, average TTFT) and a live timer while a reply streams, plus a cache line:
prompt tokens served from the server's prompt cache, hit rate, what they cost
and what they saved versus the full input price.

Token counts come from the server's `usage` report. If a server doesn't send one,
output tokens are estimated from the stream and marked `~`.

## Run

```sh
cargo run
```

Keys: type, `Enter` to send, `↑`/`↓`/`PgUp`/`PgDn` to scroll history, `Ctrl-R` to show/hide model reasoning, `Ctrl-B` to show/hide the side panels, `Ctrl-L` to reload the context files and config, `Esc` / `Ctrl-C` to quit.

## Layout

The chat sits on the left; on terminals at least 100 columns wide, panels on the right show:

- **Session**: what the assistant is doing right now (idle / waiting / thinking / streaming / running a tool, with a timer), model and server, replies, last reply's TTFT and speed, tokens, cache hit rate and cost.
- **Agent**: system prompt size, loaded SOUL/USER/AGENT files, tools, and embedding/reranker health.
- **Memory**: active memories by kind, vector coverage, what needs approval (proposals, contradictions, duplicates, expired), working memory (goal, plan, notes), how many memories the last reply used, and the most recent memories (`?` marks unsure ones).
- **Skills**: learning mode, average reliability, what needs review (proposals, conflicts, duplicates, failing or stale skills), the last reply's skills, and each active skill's reliability and use count.
- **Plan**: the current plan's goal, steps with their status (✓ ▸ ⏸ ✗ ○, ⚠ for approval), budget use and any note.
- **Activity**: a timestamped log of requests, first tokens, tool calls and results, memory, skill and plan events, reloads and errors.

When the panels are hidden or don't fit, a one-line status bar shows the session totals instead.

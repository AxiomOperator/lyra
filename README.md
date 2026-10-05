# lyra

A dead simple terminal chat client for a local LLM.

Talks to any OpenAI-compatible `/chat/completions` endpoint (Ollama, llama.cpp server, LM Studio, vLLM, ...).

## Files

Everything lyra keeps lives in one folder, `~/.lyra` (set `LYRA_HOME` to use another):

```text
~/.lyra/
├── config/    config.toml, behavior.toml (evolved behavior)
├── context/   SOUL.md, USER.md, AGENT.md
├── capabilities/ capabilities.db (usage), index/ (discovery, LanceDB)
├── evolution/ evolution.db (runs, candidates, generations)
├── memory/    lance/ (LanceDB: memories, vectors, history)
├── plans/     plans.db
├── skills/    <name>.md, one file per skill
├── tools/     <name>.toml, composite tools
└── workflows/ <name>.toml
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

Optional. The embedding model gives memories their vectors (meaning-based
recall); without it, memory uses keywords only. The reranker isn't used yet.
When configured, lyra sends each a tiny test request at startup and on
`Ctrl-L` and reports whether it's ready.

```toml
[embedding]           # OpenAI-style /embeddings
url = "http://localhost:8082/v1"
model = "embedding"
# dimensions = 4096   # the vector size; asked of the model when not set

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
(working memory), built following `docs/done/memory_system.md` in the `memory/`
crate (`lyra-memory`). Everything goes through `MemoryManager`: the model
proposes memories, the manager decides.

**Storage is LanceDB** (`docs/done/lancedb_migration.md`), a local directory,
`~/.lyra/memory/lance`, with no server. Each memory is a row with typed
columns and its embedding (a fixed-size vector sized for the embedding model,
with the model and generation that made it). Keyword search uses LanceDB's
full-text index, meaning uses vector search (exact, or an index once the
collection passes `vector_index_threshold`), and the two are merged and
ranked by the manager. The history around memories (versions, links,
events, usage, episodes, proposals) is kept in small tables next to it.
Vectors come from the `[embedding]` model through an embedding provider the
store never sees. When that model changes, its old vectors stop being used
and memories are re-embedded (`/memory reembed`, also done on startup).
`/memory backup` copies the store to `~/.lyra/backup/memory-<time>`;
`lyra --restore-memory <dir>` puts one back (the replaced store is kept
aside). `/memory` shows the store's size and how fast each operation is
(P50/P95). A SQLite backend is still available (`backend = "sqlite"`).

- **Remembering.** The model saves facts with `memory_remember`. After a turn
  where you shared something that sounds durable ("we use…", "I prefer…", "we
  moved to…"), lyra also asks the model to review it and capture what it missed.
  Each memory records its scope, kind, source, the run it came from, importance
  and confidence (what you say outright is trusted more than what's inferred).
- **No duplicates, no silent overwrites.** Saying something again reconfirms the
  existing memory and makes it a little more trusted. A changed fact *supersedes*
  the old one, which is kept and linked; a fixed typo is a new *version*.
  Nothing is overwritten without history.
- **Relating new facts.** When the model saves a memory, similar ones are shown
  to the model, which says how they relate: an update supersedes the old memory,
  a contradiction is flagged (`/memory`), and support or relatedness becomes a
  link that ranking uses (supported memories rank higher, contradicted lower).
- **Projects.** Memories about a project live in `project:<name>`. The current
  project (`[memory] project`; by default the git checkout lyra was started in;
  `/memory project` to switch) decides what's recalled: your memories, the
  agent's, and the current project's, never another project's unless asked for
  by scope.
- **Recall.** Before each message the *context compiler* ranks candidate memories
  by meaning (vectors from the `[embedding]` model), keywords, importance,
  confidence, recency and relationships, drops near-duplicates and anything that
  doesn't actually match the message, and adds the best few within a token
  budget. Replies show `used memories: …`; your reaction (thanks, or a
  correction) marks them helpful or not, which feeds back into ranking.
  Without an embedding model, recall uses keywords only.
- **Working memory.** The model keeps the current goal, plan and scratch values
  with `working_memory`; identifiers either side mentions and recent tool
  results are tracked too. It's shown in the prompt and the panel, and never
  saved. (The recent messages themselves are already in the prompt.)
- **Episodes.** Multi-step tasks, finished plans and `/memory episode` are
  summarized as episodes: what happened and how it ended, when it started,
  linked to the run or plan. A finished plan's results also go through capture,
  so durable facts it discovered are remembered.
- **Upkeep.** Expired memories are archived, old ones fade in ranking
  (half-lives per kind, and per category for facts tagged `preference`,
  `decision` or `configuration`), and `/memory curate` (or `curate =
  "daily"`/`"weekly"`, checked while lyra runs too) proposes consolidating
  duplicates and flags contradictions.
- **Provenance.** Each memory records its source, the run and tool call it came
  from, and the session (conversation) it was saved in.
- **Safety.** Passwords, tokens, keys and other credentials are refused before
  anything is stored. `allowed_scopes` limits which scopes lyra may use; in
  plans, the `archivist` helper may only write the agent and project scopes.

Model tools: `memory_remember`, `memory_recall`, `memory_list`, `memory_inspect`,
`memory_correct`, `memory_supersede`, `memory_archive`, `memory_forget`,
`working_memory`. Ctrl-L reloads the `[memory]` settings and embedding model.

Commands: `/memory` (stats, storage and latency, what's waiting for approval), `/memory search <q>`
(with each ranking signal), `/memory list [scope]`, `/memory inspect <id>`
(provenance, links, versions, history), `/memory correct <id> <text>`,
`/memory forget|archive|restore|purge <id>`, `/memory approve|reject <id>`,
`/memory working [clear]`, `/memory curate`, `/memory episode`,
`/memory events` (what happened to memories lately), `/memory project [name|none]`,
`/memory reembed`, `/memory backup`.

```toml
[memory]
enabled = true
backend = "lance"            # lance | sqlite
# path = "~/.lyra/memory/lance"   # the LanceDB directory (or SQLite file)
table = "memories"
vector_index_threshold = 10000    # index vectors from this many on; 0 = never
default_scope = "user"
allowed_scopes = ["*"]       # e.g. ["user", "agent", "project:*"]
capture = "auto"             # auto | off
maintenance = "propose"      # off | propose | auto
curate = "manual"            # manual | daily | weekly
inject = true                # add relevant memories to each prompt
project = "auto"             # auto (the git checkout's name) | none | a name
same_wording = 0.85          # word overlap that counts as the same memory
same_meaning = 0.95          # vector similarity that counts as the same memory
duplicate_wording = 0.6      # word overlap the curator flags as a duplicate pair
stale_days = 180             # unused this long (and not important) is stale

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
preference = 730             # facts tagged preference
decision = 365               # facts tagged decision
configuration = 90           # facts tagged configuration or config
```

## Plans (planning and execution)

For work with several steps, `/plan <request>` turns the request into a **goal**
with explicit success criteria (plus constraints and open questions) and a
**structured plan**: steps with dependencies, an action each, an expected outcome
and how to verify it. Built following `docs/done/planning_execution_system.md` in the
`execution/` crate (`lyra-execution`); plans persist in `~/.lyra/plans/plans.db`.

A step's action is one of:

- **tool**: one tool call. Arguments that depend on an earlier result are
  written as `{{s3}}` and filled in from that result just before the step runs.
- **reasoning**: the model works on it, using tools as needed.
- **workflow**: follow one of your learned skills.
- **subagent**: a helper with its own scoped task and tools (`researcher` is
  read-only; `archivist` can also record in memory, in the agent and project
  scopes only). Its report ends with a status, so a helper that couldn't do
  the task fails the step instead of passing it on.

Every step has an expected outcome, and says whether repeating it is safe.
A *safe* step only gets read-only tools; a step that may change things is
*conditional* (or *unsafe*) and runs on its own. If an attempt changed
something and then failed, it isn't simply retried: the replanner sees what
was already done. Tool steps that change things get an operation id, and a
completed operation is recorded, so a retry or a resume after a restart
reuses its result instead of acting twice.

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
  approval again. Reasoning steps can't use them. When the goal itself is
  destructive or external, every step that changes things needs approval.
- a goal with **open questions** isn't run until you answer them with a
  clearer request, or say `/plan run anyway`;
- **budgets** (model calls, tool calls, replans, tokens, time) are enforced;
  a step stops within what's left, and running out pauses the plan until
  `/plan budget raise` gives it more;
- a **checkpoint** is saved before anything that changes things;
- at the end the **goal** is judged against its success criteria: a plan whose
  steps all finished can still be only *partial*.

If lyra stops mid-run, the plan is found on the next start; a step that was
running is only rerun if repeating it is safe or its operation is on record,
otherwise it waits for `/plan retry` or `/plan skip`. A finished plan is
recorded as a memory episode, its results go through memory capture,
recoveries (retries, replans) are offered to the skill reviewer, and its full
telemetry goes to evolution; your next message counts as feedback on it.

Planning, verification, memory capture, skill reviews and curation all make
internal JSON calls to the chat model. With a reasoning model these can think for
a long time, so two top-level settings bound them: `structured_max_tokens`
(default 8192) caps each call, and `structured_thinking = false` asks the server
to skip thinking for them (`chat_template_kwargs.enable_thinking`, understood by
llama.cpp and vLLM for Qwen3-style models), several times faster.

Commands: `/plan <request>`, `/plan` (show), `/plans`, `/plan run|resume [id]`,
`/plan run anyway`, `/plan approve <step>`, `/plan retry|skip <step>`,
`/plan cancel`, `/plan events` (the full event log with metrics),
`/plan budget [raise]`, `/plan checkpoints`, `/plan revisions`.

Steps are stored with their plan (one JSON column) and every attempt,
verification, revision, checkpoint, approval and operation has its own table;
dependencies and resource locks are part of each step.

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
following `docs/done/skill_learning.md` (V1–V7) and live in their own crate
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

## Self-evolution

Skills are what lyra learns; **evolution** changes *how it works*, from
evidence about its own runs, progressively and reversibly. Built following
`docs/done/self_evolution.md` in the `evolution/` crate (`lyra-evolution`).

- **Telemetry.** Every chat turn and plan run is recorded in
  `~/.lyra/evolution/evolution.db`: model and tool calls, tool errors,
  retries and replans, tokens, time, the skills used and the generation that
  did the work. Your next message (a correction or thanks, or `/outcome`)
  becomes its outcome, and what you said is kept with it; a plan's outcome is
  its goal status.
- **Review.** Deterministic detectors look at recent runs for problems with
  evidence: heavy runs, the same error again and again, frequent corrections,
  the same chain of tools in many runs, failing skills, plans that need rework.
- **Evolve.** For each problem the model, as a separate evolver, proposes 1–3
  competing **candidates**, each a small, structured change:
  - *prompt*: a behavior guideline added to the system prompt (no rewrites,
    nothing that weakens safety, no secrets);
  - *configuration*: one behavior setting (`max_tool_rounds`,
    `plan_step_rounds`, `recall_before_answering`,
    `search_skills_before_planning`, `verify_reasoning_steps`);
  - *workflow*: phases the planner (and the chat) follows for requests with
    certain trigger words, in `~/.lyra/workflows/<name>.toml`;
  - *tool*: a composite tool that calls existing, non-destructive tools in
    order (`{{input}}` placeholders), in `~/.lyra/tools/<name>.toml`. It's
    data, not code;
  - *skill*: revised instructions for a learned skill, applied as a new
    skill version.
- **Validate.** `/evolve test` checks a candidate (it applies, is safe, its
  tools exist), then **benchmarks** it: recent tasks (the evidence first) are
  replayed headless with the current agent and with the candidate. Only
  read-only tools really run (changes are simulated, destructive calls count
  as safety violations). Changes that only affect planning (step rounds,
  skill search, verification, workflows) are measured by planning and running
  the tasks on a throwaway plan store instead. The model judges each answer
  against the task and, when you corrected the original, your correction. The
  fitness score weighs success, accuracy, efficiency (calls, tokens, time),
  reliability and safety. `/evolve compare` tests every open candidate for a
  problem and ranks them.
- **Deploy and roll back.** Only a tested candidate that passed its checks
  can be approved, and one that didn't beat the current agent needs
  `/evolve approve <id> force`. Each deployment is a new **generation** with a
  full snapshot of `behavior.toml`, the workflows and the tools (skill
  revisions are generations too, recording the skill's versions); deploying
  one candidate rejects its competitors. `/evolve rollback` restores any
  earlier generation, skill versions included, and the rollback is itself a
  generation.
- **Monitor.** Once a new generation has enough runs, it's compared with its
  parent: success rate over judged runs, and how often answers get corrected
  and how many errors runs hit over all of them. A clear drop is flagged, or
  rolled back in auto mode.
- **Policy.** Each change has a level that sets what may happen without you.
  In `auto` mode, guideline and skill changes that beat the baseline deploy on
  their own. Configuration, workflow and tool changes always wait for
  `/evolve approve`. Code is manual. Architecture is never touched.
- **History.** Every proposal, test, approval, deployment, rejection and
  rollback is an event with its category, evidence and fitness; the history
  is append-only, enforced by the database.

**Code evolution** is off unless `source_repo` points at a git checkout of
lyra. `/evolve code <problem>` then has the model pick files and write a
patch against the current commit. `/evolve test` applies it in a throwaway
git worktree and runs clippy and the tests there. Patches may not touch the
evolution system or the safety checks (`evolution/`, `src/evolve.rs`,
`memory/src/safety.rs`) or anything outside the repository. Approving commits
it to a local branch `evolution/<id>`. Nothing is merged or pushed, and the
running binary is never changed; review and merge the branch yourself. A
review suggests `/evolve code` when an error keeps recurring.

Commands: `/evolve` (status), `/evolve review`, `/evolve list`,
`/evolve show <id>`, `/evolve test <id>`, `/evolve compare <id>`,
`/evolve approve <id> [force]`, `/evolve reject <id>`,
`/evolve rollback [generation]`, `/evolve generations`, `/evolve history`,
`/evolve runs`, `/evolve code <problem>`.

```toml
[evolution]
enabled = true
mode = "propose"            # off (record runs only) | propose | auto
review = "manual"           # manual | daily | weekly
window = 50                 # recent runs the detectors look at
benchmark_tasks = 3         # tasks replayed per benchmark
monitor_runs = 10           # judged runs needed before comparing generations
monitor_drop = 0.15         # success-rate drop that counts as a regression
max_problems = 3            # problems a review proposes candidates for
# source_repo = "~/Projects/lyra"   # enables code evolution

[evolution.thresholds]
heavy_tool_calls = 8
heavy_model_calls = 6
heavy_runs = 2
repeated_error = 3
corrections = 3
sequence_runs = 3
replans = 2                 # a plan run is troubled with this many replans,
plan_retries = 3            # retries,
plan_verification_failures = 2   # or failed verifications
plan_runs = 2               # troubled plan runs before it's a problem
skill_min_uses = 3          # a skill used this often
skill_reliability = 0.4     # with reliability below this is failing

[evolution.fitness]
success = 0.4
accuracy = 0.25
efficiency = 0.15
reliability = 0.1
safety = 0.1
```

## Capabilities

Everything lyra can do is a **capability**, described the same way whatever
it's made of: the memory tools, composite tools evolution generated, OpenAPI
operations, MCP tools, workflows, learned skills and helper agents. Built
following `docs/done/capabilities.md` in the `capabilities/` crate
(`lyra-capabilities`).

- **One registry.** It's loaded from every provider at startup and refreshed
  when skills, workflows or tools change. Each capability has an input schema,
  a risk level (read-only, low write, write, destructive, privileged),
  permissions, tags, the identifiers it needs and what finds them (a task
  needs a `projectId`, which `projects.list` provides), and how to check its
  effect.
- **Discovery, not everything at once.** With more than `max_tools` callable
  capabilities, the model is offered only the ones that fit the message,
  found by keywords (full-text search) and meaning (vectors from the
  `[embedding]` model, kept in LanceDB in `~/.lyra/capabilities/index`), plus
  a `capability_search` tool to find more. Plans get the capabilities their
  goal needs, with their track record, prerequisites, approval and checks.
- **Scored by evidence.** Candidates rank by relevance, reliability (their
  success rate), whether policy lets them run, speed and how much they've been
  used, so a tool that usually works beats one that often fails.
- **Policy in Rust.** `[capabilities.policy]` sets what each risk level may
  do: `auto`, `approval` or `deny`. Denied capabilities are never offered.
  Ones that need approval run as approved plan steps, or after
  `/caps allow <name>` for the session; otherwise the
  model is told to ask.
- **Tracked and verified.** Every call is recorded in
  `~/.lyra/capabilities/capabilities.db`: success, time, retries and the kind
  of error. A successful write is checked by its verification rule (a memory
  is read back after it's saved; a created OpenAPI resource is fetched), and
  the result says whether it was verified. In plans, tool steps use the rule
  as their verification by default.
- **Health.** Providers are checked at startup, on `Ctrl-L` and with
  `/caps health`. An unreachable one is left out of discovery and planning,
  and one that keeps failing is marked degraded and ranked lower.
- **Evolution.** Capabilities that keep failing or are slow become
  evolution problems with the runs as evidence. Composite tools and workflows
  that evolution deploys become capabilities automatically.

**OpenAPI.** Each operation in a spec (JSON or YAML) becomes a capability
named `<name>.<operationId>`. Its risk comes from the method (GET read-only,
DELETE destructive, the rest write) or `x-lyra-risk`. Path identifiers become
requirements resolved by a listing operation, and creates and updates are
verified by fetching the result. Credentials are never stored: `auth_env`
names the environment variable that holds one.

**MCP.** Each server is started over stdio. Its tools become capabilities
named `<name>.<tool>`, with risk from their `readOnlyHint` and
`destructiveHint` annotations. Values in `env` that start with `$` are read
from lyra's environment.

Commands: `/caps` (everything, by kind, with health, policy and track
record), `/caps search <what>` (discovery with each score), `/caps show
<name>`, `/caps allow <name>`, `/caps health`.

```toml
[capabilities]
max_tools = 16              # offer all callable capabilities up to this many
discovery_limit = 8         # otherwise, this many per message (plus search)

[capabilities.policy]       # auto | approval | deny
read_only = "auto"
low_write = "auto"
write = "auto"
destructive = "approval"
privileged = "deny"
# overrides = { "pm.tasks.delete" = "deny", "fs.*" = "approval" }

[capabilities.scoring]
relevance = 0.55
reliability = 0.2
permission = 0.1
efficiency = 0.1
history = 0.05

[[capabilities.openapi]]
name = "pm"
spec = "~/.lyra/capabilities/pm.yaml"
# base_url = "https://pm.example.com/api"   # default: the spec's first server
auth_env = "PM_TOKEN"       # the environment variable with the credential
auth_header = "Authorization"
auth_scheme = "Bearer"

[[capabilities.mcp]]
name = "fs"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/me/notes"]
# env = { API_KEY = "$MY_API_KEY" }
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

Building needs `protoc`, the Protocol Buffers compiler, for LanceDB
(`dnf install protobuf-compiler` / `apt install protobuf-compiler`, or a
release from github.com/protocolbuffers/protobuf on your `PATH`).

Keys: type, `Enter` to send, `↑`/`↓`/`PgUp`/`PgDn` to scroll history, `Ctrl-R` to show/hide model reasoning, `Ctrl-B` to show/hide the side panels, `Ctrl-L` to reload the context files and config, `Esc` / `Ctrl-C` to quit.

## Layout

The chat sits on the left. Replies are rendered as Markdown: headings,
bold/italic/strikethrough, inline code and fenced code blocks (with their
language), nested and numbered lists, task lists, quotes, links (with their
address), rules and aligned tables, also while a reply is still streaming; on terminals at least 100 columns wide, panels on the right show:

- **Session**: what the assistant is doing right now (idle / waiting / thinking / streaming / running a tool, with a timer), model and server, replies, last reply's TTFT and speed, tokens, cache hit rate and cost.
- **Agent**: system prompt size, loaded SOUL/USER/AGENT files, capabilities by kind, how many are offered per message and any that are degraded or unavailable, and embedding/reranker health.
- **Memory**: active memories by kind, vector coverage, the store (backend, size, search latency; red when operations fail), the current project, what needs approval (proposals, contradictions, duplicates, expired), working memory (goal, plan, notes, the last tool result), how many memories the last reply used, and the most recent memories (`?` marks unsure ones).
- **Skills**: learning mode, average reliability, what needs review (proposals, conflicts, duplicates, failing or stale skills), the last reply's skills, and each active skill's reliability and use count.
- **Plan**: the current plan's goal, steps with their status (✓ ▸ ⏸ ✗ ○, ⚠ for approval), budget use and any note.
- **Evolution**: the generation and mode, runs recorded, success rate and corrections, calls per run, what has evolved (guidelines, workflows, composite tools, changed settings), candidates waiting for review, the last review, and what evolution is doing right now.
- **Activity**: a timestamped log of requests, first tokens, tool calls and results, memory, skill, plan and evolution events, reloads and errors.

When the panels are hidden or don't fit, a one-line status bar shows the session totals instead.

# lyra

A dead simple terminal chat client for a local LLM.

Talks to any OpenAI-compatible `/chat/completions` endpoint (Ollama, llama.cpp server, LM Studio, vLLM, ...).

## Files

Everything lyra keeps lives in one folder, `~/.lyra` (set `LYRA_HOME` to use another):

```text
~/.lyra/
├── agents/    <name>.toml, one file per subagent; agents.db (versions, delegations), index/ (routing, LanceDB)
├── config/    config.toml, behavior.toml (evolved behavior)
├── context/   SOUL.md, USER.md, AGENT.md
├── capabilities/ capabilities.db (usage), index/ (discovery, LanceDB)
├── evolution/ evolution.db (runs, candidates, generations)
├── goals/     goals.db (long-lived goals, their plans, blockers, triggers)
├── memory/    lance/ (LanceDB: memories, vectors, history)
├── plans/     plans.db
├── sessions/  <id>.json, saved conversations (lyra -c / -r)
├── web/       devices.json (paired devices), vapid.key (push identity)
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


### Decision model (optional)

lyra makes many small yes/no and pick-one decisions:
- which agent should take a message
- how a new memory relates to similar ones
- whether a plan step worked
- how a benchmark answer scored
- whether a turn is worth a memory capture or a skill review

By default the chat model answers them, each a full completion of a few seconds. A
**decision model** such as Cloudflare's
[clef-flash](https://huggingface.co/bartowski/Cloudflare_clef-flash-GGUF) answers them in tens
of milliseconds. It returns a probability for every option instead of text to parse.

Serve it with llama-server (a build with `/v1/systemone` support) next to the chat model:

```sh
llama-server -m Cloudflare_clef-flash-Q4_K_M.gguf --port 8091 -ngl 99 -c 16384 -b 8192 -ub 8192
```

The whole input (state and questions) must fit in one batch. llama-server's default `-ub` is 512
tokens, which is too small for a conversation turn, so raise `-b`/`-ub` and then
`max_state_chars` with them. An input that's still too big is answered by the chat model.

```toml
[decide]
url = "http://localhost:8091/v1"   # /systemone is added
model = "clef-flash"
# min_confidence = 0.75             # below it, the chat model decides
# max_state_chars = 1000            # fits the default 512-token batch; ~20000 with -ub 8192
```

**Without `[decide]`, nothing changes:** the chat model answers everything as before. With it,
an answer is used only when the model is at least `min_confidence` sure. Otherwise the chat
model decides. If the endpoint is down, the chat model takes over for a minute at a time and
Activity says so.

The per-turn memory and lesson checks also run when the keyword triggers don't fire. That
catches turns the keywords miss, and the chat model is still called only on a yes.

Each decision is logged in Activity with its answer, confidence and time. The model, how many
decisions it made and their average time are shown in the TUI's Session panel, in
`lyra connect`'s side panel ("Decisions"), and in the app's sidebar and About card. Approvals and system
checks never depend on it.

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

## Goals and autonomy

A **plan** is one attempt at something; a **goal** is what lyra is trying to
accomplish over days or weeks. Built following `docs/done/goal_manager.md`
in the `goals/` crate (`lyra-goals`); goals persist in `~/.lyra/goals/goals.db`.

- **Goals** have a title and description, success criteria, a priority (0–10),
  importance, an optional deadline, a parent and subgoals, and goals they wait
  for. Statuses: proposed, active, blocked, paused, completed, failed,
  cancelled. Goals the model suggests (`goal_create`) start as proposed.
- **Decomposition.** `/goal decompose` has the model break a goal into
  subgoals with their order. A goal with open subgoals is worked through them,
  and completes when they all have.
- **Plans are attempts.** `/goal work` plans the next useful piece of a goal,
  told what's done and what earlier plans found, and runs it. When a plan
  ends, the goal's progress (items done, a summary in words) follows from the
  plan's evaluation, and completed or failed goals become memory episodes.
- **Blockers and dependencies.** A goal that can't progress is blocked with a
  reason: missing information, missing permission, an external or failed
  dependency, approval required, or a capability unavailable. A blocked goal
  isn't retried; it waits for a state change. An approved plan, or a
  capability that's healthy again, unblocks it, and so can a trigger or
  `/goal unblock`. Plans that keep failing (`max_failures`) block their goal.
- **Priority.** Goals rank by explicit priority, deadline urgency, how many
  goals wait for them, importance and progress, less what they've cost so far
  (`/goals next` explains the pick). A goal due tomorrow that blocks three
  others can come before a higher-priority one.
- **Scheduling and triggers.** `/goal when` wakes a goal at a time, every so
  often (recurring goals reopen each period), after another goal completes,
  or when a condition becomes true: an environment variable is set, a file
  exists, a capability is healthy.
- **Autonomy** (`[goals.autonomy] mode`, or `/goals autonomy` for this run):
  - *reactive* (the default): only works when asked.
  - *assisted*: may continue goals already worked on (and resume interrupted
    plans), but every write is a plan step that waits for approval.
  - *autonomous*: picks the highest-priority goal that can progress and works
    on it.

  In both of the last two, the runtime enforces a session's limits: minutes,
  plans, tool and model calls, replans, cost, and the riskiest capability it
  may use. When a limit is reached the session stops, and another may start
  after `cooldown_minutes`. Autonomous work only starts while you're idle.
- **Review.** `/goals review` has the model suggest merging duplicate goals,
  cancelling obsolete ones, completing finished ones and fixing priorities,
  and flags goals untouched for `stale_days`. Applied with
  `/goals review apply`, or straight away in autonomous mode.
- **The model knows.** The open goals are in the system prompt, and
  `goal_list`, `goal_get`, `goal_create` and `goal_note` let it answer "how
  far are we on …" and record progress. Goals that keep getting stuck become
  evolution problems.

Commands: `/goals [all]`, `/goals next`, `/goals review [apply]`,
`/goals autonomy [reactive|assisted|autonomous]`, `/goal new <title> [-- description]`,
`/goal <id>`, `/goal decompose|work <id>`,
`/goal activate|pause|cancel|complete|fail|unblock <id>`,
`/goal priority|importance|due|criteria|depends|block|when <id> …`.

```toml
[goals]
tick_seconds = 60           # how often triggers, blockers and autonomy are checked
stale_days = 14

[goals.autonomy]
mode = "reactive"           # reactive | assisted | autonomous
max_runtime_minutes = 30    # one session's limits
max_plans = 3
max_tool_calls = 60
max_model_calls = 40
max_replans = 2
max_cost = 0.0              # 0 = no limit
max_risk = "write"          # the riskiest capability autonomous work may use
cooldown_minutes = 60
max_failures = 3            # failed plans in a row before a goal is blocked

[goals.priority]
explicit = 0.35
deadline = 0.25
dependency = 0.15
importance = 0.1
progress = 0.1
cost = 0.05
```

## Subagents

The main agent owns the conversation; **subagents** own specialties. Built
following `docs/done/sub_agents.md` in the `agents/` crate (`lyra-agents`).
Each agent is a TOML file in `~/.lyra/agents/` (edit it by hand if you like:
the change becomes a new version), with its versions and every delegation in
`agents.db`. Researcher and Archivist are installed to start with.

- **Profiles.** A name, description and role, its own instructions, the tools
  and capability patterns it may use (and a deny list and risk ceiling), its
  memory policy, its own skills, a model policy (another model, endpoint,
  temperature, thinking), and routing hints: intents, keywords, example
  requests and requests it must *not* get.
- **Creating one.** `/agent new` asks one question at a time (answer by number
  or in words); `/agent new writer` starts from a template (writer, developer,
  researcher, analyst, project-manager, assistant, reviewer, data-analyst,
  archivist) and only asks how to customize it; `/agent new expert` takes a
  TOML or YAML profile. The model then writes its instructions and routing
  examples, the agent is **tried on a test task** before it exists, and you
  `activate`, `modify <answer>`, `test` again or `cancel`. An interrupted
  interview resumes with `/agent new`.
- **Routing.** Before the main agent answers, a message goes to a specialist
  when it's named (`@writer …`, "ask the writer"), when an agent's rules match
  well (examples, keywords, intents; exclusions veto), or when it's very close
  in meaning to what an agent handles (embeddings in LanceDB). In between, the
  model is asked; anything else stays with the main agent, which can also hand
  work over itself with the `delegate` tool.
- **Delegation.** The specialist gets a structured request: the task, only the
  context it needs (the input, memories its policy lets it read), its skills,
  an output contract and a budget. It answers with its result, a confidence,
  or why it refused. The main agent checks the result and gives the user the
  answer; "handled with: Writer" shows under the reply.
- **Permissions are enforced.** An agent is only offered what its profile
  allows, every call is checked again, and its memory reads and writes stay in
  its scopes. Agents that may delegate can call others, up to `max_depth`.
- **Plans** can give steps to any agent (with its instructions, model and
  scopes); those steps are recorded as its delegations too.
- **Learning and evolution.** Your reaction to a reply counts for the agents
  that worked on it; a correction is reviewed for a lesson that becomes that
  agent's own skill. Agents that keep failing or getting corrected become
  evolution problems: revised instructions are benchmarked on the agent's own
  tasks (no tools, nothing changes) and deploy as a new agent version, which
  `/evolve rollback` and `/agent rollback` undo.

Commands: `/agents [log]`, `/agent new [template|expert …]`, `/agent <name>`,
`/agent ask <name> <task>`, `/agent edit <name> <field> <value>`,
`/agent enable|disable|delete|history <name>`, `/agent rollback <name> [version]`,
`/agent skill <skill> <agent|global>`, `/agent cancel`.

```toml
[agents]
enabled = true
auto_delegate = true        # hand matching requests over without being asked
max_depth = 2               # main → agent → agent, no further
show_handled_by = true      # "handled with: Writer" under replies

[agents.routing]
rule_threshold = 0.75       # rule score that routes on its own
semantic_threshold = 0.85   # similarity that routes on its own
semantic_floor = 0.5        # below this an agent isn't considered
model_fallback = true       # ask the model when it's in between

[agents.budget]
max_model_calls = 6         # per delegation
max_tool_calls = 12
```

## System access

lyra can work on the machine it runs on and on your servers through the
**Operator** subagent, which is installed when system access is on (delete it
and it stays deleted). The tools live in the `system/` crate (`lyra-system`):

- `system_info`: OS, kernel, uptime, load, CPUs, memory, disks, busiest processes
- `shell_run`: a command on this machine (in your home directory by default)
- `file_read`, `file_list`, `file_write`, `file_delete`
- `http_request`: HTTP(S); a header value like `"$API_TOKEN"` is read from that environment variable
- `ssh_run`: a command on a server in `ssh_hosts`, with your SSH keys (batch mode, never a password)

Only agents whose profile lists them can use these: the main agent isn't
offered them (it hands the work to the Operator) and its calls are refused,
plans only reach them through an Operator step, and other agents don't get
them unless you add them to their profile.

Every call is checked in Rust before it runs, whatever the model asked for:

- **Runs at once:** looking: `system_info`, reading and listing files, GET/HEAD
  requests, and commands that only read (`ls`, `df`, `ps`, `cat`, `grep`,
  `git status`, `systemctl status`, `docker ps`, `ping -c`, …), plus prefixes in
  `allow_commands` and file writes inside `write_roots`.
- **Asks you first:** anything that changes things: other commands, `>`
  redirects, `$(…)`, file writes elsewhere, deletes, other HTTP methods. An
  **Approval needed** box opens above the input: which agent asks, what kind
  of thing it wants to do, exactly what (the command and where it runs, the
  path, the URL), and why it needs a yes. Press `y` (allow once), `n` (deny)
  or `a` (allow that exact action for the session); no Enter needed. Deleting,
  killing, `sudo`, `git push`, stopping services and the like show in red as a
  risk. The Session state and Agents panel say who is waiting, and the chat
  keeps a record of each question and your answer. No answer within
  `approval_timeout_seconds` is a no.
- **Never runs:** `rm -rf /` or your home directory, `mkfs`, `dd` onto a disk,
  fork bombs, anything under `deny_paths` (keys, credentials, lyra's config),
  and servers not in `ssh_hosts`.

Commands have a timeout (their whole process group is stopped), output is
capped, and approved or not, each call is recorded in the capability usage
and the Operator's delegation log.

```toml
[system]
enabled = true
timeout_seconds = 60
allow_commands = []          # e.g. ["cargo test", "make check"]
write_roots = []             # e.g. ["~/lyra-work"]
ssh_hosts = []               # e.g. ["web1", "deploy@10.0.0.5"]
approval_timeout_seconds = 300
```

## Routines

A routine is something lyra does on a schedule, by itself, and tells you about only when it
matters. Just ask:

> every morning at 7, check disk space, pending updates and failed services on @all and tell me
> only if something's wrong

The Operator creates it with `routine_create` after you approve it once. You can also create
one by hand:

```
/routine new morning-check | every day at 07:00 | check disk, updates and failed services on @all
```

- **Schedules:** `every day at 07:00`, `every morning at 7`, `weekdays at 8:30`,
  `monday and friday at 9pm`, `at 6:15am`, `every 30m`, `every 6h`, `hourly` (at most every
  5 minutes). A run missed while lyra was down happens once when it's back.
- **Running:** `lyra serve` runs each due routine in its own conversation, which you can open
  from the routine. The reply starts with a verdict. The decision model (or the chat model)
  then answers "does this need the user?".
- **`notify = problems` (the default):** you get a push only for a "yes", like a failed unit,
  a full disk, or a check that couldn't run. `always` pushes every run, and `never` only logs it.
- **Only looking:** routines only look unless you turn on `changes`. A look-only run is told to
  use read-only checks, and anything it asks to change is declined at once, so a 7 a.m. run
  never waits on you. With `changes`, each change asks you as usual.
- **Commands:** `/routine` lists them with their next and last runs.
  `/routine run|pause|resume|delete|show <name>` and
  `/routine edit <name> schedule|prompt|notify|changes <value>` manage them.
- **Storage:** each routine is a TOML file in `~/.lyra/routines/`, and its last 20 runs are in
  `runs.json`.

In the app, Routines (sidebar, or More → Routines on a phone) lists each routine with its next
run and last verdict, and has **New**, **Run now**, Pause/Resume, Edit, Delete and earlier
runs. A routine whose last run needs you gets a badge. `lyra connect`'s side panel lists them
too (✓ all clear, ⚠ needs you, ↻ running).

## Coding agents: Claude Code and OpenCode

lyra hands coding work to the coding agents you already use, on the machine where the project
is. Ask in chat:

> fix the failing test in ~/Projects/foo on @desktop

> have Claude Code refactor the parser in ~/Projects/lyra into its own module

**Who does it:**
- **The Coder agent** (installed by itself) takes the request and calls `code_task` with the
  folder and a self-contained task.
- **lyra rates the task:**
  - **Simple** (one file, a rename, a small fix with a clear cause, docs, a test) goes to
    **OpenCode**.
  - **Complex** (several files, architecture, an unknown bug, a feature across modules) goes to
    **Claude Code**.
  - The decision model rates it when there is one, otherwise the chat model.
- **Backup:** when OpenCode can't finish (an error, a timeout, or it says it couldn't), Claude
  Code takes over, told what OpenCode tried and what it changed.
- **Naming wins:** "with Claude Code" or "use OpenCode" in the request overrides the rating.

**How it runs:**
- **Full auto:** you approve once, then the agent works on its own in that folder: Claude Code
  with `bypassPermissions`, OpenCode with `--auto`. It never pushes, and commits only if the task
  asks.
- **Plan only:** "just plan …" gives `mode=plan`, which reads and proposes without changing
  anything.
- **Where:** on the server, unless you name a machine ("on @desktop"), in which case it runs
  there through its lyra-node. The Machines page shows which coding agents each machine, and the
  server, has. lyra never picks another machine by itself.
- **Models and logins:** the agents keep their own (here, Opus 5.5 in Claude Code and GPT-5.6
  Terra Pro in OpenCode).

**What you see:**
- **While it works:** the chat shows its steps live ("apply_patch README.md", "Bash cargo
  test").
- **When it's done:** which agent did it and why, the summary, the files changed, the diff stat,
  any local commits, the time and cost, plus:
  - **Show diff**
  - **Continue** (the same session: "also add tests")
  - **Copy resume command** (`claude --resume …` / `opencode -s …`, to pick it up yourself)
- **Stopping:** `/stop` (or the stop button) stops the agent.
- **Past jobs:** listed in More → Coding (the sidebar's Coding) and `/coding`.

```toml
[coding]
simple = "opencode"       # who gets simple work
complex = "claude"        # and complex work
fallback = "claude"       # takes over when simple work fails
timeout_minutes = 30
allow_push = false
# enabled = true
```

### The other way: Claude Code and OpenCode using lyra

`lyra mcp` is an MCP server, using the pairing `lyra connect` made on that machine.
`lyra mcp --install` registers it with Claude Code (user scope) and OpenCode
(`~/.config/opencode/opencode.json`). In those agents you then have:
- `lyra_ask`: ask lyra; it answers in a conversation of its own
- `lyra_memory_recall`: what lyra remembers
- `lyra_status`
- `lyra_machines`: machines with their health
- `lyra_routines` and `lyra_run_routine`

## Problems researched by themselves

When something goes wrong on the server, lyra looks into it before you ask:
- the server reports a new problem (a failed systemd unit, a disk nearly full, memory or load
  too high)
- one of lyra's status checks goes down

Problems on other machines wait for you: nothing runs on another machine unless you ask. Use
"look into it" next to the problem, or `/diagnose <machine> <problem>`.

The Operator investigates on that machine with read-only checks (`systemctl status`,
`journalctl -u …`, the unit or config file, whether a host or port answers). It then writes up
what's wrong, the likely cause, and the exact fix with whether it's safe. Nothing is changed:
any change it asks for during a diagnosis is declined.

- **Where it shows:** the write-up's headline goes to Activity and to a push notification
  ("🔎 desktop: mnt-dbr2\x2drepo.mount failed — the NFS mount hangs…"). The full write-up sits
  under the problem on the Machines page (or the check on Status), with **Fix it** and **Open
  conversation**. Fix it sends the problem and the write-up to chat, where changes are approved
  as usual.
- **Commands:** `/diagnose` lists recent write-ups, `/diagnose <machine> <problem>` looks into
  something now, and `/machines health` shows them. `lyra connect`'s panel shows the headline
  under the machine.
- **Limits:** one diagnosis runs at a time, and the same problem isn't looked into again for a
  day while it lasts. Write-ups are kept in `~/.lyra/diagnoses.json`.

```toml
[diagnose]
auto = true       # false: only /diagnose and the app's "look into it"
# enabled = true
```

## Status

The **Status** page (first in the app's sidebar, a tab on a phone), `/status` in a terminal and
the TUI's Session line show everything lyra depends on. `lyra serve` checks it every minute:

- **Models:** the chat model (it answers, and offers the configured model), embedding,
  reranker, and the decision model
- **Tools & APIs:** web search (SearXNG returns results), and each OpenAPI and MCP provider
- **lyra:**
  - lyra serve itself
  - the public address through the proxy (`public_url/health`)
  - notifications (devices with push on, and failed sends)
  - storage (each store opened, memory count, disk space)
  - backups (the last one is under 26 h old)
  - routines (none failed to finish)
- **Machines:** online, with their health

Each check shows up / degraded / down / off with its latency, a sparkline of the last hour, and
uptime for 24 h and 7 days. Tap one for its recent state changes. History is in
`~/.lyra/status/status.db`, kept 8 days.

When a check is down twice in a row, it's logged and pushed ("⚠ Chat model is down: …"), and
pushed again when it's back. Machines alert through `[health]` instead. `/status now` (or
**Check now**) checks right away.

```toml
[status]
every_seconds = 60
notify = true
mute = []            # checks never pushed about, e.g. ["Web search"]
# enabled = true
```

`lyra connect`'s side panel lists whatever isn't up.

## Backups

lyra backs itself up every night: memory, skills, goals, plans, agents, saved conversations,
config and paired devices. Each backup is one `lyra-<date-time>.tar.gz` in `~/.lyra-backups`,
next to `~/.lyra` rather than inside it. SQLite databases are copied with `VACUUM INTO` and
memory through its own backup, so a backup made while lyra runs is consistent. Files sent
from the app (`uploads/`) aren't included unless you ask.

```toml
[backup]
# dir = "/mnt/nas/lyra-backups"   # another disk or a NAS survives this one failing
at = "03:30"                      # nightly, local time
keep = 7
# include_uploads = false
# enabled = true
```

- `/backup now` and `/backup list` work in a terminal and from the app's command palette.
  The app's About card shows the last backup and has **Back up now** and **Download latest**,
  so you can keep a copy off the server.
- `lyra backup [list]` backs up from a shell (with lyra stopped; while it runs, use `/backup now`).
- `lyra restore <file|latest>` puts one back. Stop lyra first (`systemctl stop lyra`). What it
  replaces is kept as `~/.lyra.before-restore-<time>`.

The TUI's Session panel, `lyra connect`'s side panel and the app all show when the last
backup was made.

## Phones and browsers (`lyra serve`)

`lyra serve` runs the same lyra (memory, skills, agents, plans, goals) without
the terminal UI and serves a web app you can install on a phone (a PWA). It's
real time: replies stream in as they're written, you see agents and tools at
work, and approvals show up as a card with **Allow once / Deny / Allow for
session**. Push notifications tell you when a reply is ready, an agent needs
your OK, a plan stops, or something fails, but only when no device has lyra
open. The web side is the `web/` crate (`lyra-web`).

**1. Run it behind your TLS proxy.** lyra speaks plain HTTP on `[web] listen`
(default `127.0.0.1:8484`); installing the app and notifications need HTTPS,
which your reverse proxy provides. In **Zoraxy**: add an HTTP proxy rule for
your hostname (e.g. `lyra.example.com`) with upstream `127.0.0.1:8484` (or the
LAN IP and port if Zoraxy runs on another machine or in a container), turn on
its TLS certificate, and keep **WebSocket** proxying on (the default). Don't
cache or buffer responses for that host. Then set:

```toml
[web]
listen = "127.0.0.1:8484"
public_url = "https://lyra.example.com"
```

**2. Keep it running.** `lyra service` writes a systemd user service; then
`systemctl --user daemon-reload && systemctl --user enable --now lyra` (and
`loginctl enable-linger $USER` so it runs while you're logged out). Logs:
`journalctl --user -u lyra -f`. Or just run `lyra serve` in a terminal.

**3. Pair each device.** Run `lyra pair` on the computer (`--minutes N` for
1–60 instead of 10). It prints a code that works once, a link with the code
filled in, and a QR code of that link. Scan it with the phone (or open
`public_url` and type the code), then tap Pair. On a server, `scripts/lyra-pair`
does the same from any shell: it finds the data `lyra serve` uses (the
service's `LYRA_HOME` or its user's `~/.lyra`). Install it with
`install -m 755 scripts/lyra-pair /usr/local/bin/` and run `lyra-pair` (or
`sudo lyra-pair`). Only
paired devices get in (each gets its own token; lyra keeps only its hash), and
`lyra devices` / `lyra devices remove <name>` manage them. Five wrong codes
cancel a code.

**The app** (React with shadcn/ui and Vercel AI Elements; source in `web/ui`, see its README) has tabs: **Chat** (with `/` commands, `@` machines, approval
and pairing cards), **Machines** (online, versions, update, remove, the
install command for a new machine), **Devices** (who's paired and online;
unpair), **Activity** (lyra's log) and **More** (saved conversations, new
conversation, notifications, agents, goals, skills, memory, versions). When
lyra is updated, an installed app shows **"A new version of the lyra app is
ready · Update"**; More → *Check for app update* does it by hand.

**4. Install and turn on notifications.**
- **Android (Chrome):** menu → *Install app* (or *Add to Home screen*). Open
  it, tap ⋯ → *Turn on notifications*, then *Send a test notification*.
  Approval notifications have **Allow** and **Deny** buttons that answer
  without opening the app.
- **iPhone (iOS 16.4+):** in Safari, Share → *Add to Home Screen*, open lyra
  from the Home Screen (notifications only work there), then ⋯ → *Turn on
  notifications*. iOS doesn't show buttons on web notifications, so tapping
  an approval notification opens lyra at the approval card.

Notifications go through your phone's push service (Google's or Apple's),
end-to-end encrypted (RFC 8291) and signed with lyra's own VAPID key
(`~/.lyra/web/vapid.key`); the push service sees only that something arrived.
That needs outbound internet from the computer, not inbound.

**Conversations run in parallel.** Each device shows its own conversation and
comes back to it (the server remembers which); `/new` starts another one for
that device only, and `/resume <id>` (or More → Conversations) switches.
Several can be answering at once — your phone in one, the terminal in
another — and two devices showing the same conversation see it live. They
all share memory, skills, agents and machines; background work (schedules,
goals) runs once. A conversation nobody has open is saved and put away after
15 minutes, and comes back when you resume it. After a restart the server
carries on the latest one. `/new`, `/sessions` and `/resume <id>` work from
the phone (⋯ has New conversation and Saved conversations), and so do all
the other commands (type `/`). `lyra -c` at the desk continues the same saved
conversations, but don't run the TUI and `lyra serve` on the same
conversation at the same time: each keeps its own copy and the last to save
wins.

**Finding a conversation:** `/sessions search <words>` lists the saved conversations that
contain every word, best match first, with the line that matched. In the app, the search box
above the conversation list (the sidebar on a desktop, More → Conversations on a phone) does
the same as you type. Click a result to open it.

### Moving to an always-on server (Fedora)

Build lyra on the server itself: a binary is tied to the glibc it was built
against, so one built on a newer Fedora won't start on an older one.

```sh
# on the server
sudo dnf install -y gcc git protobuf-compiler protobuf-devel
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # current Rust (needs 1.88+)
git clone https://github.com/AxiomOperator/lyra.git ~/Projects/lyra
cd ~/Projects/lyra && cargo install --path . --locked
curl -s http://172.99.99.11:8181/v1/models >/dev/null && echo "model reachable"
```

Then move the data (it's all in `~/.lyra`; paths in it use `~`):

```sh
# on the old machine: stop lyra (and lyra serve) first
rsync -a --exclude lyra.pid ~/.lyra/ server:~/.lyra/
```

As root, install to `/usr/local/bin` instead (`cargo install --path . --locked --root /usr/local`):
SELinux doesn't let systemd run programs from root's home, and `lyra service`
then writes a system service (`/etc/systemd/system/lyra.service`) rather than a
user one.

On the server: check `config.toml` (model URLs reachable from there,
`[web] listen`, `source_repo` if the checkout lives elsewhere), then
`lyra service`, `systemctl --user daemon-reload && systemctl --user enable --now lyra`
and `sudo loginctl enable-linger $USER`. Point the Zoraxy rule at the server
(`127.0.0.1:8484` if Zoraxy runs there; otherwise set `listen` to the server's
LAN address and allow it through firewalld:
`sudo firewall-cmd --permanent --add-rich-rule='rule family=ipv4 source address=<zoraxy-ip> port port=8484 protocol=tcp accept' && sudo firewall-cmd --reload`).
Paired phones keep working without pairing again as long as `public_url`
stays the same (`web/` holds the devices and the push key).

Things that move with it: the Operator's shell and file access now act on
the server, and `ssh_run` uses the server's SSH keys.

`GET /health` answers 200 with uptime and whether lyra is busy (nothing
private) and 503 if lyra's main loop is stuck: point Zoraxy's uptime monitor
at it. Only one lyra runs per `~/.lyra`: starting the TUI while `lyra serve`
is running (say, over SSH) is refused with a note, because each would keep
its own copy of the conversation (`--force` overrides).

## Web search

lyra can look things up: **`web_search`** asks a SearXNG instance and
**`web_fetch`** reads a page as plain text (scripts, styles and menus
dropped). Both only read, so the main agent uses them directly (and the
Researcher agent has them too); replies cite the pages they used as links.

```toml
[search]
searxng_url = "http://127.0.0.1:8080"   # SearXNG with `formats: [html, json]`
max_results = 8
```

SearXNG's engines get rate-limited or CAPTCHA'd from time to time; when a
search finds nothing, lyra's result says which engines didn't answer. Enable
ones that work from your network in SearXNG's `settings.yml`.

**Stopping a reply:** `/stop`, Ctrl-X (terminal), or the stop button in the
app ends the reply being written — including an agent's work and any approval
it's waiting for. What was written so far stays, marked *(stopped)*.

## Sending files, voice and sharing (the app)

- **Attachments:** the paperclip in the app uploads files (up to 25 MB each)
  to `~/.lyra/uploads/<id>/`. Text files up to 200 KB are put into the message
  so lyra reads them; other files are described (name, type, size, upload id).
  Photos go to the model only with `vision = true` in `config.toml` (a model
  that reads images).
- **Putting a file on a machine:** ask, e.g. "put the photo I sent at
  ~/Pictures/x.png on @desktop". The Operator's `upload_place` tool copies it
  there, checked like `file_write` (asks unless the folder is in
  `write_roots`). The machine downloads the upload itself with its node token;
  node tokens can read uploads only, never the chat. The model never chooses
  the source file, only the upload id.
- **Voice:** the microphone button dictates into the message box where the
  browser supports speech recognition (Chrome/Edge on Android, Safari on iOS).
- **Share to lyra (Android):** with the app installed, lyra appears in the
  share sheet; shared text, links and files land in the message box, ready to
  send. iOS doesn't support sharing to web apps.

- **From `lyra connect`:** `/attach <path>` uploads a file from that machine. It goes with your
  next message (📎 shows in the input box's title). `/attach` lists what's attached and
  `/attach clear` drops it.

## Managing lyra from the app

More → **Memory**, **Skills**, **Goals** and **Model** are working pages, not
just read-outs. They run lyra's own commands, so they do exactly what the
terminal does:

- **Memory:** search (with match scores), filter by scope, correct a memory
  (the old version is kept), archive or forget it, approve or reject the
  curator's suggestions.
- **Skills:** approve or reject proposed skills and changes, stop using a
  skill, or use a deprecated one again.
- **Goals:** add a goal, set its priority, pause/resume, mark it done or
  cancel it.
- **Model:** the models the endpoint offers (`GET /models`). Switching applies
  to every conversation and is saved to `config.toml` (`/model <name>` does the
  same in a terminal).
- **Machines → Rules:** what lyra may do without asking on a machine (or on
  the server): commands that run without asking, folders it may write in,
  paths that are off limits, SSH hosts, timeouts. A machine checks the new
  rules itself and saves them to its `node.toml`. The server's go to
  `[system]` in `config.toml`. Both apply at once. Rules that would open
  everything (`/` as a write root, allowing `sudo` or `*`) are refused. Only a
  person changes rules, from a paired device; the model has no tool for it.
  In a terminal: `/machines rules <name|server>` shows them, and e.g.
  `/machines rules desktop allow add git pull` or `… write remove ~/tmp` or
  `… off` changes them, with the same checks.

Page answers go back to the page, not into the conversation. Changes are
logged in the Activity panel.

## Other machines and terminals: `lyra-node` and `lyra connect`

With lyra on a server, other machines join it in two ways.

### Machines lyra works on: `lyra-node`

`lyra-node` lets lyra's Operator work on a machine: shell, files, system info.
It connects *out* to the server (no open ports, no SSH) and is a small static
program (x86_64 Linux, any distribution) that the server hands out, so a
headless box needs nothing but `curl`:

```sh
curl -fsSL https://lyra.example.com/install.sh | sh -s -- --name web1
```

The installer downloads `lyra-node`, checks it against the server's checksum,
installs it (root: `/usr/local/bin`, else `~/.local/bin`), asks lyra to pair
and starts its service (a system service as root, a user service otherwise).
**Headless pairing** needs no code typed in: the machine shows a short code,
and you approve the request in lyra — the card in the web app (Chat, Machines
or Devices), a phone notification, or `/devices approve <code>` in a lyra
terminal. Check the code matches what the machine shows. With a code from
`lyra pair` it works too: `lyra-node pair <url> <code> --name web1`.

Then, from any device:

- **`@web1 …`** in a message sends the work there: the Operator handles it, and
  its tools default to that machine. Typing `@` opens the list of machines
  online (and `@server`), like `/` does for commands.
- **`/machines`** lists them: online or not, host and OS, version, and whether
  the server has a newer `lyra-node`. **`/machines update <name|all>`** has the
  machine download the server's build, verify it and restart into it;
  **`/machines remove <name>`** has it uninstall itself (service, settings,
  program) and unpairs it. Both are buttons on the web app's Machines page.

Everything is decided **on that machine**, with its own rules in
`~/.config/lyra/node.toml` (`[system]`: `allow_commands`, `write_roots`,
`deny_paths`, timeouts): reading runs at once; changes wait for your approval
(the card says "write a file on web1"); forbidden things and `~/.ssh`,
`~/.gnupg` and other credentials are refused there even when "approved". A
node's token can only lend tools: it can't chat or approve. When a machine is
offline, lyra says so instead of doing the work somewhere else.

The server hands out the `lyra-node` that sits next to its `lyra` (or
`[web] node_binary`). Build it static and put it there whenever lyra is
updated (then `/machines update all`):

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --release -p lyra-node --target x86_64-unknown-linux-musl
install -m 755 target/x86_64-unknown-linux-musl/release/lyra-node /usr/local/bin/lyra-node
```

(`.cargo/config.toml` points the musl build at the system `gcc`.) `lyra node`
inside the full `lyra` does the same job but can't update itself.

### Several machines at once: `@all` and groups

`@all` means the server and every machine that's online. A group is a name for some of them:

```toml
[groups]
web = ["web1", "web2"]
lab = ["nas", "desktop"]
```

- **Running:** "update packages on @web" or "disk usage on @all" goes to the Operator, which
  uses `fleet_run`: the same command on every machine at once.
- **Approval:** each machine checks the command against its own rules. The ones that need a yes
  are combined into one approval ("run a command on web1, web2"), machines that allow the
  command just run it, and anything a machine forbids is refused there.
- **Results:** one per machine, offline ones included. The app shows a card per machine (✓/✗,
  exit code, output), and the terminals show a line per machine.
- **Suggestions:** `@all` and your groups appear when you type `@`.

### Machine health and alerts

Every machine reports its health every 5 minutes, and `lyra serve` checks the server the same
way. A report covers:
- disks (percent used)
- memory
- load
- failed systemd units
- pending package updates, from dnf's or apt's cache, checked every 6 hours

When something crosses a limit, lyra logs it in Activity and sends a push notification:
- a disk 90% full
- memory 95% used
- a 15-minute load over 2 per CPU
- a failed unit
- a machine offline for 10 minutes

It tells you once, and again when the problem clears or the machine is back.

```toml
[health]
disk_percent = 90
memory_percent = 95
load_per_cpu = 2.0
failed_units = true
offline_minutes = 10   # 0: don't report quiet machines
notify = true          # push; Activity logs them either way
# enabled = true
```

Where it shows:
- **App:** the Machines page shows each machine's health as badges (red when over a limit),
  including the server's own card. The Machines tab counts machines with a problem.
- **Terminal:** `/machines` adds a health line per machine, and `/machines health [name|server]`
  gives the full report.
- **`lyra connect`:** the side panel lists problems under each machine.

### Windows machines

lyra-node also runs on 64-bit Windows, as a service. In PowerShell **as administrator**:

```powershell
irm https://lyra.example.com/install.ps1 | iex
# or with a name: & ([scriptblock]::Create((irm https://lyra.example.com/install.ps1))) -Name office-pc
```

**What the installer does:**
- downloads `lyra-node.exe` and checks it against the server's checksum
- installs it to `C:\Program Files\lyra`, with settings in `C:\ProgramData\lyra\node.toml`
  (readable by SYSTEM and Administrators only)
- asks lyra to pair: approve the request in the app, which shows the code to compare
- starts it as the **lyra node** service (automatic, runs at boot as LocalSystem). The
  Machines page's "Add a machine" card has a Windows tab with this command.

**Commands on Windows** are PowerShell (`pwsh` when installed, else Windows PowerShell), judged
by the same rules as on Linux:
- *Run at once (read-only):* Get-, Test-, Select-, Measure- and other reading verbs, and tools
  like ipconfig, systeminfo, tasklist, netstat, `ping -n`, `git status`.
- *Ask first (changes):* Set-, New-, Copy-, Start-, Restart- and anything unknown.
- *Warn (dangerous):* Remove-Item, Stop-Process, taskkill, `reg delete`, Restart-Computer,
  Invoke-Expression.
- *Never run:* wiping a drive, `C:\Windows`, the user profiles or Program Files.

**Off limits by default:** credentials, browser profiles and lyra's own settings.

**Same as Linux nodes:**
- **Health:** disks, memory, CPU, uptime, automatic services that are stopped, and pending
  Windows updates.
- **Management:** `/machines update` replaces the program, and the service's recovery setting
  restarts it. `/machines remove` deletes the service, its settings and the program.

**One limit:** as a LocalSystem service it doesn't see coding agents (Claude Code, OpenCode)
that are logged in under your own account.

### Terminals: `lyra connect`

`lyra connect` is the terminal UI for the server: the same chat, Markdown,
`/` and `@` palettes and approval box (y / n / a), streaming live, with a side
panel (Ctrl-B) of the devices and machines online and machines waiting to
pair. Pair it once with a code from `lyra pair`:
`lyra connect --pair <code> --url https://lyra.example.com`. After that, plain
`lyra` opens it on a machine with no lyra of its own. Nothing is stored
locally but the token (`~/.config/lyra/remote.toml`, 0600). An open terminal
counts as watching, so phones aren't notified meanwhile.

**On the server itself**, `lyra` while `lyra serve` runs opens the same
terminal UI connected to it (with its own terminal device, kept in
`~/.lyra/web/terminal.toml`), instead of starting a second lyra.

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
cargo install --path .   # installs the `lyra` command into ~/.cargo/bin (run again after updating)
lyra
```

Or straight from the checkout without installing: `cargo run -- [options]`.

Every conversation is saved in `~/.lyra/sessions/` after each reply and when
you quit, so you can pick it up again:

```sh
lyra -c              # continue the latest conversation started in this folder (else the latest)
lyra -r              # list saved conversations
lyra -r 20261005-1234  # resume one (any unique part of its id)
lyra --help
```

Inside lyra, `/sessions` lists them and `/resume <id>` switches to one (the
current one is saved first). Resuming brings back the whole chat, tool calls
included, and the model sees it as before.

Building needs `protoc`, the Protocol Buffers compiler, for LanceDB
(`dnf install protobuf-compiler` / `apt install protobuf-compiler`, or a
release from github.com/protocolbuffers/protobuf on your `PATH`).

Keys: type, `Enter` to send, `/` to open the command palette (it narrows as you type; `↑`/`↓` pick, `Tab` or `Enter` fills the command in, `Esc` closes it), `↑`/`↓`/`PgUp`/`PgDn` to scroll history, `Ctrl-R` to show/hide model reasoning, `Ctrl-B` to show/hide the side panels, `Ctrl-L` to reload the context files and config, `Esc` / `Ctrl-C` to quit.

## Layout

The chat sits on the left. Replies are rendered as Markdown: headings,
bold/italic/strikethrough, inline code and fenced code blocks (with their
language), nested and numbered lists, task lists, quotes, links (with their
address), rules and aligned tables, also while a reply is still streaming; on terminals at least 100 columns wide, panels on the right show:

- **Session**: what the assistant is doing right now (idle / waiting / thinking / streaming / running a tool, with a timer), model and server, replies, last reply's TTFT and speed, tokens, cache hit rate and cost.
- **Agent**: system prompt size, loaded SOUL/USER/AGENT files, capabilities by kind, how many are offered per message and any that are degraded or unavailable, and embedding/reranker health.
- **Memory**: active memories by kind, vector coverage, the store (backend, size, search latency; red when operations fail), the current project, what needs approval (proposals, contradictions, duplicates, expired), working memory (goal, plan, notes, the last tool result), how many memories the last reply used, and the most recent memories (`?` marks unsure ones).
- **Skills**: learning mode, average reliability, what needs review (proposals, conflicts, duplicates, failing or stale skills), the last reply's skills, and each active skill's reliability and use count.
- **Agents**: the main agent and every subagent: who is working right now (↪, highlighted), how many delegations each has had and how many were corrected, which ones only work when asked, and the agent wizard's state. While a specialist works, the Session state shows `↪ Writer` (and its tool), and the chat shows the hand-off and its result.
- **Goals**: open goals by priority with progress bars (subgoals indented, blocked ones with their reason), the autonomy mode and the current session's spending, or why it stopped.
- **Plan**: the current plan's goal, steps with their status (✓ ▸ ⏸ ✗ ○, ⚠ for approval), budget use and any note.
- **Evolution**: the generation and mode, runs recorded, success rate and corrections, calls per run, what has evolved (guidelines, workflows, composite tools, changed settings), candidates waiting for review, the last review, and what evolution is doing right now.
- **Activity**: a timestamped log of requests, first tokens, tool calls and results, memory, skill, plan and evolution events, reloads and errors.

When the panels are hidden or don't fit, a one-line status bar shows the session totals instead.

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

lyra has a persistent notebook (V1): facts in a single SQLite file with FTS5
keyword search, at `~/.lyra/memory/memory.db` by default. The model gets
four tools and decides when to use them:

| Tool | Does |
|---|---|
| `memory_remember` | save one fact: `content`, optional `scope`, `tags`, `source` |
| `memory_recall` | keyword search (words OR-ed, ranked by bm25, stemmed), optional `scope`, `limit` |
| `memory_forget` | delete by `id` |
| `memory_list` | most recent first, optional `scope`, `limit` |

Scopes keep memories apart: `user`, `agent`, `project:<name>`, ... Recall and list
search all scopes unless one is given. Tool calls and their results show as dim
`→` / `↳` lines in the chat.

The store lives in the `memory/` crate (`lyra-memory`): `MemoryManager` over a
`MemoryStore` trait, with `SqliteStore` as the one backend. lyra only talks to
`MemoryManager`, so the backend can be swapped later.

```toml
[memory]
enabled = true            # needs a server with tool calling
path = "~/notes/memory.db"
default_scope = "user"
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
- **Memory**: total memories, counts per scope and the most recent ones; updates as the model uses its memory tools.
- **Skills**: learning mode, average reliability, what needs review (proposals, conflicts, duplicates, failing or stale skills), the last reply's skills, and each active skill's reliability and use count.
- **Activity**: a timestamped log of requests, first tokens, tool calls and results, reloads and errors.

When the panels are hidden or don't fit, a one-line status bar shows the session totals instead.

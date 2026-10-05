# lyra

A dead simple terminal chat client for a local LLM.

Talks to any OpenAI-compatible `/chat/completions` endpoint (Ollama, llama.cpp server, LM Studio, vLLM, ...).

## Files

Everything lyra keeps lives in one folder, `~/.lyra` (set `LYRA_HOME` to use another):

```text
~/.lyra/
├── config/   config.toml, SOUL.md, USER.md, AGENT.md
├── memory/   memory.db
└── skills/   skills.db
```

Upgrading from an older version moves things here automatically: on first run,
files from `~/.config/lyra` and `~/.local/share/lyra/data` are copied in (the
originals are left in place, so delete them once you're happy).

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

Each is looked up in `~/.lyra/config/`, then in every directory from `/` down to
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

Memory holds facts; **skills** hold procedures lyra learned. They live in their
own crate (`learning/`, `lyra-learning`) and database (`~/.lyra/skills/skills.db`).

1. **Spot a lesson.** After each reply a cheap check looks for a reason to learn:
   you corrected the assistant, asked it to remember how something was done
   ("next time…", "remember how…"), a failed tool step was followed by a working
   one, or a multi-step tool run succeeded. Ordinary turns are skipped.
2. **Review it.** When the check fires, lyra asks the chat model in the background
   whether the conversation taught a reusable procedure. It must not learn facts
   about you (that's memory), one-offs, failures or anything resembling a
   credential; lessons below `min_confidence` are dropped.
3. **Approve it.** In `propose` mode (the default) a lesson becomes a *proposed*
   skill and shows up in the chat and the Skills panel. `/approve <id>` makes it
   active; `/reject <id>` discards it for good.
4. **Use it.** Before each message, active skills matching it (keyword search)
   are added to the system prompt; the Activity panel logs `applying skills: …`.

Commands: `/skills`, `/approve <id>`, `/reject <id>`, `/forget-skill <id>`,
`/learn` (review the conversation now), `/help`.

```toml
[learning]
mode = "propose"      # off | propose | auto
min_confidence = 0.6
max_skills = 3
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
- **Skills**: learning mode, skills waiting for review and active skills.
- **Activity**: a timestamped log of requests, first tokens, tool calls and results, reloads and errors.

When the panels are hidden or don't fit, a one-line status bar shows the session totals instead.

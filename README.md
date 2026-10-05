# lyra

A dead simple terminal chat client for a local LLM.

Talks to any OpenAI-compatible `/chat/completions` endpoint (Ollama, llama.cpp server, LM Studio, vLLM, ...).

## Config

`~/.config/lyra/config.toml` (or `$XDG_CONFIG_HOME/lyra/config.toml`):

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

Each is looked up in `~/.config/lyra/`, then in every directory from `/` down to
the one lyra is started in. So a project can override the global `SOUL.md`, and
a project `AGENT.md` adds to the global one. The first line in the chat shows
which files were loaded. Templates are in `examples/`.

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

Keys: type, `Enter` to send, `↑`/`↓`/`PgUp`/`PgDn` to scroll history, `Ctrl-R` to show/hide model reasoning, `Ctrl-L` to reload the context files and config, `Esc` / `Ctrl-C` to quit.

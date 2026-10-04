# lyra

A dead simple terminal chat client for a local LLM.

Talks to any OpenAI-compatible `/chat/completions` endpoint (Ollama, llama.cpp server, LM Studio, vLLM, ...).

## Config

`~/.config/lyra/config.toml` (or `$XDG_CONFIG_HOME/lyra/config.toml`):

```toml
url = "http://localhost:8080/v1"
model = "qwen3"
```

Both keys are optional; defaults are `http://localhost:11434/v1` and `llama3.2`.
`LYRA_URL` / `LYRA_MODEL` env vars override the file. See `config.example.toml`.

## Run

```sh
cargo run
```

Keys: type, `Enter` to send, `↑`/`↓`/`PgUp`/`PgDn` to scroll history, `Ctrl-R` to show/hide model reasoning, `Esc` / `Ctrl-C` to quit.

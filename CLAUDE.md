# lyra

Rust TUI chat client for local OpenAI-compatible LLMs, built up step by step.

## Layout

- `src/` — the TUI binary: `main.rs` (app state, streaming, tool loop), `ui.rs` (all drawing),
  `config.rs`, `context.rs` (SOUL/USER/AGENT.md), `tools.rs` (memory tools), `learn.rs`
  (self-learning glue), `retrieval.rs` (embedding/reranker clients), `stats.rs`.
- `memory/` — `lyra-memory` crate: SQLite + FTS5 notebook behind `MemoryManager`.
- `learning/` — `lyra-learning` crate: skills behind `SkillManager` (Markdown files + SQLite ledger
  of versions, usage, relationships, proposals, audit log). Design: `docs/skill_learning.md`.
- `docs/` — design notes.
- Runtime files live in `~/.lyra` (`$LYRA_HOME`): `config/config.toml`, `context/`
  (SOUL/USER/AGENT.md), `memory/memory.db`, `skills/<name>.md` (one Markdown file per skill) + `skills/ledger.db`.
  `src/migrate.rs` brings older layouts up to date on startup.

## Checks

```sh
cargo clippy --all-targets --workspace   # must be warning-free
cargo test --workspace
```

## Rules

1. **Commit and push after every completed feature or fix.** Once the checks pass, commit
   straight to `main` and `git push origin main`. Do not open a pull request.
2. **After every feature, review the TUI panels.** Decide whether what was added should show
   up in the side panels (Session, Agent, Memory, Skills, Activity in `src/ui.rs`) or be
   logged to the Activity panel, and update them when it makes sense. Say in the summary
   what was changed in the UI, or why nothing needed to be.

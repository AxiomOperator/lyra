# lyra

Rust TUI chat client for local OpenAI-compatible LLMs, built up step by step.

## Layout

- `src/` — the TUI binary: `main.rs` (app state, streaming, tool loop), `ui.rs` (all drawing),
  `config.rs`, `context.rs` (SOUL/USER/AGENT.md), `tools.rs` (memory tools), `mem.rs` (memory glue), `learn.rs`
  (self-learning glue), `retrieval.rs` (embedding/reranker clients), `stats.rs`.
- `memory/` — `lyra-memory` crate: memory behind `MemoryManager` (SQLite + FTS5, vectors in a table;
  kinds, provenance, supersede/versions, hybrid ranking, context compiler, capture, curator,
  safety scan, working memory). `src/mem.rs` is lyra's side (embeddings, model calls, /memory).
- `learning/` — `lyra-learning` crate: skills behind `SkillManager` (Markdown files + SQLite ledger
  of versions, usage, relationships, proposals, audit log). Design: `docs/skill_learning.md`.
- `docs/` — design guides and examples (not binding; see rule 3).
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
3. **`docs/` files are guides and examples, not instructions to change what's established.**
   Use them for ideas, features and structure, but adapt them to the project as it is. If a
   doc assumes something different from an established decision (e.g. it says skills live in
   SQLite, but skills are Markdown files), keep the established decision and fit the doc's
   idea around it; never undo or migrate away from it because a doc says so. Established
   decisions are what the code and this file already do, for example:
   - everything lyra keeps lives in `~/.lyra` (`config/`, `context/`, `memory/`, `skills/`)
   - skills are Markdown files, one per skill; their history/evidence is in the ledger
   - memory is SQLite + FTS5 behind `MemoryManager`
   Only the user changes an established decision. If a doc's approach seems clearly better,
   say so and ask; don't switch on your own.

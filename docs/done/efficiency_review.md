# Efficiency review

Where lyra spends time, model calls and resources, and what to change. This
draws on production's usage log for October (2026-10-12) and three read-only
passes through the code: each chat turn, the always-on server loop, and the
app. This is a guide (CLAUDE.md rule 3): each fix keeps lyra's rules.

## 1. What production shows

| | Model hours (Oct) | Calls | Notes |
|---|---|---|---|
| lyra's own background work | **8.0** | 280 | reviews, capture, page summaries, checks; avg 80–167 s each |
| Agents (delegations) | 3.1 | 188 | |
| Chat replies | 4.1 | 313 | avg prompt **~30k tokens** |
| Decision / embedding / reranker | 0.6 | 24k | about **4,300 a day** are the Status check |

- **lyra's own work costs twice what answering does.** Today alone it was 6.3
  model-hours.
- **Page summaries waited minutes:** they took 7–10 minutes each while the
  GPUs were busy. That one is fixed: 0.80.2.237 waits 45 s at most.
- **The cache is lost between turns.** The prompt cache serves about 70% of
  input within a turn (the main model and gemma), but only 25% on the first
  fallback. Across turns, the whole prompt is read again, because its opening
  part changes every turn (see 2.1).
- **Even idle, there's a probe every minute:** embedding, reranker and
  decision-model calls, all from the Status check.

## 2. Findings, ranked

### Tier A: biggest wins (latency people feel)

**A1. Keep the start of the prompt the same from turn to turn (prompt cache).**
- **What happens:** the system message (`messages[0]`) collects what changes
  every turn: memories, skills, working memory ("recently mentioned" updates
  on each message), goals with their progress, the workflow picked for this
  message, avoid notes, and the chat gate's note (`src/turn.rs:153-230`,
  `add_to_system`).
- **Tools too:** the offered tool set is picked per request
  (`caps.definitions(request)`), and Qwen's template puts tools first.
- **Cost:** llama-server reuses only the unchanged start of a prompt, so each
  turn re-reads the whole conversation (25–30k tokens). That's the 5–70 s
  before the first word.
- **Fix:**
  - Keep `messages[0]` byte-stable: SOUL, AGENT, USER, the rules.
  - Put the changing context in a block right before the latest user message
    (not saved in the conversation).
  - Keep each conversation's tool set stable, only ever added to.
  - Send `cache_prompt: true` and pin `id_slot` per conversation.
- **Expected:** first token in seconds instead of tens of seconds on long
  conversations, and far less GPU time per turn.

**A2. A small model for lyra's own work, and nothing of it on the chat model while a reply runs.**
- **What happens:** judging the last reply runs at the same moment as the new
  one (`turn.rs:70` → `judge_last_run`). After each reply, the review gate,
  review, capture gate, capture, credit and avoid notes all run on the 27B
  models (`complete_light` → fallback chain).
- **Cost:**
  - They compete with replies.
  - On a single slot, each one evicts the conversation's cached prompt (A1).
  - They're 8 of this month's 16 model-hours.
- **Fix:**
  - Add a `[light_model]` (about 4B) tried first for light jobs.
  - Queue light jobs while a reply runs, and start them when it's done.
  - Judge after the reply, not alongside it.
  - Merge the review and capture gates into one decision call.

**A3. Fewer blocking calls before the reply starts, and run the rest side by side.**
- **What happens:** before the first token, these run one after another
  (`turn.rs:153-231`): compaction (a model call), vision, memory, skills, the
  chat gate (`decide::yes`, up to 15 s), and agent routing.
- **Routing's last resort:** with `model_fallback = true`, routing can make a
  full 27B call (`agents.rs:~289`).
- **Fix:**
  - Run memory, skills, the gate and routing in parallel (`thread::scope`).
  - Compact in the background after the previous reply.
  - Make routing's model fallback a light job, or turn it off by default.

**A4. Embed the message once per turn.**
- **What happens:** the same text is embedded for memory, skills, routing and
  capability discovery. Discovery does it again on every tool round, with its
  full-text search and scoring each time.
- **Fix:** one embedding per turn, passed along (or a small cache in the
  shared embedder), and tool definitions computed once per turn unless `found`
  changes.

### Tier B: the app (smoothness while replies stream)

**B1. Every streamed token re-renders the whole app.**
- **What happens:** each `append` makes a new `messages` array, and the context
  value spreads `...state` (`store.tsx:59-66, 353-356`). Shell, sidebar,
  composer, every message and every open data page re-render.
- **Fix:** split a stable "actions" context from the state, and read state
  with selectors (`useSyncExternalStore`).

**B2. Messages aren't memoized, and every tool result is re-parsed per token.**
- **What happens:** `MessageView` and `ToolCallView` re-run for every message
  (`chat.tsx:180, 683-734`). They `JSON.parse` every tool call's arguments and
  result, and `foldFailures` parses them again.
- **Cost:** the work grows with conversation length × tokens. It's the jank in
  long chats with tools.
- **Fix:**
  - `React.memo` with per-message props.
  - Parse results once (cached by string).
  - Rebuild results and folds only when a tool message arrives.

**B3. Batch token updates per animation frame.**
- **What happens:** one React commit per websocket frame (`store.tsx:202`),
  which can be 50+ a second.
- **Fix:** collect `append`s and flush on `requestAnimationFrame`.

**B4. Smaller app items:**
- **Outbox:** its effect runs per token (`store.tsx:328-350`).
- **Code highlighting:** a streaming code block is re-highlighted on every
  token (`highlight.ts:83-96`). Throttle it, and replace the cache's
  "clear all at 500" with LRU.
- **Main bundle:** 1.05 MB (324 KB gzipped). Lazy-load Coding, What's new and
  Everything search; split the markdown and Radix vendor chunks.
- **Service worker:** assets go network-first and are re-cached on every load
  (`public/sw.js:58-69`). Make them cache-first.

### Tier C: the server's own load (always on, even idle)

**C1. The Status check runs real inference every minute** (`status.rs:243`).
- **What happens:** an embedding, a reranking and a decision-model question,
  plus a model list per chat model. That's about 4,300 usage-log lines a day.
- **Fix:**
  - Probe `/health` or `/v1/models` instead.
  - Skip a probe when the model served a real call recently.
  - Don't record probes as usage.

**C2. A full status blob goes to each device every second while a reply runs.**
- **What happens:** `Mirror::updates` (`serve/mod.rs:327`) rebuilds status plus
  the whole Status board, machines, routines, briefing and PMI, and sends all
  of it again when anything differs. The phase timer ("thinking 4s") changes
  every second. All conversations rebuild every minute even with no device
  attached.
- **Fix:**
  - Channels with revisions (status, board, routines, briefing, pmi,
    machines), each sent only when changed.
  - Let the app count the phase timer up itself.
  - Skip conversations nobody is looking at.

**C3. The loop wakes 25 times a second and deep-clones the board each time** (`loop.rs:107, 507-518`).
- **Fix:**
  - Keep it in an `Arc` with a revision.
  - Build it only when something changed.
  - Sleep until the next timer instead of polling every 40 ms.

**C4. Files re-read constantly.** Each of these should be cached in memory and
reloaded only when written (an mtime/revision cache in `JsonStore` would cover
most of them):
- `feedback.json` and `known_down.json`: read on every status update.
- Routines and their runs: every 20 s.
- `users.json` and `devices.json`: every 10 s, on every websocket message, and
  rewritten on each connect.
- Diagnoses: twice every 5 s.

**C5. The Status worker's SQLite work.**
- **What happens:** about 120 queries a minute, including 7-day `SUM`s over
  128k samples. It also re-parses the config and forks `df`.
- **Fix:** running counters or an hourly rollup, one sparkline query, and read
  the config only when it changes.

**C6. Usage, Sessions and Search pages re-read everything.**
- **What happens:** the whole month's usage log is parsed, and every session
  file is parsed in full, on each visit. The pattern offer (4.5) also parses
  every session after each reply.
- **Fix:**
  - Cache monthly usage aggregates, and parse only what's appended.
  - Keep a small session index (id, owner, title, updated, kind) updated on
    save.

**C7. Smaller server items:**
- **Tool results:** `fit()` re-measures the whole history for each tool result
  it checks (quadratic).
- **Saving:** `save_session` serializes the whole conversation, images
  included, on the main loop several times per turn. Move it to a coalescing
  writer thread.
- **Vision:** with it on, every past image is re-sent each turn.
- **Goals:** `goals_tick` does synchronous SQLite work on the loop every 60 s.

### Done (2026-10-12)
- **A1 + A2** (0.82.0.240): the prompt's start stays the same, the tool list
  only grows at its end, `[light_model]`, and background work waits for
  replies.
- **C1–C3** (0.83.0.241): Status probes ask for the model list; the status
  goes as patches of what changed; the timer counts on the device; the loop
  rests when idle and skips conversations nobody shows.
- **B1–B3** (0.84.0.242): messages in their own context, memoized message
  views, a stable tool-results map, updates batched per frame, tool JSON
  parsed once (and the outbox no longer reacts to each word).
- **A3 (part), A4, C4, C6** (0.85.0.243): memories, skills and the chat gate
  run side by side; a query is embedded once (10-minute cache in
  `retrieval::embed`); files read again only once changed
  (`jsonfile::cached`: devices, users, feedback, diagnoses, known down,
  routine runs, sessions); the usage log read incrementally.

- **The rest** (0.92.0.255):
  - **A2:** the two checks after a reply ("a lesson here?" and "worth
    remembering?") are one decision call, and one fallback call when it's
    unsure (`decide::gate_both`).
  - **A3:** a long conversation is compacted in the background after a reply
    (a light job), not when the next message waits. Routing's model fallback
    is a short light call.
  - **A4:** the tool list is worked out once per turn, and again only when the
    search finds more.
  - **B4:**
    - a streaming code block is highlighted at most every 150 ms, with an LRU
      cache;
    - What's new, Coding and the Everything search load on demand;
    - libraries are in chunks of their own (main bundle 1.08 MB → 252 KB);
    - built files come from the cache, kept across versions.
  - **C5:** the Status worker uses a few grouped queries a pass instead of
    four per check; the 7-day uptime adds only new samples to an hourly base.
    `df` runs at most every 10 minutes, and `[speech]` is read only when
    config.toml changed.
  - **C7:**
    - `fit()` measures once;
    - conversations are written by a coalescing writer thread (flushed on
      stop);
    - pictures are re-sent only with the last two exchanges;
    - the goal loop's look runs off the server loop.

### Already fixed today
- **Searching in circles:** near-repeat searches give back the earlier
  results, and there are 8 searches at most per reply (0.78.2.233).
- **Page summaries:** waited for 45 s at most, then the start of the page is
  used (0.80.2.237).

### Outside lyra (no code)
- **A draft model** for speculative decoding (Qwen3 0.6B/1.7B with `-md`) next
  to the 27B: often 2–3× faster replies, with identical output.

## 3. Recommended order

1. **A1 + A2** (prompt cache, light model, queued background jobs): the largest
   gains in how fast replies feel, and in GPU hours.
2. **C1 + C2 + C3**: the server stops working when nobody's there, and sends a
   fraction of the bytes.
3. **B1 + B2 + B3**: smooth streaming in long chats.
4. **A3 + A4**, then C4–C7 and B4 as time allows.

# Self-learning, self-evolution and self-advancing: a deep dive

> **Status (2026-10-12): done.** Phases 1–4 shipped in 0.74–0.78. Phase 5 (the code loop)
> shipped in 0.94.0.257:
> - **5.1:** a code candidate is a repro test plus a fix. The test must fail at the base
>   commit, and then pass with the fix, with clippy and every test passing too
>   (`lab::validate_repro`, `branch_repro`).
> - **5.2:** the Proposals page shows both diffs and every sandbox step, and lists approved
>   branches with whether each is still waiting to be merged.
>
> Merging stays a person's action.

A review of every loop by which lyra gets better on its own (skills, evolution, memory, agents,
goals), checked against what the production server has actually done (data read on 2026-10-11),
with proposed upgrades. This is a guide (CLAUDE.md rule 3), not a decision: each upgrade keeps
lyra's rules (approvals in Rust, members' limits, nothing off the host, secrets never shown).

## 1. The one-line finding

**The loops are built, but they are starved and switched to manual.** Nearly every loop learns from
one signal: a keyword match on the user's *next* message (`learning/src/evaluator.rs`
`outcome_signal`). In production, that signal has never fired usefully:

| What | Rows | With a user-judged outcome |
|---|---|---|
| Skill uses (`ledger.db` `skill_usage`) | 129 | **0** (all `unknown`) |
| Delegations (`agents.db`) | 52 (17 failed) | **0** (outcome NULL) |
| Evolution runs (`evolution.db`) | 71 | **0** from the user (3 failures were lyra's own diagnosis runs) |
| Evolution candidates / events | 0 / 0 | n/a. Evolution has never proposed anything |
| Goals, plans | 0 | n/a |
| Memory feedback | ~1 "marked helpful" line in 6 days | n/a |

Every review is manual too: evolution `review = "manual"`, skill curation `curate = "manual"`,
memory curation `curate = "manual"`, and goals autonomy `reactive`. The only loop that has produced
anything is **skill capture**: 10 skills created, 4 approved, 4 rejected, 1 updated. Even those 4
active skills (129 uses between them) can never be promoted or deprecated, because no use has an
outcome.

So the highest-value work isn't new loops. It's **giving the existing loops a real signal, then
letting them run.**

## 2. How each loop works today

### Skills (`learning/`, `src/learn.rs`)
- **Storage:** one Markdown file per skill, with a header. The SQLite ledger holds versions, usage,
  relationships, proposals and events.
- **When a review runs:**
  - The user asks explicitly, or corrects lyra.
  - A tool errors and a later one succeeds.
  - A turn makes 3 or more clean tool calls.
  - Otherwise, the decision model's "is there a lesson here?" gate decides.
  - The reviewer runs on the fallback model, with `min_confidence` 0.6.
- **How a skill is picked:**
  - Matching is IDF over keywords, normalised so the top hit is 1.0, with no floor.
  - Score: 0.50 relevance + 0.25 reliability + 0.15 confidence + 0.10 freshness.
  - Up to 3 skills are injected, with no token budget.
- **Outcomes:** the next message's keyword signal is credited to every injected skill. `/outcome`
  also sets one.
- **Lifecycle:**
  - Promote at confidence ≥ 0.85 with 2 or more successes and no failures.
  - Deprecate below 0.4 reliability after 5 or more outcomes.
  - "Stale" after 90 days, which is only reported.

### Evolution (`evolution/`, `src/evolve.rs`)
- **Telemetry:** a `RunRecord` per chat turn (owner only) and per plan. A chat outcome stays
  `Unknown` unless the stream errors or the keyword signal fires.
- **Detectors**, over a window of 50 runs:
  - inefficient runs, repeated errors, frequent corrections, repeated tool chains;
  - failing skills, plans that need rework, failing or slow capabilities;
  - stuck goals and struggling agents (both with empty evidence).
- **Evolver:** proposes 1–3 candidates per problem: prompt guidelines, knobs, workflows,
  composite tools, skills, agents. APIs come from `/evolve api`, and code from `/evolve code` in
  the git-worktree lab, where approval only makes a local branch.
- **Benchmark:**
  - Replays 3 recent or evidence tasks (2 for plans). Read-only tools run; writes are faked.
  - A judge scores the run: the decision model if it's confident, otherwise the same chat model.
  - Fitness = 0.40 success + 0.25 accuracy + 0.15 efficiency + 0.10 reliability + 0.10 safety.
  - A candidate passes if fitness rises by at least 0.01.
- **Generations:** each deploy snapshots behavior, workflows and tools, and rollback is a new
  generation. The monitor compares the current generation with its parent over 10 or more runs.
  Auto mode rolls back a regression; propose mode warns.

### Memory (`memory/`, `src/mem.rs`)
- **When it captures:**
  - after 2 or more tool calls;
  - after a "durable" phrase ("my …", "I use …", "always …");
  - otherwise when the decision model says yes. That model has been reported down in production
    ("501"), so this path is likely dead there.
- **How it ranks:** hybrid. Helpful and unhelpful counts move a memory's rank by at most ±0.2,
  and only through the keyword signal.
- **Curator:** finds duplicates by word overlap, stale memories, and contradictions (judged by the
  model). It proposes; it doesn't apply. It runs manually.

### Agents (`agents/`, `src/agents.rs`)
- **Routing:** rules, then similarity, then the model. The examples change only by hand edits, so
  outcomes never teach routing.
- **On a failed delegation:** a skill review runs. A success reinforces nothing.
- **Agent-instruction evolution:** it reads delegation outcomes, which are always NULL, so it has
  nothing to work from.

### Goals (`goals/`, `src/goals.rs`)
- `goals_tick` fires triggers (`env:`, `file:`, `capability:`), lifts blockers, resumes plans, and
  in assisted or autonomous mode does the next piece of work.
- Production is `reactive`, and members are always reactive.
- Goals are never inferred from conversations.

### What learns without the keyword signal
- `style.rs` learns from sent mail and corrections.
- Skill capture triggers from tool errors and successes.

These are the two loops that actually work, which is telling.

## 3. Weaknesses, with evidence

### A. Signal (the root cause)
1. **Keyword outcomes are both rare and wrong.**
   - The signal reads only the very next message, so most messages match nothing.
   - It matches bare substrings: "great**er** than", "use X instead" and "what should be the name"
     all count.
   - Correction is checked before praise, so "thanks, now do Y instead" counts as a failure.
2. **No way to rate a reply in the app.** `/outcome` exists only as a command. The web Feedback
   pages are a bug tracker.
3. **Only the owner's turns are judged** (`main.rs:1229`, `1803`). Every other person's
   conversation teaches nothing.
4. **Signals are lost:**
   - `last_run` is cleared on resend, resume and `/new`.
   - Hitting the tool-round limit isn't recorded (`Limit` arrives after `Done`).
   - Stopping a reply, regenerating it, "Keep going", rephrasing and abandoning a conversation are
     all ignored.
5. **Credit goes to everything at once:** every injected skill gets the same outcome, and a
   Failure is also counted as a correction, so the two metrics double-count.

### B. Measurement
6. **The benchmark is noise-sized.**
   - With 3 tasks, success moves in steps of 0.33, yet a candidate passes on +0.01.
   - Each task runs once, with no history (follow-ups like "now the same for Go" lose their
     meaning).
   - The expectation is generic, with no reference answer.
7. **Self-judging.** The agent under test, the evolver and the fallback judge are often the same
   model. There's no pairwise or blind grading.
8. **The benchmark tests a different agent.** It leaves out memory recall, skill injection and
   capability discovery.
9. **Goodhart through efficiency:** "same success, fewer tokens" counts as an improvement, so
   "be brief" guidelines can win on cost alone.
10. **The monitor is weak:**
    - It compares absolute rates across different periods and task mixes, with n = 10 and a 0.15
      threshold.
    - It skips the check when the parent generation has fewer than 10 runs, and looks only one
      generation back.

### C. Process
11. **Everything is manual, and propose mode doesn't even test its candidates.**
12. **No memory across reviews:**
    - The same problem is re-proposed every review, and rejected or rolled-back attempts aren't
      shown to the evolver.
    - In auto mode this could loop: deploy, regress, roll back, re-propose.
13. **Gates that are too loose:**
    - `Category::Agent` counts as Behavior, so whole agent rewrites can auto-deploy, judged with
      no tools.
    - Evolution's skill `refine` bypasses the learning Propose gate.
14. **Settings conflict:**
    - The Settings page allows `max_tool_rounds` up to 64, but evolution's range is 1–16.
    - User edits make no generation, so a rollback silently reverts them.
15. **The code lab is a dead end:** approved branches are never merged, the author sees ≤ 3
    files, and validation never checks that the evidenced problem is actually fixed.

### D. Retrieval and quality
16. **Skills:**
    - Matching is keyword-only, with no relevance floor, so some skill is always injected.
    - There's no token cap.
    - Conflicting skills are flagged but still both injected.
    - O(N) file reads every turn.
17. **No anti-patterns:** failures never become "don't do X" knowledge.
18. **Personal skills** get no outcomes, can't be promoted to shared, and their names leak into
    the shared curator. There's also an owner-versus-member name-collision bug.
19. **Reviews are dropped** while another one runs (a single `reviewing` flag).
20. **Error detection is a substring:** a tool result containing `"error"` counts as an error.
21. **Routing never learns** from good delegations, and **goals are never proposed** from
    repeated requests.

## 4. Proposed upgrades

Ordered by value for effort. Phase 1 alone would wake every loop up.

### Phase 1: give the loops a signal (quick wins, highest value)

**Done in 0.74.0.226.** All of 1.1–1.7 are in place:
- **The core:** `src/outcomes.rs` keeps one verdict per reply, ranked rated > signal > judged > hint, saved with the reply in the session.
- **Rating:** 👍 / 👎 with "what was wrong?", plus Copy, in the app. `+` / `-` in both TUIs.
- **The judge:** the decision model, then the fallback model, then whole-word lists.
- **Signals:** Stop, `/retry` and `/edit`, the tool-call limit (`limited` on the run), Keep going, the same question again, copy and read aloud.
- **Everyone counts:** members' runs are counts only.
- **Tool failures:** marked `ok:false` with an `error_kind`.
- **Gates:** they fall back to the fallback model, with the down state shown on Status, in the app and in the TUI.

| # | Upgrade | What it changes |
|---|---|---|
| 1.1 | **Rate a reply in the app**: 👍 / 👎 (with an optional "what was wrong?") on each reply in the PWA and TUI (`+`/`-` on the last reply). It goes through the same path as `/outcome`. | It gives a direct, unambiguous signal to skills, memory, delegations and evolution at once. |
| 1.2 | **A model outcome judge**: after each turn, when the next message arrives, the decision model (falling back to the fallback model) classifies it as `success / partial / correction / new-topic / unclear`, with a short reason. The keyword lists are kept only as a last resort, matched on whole words. | Most turns get an outcome; "thanks, now do Y instead" is read correctly; the correction text becomes the evolver's evidence. |
| 1.3 | **Implicit signals**: regenerate or resend (bad), Stop mid-reply (bad), the tool-round limit hit (partial, recorded on the run), "Keep going" (partial), the same question re-asked within 2 minutes (bad), a reply copied or read aloud (mild good). | Signals that cost the user nothing. |
| 1.4 | **Every person's turns count**: judge and record for members and web users too, tagged by person. Shared loops learn from aggregates, and personal skills get their own outcomes. Members' data never shows up to other people; only counts feed the shared detectors. | It multiplies the data, which today comes from the owner alone. |
| 1.5 | **Keep `last_run` across resume and resend**: judge it, or persist it in the session. | Stops losing outcomes. |
| 1.6 | **Real error detection**: tools return a structured `ok: false` or error kind, which replaces the `"error"` substring. | Cleaner detectors and skill triggers. |
| 1.7 | **Fix the decision model's health**: when `[decide]` is down, every gate silently says no. Warn on the Status page and in the Activity panel, and fall back to the fallback model for gates. | The memory gate and skill gate work again. |

### Phase 2: credit and measurement

**Done in 0.75.0.227.** All of 2.1–2.6 are in place:
- **Credit:** only the skills a reply followed get its outcome, as read by the decision model or the fallback.
- **Tests:** a test set in `evolution/golden/`, from 👍 and "Add to tests". Benchmarks use 10 tasks × 2 answers with a 95% confidence interval.
- **Real turns:** benchmarks build the prompt as live turns do, replaying earlier turns.
- **Judging:** pairwise and blind, by the fallback model.
- **Trials:** on 30% of turns. Auto-deploy happens only through a trial, and agent changes always wait for a person.
- **Visible:** the How lyra is doing page, and the TUI Evolution panel's trial and tests lines.

| # | Upgrade |
|---|---|
| 2.1 | **Per-skill credit:** the judge (1.2) is told which skills were injected and which ones the reply actually followed, and credits only those. |
| 2.2 | **A golden task set:** `evolution/golden/*.toml`, holding curated tasks with reference answers or checks. They're seeded from highly rated replies (1.1), and an admin can "add to tests" from any reply. Benchmarks use ≥ 10 tasks with 2 samples each, and report a confidence interval. A candidate passes only when the gain beats the noise. |
| 2.3 | **Benchmark the real agent:** run tasks through the same prompt assembly as a live turn (memory recall, skills, discovery) and replay the conversation's earlier turns for follow-up tasks. |
| 2.4 | **Pairwise, blind judging:** show A and B in random order, judged by a model other than the one under test when one is configured (e.g. the fallback). Efficiency counts only as a tie-break. |
| 2.5 | **A/B in live use:** a deployed candidate starts as a *trial* on, for example, 30% of turns for N turns, with outcomes compared to the control on the same period. It is kept only if it wins, which replaces the cross-period monitor. Auto-deploy is allowed only through a trial. |
| 2.6 | **A "How lyra is doing" page** (admins), with an Evolution panel line in the TUI: success rate, correction rate and the top problems over time, plus each generation and skill's effect. This makes the loops visible and trustworthy. |

### Phase 3: let the loops run (safely)

**Done in 0.76.0.229.** All of 3.1–3.5 are in place:
- **Reviews:** weekly by default. In propose mode they test their best candidates, and the owner gets an in-app digest.
- **Attempt memory:** problems are matched by fingerprint, so open or recently rejected ones are skipped and the evolver sees past attempts.
- **Gates:** an agent's instructions are now their own level and always wait for approval. Skill changes follow the learning mode. Edits on the Settings page are generations, a rollback keeps them, and tool calls per reply use one range, 1–64.
- **Review queue:** reviews wait their turn (one per turn, up to 5) instead of being dropped.
- **Memory:** exact duplicates merge on their own.

| # | Upgrade |
|---|---|
| 3.1 | **Scheduled reviews by default:** evolution weekly, and skill and memory curation weekly. In propose mode a review also **tests** its candidates, so the Proposals page shows ready-to-approve items with their scores. A weekly digest (in-app, or by email to the owner only) lists them. |
| 3.2 | **Attempt memory:** a problem's fingerprint is linked to its past candidates (rejected, rolled back, why). The evolver sees them, and a problem with an open or recently rejected candidate is skipped. |
| 3.3 | **Tighten the gates:** `Category::Agent` always needs approval; evolution's skill `refine` follows the learning mode; and a rollback keeps user edits. Settings changes become generations, and the knob ranges are unified with `limits.rs`. |
| 3.4 | **A review queue** in place of the single `reviewing` flag: reviews are queued and deduped by turn. |
| 3.5 | **Auto-apply the safe memory curation:** exact duplicates merge on their own. Contradictions still wait. |

### Phase 4: learn more kinds of things

**Done in 0.77.0.230.** All of 4.1–4.7 are in place:
- **4.1** Skills are found by meaning (LanceDB `skills/index/`), with a floor, a token budget, and a conflict's loser left out.
- **4.2** Avoid notes come from corrections.
- **4.3** Lessons several people learned are offered for sharing. Shared and personal names no longer collide, and the curator sees no one's personal skill names.
- **4.4** Routing examples are learned and unlearned, and misrouting is flagged.
- **4.5** Routines and goals are offered from patterns, with a one-tap card.
- **4.6** New detectors: the tool-call limit, slow or long replies, workflow mis-triggers, guideline bloat and unhelpful guidelines, memory misses (with a capture review).
- **4.7** Runs record what was in play, guideline and workflow effects show on the page, and unhelpful memories are proposed for archiving.

| # | Upgrade |
|---|---|
| 4.1 | **Embedding skill retrieval with a floor:** reuse the memory embedding provider, store skill vectors in LanceDB with the files unchanged, inject a skill only above a relevance threshold, cap skills at about 800 tokens in all, and leave out the loser of a known conflict. |
| 4.2 | **Anti-pattern skills:** a correction or a failure becomes a short "avoid" note attached to the skill or tool involved, injected when that tool is likely. |
| 4.3 | **Promote a personal skill to shared:** a skill used successfully by 2 or more people (counts only) is proposed for sharing, with an admin approval and the personal details stripped. Also fix the name collision and keep personal names out of the shared curator. |
| 4.4 | **Routing that learns:** a well-rated delegation adds its task as a routing example, capped and deduped. Repeated mis-routes propose a rule change. |
| 4.5 | **Goals from patterns:** the same kind of request three times in two weeks, or "remind me / keep an eye on", proposes a goal or a routine, which the user accepts in one tap. |
| 4.6 | **New detectors:** tool-round-limit hits, slow or long replies, workflow mis-triggers (a workflow fired, then a correction), guideline bloat, and memory misses ("I told you that already" leads to a capture review). |
| 4.7 | **Attribution:** the run record notes which guidelines, workflows, skills and memories were in play, so each can be scored and pruned when it doesn't help. |

### Phase 5: close the code loop (later, owner's call)

| # | Upgrade |
|---|---|
| 5.1 | **A repro test first:** a code candidate must add a failing test from its evidence that its patch then makes pass. Before-and-after benchmark scores are shown with the branch. |
| 5.2 | **Review in the app:** the Proposals page shows the code lab branch diff with test results. Merging stays a human action outside lyra (never merge, push or touch the running binary). |

## 5. Recommendation

Start with **1.1 (rating in the app), 1.2 (the model outcome judge), 1.3 (implicit signals) and 1.7
(decision-model health)**. They are small, they touch no approval rules, and within days they turn
129 unknown skill uses and 52 unjudged delegations into real data. Then run **2.6 (the
"How lyra is doing" page)** and **3.1 (scheduled, auto-tested reviews)**, so the evolution that
already exists starts proposing. The measurement work (2.2–2.5) should land before auto mode is
ever switched on.

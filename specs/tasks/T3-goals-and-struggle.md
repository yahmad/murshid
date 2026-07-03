# T3 — Session goals & struggle prompts

**Status:** Planned (after T2)
**Traces to:** SPEC v0.5 — D13–D16, I8–I14, C2/C3/C5/C7/C12.
**User outcome:** the pane knows what the founder is working on without ever
asking, prioritizes advice accordingly, notices when he's genuinely stuck,
and offers help exactly once, politely, with its evidence on the table.

## Scope

IN: goal inference + banner + goal file + `goal` command, goal drift notice,
goal-relevance queue term, session-end bookend, the two inferred struggle
signals + convergence rule, help-flavored comment detection, struggle
offers, decline memory.
OUT: murshid-comment channel `// murshid:` (T4 — that is a *direct ask*,
not a struggle signal), memory-driven anything (T5), retrieval questions
(T5).

## Requirements

### Goal capture (D13, I8)
1. On watcher start, infer the goal: branch name (issue-id + slug patterns
   parsed; `main`/`master`/`dev`/`wip`-like names are non-informative) →
   last 3 commit subjects → first session-diff file cluster. Render one
   banner line: "goal: <text> (g to edit)". NEVER prompt for input.
2. Goal lives in `.murshid/goal` (plain text, one line + optional notes,
   user-editable at any time; file watched). `murshid goal [text]` sets it;
   bare `murshid goal` prints it. `g` in-pane opens $EDITOR on the file.
3. Inference never overwrites an explicit goal (file mtime > inference
   time wins). Goal changes are `goal_set`/`goal_inferred` events.
4. Drift: if ≥ 70% of newly changed files over the last 30 min fall outside
   the goal's file cluster, surface ONE line per session ("looks like
   you've moved on — g to update"); never repeats, never blocks.

### Goal semantics (D14, C7)
5. Queue ordering gains the goal-relevance term ahead of category rank
   (C7): advice whose site's file is in the goal cluster (or whose concept
   was referenced in the goal text) ranks first. The quiet detent's floor
   reads "goal-relevant idiom" as specified in T2 req 1.
6. Bookend at session end (auto, one screen, zero required interaction):
   goal line; counts (cards shown/applied/queued-unshown); concepts taught
   (names only); throttled categories; queue's last call (top 3 one-liners).
   Rendering rules per I21. Bookend is an event.

### Struggle signals (D15, C12, I14)
7. Signal 1 — repeated same-error: 3 consecutive `cargo check` failures
   whose primary error code (E-code) is identical, same session.
8. Signal 2 — time-in-red beyond self-baseline: continuous failing check
   state exceeding the user's 75th-percentile time-to-green, computed from
   `events` history (cold start: 25 min; recompute per session start).
9. Inferred signals fire an offer ONLY on convergence (both true
   simultaneously) — I14. Churn/idle are NOT signals in v1 (idle only times
   delivery, req 11).
10. Signal 3 — fresh help-flavored comment in the session diff: new comment
    line matching help-seeking phrasing (question mark endings; "not sure",
    "why does", "how do", "doesn't work", "stuck"; requirement-debt
    phrasing "doesn't handle X yet") — pattern list ordered most-specific
    first per repo convention, seeded in the pack's surface.toml. EXCLUDE
    on-hold TODOs (containing issue refs, "when X lands/fixed", version
    conditions). Fires alone (self-declared), no convergence needed.

### The offer (I9–I13, D16, C3)
11. An offer is one line, non-modal, shown only at idle (no file events ≥
    20 s): "stuck on E0308 for 14 min — hint? [y/N]" — the evidence clause
    is mandatory (I11). The never-while-green gate applies to INFERRED
    signals only (signals 1+2 describe stuck-ness, which requires red);
    signal 3 (self-declared help comment) may fire on green builds —
    "why does this need a clone?" is a green-build question.
    (Clarified 2026-07-03 after T3 review surfaced the tension.) Continuing to
    type dismisses silently (expired). `y` runs the judge on the struggle
    site and shows the card through the normal slot.
12. Offers share the push budget; when the bucket is empty they may borrow
    exactly one token (C7). Offer + response are events; category
    "struggle-offer" counts toward D12's throttle (T2 machinery).
13. Declines persist: a declined offer for the same (signal, E-code|concept)
    never re-fires in the same session; two declines across sessions for
    the same concept suppress that concept's offers for 7 days (suppression
    row, scope `offer-concept`).

## Acceptance

- `cargo test` green. New tests: branch-name inference incl. non-informative
  fallbacks; explicit-goal-wins rule; drift threshold fires once only;
  goal-relevance ordering; bookend content assembly; same-E-code streak
  counter (reset on different code / on green); 75th-pct baseline
  computation from fixture events + cold start; convergence gate (either
  signal alone ⇒ no offer); help-comment matcher incl. on-hold exclusions
  (most-specific-first order asserted); offer idle-gating; decline
  persistence in-session and 7-day cross-session.
- Manual dogfood: watcher on a real branch infers a sane goal line; a
  deliberately stuck session (repeat same E-code + long red) produces
  exactly one evidenced offer at idle.
- No TODO markers in covered files.

# T15 — TUI spike (gitui-inspired; replaces the watch pane)

**Status:** Active — spike, founder-directed 2026-07-05 (MUR-7), OVERRIDING the
feasibility doc's "hold + read-only dashboard" recommendation. This is a
throwaway-able branch (`yasir/mur-7-tui-spike`) for the founder to feel the
full impl; it is NOT merged to main on completion without a separate decision.

**Traces to:** `specs/explorations/tui-feasibility-and-design.md` + the four
MUR-7 founder decisions.

## Dependency exception (Decision 1)
For this spike branch only, SPEC C10's stdlib allowlist is extended to permit
exactly **`ratatui`** + **`crossterm`** (frontend-only). Syntax highlighting
(syntect) is explicitly excluded — diffs render plain / ANSI. The pedagogy
engine and all `lib.rs` domain modules stay stdlib/C10-allowlist; the TUI lives
in its own `src/tui/` module and the engine never depends on ratatui types.
Ratifying this into the design-authority SPEC is deferred to the merge decision.

## Scope (what "the full impl" means here)
Replace the `murshid watch` terminal presentation with a full-screen ratatui
app (Decision 2). **Keep the existing engine intact**: the file-event →
quiescence → sweep worker and the offer-poll worker keep running on their
threads, populating `WatchSession` + `profile.db` exactly as today. Swap ONLY
the presentation + input layer:
- Out: the blocking `keys::run_stdin_loop` + the `println!` scroll.
- In: a crossterm event loop on the main thread that (a) polls input with a
  timeout, (b) on tick re-reads live state and redraws, (c) maps keys to the
  existing interaction logic.

### Views (tabs / panes)
1. **Dashboard (flagship):** active card (rendered), queue depth + top queued
   concepts, budget/throttle state, current goal + drift, judge/degraded mode,
   a recent-activity strip.
2. **Mastery meter:** `progress::build_rows` as a navigable list (mastery, help
   level/rung, staleness, throttle flag); enter a concept → its history.
3. **Event / history:** the `events` table as a scrollable, filterable log,
   rendering T14's `judge_declined` vs `judge_drop` (+ `card_shown`) distinctly
   — this is the "silence is ambiguous" fix made visible.
4. **Card + thread:** the card's six fields with the worked diff in a diff pane
   and the anchored thread transcript.
- **Always-visible contextual keybar** (the founder's core ask — show the keys
  valid on the focused object, no memorization) + a `?` help overlay.

### Card interaction (reuse, don't duplicate)
Card responses `a/g/u/n/e/t/k` must work from the dashboard/card view. REUSE the
existing logic: `response::classify_card_key` already maps keys →
`CardKeyAction`; the per-action bodies currently inline in `keys::run_stdin_loop`
(Response(verb) → record + snooze scopes, Escalate/TellMe → rung bump, Ask →
thread turn) should be **refactored into callable functions** the TUI invokes,
so behavior stays identical and both paths (if the old loop is kept behind a
flag) call the same code. Struggle offers (y/decline) likewise.

### Live-state coupling (Decision 4)
Because the TUI is in-process (it replaced the pane), it reads `WatchSession`
directly for the live view (pending card, queue, budget bucket, throttle set,
goal, drift, struggle) — no snapshot needed for v1. Implement a lightweight
**session-snapshot checkpoint** (queue depth + budget bucket written to a small
known file/table) ONLY as a stretch/enabler for a future separate dashboard;
if it adds risk to the core spike, defer it and note it here.

## Terminal lifecycle (the hazard the feasibility doc flagged)
- Enter raw mode + alternate screen on start; restore on exit AND via a panic
  hook (a panic must not leave the user's terminal wedged).
- Ctrl-C: the TUI owns it as "quit" (crossterm delivers it as a key event in
  raw mode). Reconcile with the existing SIGINT-cleanup path in `watch/mod.rs`
  (the session-end bookend + cleanup must still run on quit).
- Handle SIGWINCH/resize.

## Acceptance (spike bar — runnable, not production-gated)
- `cargo build --release` in the worktree succeeds; `murshid watch` launches
  the full-screen TUI and the four views + keybar + `?` are reachable.
- Card responses `a/g/u/n` record correctly (same DB effect as the old loop),
  verified by a test or a scripted check.
- `cargo test` green in the worktree; `cargo clippy` clean (ratatui/crossterm
  allowed). stdlib + the two permitted crates only — no other new deps.
- Engine untouched: no ratatui types leak into non-`tui` modules; the sweep /
  offer / DB layers are unchanged except where interaction logic was extracted
  into shared callable functions.

## Out of scope (acceptable to stub in v1 if integration proves invasive)
- Full thread/ask (`k`, `t`) round-trips MAY be read-only or minimally wired in
  v1 if the refactor is too invasive — but card responses `a/g/u/n` and
  escalation `e` MUST work (that's the core loop). Note whatever is stubbed.
- syntect syntax highlighting (diffs plain/ANSI).
- Merging to main; ratifying the C10 amendment into SPEC.md.
- A separate read-only `murshid tui` command (this spike replaces the pane).

## Implementation notes (what actually shipped, honestly)

Built: the full-screen ratatui replacement (`src/tui/`), all four views + the
always-visible contextual keybar + `?` help overlay, terminal lifecycle
(raw mode/alt screen entry, panic-hook restore, Ctrl-C-as-quit reconciled
with the SIGINT-cleanup path, resize handled for free via `Terminal::draw`'s
`autoresize`), and `a/g/u/n/e/t` reusing the exact functions the retired
stdin loop's inline arms called (`keys::apply_card_response`,
`keys::apply_escalate_or_tell_me`, `keys::handle_card_key` — test-verified
against a real DB, see `src/watch/keys.rs`'s `tests` module). Struggle-offer
`y`/`n` likewise reuses `keys::apply_offer_accept`/`apply_offer_decline` via
`keys::handle_offer_key`.

Deferred / stubbed (narrower than the spec text allowed, recorded honestly):
- **Ask (`k`)**: fully read-only — the Card view shows the existing
  `threads` transcript, but there is no in-TUI way to submit a NEW question
  (not even a minimal one). The spec's "MAY be read-only or minimally wired"
  allowance is used at its most conservative end; wiring a real text-input
  mode was judged too invasive for the time-box. `handle_card_key` returns a
  notice ("ask (k) is read-only...") instead.
- **Goal editing (`g` with no pending card)**: ~~dropped~~ **IMPLEMENTED
  2026-07-05 (founder request "adjust goal too?").** Rather than the old
  loop's $EDITOR handoff (which fights the alt-screen/raw-mode state), `g` on
  the idle home surface opens an inline text-input overlay (`App::goal_edit`):
  it captures all keys until Enter (persists via the same
  `goal::write_goal_file` the CLI uses) or Esc (cancel). The goal also renders
  prominently on the empty surface ("goal: … (g to change)") and as a keybar
  chip. Verified end-to-end (expect-driven: `g` → type → Enter wrote
  `.murshid/goal`). `murshid goal <text>` outside the TUI still works too.
- **`r` (in-pane `murshid review`)**: dropped entirely along with the
  retired stdin loop — it was never one of the four required views.
- **Interactive queue-pull-into-slot**: dashboard shows queue depth + top-3
  concepts READ-ONLY (matches the spec's literal V1 bullet). Turning a
  queued concept into the active card from the TUI is not wired — partly
  scope, partly because bare digit keys are already spent on `1`-`4`
  view-switching, so a queue-pull UI would need a different keybinding
  scheme than the old `m`+number flow.
- **Mastery drill-down** ("enter a concept → its history"): not built: V2 is
  a flat navigable list only (mastery/help-level/staleness/throttle, per the
  spec's literal V2 bullet). Drill-down was the feasibility doc's
  aspirational "beats today" framing, not the T15 acceptance bar.
- **Session-snapshot checkpoint (Decision 4)**: deferred per the spec's own
  allowance — the TUI is in-process and reads `WatchSession` directly, so it
  isn't needed for this build; left as a future enabler for a hypothetical
  separate dashboard process.
- **Worker stdout suppression**: implemented as an unconditional route
  through `WatchSession::notice` (a bounded activity-log buffer the
  dashboard renders) rather than a runtime `tui_mode` toggle — since
  Decision 2 REPLACES the ambient pane (no dual-mode fallback), there is no
  second code path that still wants raw `println!`, so an always-on buffer
  was simpler than a boolean gate at every call site.
- **A synthetic terminal-loss edge case** (the pty disappearing without a
  signal — not one of the spec's named hazards, which are raw-mode
  enter/exit, the panic hook, Ctrl-C, and SIGWINCH, all handled) was found
  under test-harness conditions and could burn CPU if `event::poll` stops
  blocking; a circuit breaker in the event loop bails out after a run of
  suspiciously-fast empty polls rather than spinning. A real terminal
  closing ordinarily delivers SIGHUP (default-fatal, unhandled — same as the
  pre-T15 code), so this is a defensive addition, not a fix for a discovered
  regression.

## Follow-up added 2026-07-05 — parse-gate visibility
Founder dogfood diagnosis: saves produced no cards and the founder couldn't
tell "murshid is broken" from "murshid is deliberately holding because the
file doesn't parse yet" (C12 requires a clean tree-sitter parse before the
compiler check; `sweep.rs` skips at `!parses_without_errors`). Not a bug —
but the silence was ambiguous (same theme as the earlier findings). Fix:
`WatchSession::parse_waiting` records the files the parse gate is currently
holding (set at the gate, cleared when a file parses again), and the TUI
dashboard renders `view::parse_wait_line` — "⏸ waiting — N file(s) don't
parse yet (fix syntax to resume): …". The parse gate itself is UNCHANGED and
was deliberately kept (removing it would break site-identity/anchoring, add
mid-edit noise + LLM cost, and drift toward line-completion, a non-goal).
Pure `parse_wait_line` unit-tested; population verified by inspection (the
sweep reaches the gate regardless of degraded mode). 622 tests, clippy clean.

## Follow-up added 2026-07-05 — offer-accept froze the UI (real bug)
Founder dogfood: accepting a struggle offer (`y`) froze the whole TUI for
seconds, then "reset" with no visible result. Root cause: `handle_offer_key`
(→ `apply_offer_accept` → `run_struggle_judge_and_show`, a BLOCKING network
dispatch to the judge) ran synchronously on the event-loop thread, so the
loop could neither redraw nor read input until the call returned; the result
(a card via `pending_card`, or a "nothing new" notice) only appeared after
the freeze and was easy to miss. This is the classic TUI mistake — never do
network I/O on the render thread. (The card keys `a/g/u/n/e/t/k` are all fast
in this spike — `e/t/k` are read-only stubs — so offer-accept was the sole
blocking path.) Fix: offer-accept now runs on a background thread (opens its
own WAL DB connection; delivers via the shared `WatchSession`), guarded by a
new `WatchSession::busy: Mutex<Option<String>>` (single-flight + a "⏳ asking
the model…" line the dashboard renders). A `BusyGuard` (Drop) clears `busy`
even on panic so a failed dispatch can't wedge the UI in "working". Decline
stays inline (fast). Thread-safety proven by compilation (all captured data
is `Send`); 622 tests, clippy clean. Live struggle-offer visual confirm is
the founder's step (provoking an offer needs specific red-streak conditions).

## UX redesign implemented ("Focus", `specs/explorations/tui-ux-redesign.md`)

Founder verdict on the four-tab dashboard: "I don't like the UI." The
redesign doc's build plan (§7, Steps 0-9) is implemented in full — home is
the app (not a tab), mastery/events/concept-detail are summoned overlays,
the card is the hero. All 9 steps shipped; nothing deferred to a later PR.

**Files changed:** `src/tui/theme.rs` (new), `src/tui/app.rs` (rewritten),
`src/tui/view.rs` (rewritten), `src/tui/mod.rs` (key-handling routed through
`App::Focus`), `src/budget.rs` (+`TokenBucket::capacity()`),
`src/db/events.rs` (+`get_concept_events`).

**The two flagged engine additions, exact signatures:**
- `budget::TokenBucket::capacity(&self) -> u32` — trivial getter, no
  behavior change; feeds the ambient band's budget gauge fraction.
- `db::get_concept_events(conn: &Connection, concept_id: &str) ->
  Result<Vec<EventRecord>, rusqlite::Error>` — a `json_extract` filter over
  the existing `events` table (no schema change); feeds the concept-detail
  `Sparkline` trend + recent list.

**Step-by-step:**
- **Step 0 (theme):** `theme::Role` — the `(glyph, ascii, color, word)`
  table from §4.1, `color_allowed()`/`glyphs_ok()`, `category_style`,
  `state_style`, `chip`, the working-pulse frame cycle. Unit-tested
  (fallback/no-color behavior, chip shape, pulse cycling).
- **Step 1 (hero card):** `view::render_card_block`/
  `render_card_block_with_color(&PendingCard, &SurfaceConfig, bool) ->
  Vec<Line>` — rung-aware (R0 recall / R1 nudge / R2 full / R3 worked
  example), gutter anchor, why, labeled Rule + `→` doc line, multi-site
  line. `view::draw` restructured into the header/surface/ambient/keybar
  4-region layout; the empty/caught-up state and queue presence line ship
  here too.
- **Step 2 (pulse + ambient band):** the header's reversed wordmark +
  `●/◐/⏸` pulse (derived from `busy`/`parse_waiting`), animated via
  `App::tick` (incremented once per ~200ms poll in `mod.rs`); the dim
  ambient band (goal · budget gauge · judge).
- **Step 3 (working/waiting faces):** full-surface "asking the model…" and
  "waiting on a clean parse" states, reusing `parse_wait_line`'s exact
  summary sentence (kept, with its pinned tests) plus a bulleted file list.
- **Step 4 (offer callout):** the struggle offer renders as the centered
  `⚑` callout; the evidence line reconstructs an `offer::Evidence` from the
  existing `PendingOffer` (its key + elapsed-since-fired), reusing the
  already-tested `offer::offer_line` — no new offer-evidence storage.
  Behavior unchanged: still `keys::handle_offer_key`.
- **Step 5 (ack beat):** `App::start_ack`/`ack_active`/`acked_card` — the
  card border flashes green for `ACK_BEAT_TICKS` (3 ticks) after `a`/`g`.
  Since `handle_card_key` already frees `ws.pending_card` the instant a
  response resolves (unmodified), `mod.rs` snapshots the card BEFORE calling
  `handle_card_key` purely for this flash.
- **Step 6 (mastery bars):** real span-built `█`/`░` bars colored by
  `ConceptState`, category group headers, `%`, help level, age, state word,
  all from `progress::build_rows` unchanged.
- **Step 7 (concept detail):** `⏎` on a mastery row pushes
  `Focus::ConceptDetail(concept_id)` — mastery bar, a ratatui `Sparkline`
  grade trend (fail=1/hard=2/pass=3) and a "recent" list, both fed by
  `db::get_concept_events`.
- **Step 8 (events overlay):** each event `kind` (+ `verb`, where relevant)
  maps to a `(glyph, color, word)` role; a pure `summarize_event_payload`
  helper renders a human payload column instead of raw JSON.
  `EventsFilter`/`f` kept.
- **Step 9 (nav swap):** `App::Focus` (`Home`/`Mastery`/
  `ConceptDetail(String)`/`Events`) replaces the `1`-`4` tab enum; `m`/`e`
  summon, `esc` pops one level, `m`/`esc` from Mastery/ConceptDetail jump
  straight home (per the design doc's own mockup keybars). The keybar is
  fully restyled into `[k] label` chips driven by focus + card/offer
  presence.

**Pure helpers unit-tested:** `theme`'s role/span/chip/pulse-frame
functions; `render_card_block_with_color`'s per-rung shape;
`justify_line`, `budget_gauge_span`, `mastery_bar`; `event_role`/
`summarize_event_payload`; `grade_history_heights`/`trend_description`;
the stdlib-only SQLite-timestamp parser (`parse_sqlite_ts_epoch_secs`) +
`relative_age`.

**Verification:** `cargo test --manifest-path Cargo.toml --
--test-threads=1` → 656 passed (lib) + 1 (integration test) + 0 (main), 0
failed; `cargo clippy --all-targets -- -D warnings` clean; `cargo build
--release` succeeds. No `TODO` markers.

**Deviations from the design doc, with reasons (diff-verified, so stated
honestly rather than glossed over):**
- The header's per-file context (design doc: "● watching · src/main.rs")
  is approximated from `WatchSession::pending_files`/`parse_waiting`
  (files touched/held, already tracked) rather than a literal "file last
  judged" field — adding one would be a THIRD engine change beyond the two
  flagged needs, so it was deliberately avoided.
- `glyphs_ok()` always returns `true` — v1 has no real terminal
  glyph-capability probe (the design doc itself says "no need to probe
  terminal capability beyond NO_COLOR for v1"); every `Role::ascii`
  fallback is present on the table, unused by any code path, ready for a
  future probe.
- The optional `Tab`-cycle nicety (design doc §5.2: "can be kept... but
  it's no longer the primary model") was NOT implemented — the doc marks
  it explicitly optional, and the summon/pop model (`m`/`e`/`⏎`/`esc`)
  fully covers navigation without it.
- Rule/why text wraps via ratatui's own `Wrap` (reflows with the terminal
  width) rather than a manually pre-computed hanging indent — more robust
  under resize; the continuation lines flush left instead of aligning
  under "Rule" as in the ASCII mockup (cosmetic only).
- Concept-detail's "help shifted N → M" line (design doc §3.6) is omitted:
  `concept_memory` only stores the CURRENT `help_level`, not a shift
  history, and no event captures a level-shift note today; deriving one
  would need a new event kind, which is out of the two flagged needs.
  Up/down navigation within concept-detail (moving to an adjacent
  concept) is also not wired — `esc`/`m` (back/home) are.
- The budget gauge is a genuine `tokens/capacity` fraction (now that
  `capacity()` exists) rendered as a `BUDGET_GAUGE_WIDTH=7`-cell bar. But
  every detent's real burst is 1 (`STANDARD_BURST`), so with capacity 1 the
  7 cells are all-filled or all-empty (and only briefly partial mid-refill),
  rather than the mockup's illustrative multi-segment fill (which assumes a
  hypothetical higher-capacity bucket); the gauge is correct and would show
  graduated fill if `capacity` ever exceeds 1.

## Follow-up added 2026-07-05 — live mentor-state indicator + events demotion

Founder ask: the header pulse only ever showed `● watching` / `⏸
waiting·parse` / `◐ thinking` (the last one ONLY during offer-accept's
struggle judge) — the NORMAL save→check→screen→judge sweep set no live
status at all, and a review that found nothing was silent. Also: the events
overlay is debug/history, not primary — demote it and free `e`.

**Engine (`watch/mod.rs`, `watch/sweep.rs`), additive telemetry only:**
- `WatchSession` gains two new fields, mirroring `busy`'s shape: `review_state:
  Mutex<ReviewState>` (`Watching` / `Reviewing { file }`) and `last_review:
  Mutex<Option<LastReview>>` (`LastReview { file, result: ReviewResult, at:
  SystemTime }`, `ReviewResult` = `Suggested` / `NothingToFlag` /
  `CouldNotReview`). All three types live in `watch/mod.rs` alongside
  `WatchSession` (not a separate `review_state.rs` — small enough not to
  need it, and `src/review.rs` already names something else).
- `sweep_pending` (`sweep.rs`) sets `review_state = Reviewing{file}` the
  moment a file passes the parse gate and enters `run_diagnostics_check` (the
  representative file is the FIRST one to pass the gate this pass, per the
  spec's "single representative file is acceptable" allowance for a
  multi-file pass). A new `finish_review_pass` helper closes out the pass:
  writes `last_review` (only when something was actually attempted — a pass
  that never got past the parse gate leaves `last_review` untouched, never
  fabricating an outcome) and unconditionally resets `review_state` back to
  `Watching`.
- `ReviewResult` derivation (`derive_review_result`, pure, unit-tested) is a
  simple 3-way priority: `degraded` (no usable judge/screen model this
  session — `judge::JudgeMode::Degraded`) OR a live dispatch error this pass
  both collapse to `CouldNotReview`; otherwise `shipped_any` (a card was
  shown OR queued this pass) → `Suggested`; otherwise `NothingToFlag` (judged
  cleanly, nothing card-worthy — declined/dropped/silenced/suppressed/
  already-known, or no new hunks). This required two small non-behavioral
  signature changes, both diff-verified to touch return values only, never
  the judging logic/event-logging/parse gate themselves:
  - `judge_and_collect_finding` now returns a 3-variant `JudgeAttempt`
    (`Found(SweepFinding)` / `Clean` / `DispatchFailed`) instead of
    `Option<SweepFinding>` — `Clean` and `DispatchFailed` are exactly the two
    ways the old `None` used to collapse (judged-but-not-card-worthy vs. a
    genuine pipeline error); the three pre-existing unit tests exercising it
    directly were updated to match via a `#[cfg(test)]`-only
    `.into_finding()` convenience, no assertions changed.
  - `aggregate_and_dispatch` now returns `bool` (`shipped_any`) — `true` iff
    the pass actually inserted a `shown` or `queued` card row this pass
    (tracked at the exact 3 sites that already did an `insert_card`/
    `enqueue_finding`); every existing call site (1 production + 3 tests)
    ignores the return value with no compile/clippy impact, since a plain
    `bool` isn't `#[must_use]`.
- A new sweep-level test (`test_sweep_pending_degraded_pass_over_real_file_
  records_could_not_review`) drives the REAL `sweep_pending` end to end (not
  just its helpers) over one real pending file, asserting `last_review` lands
  on `CouldNotReview` and `review_state` resets to `Watching`. It runs in
  `judge::JudgeMode::Degraded` mode specifically because that's the one path
  through `sweep_pending` that never reaches a live model dispatch (the
  `models` argument's `ResolvedSlot`s are dummy values, never called), and it
  passes a deliberately bogus `pack_dir` basename (matches no registered
  language id) so `run_diagnostics_check`'s adapter lookup fails fast instead
  of shelling out to a real `cargo check` — so the test stays fast/
  deterministic. A live (non-degraded) sweep-level test proving `Suggested`/
  `NothingToFlag` end to end was judged to need the same heavy harness (a
  live or fixture-injected judge dispatch through the FULL `sweep_pending`,
  which none of the existing sweep tests attempt — they all drive
  `judge_and_collect_finding`/`aggregate_and_dispatch` directly instead) — so
  those two outcomes are covered at the `derive_review_result`/
  `finish_review_pass` unit level plus the pre-existing `judge_and_
  collect_finding`/`aggregate_and_dispatch` flow tests, not a second
  `sweep_pending`-level test. Noted here rather than hacked around.

**TUI (`tui/theme.rs`, `tui/view.rs`), presentation only:**
- `theme::REVIEWING_PULSE` — a new `Role` (Yellow, ascii fallback `~`) for
  the mentor-state "reviewing" family; the header animates its glyph via the
  existing `working_pulse_frame` tick cycle (same as "thinking") rather than
  this `Role`'s own static glyph field.
- `view::home_pulse_span`'s precedence is now a pure, unit-tested
  `select_pulse_state(busy, reviewing_file, parse_waiting) -> PulseState`
  (`Thinking` / `Reviewing(file)` / `WaitingParse` / `Watching`): `busy` (the
  offer-accept struggle judge) still wins outright; `Reviewing{file}` (the
  NORMAL sweep) is next, ahead of the parse-gate hold; then `parse_waiting`;
  then plain `watching`. Degradation-safe per the design doc's posture
  (glyph + word carries meaning with color off).
- `view::empty_state_lines` (the caught-up/idle home surface) now appends one
  calm, dim one-liner when `ws.last_review` is present and its result is NOT
  `Suggested` (a `Suggested` review means a card is already on screen/queued
  — this empty surface wouldn't be showing) via a pure `review_outcome_line`
  formatter (unit-tested): `"looked at {file} {age} — nothing worth
  flagging"` for `NothingToFlag`, `"couldn't review {file} {age} — {reason}"`
  for `CouldNotReview` (the reason is read off `ctx.mode`'s
  `JudgeMode::Degraded { reason }` when degraded, else a generic "couldn't
  reach the model" fallback — `ReviewResult` itself carries no reason
  string, so this is the one place a non-`LastReview` input feeds the
  formatter). Reuses the existing `relative_age` helper verbatim.

**Events demotion (`tui/mod.rs`, `tui/view.rs`):** `e` is now
escalate-only (a card action, no idle meaning) — removed from the idle
`Focus::Home` key handler and the idle keybar's `[e] events` chip. Events is
now reached via `E` (uppercase), wired exactly like `G`/goal — an
always-available Home-surface key regardless of card/offer presence, not
gated behind the "idle" check `m` still uses. `Focus::Events`'s own
exit-to-home key changed from `e`/`Esc` to `E`/`Esc` (mirrors Mastery's
`m`/`Esc` symmetry) and its keybar chip now reads `[E/esc] home`. The `?`
help overlay drops the old "mastery/events are summoned" `e` line and gains
a new line under "Global": `E  event log — raw session event history (debug
/ history view, not primary)`. `Focus::Events`'s own code/behavior
(filtering, list, `f`) is untouched — only how you get there changed.

**Hard invariants held (verified, not just asserted):** `keys::
handle_card_key`/`handle_offer_key` and every `apply_*` function are
byte-for-byte unchanged (only new call-site wiring around
`judge_and_collect_finding`'s return type, never inside `keys.rs`); the
async offer-accept `BusyGuard` and single-flight `busy` are untouched;
terminal lifecycle untouched; the parse gate and `parse_waiting` are
untouched (only read, never gated on, by the new telemetry).

**Verification:** `cargo test --manifest-path Cargo.toml --
--test-threads=1` → 671 passed (lib) + 1 (integration test) + 0 (main), 0
failed; `cargo clippy --all-targets -- -D warnings` clean (one
`#[allow(clippy::large_enum_variant)]` added on the new internal
`JudgeAttempt` enum — boxing `SweepFinding` would ripple into every
`findings.push`/`aggregate_by_concept` call site for a per-pass enum that's
never hot-looped; matches this repo's existing posture on
`#[allow(clippy::too_many_arguments)]` for inherent-shape lints); `cargo
build --release` succeeds. No `TODO` markers.

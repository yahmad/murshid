# T15 — TUI spike (gitui-inspired; replaces the watch pane)

**Status:** Done — merged to main 2026-07-06 (`c45707a`). Originally a spike,
founder-directed 2026-07-05 (MUR-7), OVERRIDING the feasibility doc's "hold +
read-only dashboard" recommendation; started on a throwaway-able branch
(`yasir/mur-7-tui-spike`) for the founder to feel the full impl before the
merge decision was made.

**Note:** some intermediate designs recorded in this journal (full-screen
settings on `s`, `E`-for-events, summon-overlay navigation) were later
superseded by the "one living workspace" redesign — see
[specs/INDEX.md](../INDEX.md) and
[explorations/tui-redesign-2-workspace.md](../explorations/tui-redesign-2-workspace.md).

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

The initial four-tab dashboard was found unsatisfying and prompted a
redesign. The redesign doc's build plan (§7, Steps 0-9) is implemented in
full — home is the app (not a tab), mastery/events/concept-detail are
summoned overlays, the card is the hero. All 9 steps shipped; nothing
deferred to a later PR.

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
- ~~The budget gauge is a 7-cell bar…~~ **REPLACED 2026-07-05 (founder
  feedback).** The "budget" label was engine jargon and the 7-cell bar read
  as all-full/all-empty at burst 1 (misleading granularity). Now the ambient
  band shows a plain-language "next nudge" state instead: `next nudge: ready`
  when a proactive card may fire now, or `next nudge: ~Nm` while the push
  bucket refills (ETA from `TokenBucket::time_until_ready_at`, rounded up;
  `<1m` under a minute). No bar, no jargon; honest at burst 1 and correct at
  any capacity. New pure bucket methods `is_ready_at`/`time_until_ready_at`
  (projected without mutating) + `view::next_nudge_span`/`format_nudge_eta`,
  all unit-tested. `capacity()` retained (unused by the band now, kept for a
  future higher-capacity bar).

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

## Follow-up added 2026-07-05 — live SETTINGS overlay (`s`)

Founder ask: the startup dials (`frequency`, `directness`) were set once at
`watch::run` and never visible/adjustable again for the rest of the session.
Built a summoned settings overlay (`Focus::Settings`, `s`/`esc`, mirrors
`m`/Mastery's summon+pop and idle gate) that lists both dials with their
current value and a one-line plain-language meaning, navigable with
`↑/↓`/`j`/`k` (select row) and `←/→`/Enter (cycle that row's value).

**Adjustable, session-scoped only:** both dials are session-scoped —
changing them in the overlay takes effect for the rest of the running
session but is never written back to `config.toml` (a naive rewrite risks
clobbering the user's file/comments; explicitly out of scope, same posture
as the goal editor's own persistence boundary). `murshid <dial>`-style CLI
persistence was not requested and isn't built. A future follow-up could add
opt-in "save this as my new default" persistence; deferred here.

**The directness → shared-state move (the core engine work):** `directness`
used to be a `Copy` value threaded by parameter through every rung-resolution
call site, captured once at each thread's spawn time (the sweep worker) or
function-call time (the TUI) — a runtime change couldn't reach anything
already spawned. Moved to `WatchSession::directness: Mutex<ladder::Directness>`
(seeded from `[dial] directness` right after construction, before any thread
that reads it is spawned) — every call site that used to take a `directness`
value parameter now takes `ws`/has `ws` already in scope, and reads
`*ws.directness.lock_poison_safe()` FRESH on each use instead:
- `watch::resolve_entry_rung` (was `mod.rs:381`, now takes `ws: &WatchSession`
  instead of a `directness` param) — the single wrapper most rung-resolution
  call sites already funneled through (`sweep.rs`'s `run_comment_asks` and
  both `aggregate_and_dispatch` sites; `keys.rs`'s
  `run_struggle_judge_and_show`).
- `sweep.rs`'s `judge_and_collect_finding` — its one DIRECT
  `memory::entry_rung_for` call (the mastered/"silenced" check, not routed
  through `resolve_entry_rung`) now reads `*ws.directness.lock_poison_safe()`
  inline, same fresh-read posture.
- `sweep::run_quiescence_worker`/`sweep_pending` no longer take a
  `directness` parameter at all (removed from the sweep-worker thread's
  spawn args in `watch::run` and every call in between) — nothing left to go
  stale; the worker's one long-lived `ws: Arc<WatchSession>` was already
  there.
- `keys.rs`'s `apply_offer_accept`/`handle_offer_key` similarly drop the
  `directness` parameter — `run_struggle_judge_and_show` takes `ws: &WatchSession`
  in place of its old `bucket: &Mutex<TokenBucket>` param (still reads
  `ws.bucket` internally, so the accepted-offer budget-consume behavior is
  byte-for-byte unchanged) and reads directness via `resolve_entry_rung`.
- `tui::run`/`tui::handle_key` drop the `directness: ladder::Directness`
  parameter entirely — the TUI reads/writes `ws.directness` directly now.

Verified: `handle_card_key`/`handle_offer_key`/every `apply_*` function
body is unchanged — only the SOURCE of directness moved (value-param →
`ws.directness` read); the resolved rung for a given directness value is
identical to before (same `ladder::compose_entry_rung`/`entry_rung_for`
logic, untouched). A change written by the settings overlay reaches the
sweep worker's very next pass and the TUI's very next render — no restart,
because both now read the same live `Mutex` instead of a value captured at
some earlier spawn/call moment.

**Frequency → live `TokenBucket`:** `budget::TokenBucket::set_refill_period`
(new) updates the bucket's `refill_period` in place without touching
`tokens`/`last_update` (clamped to the unchanged `capacity`, defensively) —
since `is_ready_at`/`time_until_ready_at` already project from
`self.refill_period` fresh on every call (T15's earlier "next nudge" work),
the ambient band's ETA reflects a frequency change on its very next render,
no separate signal needed. `WatchSession::frequency: Mutex<String>` (new)
holds the label (`"quiet"`/`"standard"`/`"chatty"`) the bucket itself
doesn't remember, seeded from `cfg.dial.frequency` alongside `directness`
above; the overlay's write path (`tui::apply_settings_cycle`) updates both
the label and the bucket's rate (via `noise::detent_for`) in the same call
so they can never drift apart.

**Pure cycle helpers, unit-tested:** `ladder::Directness::next`/`prev`
(guide-me → balanced → tell-me → guide-me, and the exact reverse) +
`as_str` (the config-string label, round-trips through
`directness_from_config`); `noise::next_frequency`/`prev_frequency` (quiet →
standard → chatty → quiet, degrading an unrecognized label to index 0 rather
than panicking, matching `detent_for`'s own fail-toward-quiet posture);
`budget::TokenBucket::set_refill_period` (asserts a shortened period
shortens `time_until_ready_at`'s ETA, and that tokens never exceed
capacity after the call).

**Overlay implementation:** `tui::app::Focus::Settings` (+
`SETTINGS_ROW_COUNT`, `App::settings_selected`) — summoned by `s` from the
idle Home surface (same `home_surface_is_idle` gate `m` uses — free while a
card/offer is on screen, matching the spec's "s is free" note), popped by
`s`/`esc`. `tui::view::settings_rows`/`draw_settings` render the two-row list
styled like the mastery list (`›` + `REVERSED` selection, no manual
borders/separators — matches this app's existing Mastery/Events convention
rather than the design mockup's literal ASCII box). Keybar chip `[s]
settings` added to the idle Home row; a `Focus::Settings`-specific keybar
(`↑/↓ select`, `←/→ change`, `s/esc home`); the `?` help overlay documents
`s` under its "mastery/settings are summoned" line.

**Deviations/assumptions:**
- No full-`Frame`/`TestBackend` render smoke test for `draw_settings` — the
  codebase has no existing `ratatui::backend::TestBackend` usage anywhere,
  and every other `Focus` overlay (Mastery/Events/ConceptDetail) is likewise
  untested at the `draw_*`/`DrawContext` level (`WatchSession::new` is
  private to the `watch` module, so `tui`'s own tests can't construct one
  without a visibility change nobody asked for) — only their pure data/line
  helpers are unit-tested. `settings_rows`/`draw_settings` follow the same
  established convention; the render itself is exercised by `cargo build
  --release` succeeding and by the pure cycle helpers it calls.
- `←/→` only (plus Enter for forward) — no `h`/`l` vi-alternate bindings were
  added for cycling (unlike `j`/`k` for up/down, which the spec's own
  wording called out); kept literal to the spec's "←/→ (and maybe Enter)"
  text rather than inventing an unrequested binding.
- The overlay's header-right text ("session only · not saved to
  config.toml") and the exact row/column widths are new visual choices not
  dictated by the spec's mockup; the mockup's literal separators
  (`──────...──────`) were not reproduced, matching the app's existing
  overlay convention (no other summoned view draws manual box-drawing rules
  either).

**Verification:** `cargo test --manifest-path Cargo.toml --
--test-threads=1` → 684 passed (lib) + 1 (integration test) + 0 (main), 0
failed; `cargo clippy --all-targets -- -D warnings` clean; `cargo build
--release` succeeds. No `TODO` markers.

## Follow-up added 2026-07-06 — comment-ask observability (dev-context + user-facing)

Founder ask: `// murshid: …` direct asks were a total black box —
`run_comment_asks` (`watch/sweep.rs`) answered every failure with a bare
`continue`: no trace, no event, no user notice. The DB confirmed zero
comment-ask cards had ever been created; the normal sweep judge already had
T14's trace + `judge_drop`/`judge_declined` events, but this channel had
neither. Three additive fixes, the SUCCESS path (card produced, `comment_ask`
event, suppression-clear, requeue-displaced) byte-for-byte unchanged:

**1. Dev trace (`watch/sweep.rs`, `trace_dir` threading):** `run_comment_asks`
now takes a `trace_dir: Option<&Path>` parameter (`sweep_pending` already had
one — threaded straight through, same discipline as the sweep's own
stage-1/stage-2 closures: never held across the dispatch, no lock crosses the
call). The `judge_slot.dispatch(Lane::Interactive, &prompt)` call is wrapped
in `crate::trace::record_dispatch(trace_dir, session_id_now, "comment-ask",
&judge_slot.provider, &judge_slot.model, "{file}:{line}", &prompt, &result)`
— a distinct `"comment-ask"` stage (never conflated with `"screen"`/`"judge"`)
whose `site_hint` is `{rel_str}:{comment_line}`, more precise than the sweep's
own file-only `site_hint` since a real `Site` is already known by dispatch
time here.

**2. `comment_ask_dropped` DB event (events-feed observability):** every
failure point now logs one via a new `log_comment_ask_dropped` helper
(payload: `reason`, `detail`, `site`, `question` — mirrors
`judge::judge_drop_payload`'s shape under its own event kind), with reasons:
`no_site` (comment has no enclosing item — `site::compute_site` returned
`None`), `dispatch_error` (+ the error string), and the three
`JudgeDropReason::reason_tag()` values reachable here (`parse_error`,
`missing_leg`, `non_taxonomy_concept`, `unverifiable_quote` — `declined` is
also possible and reachable, `reason_tag()`/`detail()` cover it uniformly).
**Deviation (physical constraint, not a choice):** the `no_db` failure (no
`Connection` at all) cannot log a DB event — there is nothing to log it
to — so that one case gets the user notice (below) only, not a DB row. This
is called out explicitly since the task text said "log them too" for both
`no_site`/`no_db`; `no_site` IS logged (a connection may still be present),
`no_db` cannot be by definition.

**3. User-facing notice, once per session per comment:** every failure ALSO
fires `ws.notice(...)` via a new `comment_ask_failure_notice(reason) ->
&'static str` — `"declined"` gets `"asked murshid about your comment — it
didn't find a clear teaching point there"`; every other reason (`no_site`,
`no_db`, `dispatch_error`, `missing_leg`, `non_taxonomy_concept`,
`unverifiable_quote`, `parse_error`) gets `"couldn't answer your murshid
comment just now (the model's reply didn't fit) — try rewording it"`.
Deduped via a new `notice_comment_ask_failure_once(ws, key, reason)` against
a new `WatchSession::comment_ask_noticed: Mutex<HashSet<String>>` field
(session-scoped, cleared at every session split alongside
`dispatched_hunk_signatures`/`queue_state` — same lifecycle as the repo's
other per-session dedup state) — `key` is the comment's own `comment_fp`
(the same identity the answered-comment ledger dedup already uses) wherever
one could be computed, or a plain `{file}:{line}:{question}` fallback for
the `no_site` case (no `Site`, so no real fingerprint yet). The dev
trace/event still logs EVERY attempt (by design — that's the diagnostic
value); only the user-facing line is throttled to once.

**Hard invariants held (verified):** the success path's event
(`comment_ask`), `pending_card` set, suppression-clear, and displaced-card
requeue are untouched; the answered-comment ledger dedup
(`find_ledger_card`/`card_exists_with_advice_fp`) still gates BEFORE any of
this new failure-path code runs, so an already-answered comment never
re-fires a drop event or a notice.

**Tests added (`watch/sweep.rs`):** the pre-dispatch failure points
(`no_site`, `no_db`) need no live dispatch at all, so they're driven END TO
END with real fixture content: `test_run_comment_asks_no_site_logs_event_
and_notices_once` (a bare top-level `// murshid: …` comment with no
enclosing item, swept twice — asserts 2 `comment_ask_dropped` events land
but the user notice fires exactly once) and
`test_run_comment_asks_no_db_notices_once_without_a_connection` (a real
enclosing item so `no_db` — not `no_site` — is the reason reached, `conn_opt
= None`, swept twice — asserts the once-only notice with no DB to assert an
event against). Plus two narrower unit tests:
`test_comment_ask_failure_notice_wording_by_reason` (declined vs.
everything-else wording) and `test_notice_comment_ask_failure_once_dedups_
by_key` (same key notices once, a different key notices independently).
**Deviation (unavoidable, per the pre-existing NOTE this task's own text
didn't account for):** the post-dispatch branches
(`dispatch_error`/`parse_error`/`missing_leg`/`unverifiable_quote`) still
have no injectable-fixture seam — `run_comment_asks` dispatches through a
live `&crate::ResolvedSlot` (real curl), exactly the constraint the
pre-existing "ROADMAP item 13" NOTE already documented for this function.
Driving those branches would need either a live network call or a
production seam change (an injectable dispatch closure, mirroring
`dispatch_stage1`/`dispatch_stage2`'s pattern) — judged out of scope for
this observability-only pass per the task's own "no TODO markers / don't
guess, report options" instruction; the NOTE comment above the test module
was updated to say so precisely rather than silently narrowing coverage.

**Verification:** `cargo test --manifest-path Cargo.toml --
--test-threads=1` → 692 passed (lib) + 1 (integration test) + 0 (main), 0
failed; `cargo clippy --all-targets -- -D warnings` clean; `cargo build
--release` succeeds. No `TODO` markers.

## Follow-up added 2026-07-06 — comment-ask attempted-marker (perf/cost fix)

Founder ask (review-gate flagged): the observability follow-up above traced
and noticed comment-ask failures, but never stopped re-dispatching them — a
comment that keeps FAILING (judge declines / can't ground / dispatch error)
is never ledger-deduped the way an ANSWERED comment is (`find_ledger_card`/
`card_exists_with_advice_fp`), so `run_comment_asks` re-hit the model with a
live judge call on EVERY sweep (every save) for as long as the comment sat
there failing — wasteful (cost) and pointless.

**Fix, reusing existing state (no new field):** `WatchSession::
comment_ask_noticed` (the `HashSet<String>` already populated on every
comment-ask failure, keyed by `comment_fp` or the `{file}:{line}:{question}`
`no_site` fallback, via `notice_comment_ask_failure_once`) now doubles as the
attempted-marker. Two new gates in `run_comment_asks` (`watch/sweep.rs`),
placed BEFORE any log/notice/dispatch for that comment on a given sweep:
- In the `no_site` branch: `if ws.comment_ask_noticed.lock_poison_safe().
  contains(&fallback_key) { continue; }`, checked right after computing
  `fallback_key`, before `log_comment_ask_dropped`/`notice_comment_ask_
  failure_once`.
- In the main path: `if ws.comment_ask_noticed.lock_poison_safe().
  contains(&comment_fp) { continue; }`, placed immediately after the
  existing `already_answered || already_known_this_session` gate and BEFORE
  the `no_db` check / the live `judge_slot.dispatch(...)` call — so it
  covers `no_db`, `dispatch_error`, and every post-dispatch parse/validate
  failure reason uniformly, since they all share the same `comment_fp` key.

No new `HashSet`, no ordering problem: `notice_comment_ask_failure_once`
already inserts its key unconditionally (`HashSet::insert`) on every call,
so by the comment's SECOND sweep the key from its first failure is already
present — the new gates just check it before doing anything else.

**Retry semantics:** editing the comment's text changes the question →
a new `comment_fp` (main path) or fallback key (`no_site` path) → not yet
in the set → dispatches/attempts again. This is the intended retry path
(the user rewords to retry). A code change that moves the enclosing item
also changes the fp (site shifts) → re-tries too; accepted, per the task's
own allowance. Session-scoped only: `comment_ask_noticed` is already
cleared on session split (`handle_session_split`), so a new session
re-attempts once — unchanged.

**Untouched:** the success path (card produced, `comment_ask` event,
`pending_card`, suppression-clear, requeue) and the answered-comment
ledger dedup are byte-for-byte unchanged — the attempted-marker only
short-circuits comments that already failed THIS session. The first
failure for any comment still logs `comment_ask_dropped` + notices once,
exactly as before; only the *re*-attempt on later sweeps is now skipped
(no re-dispatch, no re-logged event, no re-notice).

**Tests (`watch/sweep.rs`):** the pre-existing `test_run_comment_asks_
no_site_logs_event_and_notices_once` (swept twice) had its assertion
corrected — the comment "the dev-facing event logs EVERY attempt" was
accurate before this fix and is no longer; it now asserts exactly 1
`comment_ask_dropped` event (the marker skips the second, identical
sweep). A new sibling, `test_run_comment_asks_attempted_marker_skips_
repeat_failure_but_retries_on_text_change`, drives the `no_site` path
(drivable end to end with real fixture content, no live dispatch involved)
across three sweeps: pass 1 fails/logs/notices; pass 2 (identical comment)
is a total no-op (event count and notice count both stay at 1); pass 3
(edited question text) is a fresh, unguarded failure (event count and
notice count both grow to 2) — proving the marker gates repeats but never
blocks a genuine retry.

**Deviation/limit (same physical constraint as the observability
follow-up):** the post-dispatch failure reasons (`dispatch_error`/
`parse_error`/`missing_leg`/`unverifiable_quote`/`declined`) share the exact
same `comment_ask_noticed`-keyed gate as `no_db`, but still have no
injectable-fixture seam to prove the SKIP end to end without a live
network dispatch (same constraint the observability follow-up's own test
notes document) — covered instead by the `no_site`/`no_db` paths (both
reach the identical gate, just via the fallback key vs. `comment_fp`) plus
the pre-existing `test_notice_comment_ask_failure_once_dedups_by_key` unit
test proving the underlying `HashSet` semantics the gate relies on.

**Verification:** `cargo test --manifest-path Cargo.toml --
--test-threads=1` → 693 passed (lib) + 1 (integration test) + 0 (main), 0
failed; `cargo clippy --all-targets -- -D warnings` clean; `cargo build
--release` succeeds. No `TODO` markers.

## Follow-up added 2026-07-06 — HISTORY view (`h`): persist the card body,
## scroll back through past cards + threads

Founder decisions (2026-07-06, MUR-7): persist the card's own prose so
HISTORY re-shows the FULL original card, not just metadata; open with `h` as
a full-screen overlay (not a split pane); recent cards are cross-session,
bounded to ~50 newest; keep the existing `E` events view unchanged
(HISTORY is a separate, readable card+thread reader, not a debug feed).

**1. Persisting the card body (migration 13 + `insert_card` + reads,
`db/migrations.rs`, `db/cards.rs`):** an additive, nullable
`cards.card_body_json TEXT` column (`ALTER TABLE`, `PRAGMA user_version =
13`) — old rows read back `NULL`, never an error. A new
`db::PersistedCardBody { concept_name, grounding_quote, why, rule, doc_ref,
category }` (serde, plain struct — no ratatui) + `db::card_body_json(card:
&card::Card, category: &str) -> Option<String>` serialize it; `CardRecord`
gained one field, `card_body_json: Option<String>`, written straight into
the new column by `insert_card_stmt`.

*Deviation from the task text, reasoned explicitly:* the task's
investigation note pointed at ONE call site (`watch/keys.rs`'s
`run_struggle_judge_and_show`, ~line 110) as "where `insert_card` lives."
That function is actually just one of several PRODUCTION sites that insert
a card with a real display body — the vast majority of real-world cards are
shipped by `watch/sweep.rs`'s normal save→judge sweep, not the struggle-offer
path. Persisting the body at only that one site would have left HISTORY
showing "(card text not recorded)" for nearly every card a user actually
sees, defeating Decision 1's literal purpose ("re-shows the FULL original
card"). So the write was added at every production site that has a real
`card::Card` value in hand at `insert_card` time:
`watch/keys.rs::run_struggle_judge_and_show` (struggle-offer-accepted card),
`watch/sweep.rs::run_comment_asks` (murshid-comment direct-ask card),
`watch/sweep.rs::aggregate_and_dispatch`'s `enqueue_finding` closure (queued
card — excluded from `recent_cards` today, but its row is the SAME one a
future pull-into-slot would flip to `shown`, so the body is there already)
and its shown-push branch, and `lib.rs::persist_review_digest` (`murshid
review`'s digest cards). Two production sites deliberately get `None`:
`watch/offers.rs`'s struggle-OFFER card (a bare prompt with no `Card` at
all — nothing to persist) and every `#[cfg(test)]` fixture (14 call sites,
mechanically given `card_body_json: None` — behavior-neutral). No site's
existing INSERT/status/event logic changed; this is purely an additive field
threaded from an already-computed `Card` value already in scope at each
call.

**2. The two read queries (`db/cards.rs`):** `db::recent_cards(conn, limit:
i64) -> Vec<CardListRow>` — `id, concept_id, category, status, rung_shown,
created_ts, resolved_ts` + a correlated `thread_turns` count +
`worked_diff IS NOT NULL`, `WHERE status NOT IN ('queued', 'collapsed')
ORDER BY id DESC LIMIT ?`, cross-session (no `session_id` filter) per the
founder's decision. `db::card_detail(conn, card_id) -> Option<CardDetail>`
— the same metadata + `worked_diff` + the deserialized
`Option<PersistedCardBody>` (a malformed/missing `card_body_json` degrades
to `None`, never an error — "never trust stored TEXT blindly," this view
layer's existing posture).

**3. The `h` overlay (`tui/app.rs`, `tui/mod.rs`, `tui/view.rs`):**
`Focus::History` (the list) + `Focus::HistoryDetail(i64)` (holds the
card id, mirrors `ConceptDetail(String)`'s shape) added to the `Focus`
enum. `App` gained `history_selected: usize` (clamped at read time via
`history_selected_clamped(len)`, same posture `draw_mastery`/`draw_events`
already use inline) and a private `history_scroll: u16` (saturating
up/down, reset to `0` whenever `open_history_detail(card_id)` pushes a new
card's detail — a stale scroll position never leaks across cards).

Key routing (`tui/mod.rs::handle_key`): `h` opens `Focus::History` from
Home — placed alongside `G`/`E` in the ALWAYS-available block (not gated on
`home_surface_is_idle` the way `m`/`s` are), since lowercase `h` was
already free (`response::classify_card_key('h')` → `Ignore`, verified by
reading `response.rs`; it was never a card action, so this adds no
collision). Inside `Focus::History`: `↑/↓`/`j`/`k` move the selection
(unclamped store, clamped at read — same convention as Mastery/Events),
`⏎` re-fetches `view::history_rows(conn)` and opens
`app.open_history_detail(row.id)` for the clamped-selected row, `h`/`esc`
→ `app.go_home()`. Inside `Focus::HistoryDetail`: `↑/↓`/`j`/`k` scroll the
transcript, `esc` → `app.pop_focus()` (back to the list, exactly as the
founder specified — no `h`-jumps-home shortcut was added inside detail,
since the task text named only `esc` there). Each arm `return`s immediately,
the same "no other keys leak" pattern every existing overlay arm follows.

Render (`tui/view.rs`): `draw_history`/`draw_history_detail` occupy the
full surface region between the header and the ambient band/keybar —
exactly like Mastery/Events/Settings/ConceptDetail (a genuine full-screen
overlay, never a split pane, per Decision 2). Two PURE helpers back them:
`history_row_line(row: &CardListRow, now_epoch, use_color) -> Line` (`{age}
{status} {concept} · {category}` + a `{n}↩` thread marker only when
`thread_turns > 0`; meaning survives `use_color=false`) and
`history_detail_lines(detail: &CardDetail, msgs: &[ThreadMessage],
now_epoch, use_color) -> Vec<Line>` (category + concept-name header, status/
rung/age line, the body's grounding-quote gutter + why + labeled `Rule` +
`→ doc_ref` — or a dim "(card text not recorded)" note when `body` is
`None` — the worked diff with the same `+`/`-` green/red convention the
live card's worked-example rendering already uses, then the thread
transcript interleaved in turn order or a "(no thread messages)" note).
`draw_history_detail` scrolls via `Paragraph::scroll((app.history_scroll(),
0)).wrap(Wrap { trim: false })`, the same idiom the goal editor already
uses. `HISTORY_LIST_LIMIT = 50` (the founder's "~50 newest") is the one
shared constant both `draw_history` and the `⏎` handler read through
`view::history_rows(conn)`, so the rendered list and the selectable list
are provably always the same rows.

Keybar (`draw_keybar`) gained `Focus::History` (`↑/↓ move · ⏎ open · h/esc
home · q quit`) and `Focus::HistoryDetail` (`↑/↓ scroll · esc back · q
quit`) arms, plus an `[h] history` chip on the idle Home keybar (alongside
`m`/`s`) — the card keybar (rung-aware set) is untouched, per the founder's
explicit "do NOT add it there." The `?` help overlay gained one line under
the summoned-overlays section. `E`/events is untouched — same code, same
keys, same behavior, exactly as decided.

**Hard invariants held (verified, not just asserted):** no ratatui/
crossterm type appears in `db`/`card`/`watch` (`CardListRow`, `CardDetail`,
`PersistedCardBody` are plain/serde structs in `db::cards`); the migration
is additive/back-compat (nullable column, `test_migration_13_adds_
nullable_card_body_json_column` proves a fresh insert with no body reads
back `NULL`); no lock is held across a DB query in the new routing/render
path (`history_rows`/`card_detail`/`get_thread_messages` are plain
`conn`-scoped calls, no `WatchSession` mutex involved at all — HISTORY
reads only `profile.db`); `handle_card_key`/`handle_offer_key`/every
`apply_*` function, the async offer-accept `BusyGuard`, mentor-state, the
parse gate, the rung-aware card keybar, the card-height wrapping fix, and
the comment-ask path are all byte-for-byte unchanged — the only production
behavior change anywhere outside `tui/` is the additive `card_body_json`
write at the five insert sites named above.

**Tests added:** `db/migrations.rs`:
`test_migration_13_adds_nullable_card_body_json_column`. `db/cards.rs`:
`test_recent_cards_newest_first_excludes_queued_and_collapsed_counts_
threads`, `test_recent_cards_respects_limit`,
`test_card_detail_round_trips_a_persisted_body`,
`test_card_detail_handles_null_body_gracefully`,
`test_card_detail_none_for_unknown_id`. `tui/app.rs`:
`test_history_focus_pushes_and_pops_like_mastery`,
`test_history_selected_clamped`,
`test_open_history_detail_pushes_focus_and_resets_scroll`,
`test_history_scroll_up_saturates_at_zero`. `tui/view.rs`:
`test_history_row_line_shows_age_status_concept_category`,
`test_history_row_line_shows_thread_marker_only_when_present`,
`test_history_row_line_missing_timestamp_degrades_to_dash`,
`test_history_detail_lines_renders_body_and_worked_diff`,
`test_history_detail_lines_null_body_shows_not_recorded_note`,
`test_history_detail_lines_empty_thread_shows_a_note`,
`test_history_detail_lines_interleaves_thread_turns_in_order`. No
`Frame`/`TestBackend` render test for `draw_history`/`draw_history_detail`
themselves — same established gap every other `draw_*` in this module has
(no `WatchSession` constructor visible outside `watch`, no `TestBackend`
precedent anywhere in the crate); the pure line-builders they call are
fully covered instead, matching the codebase's existing convention exactly.
`h`-from-Home routing is verified by reading `handle_key`'s code path (it
touches only `app`, never `ws`) rather than a dedicated test, the same
untested-at-this-level posture `G`/`E`'s own routing already has (no
precedent test exists for those either, for the same `WatchSession::new`
visibility reason).

**Verification:** `cargo test --manifest-path Cargo.toml --
--test-threads=1` → 713 passed (lib) + 1 (integration test) + 0 (main), 0
failed; `cargo clippy --all-targets -- -D warnings` clean; `cargo build
--release` succeeds. No `TODO` markers.

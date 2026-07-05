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
- **Goal editing (`g` with no pending card)**: dropped. The old loop's
  $EDITOR handoff (suspend the pane, shell out, resume) isn't wired for the
  TUI's alternate-screen/raw-mode state; the dashboard shows the goal as
  read-only text. Editing still works via `murshid goal <text>` outside the
  TUI.
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

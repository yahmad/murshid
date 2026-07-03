# T11 — Provider dispatch lanes (kill the fixed debounce + cross-abort)

**Status:** Approved (2026-07-03, queued after T10) · **Traces to:** SPEC v0.5
C6 (model seam), C12 (latency/noise); 2026-07-03 architecture review finding 3.

## Why
Every provider call pays an unconditional 1.5s busy-sleep (≈3s dead time per
save for screen+judge), and one process-global `ACTIVE_CONN`/
`CURRENT_REQUEST_ID` means ANY dispatch aborts ANY other in-flight one: a
stdin-thread action (review `r`, thread turn `k`, recall grading) racing the
watcher's stage-1/stage-2 mutually cancels as "Aborted", surfacing as silently
dropped judgments. Quiescence gating already de-bounces typing bursts; the
provider-level sleep is pure latency.

## Requirements
1. Remove the fixed 1.5s pre-dispatch sleep entirely. No replacement delay at
   the provider layer (the event pipeline's quiescence gate owns burst
   coalescing).
2. Introduce two dispatch lanes, e.g. `enum Lane { Sweep, Interactive }`, each
   with its OWN request-id counter and active-child slot:
   - `Sweep` — watcher-driven stage-1/stage-2 card judging. A new Sweep
     dispatch supersedes (aborts) only the in-flight Sweep dispatch.
   - `Interactive` — user-initiated calls (review digest, thread turns,
     struggle judge, comment-asks, recall grading). Never aborted by Sweep;
     serialized among themselves (a second Interactive call waits or aborts
     the prior Interactive one — pick waiting; user actions must not
     self-cancel).
3. After `wait_with_output` returns, re-check the lane's request id: a
   superseded result is dropped (return the existing "Aborted" error), never
   delivered as fresh.
4. Every `dispatch_debounced*` call site names its lane explicitly; no default
   that silently picks one.
5. Existing abort mechanics (SIGKILL of the curl child) and the T8 request
   builder/parse path are unchanged.
6. Tests: (a) Interactive dispatch survives a concurrent Sweep supersede;
   (b) newer Sweep aborts older Sweep; (c) superseded result is dropped after
   completion; (d) no fixed sleep — a lone dispatch on an idle lane completes
   without artificial delay (bound: well under 1s excluding transport).
   Use a fake transport/local listener, `--max-time`-bounded; never depend on
   external services.

## Out of scope
- Moving heavy work off the watcher event-delivery thread / event coalescing
  via channels (architecture finding 5 — separate future task).
- Config knob for debounce (the removed sleep is not made configurable).
- Async runtime; new dependencies.

## Acceptance
- `cargo test` green (including new lane tests, parallel-safe).
- Grep: no `sleep(100)`-loop debounce remains in provider.rs.
- Dogfood sketch: with `watch` running, pressing `r` (review) while a save's
  judging is in flight yields BOTH results; a rapid double-save yields one
  judged result (the newer), with the older logged as superseded, not shown.

# T12 — Watch-arm decomposition (behavior-preserving)

**Status:** Approved (2026-07-03, queued after T11) · **Traces to:**
2026-07-03 architecture review findings 1, 7, 8. Pure code motion: ZERO
behavior change is the contract.

## Why
`fn main()` is ~2,550 lines; the `watch` arm alone ~2,300 with three giant
inline closures (stdin key handling ~615 lines, struggle-offer poll ~170,
file-event sweep ~1,200) sharing ~18 separately-cloned `Arc<Mutex<…>>`s.
Business logic (tiered snooze, slot contention, comment-ask flow) lives at
main.rs altitude, unreviewable and untestable in isolation.

## Requirements
1. `WatchSession` struct (new module) bundling the shared watch-loop state —
   one `Arc<WatchSession>` clone per thread replaces the `_for_stdin`/
   `_for_poll` clone lists. Interior `Mutex`es stay per-field where they are
   today (do NOT coarsen lock granularity — that would change contention
   behavior).
2. Extract the three closure bodies into `src/watch/keys.rs`,
   `src/watch/offers.rs`, `src/watch/sweep.rs` (module `src/watch/mod.rs`
   holding `WatchSession`). Function-by-function motion with identical
   logic; rustfmt the moved code but make no logic edits, however tempting —
   defects found during motion get logged in the T12 spec's addendum for a
   later task, not fixed inline (review will diff for semantic drift).
3. The duplicated blocks (finding 7) become shared helpers as part of the
   motion: review-digest persistence (review arm + `r` key), goal
   re-resolution (session start + split), PendingCard site-identity
   re-derivation (key handler + sweep).
4. `src/cli/` becomes a real module: `src/cli/mod.rs`; move the inline
   `progress` (~45 lines) and `review` (~130 lines) arm bodies into it
   alongside setup/goal; the two `#[path]` mounts in main.rs go away
   (pack.rs's seam-mandated `#[path]` mounts are untouched).
5. `main.rs` after: command dispatch + thin `watch` orchestration only;
   target ≤ 1,000 lines including its retained unit tests. Existing unit
   tests move with their code, unmodified beyond `use` paths.

## Out of scope
- Any behavior/logic/timing change (T11 owns dispatch; T9 owned fixes).
- Lock-granularity changes; new abstractions beyond `WatchSession`.
- The keep-alive/signal handling (T9 req 3 already restructured it).

## Acceptance
- `cargo test` green with the SAME 490+ test count (moves, no deletions;
  new tests allowed only for the three extracted shared helpers).
- `main.rs` ≤ 1,000 lines; no `#[path]` mounts left in main.rs.
- Reviewer gate: side-by-side diff audit confirms pure motion (identical
  logic per extracted function), explicitly hunting for dropped edge cases
  in the moved key-handler state machine.

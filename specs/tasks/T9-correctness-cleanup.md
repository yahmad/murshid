# T9 — Correctness cleanup (post-review hardening)

**Status:** Draft (pending founder go) · **Traces to:** 2026-07-03 founder-requested
cleanup pass (3 independent reviews); SPEC v0.5 C1 (local-first reliability).

## Why
Three independent reviews of the delegated-agent code found correctness debt
concentrated in concurrency, recovery paths, and dead machinery. This task fixes
the mechanical, behavior-preserving items. It does NOT touch the two larger
findings (watch-arm decomposition; provider debounce redesign) — those get their
own specs, and pack selection (Go reachability) is a separate founder decision.

## Requirements
1. compiler.rs: remove the `MutexGuard` transmute; store `&'static Mutex<()>`
   via `Box::leak` in the lock map (entries are already immortal).
2. backup.rs: `defaults write … -string <json>` so macOS progress backups
   actually write; wire `save_backup_from_db` into the production path
   (post-migration + session bookend), not just tests.
3. Signal safety: SIGINT/SIGHUP handlers set an AtomicBool only; the main
   keep-alive loop polls and runs cleanup on a normal thread. Delete the
   never-started credentials hot-reload poller OR wire `credentials::init()`
   and re-resolve keys per dispatch — pick ONE (default: delete; BYOK rotation
   can restart).
4. watcher_coordinator: compare_exchange loop in acquire_resources (fixes
   check-then-act over-admission); saturating release (fixes u32 underflow);
   rate-limit the target/-size walk to one per N minutes.
5. db.rs restore path: replace `let _ = execute_with_retry(...)` (and the two
   corrupt-rename siblings) with eprintln-on-Err.
6. curl argv key leak: pass URL and auth headers via `curl --config -` on
   stdin; no key material in argv. (Applies to gemini/claude arms post-T8.)
7. main.rs duplication: extract review-digest persistence, goal re-resolution,
   and PendingCard site-identity re-derivation into shared helpers (3 fns).
8. Daemon lock hygiene: in watcher/stdin/poll threads only, replace
   `.lock().unwrap()` with `.lock().unwrap_or_else(|e| e.into_inner())`.
9. Test hygiene (from test review, final list pending): keyring tests must
   never touch the real `murshid` service; SIGHUP order-dependent test fixed;
   `test_debounce_cancellation` gets `--max-time`/fake transport.

## Out of scope
- Watch-arm decomposition (T10 candidate), provider debounce/abort redesign
  (T11 candidate), pack selection key (founder decision), clippy cosmetics.

## Acceptance
- cargo test green; no transmute in src/; backup round-trips on macOS
  (write→read integration test); zero `let _ =` on recovery-path calls;
  `ps`-visible argv of spawned curl contains no key material (test via
  build_provider_request → config-file body assertion).

## Addendum (2026-07-03, post-draft evidence)

Req 9 mechanism confirmed live: with a second `cargo test` running in another
checkout (reviewer worktree), the credentials tests fail cross-process — both
suites mutate the same real keychain service (`murshid_test`); the first
failure poisons the shared key-cache lock and cascades PoisonError through the
module. Fix: per-process-unique service name (e.g. `murshid_test_<pid>`) plus
`unwrap_or_else(|e| e.into_inner())` on the cache lock in tests. The pipeline
full-flow fixture test also failed in the same run (mechanism unconfirmed —
fixture-pure, so likely secondary interference; diagnose while fixing).

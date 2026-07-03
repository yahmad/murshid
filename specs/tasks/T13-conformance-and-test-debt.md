# T13 — T5 conformance + high-value test debt (draft)

**Status:** Draft — queued after T12 under standing authorization (spec-settled
conformance); founder may veto. **Traces to:** T5 spec (retrieval gate), 2026-07-03
test-suite review (final report).

## Why
The test review found run_retrieval_questions (main.rs ~834) gates only on cap +
staleness — T5's "skip concepts naturally encountered in the last session" gate is
missing (spec violation). Plus the highest-value test gaps protecting core mechanics.

## Requirements
1. Implement the T5 last-session-encounter gate in retrieval selection; unit test both sides.
2. Applied-detection false-positive tests: cosmetic rename of the flagged statement must NOT
   read as applied (site.rs recheck); file rename/move identity behavior pinned by test.
3. Transport seam: extract provider's curl execution behind an injectable transport fn;
   add ONE end-to-end watch-pipeline test (save → diff → quiescence → judge_hunks with fixture
   dispatch → card persisted) in tests/ (first real integration-test target).
4. memory.rs fade_announced test actually reopens the DB (file-backed, not in-memory reuse).
5. concept_memory migration-11 schema test; open-card guard exercised with status "queued".
6. db.rs insert_thread_message: docstring claims event emission the body doesn't do — make the
   doc true or move it; assert thread_msg event in a test either way.
7. Loosen copy-pinned brittle asserts flagged by review ONLY where they broke ≥once (defer rest).

## Out of scope
- The remaining T4 event-emission coverage list (log as backlog in this spec's addendum).
- BKT numeric-edge tests beyond prior clamp + repeated-fail monotonicity.

## Addendum (post-T11 gate + implementer observations, 2026-07-03)
8. Residual keyring-test flake: with the unconditional cfg!(test) mock, any
   remaining test_keyring_get_set_delete failure can only come through the
   `use_keychain` gate in load_keys_from_source (config/env reads) — diagnose
   by asserting use_keychain==true inside the test before the set/get pair.
9. provider.rs Interactive lane `serialize.lock().unwrap()` — poison would
   wedge the lane; mitigated today by catch_unwind at judge::safe_dispatch
   call sites; switch to into_inner() for defense in depth.
10. Note (documented behavior, not a bug): Sweep "abort" is effectively
    supersede-and-drop — the in-flight curl usually runs to completion
    (bounded by max-time) with its result dropped by the req-id recheck;
    the child is rarely in the slot when abort_lane fires (T9-preserved
    registration pattern).

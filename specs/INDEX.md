# Spec Index

The specification is the source of truth. Implement against the active spec;
do not invent scope beyond it.

**Design authority:** `~/src/yahmad/dev-context/specs/murshid/SPEC.md` v0.5
(Phase 2 ratified 2026-07-03 + Core contracts C1–C12). Task specs below are
derived from it; on conflict, SPEC v0.5 wins and the task spec gets fixed.

**Re-seeded 2026-07-03** (founder directive): the 0001–0028 spec series from
the archived v6.0 effort was removed — surviving infrastructure is inventoried
in [FOUNDATIONS-INHERITED.md](FOUNDATIONS-INHERITED.md); stale subsystems
(LSP suite, Socratic redactor/state machine, bypass/eval/register) were cut
from code the same day. Depth-first ladder: each task ships a user-visible
increment; "all boxes done" is explicitly not a success state.

| ID | Title | Status | File |
|----|-------|--------|------|
| T1 | Watcher-to-card vertical slice | Done (2026-07-03; reviewed 2×) | [T1-watcher-to-card-slice.md](tasks/T1-watcher-to-card-slice.md) |
| T2 | Noise machinery (budget knob, dedup, snooze, throttle, queue) | Done (2026-07-03; reviewed 2×) | [T2-noise-machinery.md](tasks/T2-noise-machinery.md) |
| T3 | Session goals & struggle prompts | Done (2026-07-03; reviewed 2×) | [T3-goals-and-struggle.md](tasks/T3-goals-and-struggle.md) |
| T4 | Card interaction (ladder, threads, murshid-comments, review) | Done (2026-07-03; reviewed 2×) | [T4-card-interaction.md](tasks/T4-card-interaction.md) |
| T5 | Memory (BKT, open meter, rung wiring) | Done (2026-07-03; reviewed 2×) | [T5-memory.md](tasks/T5-memory.md) |
| T6 | Language-pack seam extraction | Done (2026-07-03; reviewed 2×) | [T6-pack-seam.md](tasks/T6-pack-seam.md) |
| T7 | Go pack (the architecture-honesty test: zero engine edits) | Done (2026-07-03; I29 SATISFIED) | [T7-go-pack.md](tasks/T7-go-pack.md) |
| T8 | OpenAI-compatible local provider (BYOK seam widening) | Done (2026-07-03; E2E-verified vs live stub; reviewed, PASS) | [T8-openai-compat-provider.md](tasks/T8-openai-compat-provider.md) |
| T9 | Correctness cleanup (post-review hardening) | Done (2026-07-03; gated FAIL→fix→PASS; 492 tests) | [T9-correctness-cleanup.md](tasks/T9-correctness-cleanup.md) |
| T10 | Pack auto-detection (Go pack reachable) | Done (2026-07-03; gated PASS; Go E2E verified live) | [T10-pack-autodetect.md](tasks/T10-pack-autodetect.md) |
| T11 | Provider dispatch lanes (no fixed debounce, no cross-abort) | Done (2026-07-03; gated PASS; 514 tests) | [T11-dispatch-lanes.md](tasks/T11-dispatch-lanes.md) |
| T12 | Watch-arm decomposition (pure motion) | In progress (launched 2026-07-03) | [T12-watch-decomposition.md](tasks/T12-watch-decomposition.md) |

**Active spec for current work:** [T12-watch-decomposition.md](tasks/T12-watch-decomposition.md) (T9→T10→T11→T12 serial; Sonnet implementers, Fable review gates — founder ruling 2026-07-03)

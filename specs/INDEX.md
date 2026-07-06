# Spec Index

The specification is the source of truth. Implement against the active spec;
do not invent scope beyond it.

**Design authority:** `~/src/yahmad/dev-context/specs/murshid/SPEC.md` v0.5
(Phase 2 ratified 2026-07-03 + Core contracts C1–C12). Task specs below are
derived from it; on conflict, SPEC v0.5 wins and the task spec gets fixed.
*(That SPEC lives in a private, machine-local repo and is not accessible from
here — the `specs/` in **this** repo are its public derivation and the
source of truth for outside readers.)*

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
| T12 | Watch-arm decomposition (pure motion) | Done (2026-07-03; drift-audit clean; gated fix→PASS) | [T12-watch-decomposition.md](tasks/T12-watch-decomposition.md) |
| T13 | T5 conformance + high-value test debt | Done (2026-07-03; gated PASS; 526 tests incl. first integration test) | [T13-conformance-and-test-debt.md](tasks/T13-conformance-and-test-debt.md) |
| T14 | Judge observability (trace capture, declined-vs-failed split, outcome-rate query) | Done (2026-07-05; gated PASS; 605+1 tests, clippy clean) | [T14-judge-observability.md](tasks/T14-judge-observability.md) |
| T15 | TUI (gitui-inspired full-screen watch UI; replaces the classic pane) | Done — MERGED to main 2026-07-06 (`c45707a`; gated PASS; 713+1 tests, clippy clean). "Focus" UX redesign + heavy dogfood hardening: card-as-hero, live mentor-state, live settings (`s`), goal edit (`G`), rung-aware keybar, wrapped/readable text, comment-ask observability + accurate notices, and the `h` HISTORY overlay (scroll back through past cards + threads; migration 13 persists the card body). | [T15-tui-spike.md](tasks/T15-tui-spike.md) |
| TUI redesign r2 | "One-living-workspace" evolution of the Focus TUI | Done — MERGED to main 2026-07-06 (`988be41`; 6 rungs R0–R5, each implementer→adversarial-gate PASS; 794+1 tests, clippy clean). Settings persist to config.toml (R0); keybar honesty + `?`-everywhere + esc-spring + bug fixes (R1); `Tab`-toggled rail + persistent mode token (R2); settings-as-transient + `:` command palette (R3); card why-it-spoke + ladder legibility + first-run/welcome-back (R4); `ask`-mode conversational follow-up (R5). Proposal: [explorations/tui-redesign-2-workspace.md](explorations/tui-redesign-2-workspace.md). | [explorations/tui-redesign-2-workspace.md](explorations/tui-redesign-2-workspace.md) |

| T16 | Mentor-grade struggle perception + model-directed context | **In progress** — founder go 2026-07-06; building serial+gated on branch `yasir/t16-struggle` (not yet merged). T16a model-directed judge context (done, gated PASS incl. a symlink-escape fix); T16b edit-log + temporal perception (done, gating); T16c struggle-response rebuild + T16d(opt) to come. Edit-log + temporal perception (understands *when/what* you're stuck on) + model-directed judge context (pulls the caller/type/other file). | [T16-mentor-struggle-perception.md](tasks/T16-mentor-struggle-perception.md) |

**Active spec for current work:** T16 (founder go 2026-07-06) — building on branch `yasir/t16-struggle`; T16a+T16b done, T16c/d remaining. Not yet merged to main.

## Amendments

- **C10 (dependency allowlist) — ✅ RATIFIED 2026-07-06 (founder go), shipped via
  the T15 merge (`c45707a`).** C10 now permits exactly `ratatui` (0.29) +
  `crossterm` (0.28) as a FRONTEND-only pair (syntax-highlighting / syntect
  explicitly excluded). Confinement invariant: ratatui/crossterm types live only
  under `src/tui/`; the engine and `lib.rs` domain modules never depend on them,
  and the language-pack/provider seams are unchanged. Everything else stays on
  the stdlib + prior C10 allowlist.
- [AMENDMENT-events-retention.md](AMENDMENT-events-retention.md) — ✅ RATIFIED &
  shipped 2026-07-04: bounds the struggle baseline (D15) to a 90-day window and
  the cross-session decline count (T3 req 13) to a 180-day window, with an
  events/`context_history` prune + `VACUUM` at watch startup (ROADMAP item 3).
  Follow-up: mirror the window into the design-authority `SPEC.md`.

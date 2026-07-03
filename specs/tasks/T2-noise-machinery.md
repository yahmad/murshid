# T2 — Noise machinery

**Status:** Planned · **Traces to:** SPEC v0.5 D10/D11/D12, I3/I5, C3/C7/C12.
Frequency knob (3 detents: budget+floor pairs per C12) + directness knob
storage (used in T4/T5); pull queue with presence indicator and `m` browse;
C7 ordering (goal-relevance slot wired in T3); tiered snooze (instance →
concept-for-session, cap 50); never-re-raise ledger (resolved cards) +
regression re-open (C8); category auto-throttle (action rate < 15%/20 cards
→ queue-only, notice + one-tap undo); `suppressions` table per C5.
Acceptance sketch: budget/floor honored per detent; second `not_now` on a
concept parks it with notice; throttled category never pushes; all state
observable in events.

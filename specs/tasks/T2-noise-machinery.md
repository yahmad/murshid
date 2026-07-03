# T2 — Noise machinery

**Status:** Planned (next after T1 closes)
**Traces to:** SPEC v0.5 — D10/D11/D12, I2/I3/I5/I7, C1/C3/C5(suppressions)/C7/C12.
**User outcome:** the mentor stays welcome: the founder sets one knob, never
sees the same advice twice, can park topics with two keys, and watches a
category that annoys him go quiet by itself — with every suppression visible
and reversible.

## Scope

IN: frequency knob (3 detents), pull queue + presence indicator + browse,
C7 ordering, within/cross-session dedup, tiered snooze, never-re-raise
ledger + regression re-open, category auto-throttle, `suppressions` table.
OUT: directness knob semantics (T4 ladder), goal-relevance ordering input
(T3 — the ordering slot ships here reading a constant), memory-driven
cooldowns beyond session scope (T5).

## Requirements

### Frequency knob (D10, C12)
1. `[dial] frequency = quiet|standard|chatty` in config (default `quiet` —
   I7 ship-chill overrides T1's hardcoded standard). Detents set the
   (budget, floor) pair per C12: quiet 1/20min + {bug, goal-relevant idiom};
   standard 1/10min + {bug, idiom, best-practice}; chatty 1/5min + all.
   Until T3 lands, "goal-relevant idiom" degrades to "idiom".
2. Floor-excluded and over-budget advice is never dropped: it queues (I5).
   Likely-bug strict-mode bypass (T1 req 11) is unchanged.

### Queue (I5, C7)
3. Single pull queue per session; presence indicator is one line
   ("N more thoughts — m"). `m` opens a plain numbered list (concept name +
   anchor, one line each); selecting shows the card through the normal
   single-slot rule. Queue dies at session end (C2); the T3 bookend will be
   its last call — until T3, exiting prints a one-line count of unshown
   advice.
4. Ordering: category rank (bug > idiom > best-practice > architecture),
   then age; throttled categories always tail. (Goal-relevance term joins
   in T3 ahead of category rank.)

### Dedup & aggregation (D11, C2)
5. Advice-fingerprint dedup extends cross-session: a card whose advice_fp
   matches a `cards` row with status ∈ {applied, got_it, not_useful,
   resolved} is never created again (I3 ledger) — except regression (req 9).
6. Same concept at multiple sites in one judging sweep ⇒ ONE aggregated
   card listing up to 3 anchors ("this pattern appears in N places");
   remaining sites recorded in the card payload.
7. Concept cooldown (session-scope, C8): after a card ships for concept c,
   no further pushed cards for c this session; queued cards for c collapse
   into the shown card's aggregation.

### Snooze & regression (D11/C8, C5)
8. `n` (not_now) tiers: first = instance (advice_fp) suppressed for the
   session; second not_now on the SAME concept in one session auto-widens to
   concept-for-session with a one-line notice. Snooze rows live in
   `suppressions` (scope: `instance|concept`), purged at session end; live
   cap 50 (oldest expire first).
9. Regression re-open: a new stage-2-validated finding whose advice_fp
   matches a `resolved`/`applied` card re-opens it (new card row referencing
   the old; allowed despite req 5).

### Auto-throttle (D12, C3)
10. Per category, over the last 20 counted cards (C3 denominator rules;
    D17/D18-origin cards excluded when they exist): action rate < 15% ⇒
    category demoted to queue-only, with a one-line notice and an undo
    (config key or the `m` list marks it). Throttle transitions logged as
    `throttle_change` events; state is computed from events at session
    start, never stored (C5).

### Recording
11. Every queue/snooze/throttle transition is an event (C5 kinds already
    enumerated). No new tables beyond `suppressions`.

## Acceptance

- `cargo test` green. New tests: detent → (budget, floor) mapping; floor
  exclusion queues rather than drops; queue ordering incl. throttled-tail;
  aggregation (3 sites → 1 card); cross-session ledger dedup via a reopened
  db; tiered snooze widening notice; suppression cap expiry; regression
  re-open; throttle trip at exactly the 15%/20 boundary and its undo;
  event-log completeness for one full scenario.
- Manual dogfood: with `quiet`, a session generating 5+ candidates pushes
  ≤ the budget, `m` shows the rest, two `n` on one concept parks it with
  the notice.
- No TODO markers in covered files.

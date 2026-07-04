# Spec amendment (DRAFT — ⛔ NEEDS FOUNDER APPROVAL): events retention / rolling window

**Status:** DRAFT, unratified. The code change it authorizes is **NOT shipped**
— it is gated behind founder sign-off (ROADMAP item 3). Nothing in the engine
reads a window today; the reads below still scan all-time history.

**Raised by:** ROADMAP item 3 (2026-07 review long-tail). The reviewer flagged
that two hot reads scan *all-time / all-session* history with no retention or
`VACUUM` policy, so their cost — and, more importantly, their *semantics* —
drift as a user's `events`/`context_history` grow without bound.

---

## The problem

Two derived reads aggregate over the entire lifetime event log:

1. **`db/events.rs::all_check_result_points`** — feeds the struggle-signal
   baseline (SPEC D15 / T3 req 8). Today it returns **every** `check_result`
   event the user has ever produced. A person who has used murshid for months
   computes their "typical time-to-green / red-streak" baseline against
   half-year-old data, so a genuinely-struggling *recent* session is measured
   against a diluted historical distribution and the struggle offer mis-fires
   (too eager or too shy).

2. **`db/events.rs::count_declined_offers_for_concept`** — feeds the T3 req 13
   cross-session decline suppression (2 declines → 7-day `offer-concept`
   suppression). Today it counts **all-time** declines for the concept. A
   concept declined twice a year ago stays "twice-declined" forever, so the
   suppression trigger is effectively permanent rather than reflecting recent
   intent.

Neither `events` nor `context_history` is ever pruned or `VACUUM`ed, so the DB
file only grows.

## Why this needs a spec amendment (not just a refactor)

Bounding either read to a rolling window **changes the computed value** of the
struggle baseline (D15) and the decline count (T3 req 13) — i.e. it changes
*observable engine behaviour*, not just performance. That is spec-level
semantics and must be ratified, not silently shipped. (Per the ROADMAP
constraint: "it changes struggle-baseline / decline-count computations, so it
**requires a spec amendment** … gate the change behind it; do not silently
ship the semantics change.")

## Proposed window (for founder decision)

Pick ONE window definition and apply it consistently to both reads and the
prune policy. Two candidates:

- **Option A — time-based: last 90 days.** Simple, matches the "recent intent"
  intuition, and the decline suppression is already a 7-day mechanism so a
  90-day source window is comfortably wider than its own horizon. A user who
  takes a 3-month break starts with a fresh baseline (arguably correct).
- **Option B — session-based: last 30 sessions.** Robust to bursty vs. sparse
  usage (a heavy week and a quiet month both contribute 30 sessions of
  signal). Slightly more code (join/filter on the session-id prefix ordering
  already used by `concepts_encountered_last_session`).

**Recommendation:** Option A (90 days) for the struggle baseline — time is the
natural axis for "am I struggling *right now* vs. my normal". For the decline
count, either works; 90 days keeps it a single window constant.

Open sub-question for the founder: should the decline count use the **same**
window as the baseline, or a **longer** one (declines are a stronger,
rarer signal — a shorter window could let a twice-declined concept resurface
sooner than intended)?

## Change to ship ONCE ratified (gated — do not implement before sign-off)

1. Add a single retention-window constant (e.g. `RETENTION_WINDOW_DAYS = 90`)
   in one place (`db/mod.rs` or a `retention` module), referenced by both reads.
2. `all_check_result_points`: add `AND ts >= <cutoff>` (or a session-id lower
   bound) to the `check_result` query.
3. `count_declined_offers_for_concept`: add the same cutoff predicate.
4. Add a prune + `VACUUM` policy: at session start (or on a cadence), delete
   `events` / `context_history` rows older than the window, then `VACUUM` to
   reclaim space. Deletion must be **strictly older than** the widest window any
   read uses, so pruning can never change a value a live read would compute.
   Guard the `VACUUM` so it cannot run inside an open transaction and is
   rate-limited (mirror `compiler::check_and_prune_cache`'s once-per-interval
   guard).
5. Update tests to pin the windowed semantics, and add a prune/retention test.
6. Cross-reference D15 and T3 req 13 in `SPEC.md` (design authority) so the
   window becomes normative, not just a code constant.

## Impact / risk

- Behaviour change is intentional and localized to the two reads + a new prune.
- On-disk format is unchanged (no schema change; `VACUUM` is content-preserving).
- A user upgrading mid-history sees their baseline/decline counts recomputed on
  the window at next session start — acceptable and, per the rationale above,
  more correct.

---

**⛔ Action required:** founder to (a) approve/adjust the window (Option A/B and
the decline-window sub-question), then (b) authorize implementation. Until then
the reads remain all-time and this file is the only artifact.

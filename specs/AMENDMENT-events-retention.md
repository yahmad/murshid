# Spec amendment (✅ RATIFIED 2026-07-04): events retention / rolling window

**Status:** RATIFIED by the founder 2026-07-04 and **shipped**. Decisions:
**struggle baseline = 90-day time window**; **decline count = 180-day time
window** (longer, since declines are a rarer/stronger signal); **prune floor =
180 days** (= the widest window, so pruning never removes a row a live read
consults). Implemented in `db/events.rs`
(`STRUGGLE_BASELINE_WINDOW_DAYS`/`DECLINE_WINDOW_DAYS`/`RETENTION_PRUNE_FLOOR_DAYS`
+ `prune_expired_history`), pruned once per session at watch startup.

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

## Ratified window (founder decision 2026-07-04)

- **Struggle baseline: last 90 days (time-based).** Time is the natural axis for
  "am I struggling *right now* vs. my normal"; a 3-month break yields a fresh
  baseline.
- **Decline count: last 180 days (time-based).** A *longer* horizon than the
  baseline — declines are a rarer, stronger "stop showing me this" signal that
  should persist longer; 180d = 2× the baseline keeps it a clean single
  constant.
- **Prune floor: 180 days** (= `max(windows)` = the decline window), so pruning
  can never delete a row a live read still consults.

## Change shipped 2026-07-04

1. ✅ Retention-window constants (`STRUGGLE_BASELINE_WINDOW_DAYS = 90`,
   `DECLINE_WINDOW_DAYS = 180`, `RETENTION_PRUNE_FLOOR_DAYS = 180`) in
   `db/events.rs`, referenced by both reads and the prune.
2. ✅ `all_check_result_points`: `AND ts >= datetime('now', '-90 days')`.
3. ✅ `count_declined_offers_for_concept`: `AND ts >= datetime('now', '-180 days')`.
4. ✅ `prune_expired_history(conn)`: deletes `events` / `context_history` rows
   older than the 180-day floor, then `VACUUM`s **only if** rows were removed
   (self-limiting — subsequent startups delete little and skip the VACUUM). Runs
   once per session at watch startup, in autocommit (never inside a tx). The
   floor is `>=` every read window, so pruning cannot change a live value.
5. ✅ Tests pin the windowed semantics (baseline-window exclusion, the
   distinct-and-longer decline window) and the prune (deletes past floor, keeps
   recent, idempotent).
6. ⬜ **Follow-up:** cross-reference D15 / T3 req 13 in the design-authority
   `~/src/yahmad/dev-context/specs/murshid/SPEC.md` so the window is normative
   there too (that repo is outside this checkout — do at next SPEC touch).

## Impact / risk

- Behaviour change is intentional and localized to the two reads + the new prune.
- On-disk format is unchanged (no schema change; `VACUUM` is content-preserving).
- A user upgrading mid-history sees their baseline/decline counts recomputed on
  the window at next session start — acceptable and, per the rationale above,
  more correct.

# T5 — Memory

**Status:** Planned (after T4)
**Traces to:** SPEC v0.5 — D21/D22, I18/I22–I26, C4/C5/C8/C12.
**User outcome:** the mentor visibly learns the founder: concepts he keeps
applying cleanly stop being taught (and the mentor says so once), concepts
he fumbles get more help next time, `murshid progress` shows an honest
per-concept meter, and a long-untouched concept earns one small recall
question at a session boundary.

## Requirements

### State & updates (D21, C5, C12)
1. New migration adds `concept_memory` exactly per C5. One row per taxonomy
   slug, lazily created at first encounter with the category's priors
   (C12 table: p_L0/p_T/p_G/p_S per bug/idiom/best-practice/architecture).
2. BKT update on every I22 evidence event (single function, pure —
   no side effects beyond the row): standard two-step (posterior given
   observation, then learning transition). Grades: `pass` = correct;
   `fail` = incorrect; `hard` = correct for the BKT update but applies no
   downward help-level shift (C8). Dismissals/'not now'/expired are NOT
   observations (I23).
3. Evidence sources (C8):
   - `pass`: stage-1 application detections — activate T1's reserved output
     field; detections accepted only for concepts currently below mastery
     and only at sites without an open card (guards against double-count
     with req 4).
   - `fail`: a stage-2-validated finding on a concept previously taught
     (any prior card exists for it).
   - `hard`: `applied` status on a card at the same site in the same
     session after help was shown (T4's applied-detection).
   - D22 recall answers: judge-graded pass/hard/fail (req 8).
   Every evidence event is an `encounter` event with grade + source.

### Help level & rung wiring (C4, I18)
4. `help_level` per concept follows Wood's ±1: pass ⇒ −1, fail ⇒ +1
   (clamped [0,3]); `hard` ⇒ no change. Entry rung = BKT band per C4
   (p<0.5→R3, 0.5–0.8→R2, 0.8–0.95→R1, ≥0.95→silence; R0 permitted when
   0.6≤p<0.95), then the Wood shift and the directness-knob offset applied,
   clamped. This replaces T4's static R2 default; generation-moment (R0)
   entry becomes available.
5. Mastery fade is ANNOUNCED once (I24): first time a concept crosses
   p≥0.95, one line ("backing off on <name> — applied N times straight");
   `fade` recorded (event). Level-down: a fail-grade encounter on a
   mastered concept drops p per BKT and re-enables cards, with one line.

### Staleness & decay (C8, C12)
6. Staleness windows per category (C12: idiom 21d, best-practice 30d,
   architecture 60d, bug n/a). A below-1.0-retention concept is `stale`
   when now − last_encounter_ts > window AND p ≥ 0.7 (things worth
   retaining). Staleness itself never lowers p (no silent decay in v1);
   it only makes the concept eligible for retrieval (req 7).

### Retrieval questions (D22, I26)
7. At session start or bookend only, if ≥1 concept is stale and no natural
   encounter occurred in the last session: at most 2 one-line recall
   questions per session (C12), generated from the canon entry
   ("how would you rewrite X without cloning?" style, R0 form). Skippable
   by keypress or 30s timeout (skip = no observation, but re-eligibility
   backs off ×2 per skip). Pull-priced judge grading (req 8); never during
   the work session.
8. D22 grading: the judge model grades the typed answer against the canon
   entry → pass/hard/fail + one-line feedback; one call; sanitized; counts
   as a normal encounter (req 3).

### Open meter (I24)
9. `murshid progress`: per-concept table — name, bar for p_mastery, help
   level, last encounter age, state (learning/mastered/stale/throttled
   category flag from T2). Plain text, I21 rendering rules, sorted by
   category then p. Zero-row concepts (never encountered) listed dimmed.

### Guards
10. The learner model is never updated from re-displayed advice (I22 —
    passive re-show is not a review), nor from queue browsing, nor from
    thread turns (asking ≠ evidence).

## Acceptance

- `cargo test` green. New tests: BKT update math against hand-computed
  fixtures (all four categories' priors; pass/fail/hard sequences);
  detection-guard (open card at site ⇒ no pass double-count);
  below-mastery-only detection acceptance; help_level clamps; entry-rung
  band + Wood + knob composition (property: always in [R0,R3] or silence);
  fade announced exactly once incl. across sessions (fixture events);
  regression level-down re-enables cards; staleness eligibility boundary;
  retrieval cap + skip backoff; recall grading fixture (pass/hard/fail);
  meter render snapshot (NO_COLOR); no-update guards (req 10).
- Manual dogfood: three clean unprompted applications of a taught concept
  raise its meter and eventually trigger the announced fade; a later
  misuse brings cards back with one notice.
- No TODO markers in covered files.

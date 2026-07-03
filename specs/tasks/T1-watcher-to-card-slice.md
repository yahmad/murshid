# T1 — Watcher-to-card vertical slice

**Status:** Active
**Traces to:** SPEC v0.5 — D3/D4/D8/D9, I1/I2, C2/C3(partial)/C5(partial)/C6/C12.
**User outcome:** the founder runs `murshid watch` in a real Rust repo, writes
non-idiomatic code, and — after saving and pausing — sees one grounded,
concept-named teaching card in the pane. Everything is recorded locally.

## Scope

IN: session lifecycle, session diff, quiescence gate, two-stage judge with
one starter Rust pack, one pushed card (R2 form), fixed budget, event/card
logging, degraded mode.
OUT (later tasks): knob UI + queue browsing + dedup-across-sessions +
throttle (T2), goals/struggle (T3), escalation ladder/threads/comments/review
(T4), BKT memory + rung selection (T5), pack extraction behind the seam (T6).
T1 may hardcode what T6 later extracts, but must keep pack content in data
files from day one (see Pack).

## Requirements

### Session & observation (C2)
1. A session = one `murshid watch` run; an idle gap > 4 h (no file events)
   starts a new session id. Session ids are ULID-like strings.
2. At session start, snapshot content hashes (SHA-256) of all
   tracked-and-modified files (git index + worktree state via `git status
   --porcelain`; no libgit2 — shell out or parse, matching existing repo
   practice).
3. Session diff = current file content vs session-start snapshot (or vs
   HEAD content for files unmodified at start), computed per changed file at
   each quiescence moment as unified hunks. Advice attaches only to changed
   lines (I1).

### Quiescence gate (D8, C12)
4. Judge only when: a watched file was saved, ≥ 2 s typing/file-event pause
   has elapsed, and the changed file parses (tree-sitter-rust; parse errors
   ⇒ wait). `cargo check` events (existing interceptor) additionally trigger
   a catch-up sweep of files changed since the last judged moment.

### Two-stage judge (D9, C6)
5. `[models]` config: `screen` and `judge` slots, each `provider/model`;
   providers reuse the existing dispatcher (Claude/Gemini/Ollama). Keys via
   existing keyring/env flow. All payloads pass the existing sanitizer.
6. Stage 1 (screen model) input: session-diff hunks + one line of enclosing
   context per hunk + the taxonomy slug list. Output (JSON): candidate
   moments `[{site_hint, slugs[]}]`. (Application detections: T5 — the
   output field is reserved but ignored.)
7. Stage 2 (judge model) input: one candidate + full enclosing item (fn/
   struct/impl via tree-sitter) + the canon entries for its slugs. Output
   contract: `{concept, grounding_quote, why, rule, worked_diff, category,
   likely_bug}` — `concept` MUST be one of the pack's taxonomy slugs;
   `grounding_quote` MUST appear verbatim in the current file. Any missing/
   failing leg ⇒ candidate dropped (log `judge_drop` in payload).
8. Within a session, identical `(concept, site)` advice-fingerprints are
   never judged or shown twice (site per C2: file + enclosing item name +
   normalized anchor hash).

### Presentation (I21 minimal, C3 partial)
9. At most ONE card on screen. Card renders the R2 form: `★ <concept name>`
   header, anchor (`file:line`, quote), verdict+why (≤ 3 sentences), rule
   (one line + doc ref from canon). Worked diff exists in the card record
   but renders folded behind a "(fix available — full interaction in T4)"
   line. ~80-col wrap; NO_COLOR honored; meaning never color-only.
10. Responses (subset of C3): `g` got_it · `u` not_useful · `n` not_now
    (hides for session) · no interaction by session end ⇒ `expired`.
    Overflow advice increments a one-line counter ("N more queued — T2");
    only count shown in T1.

### Budget (C12, fixed)
11. Push budget fixed at the `standard` detent: 1 card / 10 min
    (token-bucket, burst 1). `likely_bug=true` cards bypass the budget ONLY
    if stage 2 strict mode passed (second sample agrees + concrete failure
    scenario) — otherwise they wait like everyone else.

### Recording (C5 partial, I2)
12. New migration adds `events` and `cards` tables exactly per C5 (other C5
    tables come with their tasks). Every shown card and every response is an
    event; `cards.status` tracks the response verb.

### Degraded mode (C6)
13. No key / provider error ⇒ observe-only: events still recorded, cargo
    diagnostics still visible as plain lines, status line explains why. The
    watcher must never crash because a provider is down.

### Pack (data files, pre-seam)
14. `packs/rust/taxonomy.json` — 10 seed concepts, each `{slug, name,
    category}` (category ∈ bug/idiom/best-practice/architecture):
    option-combinators, question-mark-propagation, iterator-chains,
    borrow-vs-clone, string-vs-str, match-ergonomics, derive-traits,
    collect-annotations, if-let-patterns, lifetime-elision.
15. `packs/rust/canon.json` — one entry per concept: `{id, concept,
    what_it_does, why_is_this_bad, example, use_instead, refs[],
    source_rule_ids[]}` (clippy schema; refs to clippy/API-guidelines docs).
16. `packs/rust/surface.toml` — comment token (`//`), check command
    (`cargo check`), file extensions. Judge-prompt framing may live in a
    `prompts.md`. Engine reads these as data; no pack content in .rs files.

## Acceptance

- `cargo test` green, including new tests: session split logic; snapshot/
  session-diff computation; stage-2 output-contract validation (drop on
  missing leg, drop on non-taxonomy concept, drop on unverifiable quote);
  advice-fingerprint identity across a line-shift edit; token bucket;
  degraded mode (no key ⇒ observe-only, no panic); card render snapshot
  (NO_COLOR).
- Manual dogfood check: in a scratch Rust repo, an introduced
  `.clone()`-where-borrow-works produces (within budget and quiescence
  rules) a card naming `borrow-vs-clone` with a verbatim grounding quote.
- No TODO markers in files this spec covers.
- LLM calls are NOT mocked in unit tests — the judge pipeline is tested
  against recorded fixture responses; live-call integration is a manual
  dogfood step (BYOK).

# Pack pipeline

**Status:** R0 (T18 — see `specs/tasks/T18-pack-pipeline.md`).

A language pack is three bundled layers: **knowledge** (canon — the prose a
frontier model erodes but a weak/local model needs), a **memory schema**
(the concept taxonomy — the slugs BKT mastery, `hint-concept` suppression,
and spaced resurfacing key on), and **local plumbing** (tree-sitter
grammar, diagnostics adapter, detection marker). This document is the
pipeline for producing and maintaining the first two (data); the last one
(code) is covered in [§(e)](#e-the-rust-code-side-of-a-new-pack).

Every pack ships three JSON/TOML payloads under `packs/<id>/`:
`taxonomy.json` (concepts), `canon.json` (one prose entry per concept),
`grammar.json` (tree-sitter node-kind vocabulary), plus `surface.toml`
(comment token, check command, help/on-hold phrase lists) and
`prompts/stage1.md` + `prompts/stage2.md`. This pipeline covers
`taxonomy.json` and `canon.json` — the two payloads a model can usefully
draft. `grammar.json`/`surface.toml`/prompts are hand-authored alongside
the adapter, once, per pack (see §(e)).

The validation harness enforcing everything below lives in
`src/pack.rs`'s `mod tests` (search `T18 R0`), runs as ordinary
`cargo test`, and iterates every pack registered in `PACK_REGISTRY`
through the SAME loaders (`load_taxonomy`/`load_canon`/`load_grammar`)
production uses — there is no second, drifting parser in the validator.

## (a) Generation prompts

These are real, usable prompt templates: paste one into a frontier model
(the model doesn't need to be the pack's target language's runtime — it
needs broad training exposure to that language's official style guide and
common linters) with the bracketed fields filled in, and curate the
output against §(b) before it touches `packs/`.

### Taxonomy-drafting prompt

```
You are drafting a concept taxonomy for a code-mentoring tool. The tool
watches a learner's saves in a [LANGUAGE] project, and — using this
taxonomy plus a canon of "why" prose (drafted separately) — occasionally
offers a short, Socratic hint when the learner's code exhibits one of
these concepts poorly.

Draft [25-40] concepts for [LANGUAGE], covering common bugs, idioms,
best practices, and architecture patterns that:
- a working [LANGUAGE] developer with 1-3 years of experience would
  benefit from having pointed out when they get it wrong;
- are DETECTABLE from a diff/site (a specific span of changed code),
  not from whole-program intent or business logic;
- are each ONE testable habit, not a bundle. ("use context.Context for
  cancellation" is one concept; "handle errors well" is not — split it
  into "wrap errors with call-site context", "check err before checking
  a nilable return value", etc.)

For EACH concept, output exactly:
{
  "slug": "kebab-case-id",
  "name": "Human-readable name",
  "category": "bug" | "idiom" | "best-practice" | "architecture"
}

Slug rules (these are PERMANENT once shipped — see the append-only rule
below — so get them right the first time):
- lowercase ASCII letters, digits, and single hyphens only
  (`^[a-z0-9]+(-[a-z0-9]+)*$`);
- no leading/trailing hyphen, no `--`;
- name the HABIT or PITFALL, not the language feature
  ("borrow-vs-clone", not "clone-method"; "goroutine-leaks", not
  "goroutines");
- must be unique within the taxonomy.

Category rules — pick the SINGLE best fit, this is a closed set (a
5th "other" value is a validator failure, not a valid answer):
- "bug": silently produces wrong behavior or a crash/leak/race — the
  code often "works" until it doesn't (e.g. goroutine-leaks,
  slice-append-aliasing).
- "idiom": correct and safe, but not how an experienced [LANGUAGE]
  developer would write it — a style/expressiveness gap (e.g.
  option-combinators, error-wrapping).
- "best-practice": correct today, but risky/costly at scale or under
  change — a maintainability/robustness gap, not a style gap (e.g.
  derive-traits, defer-cleanup).
- "architecture": a structural/design-level habit that spans more than
  one call site or file (e.g. lifetime-elision, context-propagation,
  interface-satisfaction).

Ground every concept in something citable: the language's official
style guide, `go vet`/`clippy`/`ruff`/the dominant linter's rule set, or
a widely-cited community document (Effective Go, the Rust API
Guidelines, PEP 8, etc.) — you'll need that citation for the matching
canon entry.

Output ONLY the JSON array of concept objects, no prose commentary.
```

### Canon-drafting prompt

Run once per concept (or batched, but review field-by-field either way —
see §(b)):

```
You are drafting one canon entry for the "[SLUG]" concept in a
[LANGUAGE] code-mentoring tool's knowledge base. This prose is shown
directly to a learner as a hint's grounding — it must be accurate,
specific, and quotable without embarrassment next to the official
[LANGUAGE] style guide / [LINTER] docs.

Concept: "[NAME]" (category: [CATEGORY])

Output exactly this JSON shape:
{
  "id": "canon-[SLUG]",
  "concept": "[SLUG]",
  "what_it_does": "One sentence: what the WORSE pattern does, described
    neutrally (not yet saying why it's bad). Written as a description of
    code behavior, not an instruction.",
  "why_is_this_bad": "One to two sentences: the concrete cost — what
    breaks, what's harder, what's slower/leakier/less safe. Specific to
    THIS concept, not generic ('this is not idiomatic').",
  "example": "A short, realistic [LANGUAGE] code snippet (2-6 lines)
    showing the WORSE pattern. Must be syntactically valid [LANGUAGE].
    Use \\n for newlines in the JSON string.",
  "use_instead": "A short, realistic [LANGUAGE] code snippet (1-4 lines)
    showing the fix, in the same shape/scope as `example` so the diff
    is obvious. Must be syntactically valid [LANGUAGE].",
  "refs": [
    "A URL to the official [LANGUAGE] docs, style guide, or spec section
     that backs this up",
    "Optionally a second URL — a well-known community reference
     (Effective Go, Rust API Guidelines, PEP 8, etc.)"
  ],
  "source_rule_ids": [
    "The [LINTER]-native rule id this concept corresponds to, if one
     exists, namespaced as the adapter emits it (e.g. \"clippy::redundant_clone\",
     \"go/vet::lostcancel\"). Empty array if no linter rule covers it —
     that's fine, not every concept needs a mechanical source."
  ]
}

Every field must be non-empty prose (no placeholders, no "TODO", no
"see docs"). `example` and `use_instead` must be code, not English
description of code. Output ONLY the JSON object, no commentary.
```

## (b) Curation checklist

A model draft is a first pass, never a merge candidate as-is. Before a
taxonomy/canon delta lands in `packs/`, a human (today: the founder)
checks, per concept:

1. **Slug quality** — reads as a habit/pitfall name a mentor would say
   out loud, not a feature name; matches the kebab-case charset; doesn't
   collide with (or nearly duplicate) an existing slug in this pack or
   read confusingly close to one in another pack's taxonomy.
2. **Granularity — one testable habit per concept.** If curating the
   concept would require an "and" in its name, split it. If two drafted
   concepts would fire on the same site in practice, merge or
   re-scope one of them. Reject anything detectable only from
   whole-program intent (the tool sees a diff/site, not a design doc).
3. **Category honesty.** Re-derive the category from first principles
   (would this "silently misbehave" = bug, "read badly to an expert" =
   idiom, "cost more at scale" = best-practice, "span multiple sites" =
   architecture?) rather than trusting the model's tag — models
   frequently default everything interesting to "best-practice".
4. **Canon prose quality.** `what_it_does` is neutral description, not
   a scold; `why_is_this_bad` names a concrete, specific cost (reject
   "this isn't idiomatic" as circular); `example`/`use_instead` compile
   in your head (ideally: paste into a scratch file and actually check)
   and are minimal — no unrelated ceremony obscuring the point.
5. **Grounding.** At least one `refs` URL resolves and actually
   supports the claim (open it — models fabricate plausible-looking doc
   URLs). Prefer official docs over blog posts.
6. **`source_rule_ids` honesty.** Only list a rule id if you've
   confirmed it exists and namespaces the way the pack's adapter emits
   it (check `packs/<id>/canon.json`'s existing entries or the adapter
   module for the exact namespacing convention, e.g. `clippy::x` vs
   `rust/E0425`). An empty array beats a fabricated id.
7. **Validator green** (§ below) — mechanical, but catches slug/charset/
   uniqueness/empty-field mistakes the manual pass might miss.

## (c) Update / re-run procedure

Triggers: a new language version, a new/updated linter, or founder
demand for deeper coverage of an existing pack.

1. Re-run the taxonomy-drafting prompt (§(a)) for the language, this
   time telling the model the pack's EXISTING slugs (paste the current
   `taxonomy.json`) and asking it to propose only NEW concepts — never
   to touch, rename, or re-categorize an existing entry (the append-only
   rule, §(d), makes this a hard requirement, not a style preference).
2. Diff the model's output against the current taxonomy: drop anything
   that duplicates or near-duplicates an existing slug/concept.
3. Curate the delta per §(b).
4. Run the canon-drafting prompt (§(a)) for each newly curated concept
   only.
5. Append the new concepts to `taxonomy.json` and the new entries to
   `canon.json` (JSON array append — existing entries untouched,
   byte-for-byte).
6. `cargo test` — the validator (§ below) must be green, including the
   append-only check against `packs/<id>/taxonomy.lock`.
7. Regenerate `packs/<id>/taxonomy.lock` to include the newly landed
   slugs (append new `<slug> <category>` lines, sorted by slug; never
   edit or remove an existing line) so THEY are locked in for the next
   update cycle. Commit the lock update in the same PR as the taxonomy/
   canon delta.
8. Land as an ordinary reviewed change; dogfood is the real gate on
   whether the new concepts actually earn their keep (a concept that
   never fires can simply be abandoned in the canon/judge — it never
   needs to be renamed or removed, per §(d)).

## (d) The append-only rule, and why

**Taxonomy slugs are forever.** A slug is persisted as `concept_id` in
`concept_memory` (BKT mastery state), `cards` (issued hints), and
`suppressions` (dismissed/snoozed concepts). Renaming, deleting, or
re-categorizing a shipped slug orphans however many days/weeks of a
real learner's mastery history for that concept — there is no migration
path in v1, and building one is deliberately out of scope (it would
require reconciling BKT posteriors across a rename, which is a much
harder problem than "add a slug").

So: **enrichment only ever adds concepts.** If a concept turns out to be
mis-scoped or poorly named after it ships, the fix is to let it go
unused (the judge/canon simply stop citing it) and add a better-scoped
replacement slug — never to edit the old one in place.

This is mechanically enforced per pack by a golden lock file,
`packs/<id>/taxonomy.lock`: a flat `<slug> <category>` list, one per
line, sorted by slug, generated from the taxonomy at the time it was
locked. `cargo test` (`test_taxonomy_is_append_only_against_golden_lock`
in `src/pack.rs`) asserts every line in the lock still has a matching,
unchanged slug/category pair in the live `taxonomy.json`. A missing slug
or a changed category fails the build with a message pointing back here.
New slugs that aren't in the lock yet are NOT an error — the lock is
deliberately a floor, not a ceiling, so mid-enrichment work doesn't have
to update it line-by-line; regenerate it once per landed PR (§(c) step
7).

## (e) The Rust-code side of a new pack

Adding a wholly new language (not enriching an existing one) touches
code, not just `packs/`. **T7 (the Go pack) is the exemplar** — read
`specs/tasks/T7-go-pack.md`'s pass criterion, which is mechanical and
still binding: the ONLY permitted edits when adding a pack are —

1. **`packs/<id>/` data directory** — all payloads: `taxonomy.json`,
   `canon.json`, `grammar.json`, `surface.toml`, `prompts/stage1.md`,
   `prompts/stage2.md` (built via §(a)-(c) above for the first two;
   hand-authored for the rest, following the shape of `packs/rust/` or
   `packs/go/`).
2. **Grammar crate dependency + C10 note** — one `tree-sitter-<id>`
   crate added to `Cargo.toml`'s `[dependencies]`, under the
   PACK-scoped, compile-time-linked allowance C10 grants official
   `tree-sitter-{language}` grammar crates (D23: no dynamic plugins;
   one grammar crate per registered pack). No other new dependency is
   permitted for a pack — diagnostics tools are invoked by subprocess,
   never linked in.
3. **One diagnostics-adapter module** contributing exactly one
   `impl pack::DiagnosticsAdapter` (I27's "one narrow adapter"; native
   tool output in, `NormalizedRecord`s out), declared as a **child `mod`
   of `pack.rs`** — never `main.rs` — mirroring `#[path =
   "go_adapter.rs"] mod go_adapter;` at the top of `src/pack.rs`. This
   keeps the entire code-registration surface inside the one seam file
   (T7's tightened pass criterion).
4. **One new [`PackRegistration`] entry** in `PACK_REGISTRY`
   (`src/pack.rs`) — the single table both grammar and adapter
   resolution read, so they can never disagree about which ids are
   registered. This is the only place `resolve_ts_language` and
   `diagnostics_adapter` gain a new match arm; both go through this one
   table, not two separate ones.
5. **A detection marker** — extend `detect_pack_marker`
   (`src/pack.rs`, T10) with the new language's project marker file
   (e.g. `pyproject.toml`/`requirements.txt`/`setup.py` for Python,
   `tsconfig.json` for TypeScript), so the pack is reachable by
   auto-detection, not just by hand-editing `[pack] language` in
   config.

**Zero other edits.** Any change to `site.rs`, `quiescence.rs`,
`pipeline.rs`, `review.rs`, `watcher.rs`, `judge.rs`, `main.rs`,
`comment.rs`, `db.rs`, or any other engine module means the pack seam
failed — those modules take pack DATA as plain parameters and must
never reference a specific language (I29). Stop and reopen the seam
design rather than special-casing a new language inside engine code.

Acceptance for a new pack (beyond the validator in this file): an E2E
smoke against a fixture project for the new language, and one live
dogfood session producing a grounded, concept-named card, per T18's
R2/R3 rungs.

# T18 — Pack pipeline (bootstrap + enrichment), with Python & TypeScript packs

**Status:** Drafted 2026-07-09 — founder direction given (2026-07-08/09 pack
discussion: "why not author other packs… a pipeline for new tools or updates
might be useful"). Rungs R0–R1 (no new dependencies) may build immediately;
R2–R3 are **gated on founder ratification of the C10 amendment below**.
**Traces to:** SPEC v0.5 C9 (pack payloads), C10 (dependency allowlist), D23
(compile-time-linked packs, no dynamic plugins), I29 (pack seam honesty:
zero engine edits per language), T6/T7/T10 (seam, Go pack, auto-detection),
and the 2026-07-08 vision confirmations (mentor-grade hint quality is the
KPI; provider-neutrality incl. weak/local models is half the moat).

## Why

A pack is three different things bundled: **knowledge** (canon/prompts — the
layer frontier models erode), a **memory schema** (the concept taxonomy —
the slugs BKT mastery, `hint-concept` suppression, and spaced resurfacing
key on; durable, model-independent), and **local plumbing** (tree-sitter
grammar, diagnostics adapter, detection — deterministic, model-independent).
Two findings drive this task:

1. The current taxonomies are tiny (a handful of concepts each). The
   mentor-grade KPI is starved by taxonomy breadth *today* — enriching the
   Rust pack is the highest-leverage quality work available.
2. Hand-authoring a couple dozen language packs is the wrong shape. The
   durable asset is a **pipeline**: model-drafted taxonomy+canon →
   validation harness → founder curation → E2E smoke — with a documented
   **re-run/update path** for new language versions and linters.

The knowledge layer's value scales inversely with the configured model's
strength — with a frontier judge it's a grounding aid; with a local model
(dogfood: LM Studio qwen) it is load-bearing. That asymmetry is the
neutrality moat, so canon stays first-class.

## The crux — slugs are forever

Taxonomy slugs are persisted as `concept_id` in `concept_memory`, `cards`,
and `suppressions`. A renamed or deleted slug orphans months of a user's
learning state. Therefore the pipeline's hard rule: **taxonomies are
append-only.** Enrichment adds concepts; it never renames, deletes, or
re-categorizes an existing slug (a rename would require a user-data
migration — out of scope, deliberately). The validation harness enforces
this against the previous committed version of each pack.

## Ladder (each rung ships visible + gated; implementer → adversarial gate)

- **R0 — validation harness + pipeline docs (no new deps).** Schema/format
  documentation for `packs/*/taxonomy.json|canon.json|grammar.json`; a
  validator that runs as ordinary `cargo test` over every registered pack:
  slug format + uniqueness, category ∈ closed C12 set, canon→taxonomy
  cross-references resolve, required canon fields non-empty, grammar
  item/container kinds well-formed, and **append-only vs the prior committed
  taxonomy** (golden-file comparison). `packs/PIPELINE.md`: the generation
  prompts (model drafts taxonomy/canon for a language), the curation
  checklist, and the update/re-run procedure (new language version or
  linter → regenerate → diff → curate → validate).
- **R1 — Rust pack enrichment (data-only).** Expand the Rust taxonomy to
  ~25–40 concepts with canon entries, model-drafted then curated; slugs
  append-only (existing 5 untouched). Acceptance: validator green; founder
  spot-review of the canon; dogfood is the real gate.
- **R2 — Python pack (needs C10 amendment).** `packs/python/` +
  `tree-sitter-python` dep + `PackRegistration` + diagnostics adapter
  (strawman: `ruff check --output-format json` when on PATH, else
  `python -m py_compile` syntax-only fallback) + detection
  (`pyproject.toml`/`requirements.txt`/`setup.py`). Zero engine edits
  (I29). E2E smoke vs the stub provider on a fixture project; founder runs
  one live smoke (he's the learner persona for Python).
- **R3 — TypeScript pack (needs C10 amendment).** `packs/typescript/` +
  `tree-sitter-typescript` + adapter (strawman: project-local
  `tsc --noEmit` via the project's own toolchain) + detection
  (`tsconfig.json`). Founder reviews canon as a native TS expert.
- **R4 — update-path proof.** Re-run the pipeline against one pack
  (e.g. a new ruff/clippy lint category or language-version idiom), diff,
  curate, land the delta — proving the maintenance story, not just the
  bootstrap.

## Beyond R3 — the roster (why only two languages are named above)

Python and TypeScript are the pipeline's **proof outputs**, chosen because
the founder can verify both (native TS expert; Python learner — the actual
v1 persona), not because the roster ends there. Once R4 proves the
bootstrap+update loop, **each further pack is a pipeline run, not a new
T-task**: generation → curation → validator → I29 gate (zero engine edits)
→ E2E smoke, landing as an ordinary reviewed PR-sized change. The blanket
C10 amendment covers every official grammar crate, so no per-language
ratification is needed.

Candidate roster, ordered by (learner demand × diagnostics-tool quality ×
grammar availability — all of these have official tree-sitter grammars):
**Java** (`javac -Xlint`/ErrorProne; enterprise learners), **Kotlin**
(`kotlinc`/detekt), **C#** (`dotnet build`/Roslyn analyzers), **Swift**
(`swiftc`/SwiftLint; pairs with the Mac-app audience), **C/C++**
(`clang -fsyntax-only`/clang-tidy; the deepest "never learned it properly"
pain), **Ruby** (`ruby -c`/RuboCop), **PHP** (`php -l`/PHPStan),
**Zig/Elixir/Scala** (long tail, demand-driven). Shell/SQL are deliberately
deferred: hint-worthy, but their "project detection" and diagnostics
stories are weak, and they'd dilute the mentor-grade KPI.

Ordering among these is a founder call made on demand signals (his own
learning plans, early users' languages) — the pipeline makes the choice
cheap to change.

## Spec amendments

- **C10 (dependency allowlist) — PROPOSED, ratify before R2:** official
  `tree-sitter-{language}` grammar crates are permitted as PACK-scoped,
  compile-time-linked dependencies (D23 unchanged — no dynamic plugins),
  one per registered pack. Everything else on the allowlist unchanged.
  (Blanket form so future packs don't each require a new amendment.)
- No engine/invariant changes. I29 re-verified per new pack (zero engine
  edits — additions confined to `packs/`, the registry table, one adapter
  module, and `Cargo.toml`).

## Risks

- **Taxonomy bloat → noise:** bounded by existing machinery (severity
  floor, min_gap, learning-memory gate) — breadth changes what the judge
  *can* cite, not how often cards surface.
- **Wrong-granularity concepts poisoning BKT:** mitigated by curation +
  append-only (a bad concept can be abandoned by the canon/judge and simply
  goes unused; it never has to be renamed).
- **Adapter fragility on user machines** (Python/TS toolchains vary far
  more than cargo/go): adapters must degrade silently to
  no-compiler-evidence rather than erroring the watch loop.

## Needs founder input (non-blocking; strawmen chosen)

Python diagnostics tool (strawman: ruff-then-py_compile fallback) · TS
runner (strawman: project-local `tsc --noEmit`) · Rust enrichment target
size (strawman: 25–40) · pipeline prompts living in-repo at
`packs/PIPELINE.md` (strawman: yes — they contain no secrets and are part
of the public derivation).

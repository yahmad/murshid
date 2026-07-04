# murshid — Architecture & Onboarding

A map of the system for a developer who did **not** write it. Read time ~15
minutes; after it you should know where to look for anything.

> **Source of truth.** Specs in `specs/` are authoritative; code is derived
> from them. The design authority is `~/src/yahmad/dev-context/specs/murshid/
> SPEC.md` **v0.5** (contracts C1–C12, decisions D1–D24, invariants I1–I30).
> The code is dense with references like `C6`, `D18`, `I28` — the
> [Glossary](#5-glossary-of-spec-shorthand) decodes every one that actually
> appears in `src/`. Read it first if the annotations are slowing you down.

---

## 1. What murshid is

murshid is a **local-first CLI coding mentor**. It runs as a terminal pane
(`murshid watch`) beside your editor, watches the files you actually change,
and — at safe moments — speaks up with a short pedagogical **card**: a named
idiom, best-practice, or likely-bug teaching moment grounded in the exact
lines you just wrote. It is for an experienced engineer onboarding to an
*unfamiliar* language on a *real* codebase (Rust first, Go second). It never
writes your code; help is a dial from a direct answer down to a Socratic
question.

The mental model is a pipeline:

> **watch → quiescence/debounce → session-diff → screen → judge → card →
> learner interaction → memory (BKT) update**

You save a file. murshid waits for a typing pause and a clean parse
(*quiescence*), diffs the file against the session-start snapshot, sends the
changed hunks to a **cheap "screen" model** to spot candidate teaching
moments, then to a **strong "judge" model** that must ground the claim in the
diff, name the idiom, and produce the concrete better version — or the
candidate is dropped. What survives becomes at most **one card on screen at a
time**; the rest queue. How you respond (applied / got-it / not-now /
not-useful) feeds two systems: a **noise economy** (so murshid learns to shut
up about things you ignore) and a **per-concept mastery model** (Bayesian
Knowledge Tracing) that fades support as you improve.

Two design obsessions dominate the code. First, **noise control**: adjacent
products (PR-review bots) drowned users in nits, so murshid is budgeted,
dedup'd, diff-scoped, and auto-throttling by construction (contracts CD-1 /
C3 / C7). Second, **portability**: nothing in the pedagogy engine is
language-shaped; each language is a data-driven **pack** plus one small
adapter. Adding Go required *zero* engine edits — the "architecture-honesty
test" (I29).

---

## 2. End-to-end data flow (a save becomes a card)

```
 file save
    │
    ▼
[watcher.rs / notify]  native + polling file events, exclusions
    │
    ▼
[quiescence.rs]  wait for typing-pause (C12: 2s) + clean tree-sitter parse (D8)
    │
    ▼
[session.rs / diff.rs]  session diff = worktree vs session-start snapshot (C2/I1)
    │                    advice attaches ONLY to added lines
    ▼
[site.rs]  compute Site identity = (file, enclosing item, anchor-hash) (C2)
    │
    ▼
[judge.rs stage 1 "screen"]  cheap model: candidate moments + positive-
    │                         application detections for below-mastery concepts
    ▼
[judge.rs stage 2 "judge"]  strong model: must ground+name+rewrite, forced-
    │                         choice a concept slug from the pack taxonomy;
    │                         fail any leg → dropped (D9/C6)
    ▼
[suppression.rs / aggregate.rs]  drop if snoozed/already-taught; collapse
    │                             same concept at many sites into ONE card (D11)
    ▼
[memory.rs / bkt.rs / ladder.rs]  pick entry RUNG from BKT mastery p +
    │                              Wood shift + directness knob (C4)
    ▼
[budget.rs / noise.rs / throttle.rs]  is there a push token? is the category
    │                                  above the severity floor / not throttled?
    ├── no ──▶ [queue.rs]  demote to pull queue ("2 more thoughts — m")
    ▼ yes
[card.rs]  render R2/R3 card (~80 col markdown, NO_COLOR-safe) (I21)
    │
    ▼
[response.rs]  user presses a/g/u/n/e/t/k  →  C3 response enum
    │
    ├─▶ [memory.rs]  applied/misuse → BKT pass/hard/fail update (mastery moves)
    ├─▶ [throttle.rs] not-now/not-useful → noise state (mastery untouched, I23)
    └─▶ [thread.rs]   k = ask → card-anchored follow-up thread (D20)
```

Provider calls (screen, judge, threads, review, recall grading) all go
through **`provider.rs`**, which shells out to `curl` and enforces two
dispatch **lanes** (see below). At **watcher exit**, `bookend.rs` prints a
one-screen session summary against the goal.

The whole `watch` loop lives in **`src/watch/`** (`run` in `mod.rs` wires the
threads; `sweep.rs` is the file-event → card path; `keys.rs` the keystroke
handlers; `offers.rs` the struggle-offer poll). `pipeline.rs` is the reusable,
dispatch-injected orchestration (diff → judge → card) that both the live loop
and the integration tests drive.

---

## 3. Module map

Grouped by layer. All paths are under `src/`.

### Entry / CLI
| Module | Responsibility |
|---|---|
| `main.rs` | Thin binary: init DB, dispatch subcommand (`setup`/`watch`/`goal`/`review`/`progress`). |
| `lib.rs` | The `murshid` library crate surface; cross-module helpers (`run_review`, `persist_review_digest`, key resolution) so `tests/` can drive the real pipeline. |
| `cli/setup.rs` | Onboarding: `.env` key import → keyring, `.gitignore` hygiene. |
| `cli/goal.rs` | `murshid goal [text]` — file-backed goal (`.murshid/goal`, D13c). |
| `cli/progress.rs` | `murshid progress` — the open skill meter (I24). |
| `cli/review.rs` | `murshid review` — solicited whole-diff digest (D18). |

### Watch pipeline
| Module | Responsibility |
|---|---|
| `watcher.rs` | Native + polling file watch, exclusions, resource limits (inherited). |
| `watcher_coordinator.rs` | Coordinates watch backends. |
| `watch/mod.rs` | `watch::run` — owns the pane, spawns stdin/offer/file-event threads. |
| `watch/sweep.rs` | The file-event → judge → card "sweep" (auto-push decision). |
| `watch/keys.rs` | In-pane keystroke handling (card responses, queue browse, goal edit). |
| `watch/offers.rs` | Struggle-offer polling loop. |
| `quiescence.rs` | The D8 gate: judge only after save + pause + clean parse. |
| `session.rs` | Session lifecycle, start snapshot, session diff (C2). |
| `diff.rs` | Stdlib LCS unified diff; advice attaches to added lines only (I1). |
| `site.rs` | Site identity + tree-sitter parse check (C2/D8); language-agnostic. |
| `pipeline.rs` | Dispatch-injected orchestration: hunks → two-stage judge → card. |
| `judge.rs` | Two-stage judge (screen + judge), output-contract validation, degraded mode (D9/C6). |
| `compiler.rs` | Rust diagnostics adapter (`cargo check --message-format=json`). |

### Pedagogy / domain
| Module | Responsibility |
|---|---|
| `bkt.rs` | Bayesian Knowledge Tracing: category priors + pure two-step update (C12). |
| `memory.rs` | Memory orchestration: concept-memory rows, evidence→BKT→persist, entry-rung, fade (C8). |
| `ladder.rs` | The rung ladder (R0–R3): entry-rung composition, escalation, R3 worked-example render (C4). |
| `staleness.rs` | Per-category staleness windows; never lowers mastery, only flags for recall (I26). |
| `retrieval.rs` | Scarcity-triggered recall questions at session boundaries, ≤2/session (D22). |
| `card.rs` | Card rendering (★ header, anchor, verdict+why, rule, folded diff) (I21). |
| `response.rs` | Maps in-pane keys → C3 response enum. |
| `queue.rs` | Single pull queue: presence indicator, `m` browse, C7 ordering. |
| `budget.rs` | Push token-bucket with strict-mode bug exemption (D10). |
| `noise.rs` | Frequency knob → (budget, severity-floor) pair; `quiet` default (I7). |
| `throttle.rs` | Category auto-throttle: <15% action rate over last 20 cards → queue-only (D12). |
| `suppression.rs` | Tiered snooze (D11c) + regression re-open (I3). |
| `aggregate.rs` | Collapse same concept at many sites into one card (D11). |
| `struggle.rs` | Struggle signals: repeated-error, time-in-red, help-comment, convergence (D15). |
| `offer.rs` | The struggle offer: one line, idle-gated, evidenced, decline-persistent (I9–I13). |
| `comment.rs` | `// murshid:` addressed comments — direct ask, skips screen stage (D17). |
| `thread.rs` | Card-anchored follow-up threads, 5-turn cap (D20). |
| `review.rs` | Solicited whole-diff review digest; architecture advice allowed (D18). |
| `bookend.rs` | Session-end summary screen against the goal (D14b). |
| `goal.rs` | Goal inference + `.murshid/goal` file storage + relevance lens (D13/D14). |
| `progress.rs` | Skill-meter rendering (I24). |
| `consent.rs` | BYOK spend consent (`ask`/`always`) for solicited calls (C6). |

### Persistence
| Module | Responsibility |
|---|---|
| `db.rs` | SQLite (rusqlite bundled): open, migrations, `events`/`cards`/`concept_memory`/`threads`/`suppressions` (C5). |
| `backup.rs` | Corruption recovery + backups. |
| `session.rs` | (also persistence-adjacent — session snapshot state). |
| `sha256.rs` | Dependency-free SHA-256 for snapshots and fingerprints. |
| `config.rs` | TOML config merge/precedence + lock; knob storage (C12). |
| `credentials.rs` | Keyring cache, env override, SIGHUP reload (C6). |
| `sanitizer.rs` | Secrets/path redaction before **any** LLM call (non-negotiable). |

### Providers / packs
| Module | Responsibility |
|---|---|
| `provider.rs` | BYOK dispatcher: Claude / Gemini / Ollama-compatible via `curl` subprocess; the two dispatch lanes (C6). |
| `pack.rs` | Pack loader + registry: reads pack data payloads, resolves grammar + adapter. The one allowed home for language literals. |
| `compiler.rs` | Rust pack diagnostics adapter (payload 1). |
| `go_adapter.rs` | Go pack diagnostics adapter (`go vet ./...`, payload 1). |

Language **data** lives outside `src/`, in `packs/<lang>/`:
`taxonomy.json` (concept DAG), `canon.json` (idiom entries), `surface.toml`
(comment token, extensions, check commands), `grammar.json`, and
`prompts/` (judge-prompt fragments).

---

## 4. Key concepts & data model

- **Card** — one teaching moment about one concept (I16). Six fields (D19):
  anchor (file:line + expression), verdict+why, named rule, ladder state,
  folded worked diff, resurfacing hook. Rendered by `card.rs`; persisted in
  the `cards` table.
- **Rung / ladder** — how much help a card gives, from `ladder.rs` per the C4
  table: **R3** worked example → **R2** teach → **R1** nudge → **R0** recall
  question → **silence** (mastered). Entry rung is computed from BKT mastery
  `p`, a Wood ±1 contingent shift, and the directness knob.
- **Lane** — a `provider.rs` dispatch class (C6/T11). **`Sweep`** = the
  watcher's auto-judging path; a new Sweep aborts the in-flight Sweep only.
  **`Interactive`** = everything user-initiated (threads, review, comment-
  asks, recall grading); never aborted by Sweep, serialized among themselves.
- **Session** — one `watcher` run; an idle gap > 4h starts a new session id
  (C2). The pull queue and all snoozes die at session end; only the event log
  and concept memory are durable.
- **Site** — `(file path, enclosing item name, normalized-anchor hash)` (C2).
  Survives line shifts; an item rename retires the site. The dedup key
  (*advice-fingerprint*) is `(concept_id, site)`.
- **Bookend** — the auto session-end summary (`bookend.rs`, D14b): goal
  recap, counts, concepts taught, throttled categories, top queued cards.
- **Concept memory / BKT** — per-concept mastery in `concept_memory`
  (`p_mastery`, `help_level`, `last_encounter_ts`, `lapse_count`). `bkt.rs`
  does the arithmetic; the gate `p ≥ 0.95` means "mastered → silence". Only
  observed **application or misuse** moves it (I22/I23); dismissals do not.

**The `events` table (C5)** is the append-only spine — every
`card_shown`, `card_response`, `check_result`, `prompt_offered`,
`encounter`, `goal_set`, etc. Noise metrics (EFP, throttle) and struggle
baselines are *computed from it at session start*, never stored. Other tables:
`cards`, `concept_memory`, `threads`, `suppressions` (session-scoped).

---

## 5. Glossary of spec shorthand

**The most important section for a new maintainer.** The code annotates
almost every function with a spec ref. Decode them here. All entries below
are drawn from the readable SPEC v0.5; a few conceptual terms are marked
**(inferred)** where the code comment, not the SPEC, is the source.

### Acronyms & terms
| Term | Meaning |
|---|---|
| **BKT** | Bayesian Knowledge Tracing — the per-concept mastery model (`bkt.rs`). 4 priors per category, updates on pass/hard/fail, mastery gate p≥0.95. |
| **EFP** | Effective False Positive — Tricorder's noise metric: any card the user takes no positive action on, *regardless of truth*. Per-category EFP drives auto-throttle. The whole noise economy exists to keep EFP low. |
| **BYOK** | Bring Your Own Key — the user supplies the model API key (or a local model); murshid never pays for inference. |
| **quiescence** | The gate before judging: a save, a typing pause (C12: 2s), and a clean tree-sitter parse (D8). Judging WIP is noise. |
| **screen vs judge** | The two model stages (D9/C6). *Screen* = cheap/fast model, finds candidate moments. *Judge* = strong model, must ground+name+rewrite or drop. |
| **rung** | A help level on the card ladder R0–R3 (C4). Higher = more help. |
| **lane** | A `provider.rs` dispatch class: `Sweep` vs `Interactive` (T11). |
| **pack** | A per-language plugin = one code adapter + data files under `packs/<lang>/` (CD-5). |
| **site** | Semantic location of advice: `(file, enclosing item, anchor hash)` (C2). |
| **bookend** | The auto session-end summary screen (D14b). |
| **struggle** | Inferred difficulty (repeated error, time-in-red) that may trigger a proactive *offer* — never an unsolicited hint (D15/CD-2). |
| **sweep** | The watcher's file-event → judge → card auto-push path (`watch/sweep.rs`) *(inferred: engineering term, not a SPEC word)*. |
| **advice window / session diff** | Code changed since session start; advice attaches only here (I1/C2). |
| **fingerprint** | Two kinds (C2): *finding-fp* (pack-supplied, tool identity across runs) and *advice-fp* = `(concept, site)`, the dedup key. |
| **canon / taxonomy** | Pack data: *taxonomy* = concept DAG (the slugs BKT tracks); *canon* = idiom catalog (`what/why/example/use-instead`) (D24). |

### Contracts (C-numbers, NORMATIVE — the builder's spec)
| Ref | Subject |
|---|---|
| **C1** | Normativity & precedence (contracts > CD body > log); C12 defaults are config-grade. |
| **C2** | Vocabulary & identity: session, session diff/advice window, site, the two fingerprints, concept binding. |
| **C3** | Card response enum (`applied`/`escalated`/`got_it`/`not_now`/`not_useful`/`expired`) + action-rate/EFP definitions. |
| **C4** | The rung ladder table (R0–R3 + silence) and entry-rung mapping from BKT `p`. |
| **C5** | The store: SQLite schema (`events`, `cards`, `concept_memory`, `threads`, `suppressions`). |
| **C6** | The model seam: two slots (`model.screen`/`model.judge`), providers, stage I/O contracts, degraded mode, consent. |
| **C7** | Scheduling & contention: queue order, struggle preemption, one-card-on-screen. |
| **C8** | Memory contracts: pass/hard/fail evidence, regression re-open, concept cooldown, category priors. |
| **C9** | Pack payload amendments: payload-6 grammar reference; `severity`→`tool_level`. |
| **C10** | Implementation constraints: Rust 2024, the dependency allowlist. |
| **C11** | Goals & parity obligations at the dogfood gate. |
| **C12** | Binding v1 defaults table (quiescence 2s, 4h idle split, budgets, throttle floor, BKT priors, staleness windows…). |

### Key decisions (D-numbers, historical rationale)
| Ref | Ruling (short) |
|---|---|
| **D1–D7** | Foundations: wedge = language-onboarding mentor (D1); language-agnostic core, Rust then Go (D2); diff-inference primary trigger (D3); CLI watcher first (D4); two knobs + escalation (D5); record everything locally (D6); free BYOK core + paid app (D7). |
| **D8** | Judgment moment = quiescence-gated (save + pause + parse). |
| **D9** | Precision gate = two-stage judge; no naked confidence gates. |
| **D10** | Frequency knob moves a (budget, severity-floor) pair; one card on screen; likely-bug exempt. |
| **D11** | Dedup = `(concept, site)`; aggregate multi-site; tiered snooze. |
| **D12** | Category auto-throttle on low action rate (noise control, not mastery). |
| **D13** | Goal capture = infer-first + confirm; `.murshid/goal` file store. |
| **D14** | Goal semantics = relevance lens + session-end bookend. |
| **D15** | Struggle signals = converged inferred + self-declared. |
| **D16** | Struggle prompts share the CD-1 push budget. |
| **D17** | `// murshid:` addressed comments = direct ask, skips offer + screen. |
| **D18** | Solicited "how would you do this better?" review (`murshid review`), EFP-exempt. |
| **D19** | The six-field card anatomy. |
| **D20** | Card-anchored follow-up thread (no open chat), 5-turn cap. |
| **D21** | Learner model = per-concept BKT with forgetting, hand-set priors. |
| **D22** | Retrieval practice = scarcity-triggered recall at session boundaries. |
| **D23** | Engine/pack boundary = adapter-plus-data packs. |
| **D24** | Pack payload schema = five (six w/ grammar) payloads. |

### Invariants (I-numbers, always-true rules) — the ones in code
| Ref | Rule (short) |
|---|---|
| **I1** | Diff-scoped only: advice attaches only to changed code. |
| **I2** | EFP is the noise metric; every card records shown→response. |
| **I3** | Never re-raise once shown/snoozed (unless the code regresses). |
| **I7** | Ship chill: defaults at the quiet end (`quiet` frequency). |
| **I8–I14** | Session-goal / struggle etiquette: goal never blocks (I8); offer, never act (I9); ignorable by working (I10); show the evidence (I11); remember every no (I12); tool voice, no persona (I13); convergence before prompting (I14). |
| **I15–I21** | Card contract: task not learner (I15); one card one concept (I16); fix + rule (I17); ladder with contingent entry (I18); bottom-out is a worked example (I19); one-keystroke lifecycle verb (I20); terminal-presentation contract, ~80col/NO_COLOR (I21). |
| **I22–I26** | Memory contract: reviews implicit-first (I22); dismissal is evidence-free (I23); model is open, fade announced (I24 = the `progress` meter); concept grain validated by learning curves (I25); no standalone review surface in v1 (I26). |
| **I27–I30** | Pack seam: one narrow adapter, rest data (I27); the normalized record (I28); Go honesty = zero engine edits (I29); taxonomies are per-language (I30). |

### Tasks (T-numbers — the build ladder, all Done)
T1 watcher→card slice · T2 noise machinery · T3 goals & struggle · T4 card
interaction · T5 memory/BKT · T6 pack seam · T7 Go pack (the zero-engine-edit
test) · T8 OpenAI-compatible provider · T9 correctness cleanup · T10 pack
auto-detect · T11 dispatch lanes · T12 watch decomposition · T13 conformance
+ test debt. See `specs/tasks/T*.md`.

---

## 6. Building, testing, running

```bash
cargo build            # edition 2024
cargo test             # ~526 tests (unit, colocated in src/ + tests/watch_pipeline.rs)
cargo clippy -- -D warnings   # kept clean throughout the build ladder
```

**Dependencies** (allowlist, C10 / `Cargo.toml`): `rusqlite` (bundled
SQLite), `serde`/`serde_json`, `keyring`, `libc`, `notify` (file watch),
`tree-sitter` + `tree-sitter-rust` + `tree-sitter-go`. Adding any other
dependency requires a spec amendment.

- **Note — LLM transport.** SPEC C10 lists `ureq` for BYOK HTTP, but the
  implementation instead **shells out to `curl`** (`provider.rs::spawn_curl`),
  fed a `curl --config` document over stdin so URL/headers/body never appear
  in argv (T9 hardening). No HTTP crate is in `Cargo.toml`. `curl` must be on
  `PATH` at runtime for any model call; without it (or a key), murshid runs in
  **degraded / observe-only** mode (events still recorded, no LLM cards).
- **tree-sitter grammars** are compiled in via the two grammar crates; each
  pack also pins its grammar in pack data (payload 6, C9).
- **Tests never hit the network:** `pipeline.rs`, `judge.rs`, and
  `run_review` take the dispatch function as an injected parameter, so tests
  drive recorded fixture responses (`tests/fixtures/`). `spawn_curl` is
  likewise injectable.
- **Known flakiness:** `credentials.rs` keyring tests can be parallel-flaky
  (real Keychain + SIGHUP); run them serially if they blink.

**Running:** `murshid setup` (import keys from `.env` → OS keyring, fix
`.gitignore`) → `murshid watch` in a pane beside your editor. State lands in
`.murshid/` (git-ignored). Also: `murshid goal [text]`, `murshid review`,
`murshid progress`.

---

## 7. Where things live / how to extend

### Adding a language pack (the pack seam)
The seam is deliberately narrow (I27/I29): **one adapter of code + data
files, zero engine edits.** To add a language:

1. **Author `packs/<lang>/` data** — `taxonomy.json` (concept DAG),
   `canon.json` (idiom entries: `id/category/what_it_does/why_is_this_bad/
   example/use_instead/refs`), `surface.toml` (comment token, file
   extensions, check/test commands), `grammar.json`, and `prompts/`
   (judge-prompt fragments). See `packs/rust/` and `packs/go/` as templates.
2. **Write one `DiagnosticsAdapter`** (`src/<lang>_adapter.rs`) that shells
   the language's checker and maps native output to `pack::NormalizedRecord`
   (I28). See `compiler.rs` (Rust, `cargo check`) and `go_adapter.rs`
   (Go, `go vet`).
3. **Register it in `pack.rs`** — one arm each in `resolve_ts_language`
   (grammar), `diagnostics_adapter` (adapter), and the detection table. A
   pack's language id is just its directory name. *(These registry arms are
   the sanctioned exception — they do not count against I29's zero-edit
   criterion; see the note near `pack.rs:446`.)*

Everything downstream (dedup, budget, rung selection, rendering) is generic
and never learns which language it is — which is what makes mastery portable
across languages.

### How provider dispatch works
`provider.rs` is the single BYOK dispatcher. Two config slots (C6):
`model.screen` (cheap/fast) and `model.judge` (strong), each a `(provider,
model, key-ref/endpoint)`. Providers: **Claude API, Gemini API, and any
Ollama / OpenAI-compatible local endpoint** (T8). Keys resolve via keyring
with env override (`credentials.rs`; `lib.rs::resolve_slot_key`); Ollama needs
no key. Every request is rendered to a `curl --config` document and spawned as
a subprocess. Callers pick a **lane**: `Sweep` (watcher auto-judging,
self-superseding) or `Interactive` (user-initiated, serialized, never aborted
by Sweep). All prompts pass through `sanitizer.rs` first — secrets and paths
are redacted before any bytes leave the machine.

---

*Doc reflects the tree at v0.5 / T13 complete. When code and this doc drift,
the SPEC wins; fix whichever is stale.*

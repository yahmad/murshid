# Engineering roadmap — review-surfaced follow-ups

Tech-debt and hardening items surfaced by a multi-agent codebase review
(2026-07: five independent reviewers across persistence, watch pipeline,
provider/config, domain types, and CLI). The large clarity/architecture pass
is **done and merged** (see *Completed* below); this file now tracks the
**remaining long-tail**, all of which the founder has approved for action.

> **Status 2026-07-04 — all 15 long-tail items landed (items 1–15), 586 tests
> green.** Item 3 (events rolling-window retention) was a sanctioned *semantics*
> change: its spec amendment was **ratified by the founder 2026-07-04** (90-day
> baseline, 180-day decline window) and the code shipped
> (`specs/AMENDMENT-events-retention.md`). Each item was committed separately
> (semantic messages) with `cargo fmt`/`clippy`/`test` green per commit. See the
> per-item ✅ notes below.

Effort key: **S** ≈ <½ day · **M** ≈ 1–2 days · **L** ≈ multi-day. Line
numbers shift — items cite the primary **symbol(s)**; grep to locate.

---

## For the next agent — start here

You are picking up vetted follow-ups (not speculative — each came from the
review). Hold the code to a senior-Rust bar; behaviour-preserving unless a
change is explicitly sanctioned below. Do NOT brief yourself or any subagent
as "the code is already good, just find bugs" — that produces a minimal-diff
pass; brief as "hold to the bar, behaviour-preserving, tests green, STOP and
report if unsure."

**Gates (every commit):**
```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
A PostToolUse hook runs `cargo test` after each edit — expect intermediate
breakage mid-refactor; drive to green. `watcher::tests::test_polling_watcher_integration`
and `test_native_watcher_integration` are timing-flaky — re-run 2–3× to tell a
real break from a flake.

**Constraints:** behaviour + on-disk DB/backup format preserved, EXCEPT the two
sanctioned changes flagged below (config *warn-on-unknown*; events *rolling-window
retention*). Any enum `as_str()` must stay byte-identical to stored TEXT.
Stdlib-first — the ONLY approved new dependency is `proptest` (**dev**-dependency,
item 8); ask before adding any other.

**Founder decisions already made (apply these):**
- Do **all** items below (full scope).
- `proptest` dev-dependency: **approved** (item 8).
- Events retention (item 3): a **rolling window** is wanted — but it changes
  struggle-baseline / decline-count computations, so it **requires a spec
  amendment**. Draft it in `specs/` (propose the window — e.g. last N sessions
  or M days), FLAG it for founder approval, and gate the change behind it; do
  not silently ship the semantics change.
- Keyed remote OpenAI-compatible endpoints (item 6): **in scope** — fix the
  production-dead keyed-`openai` auth path.

**specs/INDEX.md active spec is "none"** — this is founder-directed work, not
spec implementation. Only item 3 touches spec-level semantics (handle as above).

**Orchestration:** this is large. You MAY fan out DISJOINT file-sets to
worktree-isolated subagents (review each diff before merging); keep interlocking
`watch/*` + `db/*` work sequential. Commit per cohesive change with semantic
messages; push to `main`.

---

## Idioms & tools already in the tree (use these — don't reinvent)

The prior pass built the patterns these items lean on:

- **`db/` module directory** — per-domain files (`mod`, `migrations`, `events`,
  `cards`, `suppressions`, `threads`, `concept_memory`, `history`) each with a
  co-located `#[cfg(test)] mod tests` + a `test_support` module.
- **`db::with_tx(&Connection, |tx| …)`** — retry-owning transaction helper;
  plain non-retrying `*_stmt` write variants exist for use inside it.
- **`provider::Provider { Gemini, Claude, OpenAiCompat(OpenAiKind) }`** —
  `parse`/`as_str`/`default_model`/`is_keyless`; the single provider-identity type.
- **`ResolvedSlot { provider, model, key, base_url }` + `Models { screen, judge }`**
  (lib.rs), with `Models::resolve(&cfg.models, &keys)` and
  `ResolvedSlot::dispatch(lane, prompt)`.
- **Decomposed sweep** — `watch/sweep.rs` has `handle_session_split`,
  `run_diagnostics_check`, `run_comment_asks`, `run_applied_detection`,
  `judge_and_collect_finding`, `aggregate_and_dispatch`; sweeps run on a
  quiescence **worker thread** (`run_quiescence_worker`, mpsc + `recv_timeout`
  debounce) owning one long-lived DB connection.
- **Enum idioms to copy** (`as_str`/`parse`): `ladder::Rung`, `bkt::Grade`,
  `provider::Provider`, `db::cards::CardStatus`, `response::ResponseVerb`,
  `memory::EvidenceSource`, `suppression::SnoozeScope`, `pack::Category`
  (has an `Other(String)` pack-authored fallback), `ladder::RungShown`.

See `ARCHITECTURE.md` for the mental model.

---

## Completed (2026-07 review pass — merged to main)

- **`db.rs` → `db/`** per-domain modules + facade; tests co-located per-domain.
- **`enum Provider`** — five stringly-matched sites → one parse boundary + exhaustive dispatch.
- **`ResolvedSlot`/`Models` bundle** — killed the 8-field threading, `run_review` 18→10 params, the `_for_stdin` reclone wall, 6 duplicated dispatch closures.
- **`on_file_event` decomposed** (1300 → ~320-line director) + **debounce inverted** (thin notify producer → quiescence worker; per-file diagnostics preserved, judge coalesced; one sanctioned timing change: a burst yields one sweep after the last save).
- **De-stringify** — `CardStatus`, `ResponseVerb`, `EvidenceSource`, `SnoozeScope`, plus `Category` (with `Other` fallback) and `RungShown` (offer sentinel).
- **CLI** — single exit point (`cli::dispatch`), `--help`/`-V` exit-0-to-stdout + usage-on-error-to-stderr, testable decision cores, dead `EXIT_*` removed.
- **Config-security TOCTOU** fixed (check + read one fd).
- **`db::with_tx`** — card+event paired writes now atomic, with the atomicity test that was missing.
- **`events(kind)` index** (migration 12); **`concept_memory` tests** (was zero); **budget** doc-vs-impl fixed.
- **Quick wins** — retrieval grade→`Grade`, `record_fingerprint` deduped into `pack`, real cargo parser tested, `sha256_hex` allocation, `render_age` "just now", sanitizer magic-const.

---

## Remaining (all in scope — grouped; suggested order at the bottom)

### Reliability
1. **Bound `cargo check` / `go vet` children with a wall-clock timeout.**
   `compiler.rs::run_check` (`child.wait_with_output`) and `go_adapter.rs`
   (`Command::output`) have none, unlike curl's `max-time`. A wedged toolchain
   now hangs the **single sweep worker thread** → blocks all sweeps. Add a
   watchdog that SIGTERMs via the existing `terminate_process`. **M**

### Database
2. **`db::Error` enum boundary** — `enum { Busy, Corrupt, NotFound, Backend(rusqlite::Error) }`
   classifying via existing `is_busy_error`/`is_corrupt_error`; make `NotFound`
   explicit rather than folding into `Ok(None)`. Low intrinsic value today
   (nothing branches on it) but requested — keep it mechanical. **S/M**
3. **Events retention / VACUUM — rolling window (SANCTIONED semantics change; needs spec amendment).**
   `db/events.rs::all_check_result_points` and `count_declined_offers_for_concept`
   read all-time/all-session (feeding the struggle baseline per D15 and the
   7-day decline suppression per T3 req 13); nothing prunes `events`/`context_history`.
   Draft a spec amendment in `specs/` proposing the window, get founder sign-off,
   then apply it consistently in those reads + add a prune/VACUUM policy. **M**
4. **De-JSON the hot events reads.** The `kind` index is done; the same
   functions plus `latest_throttle_action` and `retrieval_questions_asked_this_session`
   still `SELECT payload_json` + `serde_json::from_str` every row to filter on
   `$.category`/`$.source`/`$.verb`. Push the predicate into SQL via `json_extract`
   expression indexes (bundled sqlite, no dep) or promote hot discriminants to
   columns (needs a backfill migration). **M**

### Config / provider / credentials
5. **Config-value enums with warn-on-unknown** (SANCTIONED: adds a stderr warning).
   `config.rs::merge_toml` silently accepts any string for `dial.directness`,
   `consent.solicited_spend`, `pedagogy.style`, `provider.api_key_source`. The
   `api_key_source` one is **security-adjacent**: `credentials.rs`'s
   `api_key_source == "keychain"` means a typo silently downgrades secret storage
   to plaintext-env — do this one first. Promote to enums with `FromStr` that
   emit `[WARNING] unknown <field> …, keeping <default>` (mirror
   `pack::payload_fallback_notice`). `Directness` is already an enum — give it
   the warning path. **M**
6. **Generalize credentials + fix the dead keyed-`openai` path (in scope).**
   `credentials.rs::CachedKeys { gemini_api_key, claude_api_key }` hardcodes two
   providers with two copy-paste keyring blocks; `lib.rs::resolve_slot_key`
   returns `None` for `openai`, so a keyed OpenAI-compatible endpoint's
   bearer-auth arm (`provider.rs`) is production-dead. Key the cache by
   `Provider` (`HashMap<Provider, String>`) with one keyring-load loop; add the
   openai slot. KEEP the on-disk keyring usernames (`gemini_api_key`/`claude_api_key`)
   stable to avoid a migration. Note: keyring tests are flaky and gated by
   `MURSHID_NO_KEYCHAIN`; `cli/setup.rs` imports `.env`→keyring. **M**
7. **Cache `load_config` + validate `base_url` at load.** `credentials.rs::load_keys_from_source`
   calls `load_config()` twice; `main.rs` loads again. Load once, thread
   `&AppConfig` (or a `OnceLock`). Separately: resolve+validate a slot's
   `base_url` when building `ResolvedSlot` so an unconfigured `openai` slot fails
   fast at load, not silently at first dispatch (`provider.rs::resolve_base_url`). **S/M**

### Types / idiom
8. **Property-based tests** (`proptest` dev-dep, approved). Cover: BKT update
   ∈ [0,1] + pass-monotonicity (`bkt.rs`); sanitizer idempotence AND never-leak
   (embed a key shape at a RANDOM offset in random text → output always
   `[REDACTED]`, never the key — `sanitizer.rs`); exhaustive
   `parse(as_str(x)) == x` for every enum listed above. **M**
9. **`diff::hunks_signature` off `Debug`** — it hashes `format!("{:?}", hunks)`;
   hash the structural fields instead (a `Debug`/field change silently shifts
   the dedup key). Existing `test_hunks_signature_*` pin the invariant. **S**
10. **`ConceptMemoryRow.last_outcome: Option<String>` → `Option<Grade>`**
    (concept_memory.rs); written from `grade.as_str()`, re-parsed on read
    (memory.rs). Keep the column TEXT; convert only at the db boundary. **S**

### Pack seam
11. **Consolidate the pack registry.** `pack.rs::resolve_ts_language`,
    `diagnostics_adapter`, and `resolve_pack_id` are three independent
    `match language_id` sites that must agree (a new pack = three arms). One
    `PackRegistry` table keyed by `language_id` → `(grammar_fn, adapter_ctor)`. **S/M**
12. **`include_str!` the rust fallback.** `pack.rs::SurfaceConfig::default()` and
    `GrammarSpec::default()` hand-mirror ~50 lines of `packs/rust/{surface.toml,grammar.json}`,
    kept honest only by lockstep tests. `include_str!` + parse the real payloads;
    delete the literals AND the now-redundant lockstep tests. **S**

### Watch / testing
13. **Pipeline branch integration tests** (do EARLY — de-risks the rest). No test
    exercises push-vs-queue, throttle/floor, concept-collapse-on-ship, comment-ask,
    misuse-fail-without-push, or applied-detection as a FLOW — only leaf helpers.
    Drive the extracted `sweep.rs` fns against `db::initialize_db(":memory:")`
    with injected fixture dispatch (copy `tests/watch_pipeline.rs`), asserting
    `cards`/`events` rows + queue state. **M**
14. **Lock-accessor methods on `WatchSession`** (watch/mod.rs) — centralize the
    ~90 `.lock().unwrap_or_else(|e| e.into_inner())` poison-recovery sites behind
    accessor methods or a `LockExt::lock_poison_safe()` trait. **S**
15. **Wrap the last two `prompt_response` write-pairs in a transaction.**
    `watch/keys.rs`'s offer accept/decline handlers still do `update_card_status`
    + `log_event` as separate statements — wrap each in `db::with_tx`, preserving
    the outer `warn_on_err` best-effort semantics. **S**

---

## Suggested order (dependencies; reorder if you find better)

- **Phase 1 — cheap wins + safety net:** 13 (tests first), 9, 12, 15, 1.
- **Phase 2 — config/creds cluster:** 5 (`api_key_source` first), 6, 7.
- **Phase 3 — db cluster:** 4, 3 (draft+flag the spec amendment), 2, 10.
- **Phase 4 — pack + properties + cleanup:** 11, 8, 14.

---

## Completed — 2026-07-04 long-tail pass (all merged to main)

Every item above shipped. Grep the git log for the commit; one-line outcomes:

1. ✅ **Adapter subprocess timeout** — shared 120s watchdog in `pack.rs`
   (`wait_with_output_timeout` + centralized `terminate_process`); both
   `cargo check`/`go vet` map a wedged run to a TIMEOUT infra error.
2. ✅ **`db::Error`** — classified boundary type (`Busy/Corrupt/NotFound/
   Backend`) + tested `From<rusqlite::Error>`; not yet threaded through
   signatures (nothing branches on it — as scoped).
3. ✅ **Events retention** — RATIFIED 2026-07-04 + shipped: struggle baseline
   bounded to 90 days, decline count to 180 days, prune floor 180 days
   (`db/events.rs` + prune at watch startup). SPEC cross-ref is the one
   remaining follow-up (`specs/AMENDMENT-events-retention.md`).
4. ✅ **De-JSON hot reads** — `json_extract` predicate pushdown in
   `db/events.rs`; no per-row `serde_json::from_str`.
5. ✅ **Config enums warn-on-unknown** — `merge_enum_field`; `api_key_source`
   fails secure to `keychain` on a typo.
6. ✅ **Credentials keyed by `Provider`** — `KEY_SPECS` table + one load loop;
   fixed the production-dead keyed-`openai` bearer path.
7. ✅ **base_url validated at load** — `Models::config_warnings` surfaces a
   misconfigured `openai` slot at startup; `load_config` documented non-cached
   (a global memo broke `acquire_resources`'s live re-read).
8. ✅ **Property tests** — `proptest` dev-dep: BKT ∈[0,1] + pass-monotonicity,
   sanitizer idempotence + never-leak; enum round-trips completed.
9. ✅ **`hunks_signature`** off structural fields (length-prefixed), not `Debug`.
10. ✅ **`ConceptMemoryRow.last_outcome: Option<Grade>`** — convert at the db
    boundary; column TEXT unchanged.
11. ✅ **`PackRegistry`** — one table backs both grammar + adapter resolution.
12. ✅ **`include_str!` the rust fallback** — defaults parse the real payloads;
    lockstep tests deleted.
13. ✅ **Pipeline branch integration tests** — 5 sweep-flow tests (push-vs-queue,
    throttle, floor, concept-collapse, applied-detection); comment-ask skipped
    (no injectable dispatch seam — documented).
14. ✅ **`LockExt::lock_poison_safe()`** — ~100 poison-recovery sites centralized.
15. ✅ **Offer accept/decline write-pairs** wrapped in `db::with_tx`.

**Orchestration note:** items 10/11/13 were built in parallel by
worktree-isolated subagents and cherry-picked back (one `pack.rs` merge
conflict resolved by hand — watchdog + registry blocks coexist); the
interlocking config/creds/db work was done sequentially on `main`.

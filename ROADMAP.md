# Engineering roadmap — review-surfaced follow-ups

Prioritized tech-debt and hardening items from the 2026-07 codebase review
(architecture, idiom, testing, maintainability). The small, contained fixes
from that review are already landed on this branch; the items below are larger
or need their own design/spec because they change structure or runtime
behavior. Per the repo's spec-driven discipline, each should get a focused task
rather than an ad-hoc edit.

Effort key: **S** ≈ <½ day · **M** ≈ 1–2 days · **L** ≈ multi-day.
Each item cites the primary file(s); most were flagged by more than one
reviewer.

## High value

1. **Bound `cargo check` / `go vet` children with a wall-clock timeout.**
   `compiler.rs::run_check` (`child.wait_with_output`) and
   `go_adapter.rs` (`Command::output`) have no timeout, unlike the curl
   transport (`max-time = 60`). A wedged build (proc-macro loop, cargo blocked
   on an external lock, a hung toolchain) blocks the watcher/pipeline thread
   indefinitely. Add a watchdog that SIGTERMs via the existing
   `terminate_process`. *Reliability, hot path.* **M**

2. **Surface the remaining silent failures / add a `db::Error` boundary.**
   `warn_on_err` now covers state-mutating writes, but the paired
   `update_card_status`+`log_event` writes are still two independent
   non-transactional calls (`watch/keys.rs`, `watch/sweep.rs`): a partial
   failure leaves `cards` inconsistent with the event log that throttle/
   aggregation state is *derived* from. Wrap each such pair in one transaction.
   Natural moment to introduce a `db::Error` enum so callers can tell "busy"
   from "corrupt" from "not found" instead of matching on `rusqlite`. **M**

3. **Introduce a `ResolvedSlot` / `Models` bundle.** The single
   highest-leverage refactor. `{screen,judge}_{provider,model,key,base_url}` is
   threaded by hand through ~15 sites; `run_review` takes 18 params and there
   are 12 `#[allow(clippy::too_many_arguments)]`. A `struct ResolvedSlot
   { provider, model, key, base_url }` built once (next to `resolve_slot_key`)
   plus `struct Models { screen, judge }` collapses the param lists, deletes
   the ~50 per-thread field re-clones in `watch/mod.rs`, and lets the
   triplicated `safe_dispatch` closures (`lib.rs`, `watch/keys.rs`,
   `watch/sweep.rs`) become one `interactive_dispatch(slot, prompt)` helper.
   *Config/lib/watch.* **M**

4. **Index and bound the `events` table.** Hot-path queries
   (`all_check_result_points`, `count_declined_offers_for_concept`,
   `latest_throttle_action`) filter `WHERE kind = …` with no `kind` index, then
   JSON-parse every returned row in Rust, on every sweep — and nothing ever
   prunes `events`/`context_history`, so cost grows with lifetime history. Add
   `CREATE INDEX idx_events_kind ON events(kind, id)`, promote hot JSON
   discriminants (`category`, `source`, `verb`) to columns or an indexed
   `json_extract` expression, and add a retention/VACUUM policy. *`db.rs`.* **M**

## Structural (mechanical but large)

5. **Split `db.rs` (3224 LOC) into a `db/` module directory** by domain:
   `mod.rs` (path, `open_connection`, `execute_with_retry`, `warn_on_err`),
   `migrations.rs`, and one file per domain (`cards`, `events`, `suppressions`,
   `threads`, `concept_memory`, `history`). Pure move, no behavior change; the
   compiler + 40 existing tests are the safety net. Unlocks the other db work.
   **L**

6. **Decompose `on_file_event` (~1300 lines, ~10 concerns) and re-architect the
   debounce.** Today the quiescence wait is a blocking `thread::sleep` *inside*
   the single `notify` callback thread, so a burst of saves head-of-line-blocks
   and the supersede guard at `sweep.rs:~290` can almost never fire. Invert it:
   the callback records `(path, now)` and wakes a dedicated worker that waits
   out quiescence via `recv_timeout`/`Condvar` and sweeps once. In the same
   pass extract `handle_session_split`, `run_comment_asks`,
   `run_applied_detection`, `dispatch_aggregated_findings`, and inject the DB
   connection + dispatch fn (as `pipeline.rs` already does) so the split/
   aggregation/push-vs-queue logic becomes unit-testable against an in-memory
   DB. *Biggest maintainability + throughput win; also the riskiest.* **L**

7. **Replace stringly-typed provider dispatch with an enum/trait.** `provider_type: &str`
   is matched independently in four places (`provider.rs` build/parse/base-url,
   `lib.rs::resolve_slot_key`); adding a provider is a runtime `"Unknown
   provider"` string, not a compile error. An `enum Provider` (or `trait
   Provider` with `build_request`/`parse_response`) parsed once at the config
   boundary makes the set exhaustive — the pack seam already does this well and
   is the model to copy. **M**

8. **De-stringify statuses/verbs/sources and config enums.** Card `status`,
   response `verb`, evidence `source`, and event `kind` are raw `&str`
   compared across dozens of sites (`watch/*`, `memory.rs`, `throttle.rs`); a
   typo silently breaks a transition or analytics query. Config enum-shaped
   fields (`directness`, `solicited_spend`, `pedagogy.style`, `api_key_source`)
   accept invalid values in silence. Promote to enums with `as_str()`/`FromStr`
   (as `ladder::Rung`/`bkt::Grade` already do); have config `FromStr` warn on
   unknown values. **M**

## Smaller / opportunistic

9. **CLI hygiene** (`main.rs`, `cli/*`): add `--help`/`-h`/`-V` (currently
   `murshid --help` prints usage to stdout but exits 1); route usage-on-error
   to stderr; wire up or delete the dead `EXIT_*` constants in `setup.rs`; make
   `cli::review::run`/`cli::progress::run` return `Result`/exit-code instead of
   calling `process::exit` inline so their consent/degraded/empty branches
   become testable. **S–M**

10. **Config-security TOCTOU** (`config.rs::check_system_config_security` →
    `read_to_string`): stat-by-path then read-by-path lets an attacker swap the
    system config between the two syscalls, defeating the ownership/permission
    gate. Open once (`File::open`) and check `File::metadata` + read from the
    same fd. **M**

11. **`budget::consume_or_borrow` doc-vs-impl** (`budget.rs`): the doc says it
    "borrows one when empty" but the body is a no-op `try_consume` on an empty
    bucket. Implement the debt or fix the doc; add a test (currently none). **S**

12. **De-duplicate `record_fingerprint`** (byte-identical in `compiler.rs` and
    `go_adapter.rs` — engine-level identity, belongs in `pack.rs`/`site.rs`) and
    the cargo-span→`CompilerDiagnostic` mapping that the test re-implements
    instead of calling. **S**

13. **Process/idiom nits**: repo-wide `cargo fmt` is now enforced (CI); consider
    adding `cargo test` to a pre-commit hook mirroring CI; `sha256_hex`
    allocates per byte (`write!` into a pre-sized `String` avoids it);
    `progress::render_age` reports "1m ago" for a just-now encounter.

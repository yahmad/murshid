# Dogfood notes

Running log of friction/findings from real usage. Each entry: date, what
happened, severity, and whether it spawned a task. This file is the input
that decides what gets built next (INDEX.md: "next: dogfood").

## 2026-07-03 — pre-dogfood smoke test (scratch Rust project, agent-driven)

- **Pipeline works end-to-end.** Save with `E0308` → watcher → `cargo check`
  intercept → screen dispatch. Provider failure (invalid key) degraded
  gracefully, no crash. ✅
- **Keyring pollution (fixed, needs a real fix).** The real `murshid`
  keychain service can end up holding `mock-gemini-*` / mock claude keys left
  by an earlier test run — tests can write to the production service name
  when `MURSHID_TESTING` is unset.
  → Candidate task: test-suite must never touch the real service
  (under review in the cleanup pass).
- **Silent keychain-miss degrades UX.** Keychain entries not created by the
  murshid binary fail macOS ACL reads and the failure is reported as
  "no API key configured" (degraded mode) with no hint that a key EXISTS but
  is unreadable. Diagnosis cost: ~15 min with source access; a user would be
  stuck. → Candidate: distinguish "no entry" from "entry unreadable
  (keychain ACL)" in the degraded-mode reason string.
  → **RESOLVED 2026-07-04.** `credentials.rs` records providers whose keyring
    read failed with a non-`NoEntry` error (`CachedKeys::is_unreadable`),
    threaded to `ResolvedSlot::key_unreadable` and surfaced by
    `judge::determine_judge_mode` via `KeyStatus::{Present,Absent,Unreadable}`:
    a blocking unreadable slot now reports "an API key exists in the keychain
    but could not be read (access denied) — unlock the keychain, or re-add via
    `murshid setup`" instead of "no API key configured".
- **Duplicate FS events.** One `cat >` write produced three `File saved`
  events / three `[ERROR rust/E0308]` lines; dedup held (single screen
  dispatch), but budget accounting of duplicated events is untested in real
  editor conditions. Watch in real sessions.
- **Claude Max subscription ≠ API access.** Judge default (`claude`) is a
  paid-API-only path — a consumer chat subscription (e.g. Claude Max) does
  not grant API key access, so BYOK users on subscription-only plans need a
  different provider (or a separate API key) for the judge/screen slots.
  Dogfood config uses Gemini instead.
- **Local screen viability (research, 2026-07-03).** Best local model for
  M1 Pro 16GB: Qwen3.5-9B Q4_K_M (~5.7 GB, LM Studio/MLX; reportedly does
  NOT run in Ollama). LM Studio has no Ollama-native API → murshid's
  hardcoded `/api/generate` arm couldn't reach it → spawned **T8**
  (OpenAI-compat provider, approved 2026-07-03). Ollama-only alternative
  today: Gemma 4 E4B.
- **Shared-tree hazard confirmed relevant:** `murshid setup` was run on the
  murshid repo itself while an implementer agent edits `src/` — running
  `murshid watch` here during agent work would grade a firehose of
  non-human writes. Dogfood on a separate human-edited repo.

## 2026-07-05 — real dogfood session findings (judge silent-drop investigation)

- **Judge dropped a card, user saw only silence (severity: high; spawned
  T14).** The judge (`gemini-3.5-flash`) declined/failed on a candidate
  mid-session and nothing was surfaced — no card, no notice, nothing in the
  pane. Diagnosis required reading `judge.rs`/`pipeline.rs`/`watch/sweep.rs`
  source AND running SQL against `~/Library/Application Support/murshid/
  profile.db` by hand to find the `judge_drop` event. Root cause: judge
  outcomes were entirely un-instrumented — no raw model I/O was ever
  persisted anywhere (the `get_trace_logs_dir()` helper existed in
  `cli/setup.rs` but nothing wired to it), so a silent drop was
  undiagnosable without source access. → **Fixed by T14 req 1**: raw
  stage-1/stage-2 request+response (or dispatch error) now persisted per
  dispatch under `get_trace_logs_dir()`, bounded (14-day age window + 200
  MiB backstop, pruned at watch startup — mirrors
  `AMENDMENT-events-retention.md`'s pattern), behind a `[trace] enabled`
  knob (default on).
- **Declined vs. failed were logged identically (severity: medium; spawned
  T14 req 2).** `packs/*/prompts/stage2.md` instructs the model to respond
  `{}` when it can't ground a finding — a correct, deliberate "not a
  teaching moment" decision. But `validate_stage2_output` reported that
  identically to a genuinely broken partial response (`missing_leg:
  concept`), so the `judge_drop` event stream conflated "the judge is
  working as intended and just found nothing" with "the judge's output is
  broken" — no way to tell which one was actually happening, or how often.
  → **Fixed by T14 req 2**: an all-empty response now returns
  `JudgeDropReason::Declined` and logs a distinct `judge_declined` event
  kind; a genuine partial (some legs present, one missing) keeps logging
  `judge_drop` unchanged.
- **`MURSHID_NO_KEYCHAIN` silently forces degraded mode (severity: medium;
  noted, NOT fixed in T14).** A leftover `MURSHID_NO_KEYCHAIN` env var (set
  during an earlier debugging session and left in the shell) makes
  `murshid watch` silently run in "no API key configured" degraded mode with
  no indication that a bypass env var — not a genuinely missing key — was
  the cause. This is the same
  shape of gap the 2026-07-04 unreadable-keychain fix closed (distinguish
  *why* no key is available), but naming the specific active bypass var
  (`MURSHID_NO_KEYCHAIN`/`MURSHID_BYPASS_KEYCHAIN`) in the degraded-mode
  reason string needs threading through `credentials.rs` ->
  `judge::KeyStatus` -> `determine_judge_mode`, the same non-trivial shape
  of change as that fix — out of scope for T14 (which is about judge
  *outcome* observability, not credential-resolution observability).
  → Candidate follow-up task, not spawned yet.
- **Parse-gate silence is ambiguous (severity: medium; fixed on the T15 TUI
  branch, NOT yet on `main`'s pane).** During the TUI spike test, saves
  produced nothing and the founder read it as "the TUI is broken." Root
  cause: the file had a genuine *syntax* error, so the C12 quiescence gate
  (`sweep.rs`: skip at `!parses_without_errors`) correctly held — murshid
  waits for a clean tree-sitter parse before running the compiler check, by
  design (a type error like E0308 still parses and does fire; a missing
  semicolon does not). Correct behavior, but invisible: "deliberately
  waiting because your code doesn't parse" looked identical to "broken/idle."
  Confirmed by instrumentation (watcher fired + worker swept every save; the
  parse gate skipped). Design decision (founder + agent): KEEP the gate —
  removing it breaks site-identity/anchoring (needs a valid parse tree),
  adds mid-edit noise + LLM cost on half-written code, and drifts toward
  line-completion (a non-goal). → **Fixed on branch `yasir/mur-7-tui-spike`
  (MUR-7)**: `WatchSession::parse_waiting` + a dashboard status line ("waiting
  — N file(s) don't parse yet…"). → **Also fixed on `main`'s classic pane
  (2026-07-05 follow-up)**: transition-based notices `sweep::parse_hold_notice`
  / `parse_resume_notice` print "holding <file> — waiting for valid syntax"
  ONCE when a file enters the hold and "…parses again — resuming" when it
  leaves (verified live headless: 3 broken saves → 1 notice, then a resume on
  fix). This is the THIRD "silence is ambiguous" instance; general lesson:
  every deliberate hold/skip in the pipeline needs a legible reason, not just
  the judge-outcome ones T14 covered.

## Template for session entries

```
## YYYY-MM-DD — <project>, <duration>, <config: screen/judge models>
- cards shown / dismissed / applied / escalated:
- noise verdict (budget knob position, false-positive feel):
- judge quality (grounded? withheld solutions at the right rung?):
- friction / bugs:
- feature gaps felt:
```

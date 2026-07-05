# T14 — Judge observability

**Status:** Active — founder-approved 2026-07-05. **Traces to:** SPEC v0.5
(C6 two-stage judge), `specs/AMENDMENT-events-retention.md` (the ratified
bounding pattern this task mirrors for trace logs), `specs/dogfood-notes.md`
2026-07-05 findings.

## Why
During founder dogfooding, the judge (gemini-3.5-flash) dropped a card and
the user saw only silence. Diagnosing it required reading source AND running
SQL against the profile DB by hand. The root problem: judge outcomes are a
black box — there is no persisted raw model I/O, and a correct "not a
teaching moment" decline is logged identically to a broken partial response.
This task makes judge outcomes observable without building an eval harness
(explicitly deferred — see Out of scope).

## Requirements

### 1. Raw-model-I/O trace capture
Persist, per stage-1 (screen) and stage-2 (judge) dispatch inside the
automatic sweep pipeline (`watch::sweep`'s `dispatch_stage1`/`dispatch_stage2`
closures — the exact path that produced the founder's silent-drop bug):
timestamp (`ts_ms`), session_id, stage (`screen`|`judge`), provider+model,
site_hint, the raw request prompt, and the raw response text (or dispatch
error). Written as one JSON line per dispatch (`trace::TraceEntry`) under
`cli::setup::get_trace_logs_dir()`, day-bucketed (`trace-<day-index>.jsonl`),
directory created on demand.

**Bounding (mirrors `AMENDMENT-events-retention.md`'s pattern — time window +
prune-at-startup, not a novel scheme):**
- Age: entries older than `TRACE_LOG_RETENTION_DAYS = 14` are pruned.
  Shorter than the events table's 90/180-day windows because a trace entry
  carries a *full* raw prompt/response pair (much larger per-row than an
  events JSON payload) — bounding at DB-table timescales would let disk grow
  much faster under heavy sweep activity. Two weeks is enough to diagnose
  "why did last week's dogfood session drop a card" without becoming a
  permanent archive.
- Size backstop: `TRACE_LOG_MAX_TOTAL_BYTES = 200 MiB` total directory size,
  enforced (oldest day-file first) even inside the retention window, in case
  one day's traces balloon before the age prune would catch it.
- Prune runs once per watch-session startup (`trace::prune_trace_logs`),
  same call-site timing as `db::prune_expired_history` — unconditionally
  (independent of the enable knob below), so a previously-enabled trace dir
  keeps getting bounded even after a user disables live tracing.

**Config knob:** `[trace] enabled = true|false`, **default `true`**. Leaning
ON during the dogfood phase: this is local-first (trace files never leave
the machine) and diagnostic-only (no behavior change, purely additive
observability) — the exact opposite of the silence that caused this task.
Disable-able for anyone who doesn't want raw prompts/responses (which may
contain source snippets) written to disk at all.

### 2. Split the drop signal
Stage-2's prompt (`packs/*/prompts/stage2.md`) instructs the model: "If you
cannot ground the finding, respond with `{}` and no other text." That is a
**deliberate, correct decline** — but before this task it logged identically
(`judge_drop`, `missing_leg: concept`) to a broken partial response missing
the same field.

`validate_stage2_output` now checks first whether **every** leg is
empty/absent (`concept`, `grounding_quote`, `why`, `rule`, `worked_diff`,
`category`, `likely_bug`, `failure_scenario` — the same emptiness test
`non_empty` already applies per-field) and returns the new
`JudgeDropReason::Declined` in that case, before falling through to the
existing per-field `MissingLeg` checks. A **partial** object (at least one
leg present, `concept` still missing or any other leg missing) keeps
returning `MissingLeg` unchanged — that is a genuine contract failure, not a
decline. `parse_error` (malformed JSON) is untouched — it was already
distinct.

The watch layer (`watch::sweep::judge_and_collect_finding`) logs a
**distinct event kind**: `judge_declined` when `reason.is_declined()`,
`judge_drop` otherwise (same payload shape either way —
`judge::judge_drop_payload`, `{reason, detail, site_hint}` — the kind is the
discriminator, `reason` a redundant confirmation).

### 3. Documented outcome-rate path
No new subcommand (would be gold-plating for a single query). Read judge
outcome rates directly from `events`:

```sql
SELECT
  CASE
    WHEN kind = 'card_shown'                                          THEN 'shown'
    WHEN kind = 'judge_declined'                                      THEN 'declined'
    WHEN kind = 'judge_drop'
     AND json_extract(payload_json, '$.reason') = 'parse_error'       THEN 'parse_error'
    WHEN kind = 'judge_drop'                                          THEN 'contract_failure'
  END AS outcome,
  COUNT(*) AS n
FROM events
WHERE kind IN ('card_shown', 'judge_drop', 'judge_declined')
GROUP BY outcome
ORDER BY n DESC;
```

Run against `~/Library/Application Support/murshid/profile.db` (macOS) via
`sqlite3`. Separates "chose not to teach" (declined) from "failed the
contract" (contract_failure) from "unparseable" (parse_error) from "actually
shown" (shown).

### 4. Dogfood-notes findings
Append a dated (2026-07-05) findings section to `specs/dogfood-notes.md`
covering: the `MURSHID_NO_KEYCHAIN` silent-bypass gap (severity + deferred,
not fixed here), the judge-outcomes-black-box finding (this task), and the
declined-vs-failed overloading (req 2, fixed here).

## Out of scope
- **Evals / an eval harness** — deferred pending the trace data this task
  produces; deliberately cut in the July-3 realignment (founder-ratified
  scope decision).
- **Stage-2 prompt hardening** (few-shot, etc.) — deferred until trace data
  actually shows the judge failing the contract vs. correctly declining.
- **Anything TUI.**
- Tracing the `murshid review` digest path (`lib::run_review`) and the
  in-pane thread-reply/comment-ask dispatch paths (`watch::keys`,
  `watch::sweep::run_comment_asks`) — req 1 is scoped to the automatic sweep
  pipeline's stage-1/stage-2 dispatch, the concrete flow that produced the
  founder's silent-drop bug. Extending trace capture to those paths is a
  reasonable follow-up but not required by this task's motivating incident.
- Fixing the `MURSHID_NO_KEYCHAIN`-naming gap in the degraded-mode reason
  string (dogfood-notes finding (a)) — noted, not built, per the task
  brief's explicit "do NOT necessarily fix it in T14 unless trivial" (it
  isn't: it needs threading the bypass-env-var name through
  `credentials.rs` -> `judge::KeyStatus` -> `determine_judge_mode`, the same
  shape of change as the 2026-07-04 unreadable-key fix).

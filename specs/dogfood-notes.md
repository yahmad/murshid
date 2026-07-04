# Dogfood notes

Running log of friction/findings from real usage. Each entry: date, what
happened, severity, and whether it spawned a task. This file is the input
that decides what gets built next (INDEX.md: "next: dogfood").

## 2026-07-03 — pre-dogfood smoke test (scratch Rust project, agent-driven)

- **Pipeline works end-to-end.** Save with `E0308` → watcher → `cargo check`
  intercept → screen dispatch. Provider failure (invalid key) degraded
  gracefully, no crash. ✅
- **Keyring pollution (fixed, needs a real fix).** The real `murshid`
  keychain service contained `mock-gemini-*` / mock claude keys left by an
  earlier test run — tests can write to the production service name when
  `MURSHID_TESTING` is unset. Mock entries deleted by hand 2026-07-03.
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
  paid-API-only path; founder is on Max + Google AI Pro. Dogfood config is
  all-Gemini. Gemini free tier (mid-2026): Flash-class models only,
  2.5-pro removed from free tier ~April 2026. AI Pro includes $10/mo Cloud
  credit (opt-in at developers.google.com/program/my-benefits) → Tier 1
  limits + no training on prompts.
  Suggested slots: screen = `gemini-3.1-flash-lite`, judge =
  `gemini-3.5-flash` (separate per-model quota buckets).
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

## Template for session entries

```
## YYYY-MM-DD — <project>, <duration>, <config: screen/judge models>
- cards shown / dismissed / applied / escalated:
- noise verdict (budget knob position, false-positive feel):
- judge quality (grounded? withheld solutions at the right rung?):
- friction / bugs:
- feature gaps felt:
```

# T8 — OpenAI-compatible local provider (BYOK seam widening)

**Status:** Draft (pending founder go) · **Traces to:** SPEC v0.5 C6 (two-slot
model seam); founder decision "BYOK provider-agnostic incl. Ollama first-class;
no hosted inference" (DECISIONS.md).

## Why

The current local-model arm speaks Ollama's native API only, hardcoded to
`http://localhost:11434/api/generate` (provider.rs). That serves exactly one
runtime and zero configurability. OpenAI-compatible `/v1/chat/completions` is
the lingua franca of local inference in 2026: Ollama itself serves it (same
port, since 2024), as do LM Studio (`localhost:1234/v1`), llama.cpp server,
vLLM, and mlx-lm. One arm parameterized by `base_url` covers all of them and
retires a bespoke code path.

**Seamlessness invariant (founder requirement, 2026-07-03):** this task must
be invisible to users. Cloud-only users see zero change. Users who opt into a
local model already had to install a runtime and download a model — the
murshid side must be at most two config lines (`provider`, `model`) for
standard installs, with no URLs or ports.

## Requirements

1. `ModelSlotConfig` gains an optional `base_url` (TOML:
   `[models.screen] base_url = "…"`). Absent for standard installs.
2. Accepted provider values for the shared OpenAI-compat path, with alias
   defaults so standard installs need no `base_url`:
   - `"ollama"` → `http://localhost:11434/v1` (existing configs keep working
     verbatim; Ollama has served `/v1` alongside its native API since 2024)
   - `"lmstudio"` → `http://localhost:1234/v1`
   - `"openai"` → generic; `base_url` required, config error if absent
   An explicit `base_url` overrides the alias default (nonstandard port,
   LAN box).
3. Request: `POST {base_url}/chat/completions` with
   `{"model": …, "messages": [{"role": "user", "content": prompt}],
   "stream": false}`. Same curl/debounce/abort mechanics as the other arms
   (`run_query_with_child_tracking`); no auth header when no key resolves.
4. Response parse: `choices[0].message.content`. Provider errors keep
   surfacing via the existing top-level `error.message` check.
5. Keyless slots: `judge::slot_ready` currently special-cases
   `provider == "ollama"`; generalize to "this provider requires no key"
   covering all three values above. A keyless local slot must never trigger
   the "no API key" degraded mode; an unreachable local server degrades
   gracefully through the existing provider-error path (no crash, observe-only
   note).
6. Cleanup (vestigial code from the archived v6.0 effort — no callers):
   delete `build_prompt`, `format_chatml`, `format_llama3`, `format_default`,
   the `DialogueTurn.role`-based template plumbing they own, and the
   `provider.local_template_format` config key. Chat templating is the
   server's job on `/v1`.

## Out of scope

- Hosted inference of any kind (founder non-goal).
- Auto-detection/probing of running local servers (magic model selection is
  a surprise, not seamlessness; the user picks the model regardless).
- `response_format: json_schema` grammar enforcement for the screen contract
  (real quality candidate — LM Studio and Ollama both support it — but it
  touches the judge output-contract seam; separate task if wanted).
- API keys for remote OpenAI-compat endpoints.

## Acceptance

- `cargo test` green; unit tests for: config merge of `base_url`, alias
  resolution (incl. explicit-`base_url` override and `openai`-without-URL
  error), request builder output, response parse, generalized `slot_ready`.
- Grep-level check: no remaining reference to `/api/generate`,
  `local_template_format`, or the deleted formatters.
- Dogfood sketch: LM Studio serving a small model, `[models.screen]
  provider = "lmstudio"` + `model` set → a save with a compile error produces
  a screen dispatch against `localhost:1234/v1` (card or graceful provider
  error); same config minus LM Studio running → degraded note, no crash.

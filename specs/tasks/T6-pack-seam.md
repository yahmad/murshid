# T6 — Language-pack seam extraction

**Status:** Planned · **Traces to:** SPEC v0.5 D23/D24, I27–I30, C2/C9/C10.
Extract T1–T5's Rust-specifics behind the seam: normalized record
(rule_id/tool_level/message/range/fix/doc_ref/finding-fingerprint/opaque
data — I28 as amended by C9); pack = one adapter (tool invocation + output
mapping) + data payloads 1–6 (adapter config, taxonomy, canon, prompt
fragments, surface.toml, grammar reference); engine loses all
language-shaped logic; lint path per C6.
Also in scope (T1-review flag): replace the CARGO_MANIFEST_DIR pack-path
resolution — packs must resolve for an installed (brew) binary via
exe-relative + XDG/config paths; friends-cohort install depends on it.
Acceptance sketch: grep-level check — no "rust"/"cargo"/"clippy" strings in
engine modules outside the pack loader; all T1–T5 tests still green;
pack loads from a simulated installed layout.

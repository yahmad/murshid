# T7 — Go pack (architecture-honesty test)

**Status:** Planned · **Traces to:** SPEC v0.5 D2, I29.
Author packs/go/: adapter (go build/vet [+ golangci-lint if configured] →
normalized records), taxonomy (~30 coarse concepts w/ prerequisites +
category), canon (Effective Go / Go Proverbs refs), prompts, surface.toml
(`//`, `go build`), tree-sitter-go grammar ref.
**Pass criterion (I29, mechanical — tightened 2026-07-03 after T6 review):**
the ONLY permitted edits are:
1. a new `packs/go/` data directory (all six payloads);
2. one new diagnostics-adapter module contributing exactly one
   `impl pack::DiagnosticsAdapter` (I27), declared as a **child `mod` of
   `pack.rs`** (never `main.rs`) so the entire code-registration surface
   stays inside the seam file;
3. exactly one new match arm each in `pack::resolve_ts_language` and
   `pack::diagnostics_adapter`;
4. `tree-sitter-go` in Cargo.toml (already covered by C10's "per-pack
   grammar crates"; Go tooling is invoked by subprocess, no other crate).
Any edit to any other `src/*.rs` — site, quiescence, pipeline, review,
watcher, judge, main, comment, db — means the seam failed: stop, log in
the SPEC decision log, reopen D23.
Acceptance sketch: dogfood session in a Go repo produces a grounded,
concept-named card; T1–T6 test suite untouched and green.

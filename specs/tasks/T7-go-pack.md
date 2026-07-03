# T7 — Go pack (architecture-honesty test)

**Status:** Planned · **Traces to:** SPEC v0.5 D2, I29.
Author packs/go/: adapter (go build/vet [+ golangci-lint if configured] →
normalized records), taxonomy (~30 coarse concepts w/ prerequisites +
category), canon (Effective Go / Go Proverbs refs), prompts, surface.toml
(`//`, `go build`), tree-sitter-go grammar ref.
**Pass criterion (I29, mechanical): zero engine edits.** Any engine change
required ⇒ the seam failed — stop, log in SPEC decision log, reopen D23.
Acceptance sketch: dogfood session in a Go repo produces a grounded,
concept-named card; T1–T6 test suite untouched and green.

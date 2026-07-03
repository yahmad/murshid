# T10 — Pack auto-detection (make the Go pack reachable)

**Status:** Approved (2026-07-03, queued after T9) · **Traces to:** SPEC v0.5
D2/I29 (pack seam), T7 acceptance debt; founder ruling 2026-07-03: marker
auto-detection, not just a config key.

## Why
`pack::default_pack_dir()` hardcodes `"rust"` and nothing selects any other
pack — the Go pack shipped in T7 is unreachable by users. T7's acceptance
sketch ("dogfood in a Go repo produces a card") is unmet in product. Fix:
detect the language from project markers, with an explicit config override.

## Requirements
1. New resolution, single entry point (e.g. `pack::resolve_pack_id(project_root,
   &config) -> String`), used by every command that loads a pack (`watch`,
   `review`, and any other `default_pack_dir` caller). No other call sites may
   keep a hardcoded pack id.
2. Precedence: (a) explicit `[pack] language = "…"` config key (new; merged
   like other config keys, project > user > system, lock-policy respected);
   (b) marker detection at project root: `go.mod` → `go`, `Cargo.toml` →
   `rust`; (c) fallback `rust` (current behavior, silent).
3. Ambiguity (both markers present, no config key): pick `rust` and print one
   startup line naming the choice and the `[pack] language` override — never
   a prompt, never an error.
4. A detected/configured pack id that has no `packs/<id>/` directory falls
   back to `rust` with the existing payload-fallback notice mechanism (no
   crash, no new notice format).
5. Watched file extensions, comment token, quiescence gating etc. must all
   flow from the resolved pack's `surface.toml` (they already come from the
   loaded pack — requirement is: verify no residual rust-pack assumption in
   the `watch` startup path once resolution is dynamic).
6. Unit tests: detection matrix (go.mod only / Cargo.toml only / both /
   neither / config override wins over markers / locked config), missing-pack
   fallback. Marker detection must be a pure function over a directory
   listing for testability.

## Out of scope
- Watching polyglot repos with two live packs at once (one pack per session).
- New packs; changes inside `packs/`; per-subdirectory detection.

## Acceptance
- `cargo test` green. Grep: no `"rust"` literal in pack-id resolution outside
  the single fallback constant + match-arm registries.
- Dogfood sketch: `murshid watch` in a repo with `go.mod` runs `go build`/
  `go vet` and a Go diagnostic produces a concept-named card with zero config;
  a Rust repo behaves exactly as today.

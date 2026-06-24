# Project: murshid (SDD build repo, stamped from sdd-engine-template)

This repository is a spec-driven Rust project. The following are facts about how
it works.

## Source of truth

- Specifications in `specs/` are the source of truth. Code is derived from them.
- The active spec for current work is listed in `specs/INDEX.md`.
- Implementation must not add scope that the active spec does not define.

## Conventions

- This repo uses `cargo test` for verification.
- Standard library only unless a spec explicitly allows a dependency.
- Functions specified as pure must have no global state and no side effects.

## Workflow

- Work proceeds in the loop: read the active spec, plan, implement, verify.
- A `cargo test` run fires automatically after each file write; failing tests are
  reported back automatically. Fix the implementation until tests pass.
- A task is done only when `cargo test` passes and no `TODO` markers remain
  in the files the active spec covers.

## Pattern Matching and Scanning Order

- When implementing static scanners, regex parsers, or simple substring matching lists (e.g. searching for cruft print macros), **always order checks from most specific to least specific** (for example, check `eprintln!` before `println!`). This ensures that compound tokens or substrings are not incorrectly matched by their simpler components.


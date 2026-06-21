# Project: <name> (SDD build repo, stamped from sdd-engine-template)

This repository is a spec-driven Go project. The following are facts about how
it works.

## Source of truth

- Specifications in `specs/` are the source of truth. Code is derived from them.
- The active spec for current work is listed in `specs/INDEX.md`.
- Implementation must not add scope that the active spec does not define.

## Conventions

- This repo uses `go test ./...` for verification.
- Standard library only unless a spec explicitly allows a dependency.
- Functions specified as pure must have no global state and no side effects.

## Workflow

- Work proceeds in the loop: read the active spec, plan, implement, verify.
- A `go test` run fires automatically after each file write; failing tests are
  reported back automatically. Fix the implementation until tests pass.
- A task is done only when `go test ./...` passes and no `TODO` markers remain
  in the files the active spec covers.

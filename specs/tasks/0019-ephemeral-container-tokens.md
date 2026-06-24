# Task 0019 — Ephemeral Container Tokens

**Parent spec:** specs/0000-design.md
**Assigned to:** Gemini (Antigravity)

## Objective
Implement host command murshid container token signing ephemeral container leases (tier:expiration:signature) valid up to 7 days. Container CLI reads token files and verifies offline signatures on startup.

## Scope
- In: src/cli/container.rs, src/licensing.rs
- Out: other src/*.rs files

## Acceptance criteria
- cargo test passes.
- Host container token generation writes signed lease parameters.
- Container CLI validates short-lived lease signatures offline.

## Constraints
- Edit only: src/cli/container.rs, src/licensing.rs
- Standard library only unless the parent spec allows otherwise.
- Do not add scope beyond this task.

## Reporting
Do not rely on printing a summary. Make the edits in the repo and ensure tests
pass. The orchestrator reads the `git diff`, not your stdout.

# Task 0018 — Ed25519 Licensing & Offline Grace

**Parent spec:** specs/0000-design.md
**Assigned to:** Gemini (Antigravity)

## Objective
Implement subscription activations checks using Ed25519 signatures validation (ed25519-dalek). Enforce 14-day network offline grace periods checking system time differences against cached check-in times. Perform dark-site pre-signed lease files verification matching tenant_id.

## Scope
- In: src/licensing.rs
- Out: other src/*.rs files

## Acceptance criteria
- cargo test passes.
- Signed lease tokens verify properly via Ed25519 public keys.
- Clock manipulates rolled back times fail-fast checks.
- Dark-site tenant validation verifies matching client profiles.

## Constraints
- Edit only: src/licensing.rs
- Standard library only unless the parent spec allows otherwise.
- Do not add scope beyond this task.

## Reporting
Do not rely on printing a summary. Make the edits in the repo and ensure tests
pass. The orchestrator reads the `git diff`, not your stdout.

# Task 0008 — Compile Guards and Errors Isolation

**Parent spec:** specs/0000-design.md
**Assigned to:** Gemini (Antigravity)

## Objective
Enforce a strict 3000ms compile lock timeout. Implement background target cache pruning strictly for <project-root>/target/murshid/ when size exceeds 5GB. Isolate cargo infrastructure/network errors, bypassing Socratic prompt generation and mastery updates.

## Scope
- In: src/compiler.rs, src/watcher_coordinator.rs
- Out: other src/*.rs files

## Acceptance criteria
- cargo test passes.
- Compile lock timeout triggers fail-fast check abort.
- Pruning clean execution target folders works asynchronously.
- Infrastructure and registry offline errors correctly isolated.

## Constraints
- Edit only: src/compiler.rs, src/watcher_coordinator.rs
- Standard library only unless the parent spec allows otherwise.
- Do not add scope beyond this task.

## Reporting
Do not rely on printing a summary. Make the edits in the repo and ensure tests
pass. The orchestrator reads the `git diff`, not your stdout.

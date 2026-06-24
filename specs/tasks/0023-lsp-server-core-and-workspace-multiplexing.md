# Task 0023 — LSP Server Core & Workspace Multiplexing

**Parent spec:** specs/0000-design.md
**Assigned to:** Gemini (Antigravity)

## Objective
Initialize daemon socket clearing stale locks. Multiplex connections and queues across multiple workspace paths, isolating workspace contexts, locks, watch loops, and SQLite logs records.

## Scope
- In: src/lsp/server.rs
- Out: other src/*.rs files

## Acceptance criteria
- cargo test passes.
- Stale Unix socket connection testing and unlink works.
- Concurrent workspace contexts process messages in isolated channels.

## Constraints
- Edit only: src/lsp/server.rs
- Standard library only unless the parent spec allows otherwise.
- Do not add scope beyond this task.

## Reporting
Do not rely on printing a summary. Make the edits in the repo and ensure tests
pass. The orchestrator reads the `git diff`, not your stdout.

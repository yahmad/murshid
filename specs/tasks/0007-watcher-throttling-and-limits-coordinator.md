# Task 0007 — Watcher Throttling & Limits Coordinator

**Parent spec:** specs/0000-design.md
**Assigned to:** Gemini (Antigravity)

## Objective
Implement a Watcher Resource Throttling Coordinator. Limit active watch threads (max 4) and file descriptors (max 1000) across all workspaces, falling back to background polling on limits exhaustion. Skip watch event parsing if the changed file size exceeds 50KB or lines exceed 1500 lines.

## Scope
- In: src/watcher_coordinator.rs
- Out: other src/*.rs files

## Acceptance criteria
- cargo test passes.
- Resource coordinator throttles watch threads and descriptors properly.
- Changed files > 50KB/1500 lines skipped with a quiet trace.

## Constraints
- Edit only: src/watcher_coordinator.rs, specs/tasks/0007-watcher-throttling-and-limits-coordinator.md, src/main.rs, src/watcher.rs
- Standard library only unless the parent spec allows otherwise.
- Do not add scope beyond this task.

## Reporting
Do not rely on printing a summary. Make the edits in the repo and ensure tests
pass. The orchestrator reads the `git diff`, not your stdout.

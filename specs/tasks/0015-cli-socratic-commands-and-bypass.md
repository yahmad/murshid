# Task 0015 — CLI Socratic Commands & Bypass

**Parent spec:** specs/0000-design.md
**Assigned to:** Gemini (Antigravity)

## Objective
Implement first-error banner trigger on watcher startup. Implement CLI bypass command murshid bypass --duration <time> --reason [--force] logging events and locking counts (max 3/7days). Implement CLI share exporting markdown logs to local files and system clipboard.

## Scope
- In: src/cli/bypass.rs, src/cli/share.rs
- Out: other src/*.rs files

## Acceptance criteria
- cargo test passes.
- Bypass command overrides Socratic mode and validates weekly limits.
- Forced bypass logs audit trace to SQLite bypass table.
- Share command writes struggle markdown files and updates clipboard.

## Constraints
- Edit only: src/cli/bypass.rs, src/cli/share.rs, specs/tasks/0015-cli-socratic-commands-and-bypass.md, src/main.rs
- Standard library only unless the parent spec allows otherwise.
- Do not add scope beyond this task.

## Reporting
Do not rely on printing a summary. Make the edits in the repo and ensure tests
pass. The orchestrator reads the `git diff`, not your stdout.

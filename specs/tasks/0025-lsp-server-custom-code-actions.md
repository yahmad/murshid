# Task 0025 — LSP Server Custom Code Actions

**Parent spec:** specs/0000-design.md
**Assigned to:** Gemini (Antigravity)

## Objective
Register LSP custom Code Actions kinds quickfix.murshid or refactor.murshid to spawn Socratic chat panel commands. Provide hover details payload fallback for editors missing command pane support.

## Scope
- In: src/lsp/code_actions.rs
- Out: other src/*.rs files

## Acceptance criteria
- cargo test passes.
- Custom Code Actions registered correctly.
- Code Action triggers execute workspace commands to open chat panels.
- Fallback string formats markdown descriptions cleanly.

## Constraints
- Edit only: src/lsp/code_actions.rs
- Standard library only unless the parent spec allows otherwise.
- Do not add scope beyond this task.

## Reporting
Do not rely on printing a summary. Make the edits in the repo and ensure tests
pass. The orchestrator reads the `git diff`, not your stdout.

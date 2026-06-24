# Task 0024 — LSP Server Diagnostics Publishing

**Parent spec:** specs/0000-design.md
**Assigned to:** Gemini (Antigravity)

## Objective
Publish annotations under source murshid with DiagnosticSeverity Hint or Info as Markdown diagnostics. Intercept textDocument/didChange events, immediately clearing Socratic annotations from edit lines before compile check loops complete.

## Scope
- In: src/lsp/diagnostics.rs
- Out: other src/*.rs files

## Acceptance criteria
- cargo test passes.
- Diagnostics published under murshid source at Hint/Info severity levels.
- DidChange events clear active annotation ranges under 50ms.

## Constraints
- Edit only: src/lsp/diagnostics.rs
- Standard library only unless the parent spec allows otherwise.
- Do not add scope beyond this task.

## Reporting
Do not rely on printing a summary. Make the edits in the repo and ensure tests
pass. The orchestrator reads the `git diff`, not your stdout.

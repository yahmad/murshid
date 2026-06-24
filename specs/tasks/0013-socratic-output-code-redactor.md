# Task 0013 — Socratic Output Code Redactor

**Parent spec:** specs/0000-design.md
**Assigned to:** Gemini (Antigravity)

## Objective
Intercept provider completions and clippy quick-fixes. Strip code blocks matching user identifiers/structures, replacing blocks with Socratic warnings. For local model config, parse and redact completion blocks if they match >= 3 user active variables.

## Scope
- In: src/redactor.rs
- Out: other src/*.rs files

## Acceptance criteria
- cargo test passes.
- Markdown code blocks matched with user types redacted.
- Clippy direct fixes stripped and translated to conceptual hints.
- Local model output leaks check redacts blocks with >=3 user variables.

## Constraints
- Edit only: src/redactor.rs, specs/tasks/0013-socratic-output-code-redactor.md, src/main.rs
- Standard library only unless the parent spec allows otherwise.
- Do not add scope beyond this task.

## Reporting
Do not rely on printing a summary. Make the edits in the repo and ensure tests
pass. The orchestrator reads the `git diff`, not your stdout.

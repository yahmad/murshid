# Task 0009 — Context Scoping & XML Bounds

**Parent spec:** specs/0000-design.md
**Assigned to:** Gemini (Antigravity)

## Objective
Restrict LLM payload context strictly to error line span +/- 25 lines and user type signatures directly referenced in error context. Wrap code context inside XML tags <developer_code_context>, sanitizing nested tags in-memory to prevent injection.

## Scope
- In: src/context.rs
- Out: other src/*.rs files

## Acceptance criteria
- cargo test passes.
- Context bounds generated match +/-25 lines error span.
- User type signatures correctly parsed and isolated.
- Nested XML tags redacted in-memory.

## Constraints
- Edit only: src/context.rs, specs/tasks/0009-context-scoping-and-xml-bounds.md, src/main.rs
- Standard library only unless the parent spec allows otherwise.
- Do not add scope beyond this task.

## Reporting
Do not rely on printing a summary. Make the edits in the repo and ensure tests
pass. The orchestrator reads the `git diff`, not your stdout.

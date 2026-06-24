# Task 0027 — Commit Cleaner pre-commit

**Parent spec:** specs/0000-design.md
**Assigned to:** Gemini (Antigravity)

## Objective
Implement murshid clean pre-commit CLI hook. Scan staged cached diff for cruft print macros. Perform GUI popovers overlay connection timeouts fallback (action warn/fail). Limit executions to staged sizes <= 15 files and <= 1000 lines. Write bypass audit logging to database.

## Scope
- In: src/cli/clean.rs
- Out: other src/*.rs files

## Acceptance criteria
- cargo test passes.
- Pre-commit hook parses staged modifications for cruft rules.
- Non-interactive GUI client socket timeout fails back to action overrides.
- Audit bypass logs written to DB omit code segments.

## Constraints
- Edit only: src/cli/clean.rs
- Standard library only unless the parent spec allows otherwise.
- Do not add scope beyond this task.

## Reporting
Do not rely on printing a summary. Make the edits in the repo and ensure tests
pass. The orchestrator reads the `git diff`, not your stdout.

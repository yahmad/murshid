# Task 0010 — Secrets Scanner & Path Sanitizer

**Parent spec:** specs/0000-design.md
**Assigned to:** Gemini (Antigravity)

## Objective
Implement linear non-backtracking DFA secrets scanner (AWS, Claude, Google API keys, database URIs), limiting evaluation to 10k characters per span. Implement path prefix sanitizer replacing user home folders and usernames with [USER_HOME] placeholder in diagnostics.

## Scope
- In: src/sanitizer.rs
- Out: other src/*.rs files

## Acceptance criteria
- cargo test passes.
- Secrets scanner correctly identifies and redacts API keys and DB URIs under 10k limits.
- Path sanitizer scrubs local user directory prefixes from outgoing messages.

## Constraints
- Edit only: src/sanitizer.rs, specs/tasks/0010-secrets-scanner-and-path-sanitizer.md, src/main.rs
- Standard library only unless the parent spec allows otherwise.
- Do not add scope beyond this task.

## Reporting
Do not rely on printing a summary. Make the edits in the repo and ensure tests
pass. The orchestrator reads the `git diff`, not your stdout.

# Task 0001 — Configuration Merge & Lock

**Parent spec:** specs/0000-design.md
**Assigned to:** Gemini (Antigravity)

## Objective
Implement TOML configuration loading and merging precedence across system-wide, user-level, and project-level scopes. Implement system config root ownership checks (owned strictly by root/administrator and permissions 0644/0600) and enforce block overrides for keys marked with lock_policy = true.

## Scope
- In: src/config.rs
- Out: other src/*.rs files (except basic integrations in main.rs)

## Acceptance criteria
- cargo test passes.
- Config merger combines default settings, system, user, and project TOML files correctly.
- If lock_policy = true in system-wide config, any project/user overrides for that block must be ignored.
- System config file ownership check blocks parsing and raises warning if not owned by root/admin or permissions not 0644/0600.

## Constraints
- Edit only: src/config.rs, src/main.rs
- Standard library only unless the parent spec allows otherwise.
- Do not add scope beyond this task.

## Reporting
Do not rely on printing a summary. Make the edits in the repo and ensure tests
pass. The orchestrator reads the `git diff`, not your stdout.

# Task 0003 — SQLite Database Corruption Recovery & Backups

**Parent spec:** specs/0000-design.md
**Assigned to:** Gemini (Antigravity)

## Objective
Implement corruption detection. Upon catching SQLITE_CORRUPT or integrity checks failure, rename the corrupted database to profile.db.corrupt.<timestamp> and initialize a clean database. Retrieve the user's category-level mastery thresholds from the platform native defaults backup store (NSUserDefaults on macOS, Registry on Windows, Linux .state_backup file) and restore them to the newly initialized database.

## Scope
- In: src/db.rs, src/backup.rs
- Out: other src/*.rs files

## Acceptance criteria
- cargo test passes.
- Detect corruption, archive corrupted file, and trigger self-repair.
- Successfully restore category-level progress thresholds from the platform secure backup store upon DB initialization.
- Keep the platform backup store synchronously in-sync during DB writes.

## Constraints
- Edit only: src/db.rs, src/backup.rs, src/main.rs
- Standard library only unless the parent spec allows otherwise.
- Do not add scope beyond this task.

## Reporting
Do not rely on printing a summary. Make the edits in the repo and ensure tests
pass. The orchestrator reads the `git diff`, not your stdout.

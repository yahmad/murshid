# Task 0002 — SQLite Database Schema & Migrations

**Parent spec:** specs/0000-design.md
**Assigned to:** Gemini (Antigravity)

## Objective
Create the local SQLite progress memory database schema with WAL mode enabled. Set up prepopulated concepts. Implement transaction-safe database migrations utilizing PRAGMA user_version, copying the database to a backup file before starting the migration and rolling back/restoring if migration fails. Enforce busy-timeout retry loops with exponential backoff.

## Scope
- In: src/db.rs
- Out: other src/*.rs files

## Acceptance criteria
- cargo test passes.
- Database establishes connection with journal_mode = WAL and synchronous = NORMAL.
- Core concepts prepopulate automatically on fresh DB creation.
- Migrations apply sequentially. Backup file created and deleted upon success; database fully restored on error.

## Constraints
- Edit only: src/db.rs, Cargo.toml, src/main.rs
- Standard library only unless the parent spec allows otherwise.
- Do not add scope beyond this task.

## Reporting
Do not rely on printing a summary. Make the edits in the repo and ensure tests
pass. The orchestrator reads the `git diff`, not your stdout.

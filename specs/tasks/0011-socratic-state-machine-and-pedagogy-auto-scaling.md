# Task 0011 — Socratic State Machine & Pedagogy Auto-Scaling

**Parent spec:** specs/0000-design.md
**Assigned to:** Gemini (Antigravity)

## Objective
Enforce local socratic dialogue states in SQLite. Recalculate concept mastery scores using EMA formulas. Track consecutive failure threshold (T_t range 3-7) per file-error-code pair, resetting on success or TTL cache timeout. Compute failure counts strictly if the active file's normalized SHA-256 hash has changed.

## Scope
- In: src/pedagogy.rs
- Out: other src/*.rs files

## Acceptance criteria
- cargo test passes.
- Dialogue states persist in SQLite dialogues table.
- EMA concept mastery math conforms to formulas.
- Normalized SHA-256 hash comparison guards consecutive failure increments.

## Constraints
- Edit only: src/pedagogy.rs, specs/tasks/0011-socratic-state-machine-and-pedagogy-auto-scaling.md, src/main.rs
- Standard library only unless the parent spec allows otherwise.
- Do not add scope beyond this task.

## Reporting
Do not rely on printing a summary. Make the edits in the repo and ensure tests
pass. The orchestrator reads the `git diff`, not your stdout.

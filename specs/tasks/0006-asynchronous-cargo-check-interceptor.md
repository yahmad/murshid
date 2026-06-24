# Task 0006 — Asynchronous Cargo Check Interceptor

**Parent spec:** specs/0000-design.md
**Assigned to:** Gemini (Antigravity)

## Objective
Execute cargo check --message-format=json asynchronously in background. Terminate previous compilation runs via SIGTERM, followed by SIGKILL after 500ms. Set environment CARGO_TARGET_DIR to <project-root>/target/murshid/ to prevent build conflicts. Parse JSON stream diagnostics, limiting Socratic parsing to 3 errors prioritising active file.

## Scope
- In: src/compiler.rs
- Out: other src/*.rs files

## Acceptance criteria
- cargo test passes.
- Cargo check executes in background targets folder.
- Subprocess terminated properly upon new triggers before queue serialization.
- Correctly parse JSON diagnostic output for errors and rate limits.

## Constraints
- Edit only: src/compiler.rs, specs/tasks/0006-asynchronous-cargo-check-interceptor.md, src/main.rs
- Standard library only unless the parent spec allows otherwise.
- Do not add scope beyond this task.

## Reporting
Do not rely on printing a summary. Make the edits in the repo and ensure tests
pass. The orchestrator reads the `git diff`, not your stdout.

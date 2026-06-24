# Task 0005 — Watcher Core & Polling Fallback

**Parent spec:** specs/0000-design.md
**Assigned to:** Gemini (Antigravity)

## Objective
Implement filesystem watcher using notify crate to watch directory changes recursively. Canonicalize all monitored paths and check that they resolve inside the workspace root (unless external links explicitly allowed). Exclude target/, .git/, and .murshid_experiments/ paths. Implement automatic background polling fallback (2500ms) upon EMFILE/ENOSPC errors.

## Scope
- In: src/watcher.rs
- Out: other src/*.rs files

## Acceptance criteria
- cargo test passes.
- Watch loop detects rs file writes recursively, ignoring exclusions.
- Canonical paths evaluated properly against symlink directories.
- Automatic polling fallback functions correctly on EMFILE limit triggers.

## Constraints
- Edit only: src/watcher.rs, Cargo.toml, specs/tasks/0005-watcher-core-and-polling-fallback.md, src/main.rs
- Standard library only unless the parent spec allows otherwise (notify crate is explicitly allowed by the parent spec).
- Do not add scope beyond this task.

## Reporting
Do not rely on printing a summary. Make the edits in the repo and ensure tests
pass. The orchestrator reads the `git diff`, not your stdout.

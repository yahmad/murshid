# Task 0016 — Offline Reference Index

**Parent spec:** specs/0000-design.md
**Assigned to:** Gemini (Antigravity)

## Objective
Implement static local reference caching in the platform config directory. Provide error code mapping lookup index without raw file scanning. Fallback to Jaccard index similarity searches strictly on pre-indexed summary tokens.

## Scope
- In: src/offline_docs.rs
- Out: other src/*.rs files

## Acceptance criteria
- cargo test passes.
- Mapping compiler errors to references indexes works offline.
- Pre-compiled static index lookups execute under 1ms.
- Jaccard fallback functions correctly on general queries.

## Constraints
- Edit only: src/offline_docs.rs
- Standard library only unless the parent spec allows otherwise.
- Do not add scope beyond this task.

## Reporting
Do not rely on printing a summary. Make the edits in the repo and ensure tests
pass. The orchestrator reads the `git diff`, not your stdout.

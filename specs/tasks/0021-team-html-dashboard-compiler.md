# Task 0021 — Team HTML Dashboard Compiler

**Parent spec:** specs/0000-design.md
**Assigned to:** Gemini (Antigravity)

## Objective
Implement team-dashboard CLI command compiling digests into offline HTML reports. Embed all CSS styles, javascripts, SVG graphics, and summary matrices inline without remote network CDN calls.

## Scope
- In: src/team/dashboard.rs
- Out: other src/*.rs files

## Acceptance criteria
- cargo test passes.
- Dashboard script outputs a completely self-contained offline HTML page.
- Validate zero telemetry checks and no CDN lookups.
- Responsive tabs and sorting switch correctly via inline scripts.

## Constraints
- Edit only: src/team/dashboard.rs
- Standard library only unless the parent spec allows otherwise.
- Do not add scope beyond this task.

## Reporting
Do not rely on printing a summary. Make the edits in the repo and ensure tests
pass. The orchestrator reads the `git diff`, not your stdout.

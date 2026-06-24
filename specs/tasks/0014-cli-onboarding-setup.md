# Task 0014 — CLI Onboarding Setup

**Parent spec:** specs/0000-design.md
**Assigned to:** Gemini (Antigravity)

## Objective
Implement murshid setup CLI command checking env path, importing API keys from env files, and auto-updating gitignore. Implement headless CLI keys registration and configure standardized daemon exit codes and trace logs directories.

## Scope
- In: src/cli/setup.rs, src/cli/register.rs
- Out: other src/*.rs files

## Acceptance criteria
- cargo test passes.
- Setup command discovers paths, parses env files and updates gitignore.
- Silent registration flags parse and run correctly.
- Standardized exit codes returned on dependencies/configs failures.

## Constraints
- Edit only: src/cli/setup.rs, src/cli/register.rs
- Standard library only unless the parent spec allows otherwise.
- Do not add scope beyond this task.

## Reporting
Do not rely on printing a summary. Make the edits in the repo and ensure tests
pass. The orchestrator reads the `git diff`, not your stdout.

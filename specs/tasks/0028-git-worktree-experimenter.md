# Task 0028 — Git Worktree Experimenter

**Parent spec:** specs/0000-design.md
**Assigned to:** Gemini (Antigravity)

## Objective
Implement murshid experiment CLI commands. Spawn worktrees nested strictly inside .murshid_experiments/ folder with custom build target dirs and index exclusions, auto-updating project gitignore. Inject IDE multi-root settings. Update watcher target dynamically using channel messaging. Perform 30m idle target pruning and low-disk notification alerts.

## Scope
- In: src/cli/experiment.rs
- Out: other src/*.rs files

## Acceptance criteria
- cargo test passes.
- Worktree created in project directory subfolder.
- VS Code and Neovim settings files generated with cargo target paths.
- Pruning sweeps target caches on 30m idle watch periods.

## Constraints
- Edit only: src/cli/experiment.rs
- Standard library only unless the parent spec allows otherwise.
- Do not add scope beyond this task.

## Reporting
Do not rely on printing a summary. Make the edits in the repo and ensure tests
pass. The orchestrator reads the `git diff`, not your stdout.

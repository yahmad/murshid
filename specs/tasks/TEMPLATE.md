# Task NNNN — <short title>

**Parent spec:** specs/<NNNN>-<name>.md
**Assigned to:** Gemini (Antigravity)

## Objective
<One paragraph. Exactly what to implement — concrete, unambiguous. This is the
only context Gemini gets; it does not see the orchestrator's reasoning.>

## Scope
- In: <files / functions to create or change>
- Out: <do NOT touch these; do not add features beyond this task>

## Acceptance criteria
- `go test ./...` passes.
- <specific behaviours / cases the change must satisfy>

## Constraints
- Edit only: <explicit file list>
- Standard library only unless the parent spec allows otherwise.
- Do not add scope beyond this task.

## Reporting
Do not rely on printing a summary. Make the edits in the repo and ensure tests
pass. The orchestrator reads the `git diff`, not your stdout.

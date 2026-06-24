# Task 0017 — Token Regex Eval Harness

**Parent spec:** specs/0000-design.md
**Assigned to:** Gemini (Antigravity)

## Objective
Build murshid eval command compressing evaluation dataset golden files via include_bytes!. Execute token-normalization mapping identifiers to placeholders (var_n). Calculate Jaccard similarity index strictly over normalized identifiers, asserting similarity < 30%.

## Scope
- In: src/cli/eval.rs
- Out: other src/*.rs files

## Acceptance criteria
- cargo test passes.
- Eval command normalizes identifiers and strips comments/literals.
- Jaccard checker asserts threshold similarity indices under 30%.
- Timeout limits aborted on 10s limits.

## Constraints
- Edit only: src/cli/eval.rs
- Standard library only unless the parent spec allows otherwise.
- Do not add scope beyond this task.

## Reporting
Do not rely on printing a summary. Make the edits in the repo and ensure tests
pass. The orchestrator reads the `git diff`, not your stdout.

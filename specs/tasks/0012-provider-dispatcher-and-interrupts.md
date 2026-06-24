# Task 0012 — Provider Dispatcher & Interrupts

**Parent spec:** specs/0000-design.md
**Assigned to:** Gemini (Antigravity)

## Objective
Implement LLM client REST integrations (Gemini, Claude, Ollama) formatting prompt templates for ChatML/Llama-3. Manage Socratic conversation turns sliding window (last 3 turns context, older turns stripped of code context). Debounce dispatch queries by 1500ms, aborting active provider connection on file write events.

## Scope
- In: src/provider.rs
- Out: other src/*.rs files

## Acceptance criteria
- cargo test passes.
- REST dispatcher formats headers and tokens correctly.
- Turn sliding window stores and prunes turns as required.
- Query debounces stable diagnostic frames and aborts connections immediately on writes.

## Constraints
- Edit only: src/provider.rs, specs/tasks/0012-provider-dispatcher-and-interrupts.md, src/main.rs
- Standard library only unless the parent spec allows otherwise.
- Do not add scope beyond this task.

## Reporting
Do not rely on printing a summary. Make the edits in the repo and ensure tests
pass. The orchestrator reads the `git diff`, not your stdout.

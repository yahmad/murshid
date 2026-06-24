# Task 0026 — DevContainer LSP TCP Bridge

**Parent spec:** specs/0000-design.md
**Assigned to:** Gemini (Antigravity)

## Objective
Implement container stdio-to-TCP bridge listener binding to 127.0.0.1, auto-selecting port offsets in 8488-8500 range. Enforce Dynamic single-use security token authorization via container_session.json file permissions 0600.

## Scope
- In: src/lsp/bridge.rs
- Out: other src/*.rs files

## Acceptance criteria
- cargo test passes.
- Host bindings forward container TCP streams.
- Single-use handshake token session files auth correctly.
-  WSL integrations fall back to loopback forwarding.

## Constraints
- Edit only: src/lsp/bridge.rs
- Standard library only unless the parent spec allows otherwise.
- Do not add scope beyond this task.

## Reporting
Do not rely on printing a summary. Make the edits in the repo and ensure tests
pass. The orchestrator reads the `git diff`, not your stdout.

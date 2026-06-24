# Task 0022 — LSP Proxy & UDS/Named Pipes IPC

**Parent spec:** specs/0000-design.md
**Assigned to:** Gemini (Antigravity)

## Objective
Implement client proxy lsp-proxy forwarding stdio over Unix domain socket at Application Support/murshid/lsp.sock or Windows named pipe. Enforce strictly permission checks (0700 sockets folders, lstat symlinks verification, getsockopt peer matching, Windows DACL client impersonation SIDs verify).

## Scope
- In: src/lsp/proxy.rs
- Out: other src/*.rs files

## Acceptance criteria
- cargo test passes.
- LSP proxy establishes connection within 2000ms.
- Stricter permission check rejects symlinks and wrong process IDs.
- Impersonation and SID verification blocks hijack loops on Windows.

## Constraints
- Edit only: src/lsp/proxy.rs
- Standard library only unless the parent spec allows otherwise.
- Do not add scope beyond this task.

## Reporting
Do not rely on printing a summary. Make the edits in the repo and ensure tests
pass. The orchestrator reads the `git diff`, not your stdout.

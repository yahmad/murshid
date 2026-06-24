# Task 0004 — Platform Keyring Credential Cache

**Parent spec:** specs/0000-design.md
**Assigned to:** Gemini (Antigravity)

## Objective
Implement Claude/Gemini credentials retrieval using the keyring crate from the local OS store. Implement env keys fallback if the keyring is unavailable or locked, logging warning diagnostic logs. Cache API keys in thread-safe, volatile memory cache (std::sync::Arc<std::sync::RwLock<Option<CachedKeys>>>), invalidating and refreshing on SIGHUP or config modification.

## Scope
- In: src/credentials.rs
- Out: other src/*.rs files

## Acceptance criteria
- cargo test passes.
- Retrieve credentials from system keyring; fall back to environment variables with warnings.
- In-memory key cache resolves within 50ms (REQ-N-401).
- Cache hot-reloads on configuration write detection or SIGHUP.

## Constraints
- Edit only: src/credentials.rs, Cargo.toml, specs/tasks/0004-platform-keyring-credential-cache.md, src/main.rs
- Standard library only unless the parent spec allows otherwise (keyring crate is explicitly allowed by the parent spec).
- Do not add scope beyond this task.

## Reporting
Do not rely on printing a summary. Make the edits in the repo and ensure tests
pass. The orchestrator reads the `git diff`, not your stdout.

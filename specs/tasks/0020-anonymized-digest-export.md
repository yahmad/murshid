# Task 0020 — Anonymized Digest Export

**Parent spec:** specs/0000-design.md
**Assigned to:** Gemini (Antigravity)

## Objective
Implement Team Tier local digests exporter (digest_<device_id_hash>_<project_root_hash>.json) replacing workspace paths with SHA-256 hash of project names. Sign payloads with HMAC-SHA256 using rotated team key priority check.

## Scope
- In: src/team/exporter.rs, src/team/verifier.rs
- Out: other src/*.rs files

## Acceptance criteria
- cargo test passes.
- Digest exporter anonymizes project names and emails.
- JSON string generated contains valid alphabetic key-sorted HMAC signature.
- Signature validation checks prioritize comma-separated rotated variables.

## Constraints
- Edit only: src/team/exporter.rs, src/team/verifier.rs
- Standard library only unless the parent spec allows otherwise.
- Do not add scope beyond this task.

## Reporting
Do not rely on printing a summary. Make the edits in the repo and ensure tests
pass. The orchestrator reads the `git diff`, not your stdout.

# Spec Index

The specification is the source of truth. Implement against the active spec;
do not invent scope beyond it.

| ID   | Title                                                | Status | File                                                         |
|------|------------------------------------------------------|--------|--------------------------------------------------------------|
| 0000 | (frozen design — dropped in at stamp)                | —      | specs/0000-design.md                                         |
| 0001 | Configuration Merge & Lock | Done | [0001-configuration-merge-and-lock.md](tasks/0001-configuration-merge-and-lock.md) |
| 0002 | SQLite Database Schema & Migrations | Done | [0002-sqlite-database-schema-and-migrations.md](tasks/0002-sqlite-database-schema-and-migrations.md) |
| 0003 | SQLite Database Corruption Recovery & Backups | Done | [0003-sqlite-database-corruption-recovery-and-backups.md](tasks/0003-sqlite-database-corruption-recovery-and-backups.md) |
| 0004 | Platform Keyring Credential Cache | Done | [0004-platform-keyring-credential-cache.md](tasks/0004-platform-keyring-credential-cache.md) |
| 0005 | Watcher Core & Polling Fallback | Done | [0005-watcher-core-and-polling-fallback.md](tasks/0005-watcher-core-and-polling-fallback.md) |
| 0006 | Asynchronous Cargo Check Interceptor | Done | [0006-asynchronous-cargo-check-interceptor.md](tasks/0006-asynchronous-cargo-check-interceptor.md) |
| 0007 | Watcher Throttling & Limits Coordinator | Done | [0007-watcher-throttling-and-limits-coordinator.md](tasks/0007-watcher-throttling-and-limits-coordinator.md) |
| 0008 | Compile Guards and Errors Isolation | Done | [0008-compile-guards-and-errors-isolation.md](tasks/0008-compile-guards-and-errors-isolation.md) |
| 0009 | Context Scoping & XML Bounds | Done | [0009-context-scoping-and-xml-bounds.md](tasks/0009-context-scoping-and-xml-bounds.md) |
| 0010 | Secrets Scanner & Path Sanitizer | Done | [0010-secrets-scanner-and-path-sanitizer.md](tasks/0010-secrets-scanner-and-path-sanitizer.md) |
| 0011 | Socratic State Machine & Pedagogy Auto-Scaling | Done | [0011-socratic-state-machine-and-pedagogy-auto-scaling.md](tasks/0011-socratic-state-machine-and-pedagogy-auto-scaling.md) |
| 0012 | Provider Dispatcher & Interrupts | Done | [0012-provider-dispatcher-and-interrupts.md](tasks/0012-provider-dispatcher-and-interrupts.md) |
| 0013 | Socratic Output Code Redactor | Done | [0013-socratic-output-code-redactor.md](tasks/0013-socratic-output-code-redactor.md) |
| 0014 | CLI Onboarding Setup | Done | [0014-cli-onboarding-setup.md](tasks/0014-cli-onboarding-setup.md) |
| 0015 | CLI Socratic Commands & Bypass | Done | [0015-cli-socratic-commands-and-bypass.md](tasks/0015-cli-socratic-commands-and-bypass.md) |
| 0017 | Token Regex Eval Harness | Done | [0017-token-regex-eval-harness.md](tasks/0017-token-regex-eval-harness.md) |
| 0022 | LSP Proxy & UDS/Named Pipes IPC | Done | [0022-lsp-proxy-and-uds-named-pipes-ipc.md](tasks/0022-lsp-proxy-and-uds-named-pipes-ipc.md) |
| 0023 | LSP Server Core & Workspace Multiplexing | Done | [0023-lsp-server-core-and-workspace-multiplexing.md](tasks/0023-lsp-server-core-and-workspace-multiplexing.md) |
| 0024 | LSP Server Diagnostics Publishing | Done | [0024-lsp-server-diagnostics-publishing.md](tasks/0024-lsp-server-diagnostics-publishing.md) |
| 0025 | LSP Server Custom Code Actions | Done | [0025-lsp-server-custom-code-actions.md](tasks/0025-lsp-server-custom-code-actions.md) |
| 0026 | DevContainer LSP TCP Bridge | Done | [0026-devcontainer-lsp-tcp-bridge.md](tasks/0026-devcontainer-lsp-tcp-bridge.md) |
| 0027 | Commit Cleaner pre-commit | Done | [0027-commit-cleaner-pre-commit.md](tasks/0027-commit-cleaner-pre-commit.md) |
| 0028 | Git Worktree Experimenter | Done | [0028-git-worktree-experimenter.md](tasks/0028-git-worktree-experimenter.md) |

**Active spec for current work:** none

Specs 0016 and 0018–0021 were removed 2026-07-03: their subsystems were pruned
as unmandated enterprise scope in the 2026-07 realignment (commits ad2e9a5,
88b43a8, b48e253, f787fd6). specs/ will be re-seeded from the new foundations
(Linear MUR-5).

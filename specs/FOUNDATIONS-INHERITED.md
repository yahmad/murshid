# Inherited foundations (compacted from the removed 0001–0028 series, 2026-07-03)

The following subsystems were built under the archived v6.0 spec effort,
survive the 2026-07 re-seed because the ratified design (SPEC v0.5) needs
them, and are now specified by **their code + tests** plus the Core contracts
in SPEC v0.5. Their original task specs (0001–0010, 0012, 0014, plus
removed-scope 0011/0013/0015/0017/0022–0028) live in git history
(pre-re-seed tree, commit 8d4cfbc and earlier).

| Module | What it provides | SPEC v0.5 anchor |
|---|---|---|
| config.rs | TOML merge/precedence + lock; system-config security | knobs land per C12; `pedagogy.team.*` fields are dead, remove on touch |
| db.rs / backup.rs | SQLite open/migrations, corruption recovery, backups | C5 schema arrives as new migrations |
| credentials.rs | keyring cache, env override, SIGHUP reload | C6 key storage (note: tests are parallel-flaky — SIGHUP + real Keychain) |
| watcher.rs / watcher_coordinator.rs | native+polling file watch, exclusions, resource limits | C2 session diff + C12 quiescence layer on top |
| compiler.rs | async cargo-check interceptor, diagnostics parsing, guards | D3 supporting signal; C6 lint path |
| context.rs | scoped payload extraction, XML bounds, injection sanitizing | C6 stage I/O context assembly |
| sanitizer.rs | secrets/path redaction before any LLM call | non-negotiable mandate |
| provider.rs | BYOK dispatcher (Claude/Gemini/Ollama), debounce/interrupts | C6 two-slot model seam reuses this |
| cli/setup.rs | onboarding, gitignore/env hygiene | reshaped by D13/D17 in T3/T4 |
| cli/goal.rs | goal tracking CLI + prompt injection | reshaped by D13/D14 in T3 |

Removed the same day (code + specs): LSP suite (later delivery channel per
D4), Socratic state machine & auto-scaling (superseded by D5 + CD-4),
Socratic code redactor and its eval harness + bypass (withholding-as-identity,
corrected in FOUNDER-VISION), register (licensing remnant).

Known leftovers to clean when their module is next touched: the dead
`Socratic_bypass_log` table (db — its writer, cli/bypass.rs, was cut
2026-07-03; drop via migration on next db touch). CLEANED 2026-07-03 in the
T1 fix pass: `license_status` column, `pedagogy.team.analytics_opt_in`/
`pseudonym` config fields.

# Murshid — Local-First Socratic AI Coding Mentor

`murshid` is a local-first pedagogical agent designed to assist software developers learning Rust. Instead of writing code directly for the developer, it explains compiler diagnostics, reviews implementation strategies, asks guiding Socratic questions, and scaffolds challenges, while intentionally withholding complete code solutions.

---

## Project Structure

This project uses a single Rust package with multiple binaries (CLI, daemon, and editor proxies) to share a unified core engine.

```text
murshid/
├── Cargo.toml            # Package manifest
├── specs/                # Source of truth specifications
│   ├── INDEX.md          # Active task tracking
│   ├── 0000-design.md    # System design spec
│   ├── 0000-requirements.md # Product requirements
│   └── tasks/            # Decomposed task specs
├── docs/                 # System design & architecture docs
│   ├── architecture.md   # Component map & topology
│   ├── config_and_db.md  # Configuration & database schemas
│   └── watcher_and_compiler.md # Watchers & compilers design
├── src/
│   ├── config.rs         # Merged Configuration Loaders
│   ├── db.rs             # SQLite Database Schema & Migrations
│   ├── backup.rs         # OS Platform Secure Backups
│   ├── credentials.rs    # OS Keyring Cache
│   ├── watcher.rs        # Filesystem Watcher & Polling Fallback
│   ├── compiler.rs       # Asynchronous Compiler Interceptor
│   ├── watcher_coordinator.rs # Throttling Coordinator & Limits
│   └── main.rs           # Core entry point
└── macapp/               # Native macOS SwiftUI Menu Bar app (Phase 2)
```

---

## Phased Roadmap

### Phase 1: CLI Dogfooding Beta (Free Core)
A lightweight command-line tool that watches directories for Rust compiler events, intercepts JSON diagnostic streams, checks concept mastery progress, and runs the Socratic dialog.
- Core config loader & lock policy overrides
- Local SQLite database (`profile.db`) with migrations and corruption self-repair
- Filesystem watcher & asynchronous `cargo check` interceptor
- Bounded context generator, secrets filters, and path sanitizers
- Pedagogy Auto-Scaling using mastery curves
- Local offline documentation lookups
- Token-normalized Regex similarity evaluation harness (`murshid eval`)

### Phase 2: macOS Desktop App & Advanced Extensions (Pro Tier)
Deep system integrations, native UI feedback, and IDE diagnostics.
- SwiftUI Menu Bar state widget and detachable floating chat panel
- Embedded LSP Server + stdio socket proxy (`lsp-proxy`)
- DevContainer LSP Socket-to-TCP bridge
- Git pre-commit cruft cleaner (`murshid clean`)
- Isolated git-worktree refactoring sandbox manager (`murshid experiment`)
- Cryptographic subscription licensing verification (Ed25519)
- Team analytics digest sync & offline dashboard aggregator

---

## Getting Started

### Prerequisites

Ensure you have Rust and Cargo installed:
```bash
cargo --version
```

### Running Tests

Verification is driven by standard Rust testing:
```bash
cargo test
```

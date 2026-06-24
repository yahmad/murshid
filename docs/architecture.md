# Architecture Design & Component Map

This document outlines the high-level architecture and implementation details for the first two milestones of **Murshid** (Phase 1 CLI Beta).

---

## 1. System Topology

Murshid uses a multi-layered local-first architecture to monitor filesystem events, run asynchronous background cargo compilations, and track user mastery metrics locally.

```mermaid
graph TD
    Watcher[watcher.rs: File Watcher] -- File Write Event --> Coordinator[watcher_coordinator.rs: Coordinator]
    Coordinator -- "Limit Check (size/lines)" --> Interceptor[compiler.rs: Compiler Interceptor]
    Interceptor -- "Acquire Compile Lock (3000ms)" --> LockMgr[compiler.rs: LockManager]
    LockMgr -- Lock Granted --> CargoCheck[cargo check --message-format=json]
    CargoCheck -- JSON stream on stdout --> Interceptor
    CargoCheck -- warning/lock logs on stderr --> Interceptor
    Interceptor -- "Check target size > 5GB" --> Prune[watcher_coordinator.rs: Pruner]
    Interceptor -- Output & Diagnostics --> Socratic[Socratic Engine (Upcoming)]
    
    Config[config.rs: Config Manager] -- Load Config --> Watcher
    Config -- Load Config --> Credentials[credentials.rs: Keyring Cache]
    Credentials -- Lookups --> Keychain[OS Keychain / Env overrides]
    
    DB[db.rs: SQLite DB & Migrations] -- Read/Write Progress --> Backups[backup.rs: OS Secure Backups]
```

---

## 2. Key Modules & Implementations

### (a) Configuration Manager (`src/config.rs`)
*   **Purpose:** Expose user and project settings, merging defaults with system-level, user-level, and project-level TOML configurations.
*   **Lock Policy:** If a system-wide configuration sets `lock_policy = true` for a section, any attempts by users or projects to override settings in that section are ignored, resetting them to system defaults.
*   **Security:** System-level configuration files must be owned by Root/Administrator and set with tight POSIX permissions (`0644` or `0600`).

### (b) Local Database & Migrations (`src/db.rs`, `src/backup.rs`)
*   **Database Settings:** Uses SQLite under WAL (Write-Ahead Logging) mode and `synchronous = NORMAL` for highly concurrent, safe local operations.
*   **Migrations Engine:** Manages schema changes incrementally using `PRAGMA user_version` wrapped in immediate transactions. If a migration fails, the database automatically rolls back using a duplicate copy saved as a `db.migration_backup`.
*   **Corruption Recovery:** Upon catching header mismatches or PRAGMA integrity check failures, it archives the corrupted database as `profile.db.corrupt.<timestamp>`, initializes a clean database, and restores the user's progress mastery thresholds from the OS platform native backup store (`NSUserDefaults` on macOS, Registry on Windows, and `.state_backup` on Linux).

### (c) Credentials Cache (`src/credentials.rs`)
*   **Key Storage:** Integrates with OS credential managers (macOS Keychain, Linux Secret Service, Windows Credential Manager) via the `keyring` crate.
*   **Volatile In-Memory Cache:** Caches credentials in a thread-safe, global static `Arc<RwLock<Option<CachedKeys>>>` resolving in `<1ms` to avoid blocking watcher loops.
*   **Hot-Reloading:** Spawns a background file watcher thread that polls `config.toml` modifications, invalidating and reloading the cache on changes or upon receiving a `SIGHUP` signal.

### (d) Filesystem Watcher (`src/watcher.rs`, `src/watcher_coordinator.rs`)
*   **Watcher Engine:** Monitors filesystem events using `notify::RecommendedWatcher`.
*   **Throttling Coordinator:** Restricts thread usage and file descriptor counts process-wide. If FD count exceeds the configuration limit, it falls back to a background polling thread (running every 2500ms).
*   **Exclusion Boundaries:** Filters out events for directories matching `target/`, `.git/`, and `.murshid_experiments/`, and ignores files exceeding 50KB or 1500 lines.

### (e) Asynchronous Cargo Interceptor (`src/compiler.rs`)
*   **Compile Execution:** Spawns background `cargo check --message-format=json` runs, setting `CARGO_TARGET_DIR` to `<project-root>/target/murshid/` to isolate compiler runs from the developer's normal cargo builds.
*   **Cancellation Safety:** When a new file write trigger is received, the interceptor immediately sends a `SIGTERM` to the active compiler process, followed by `SIGKILL` after 500ms if it hasn't terminated, preventing compiler thread accumulation.
*   **Error Parsing & Prioritization:** Parses the stdout JSON stream, filtering warnings and prioritizing errors matching the active file, truncating the result list to at most 3 diagnostics.
*   **Infrastructure Error Isolation:** Evaluates the exit code and stderr content for environment issues (network offline, package version conflicts, lock timeouts). If detected, it bypasses Socratic prompts and mastery updates.

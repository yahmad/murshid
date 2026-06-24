# Configuration & Database Schema Design

This document details the choices, design rules, and verification patterns behind Murshid's configuration merge engine and local SQLite storage.

---

## 1. Custom TOML Merging & Precedence

To comply with standard-library-only constraints for early phase modules, Murshid uses a lightweight, line-by-line custom TOML parser in [config.rs](file:///Users/yasir/src/thabit/murshid/src/config.rs). 

### Merge Sequence
Configurations are loaded and merged from lowest to highest precedence:
1.  **Embedded Defaults:** Internal fallback values compiled within the binary.
2.  **System-level config:** Placed at `/Library/Application Support/` (macOS), `ProgramData` (Windows), or `/etc/` (Linux). Requires administrator ownership and permissions set to `0644`/`0600`.
3.  **User-level config:** Placed in user's home configuration directory (e.g. `~/.config/murshid/config.toml`).
4.  **Project-level config:** Found recursively climbing parent paths for `.murshid.toml` or `.murshid/config.toml`.

### Lock Policy Override
If the system-level config declares `lock_policy = true` for a specific configuration block (e.g. `pedagogy`), any user or project level settings inside that block are discarded. The engine automatically rolls them back to the system-defined values and prints warnings to `stderr`.

---

## 2. SQLite Database & Storage Schema

Progress metrics and interaction traces are persisted in a local SQLite file (`profile.db`).

### Connection Speed & Safety Options
*   **WAL Mode:** Enabled via `PRAGMA journal_mode = WAL;`. Write-Ahead Logging allows reader threads to access the database concurrently while a writer thread is modifying tables.
*   **Synchronous Normal:** Enabled via `PRAGMA synchronous = NORMAL;`. Limits disk syncs during database transactions. When combined with WAL, this provides high performance while ensuring durability.
*   **Busy Timeout (5000ms):** Automatically retries transaction queries with randomized exponential backoff if database files are temporarily locked by parallel writer threads.

### Table Schema Highlights
1.  **`user_profile`:** Tracks user identities, email hash, and subscription tier.
2.  **`concepts`:** Tracks category-level student mastery scores (EMA curve $S_t = \alpha \cdot M_t + (1 - \alpha) \cdot S_{t-1}$).
3.  **`compilation_errors`:** Stores error spans, file path prefixes sanitized to `[USER_HOME]`, and occurrence frequencies.
4.  **`pedagogical_interactions`:** Tracks LLM prompts and token usages.

---

## 3. Self-Repair & Secure OS Backups

To guard against database deletions (accidental or intentional attempts to reset mastery curves), Murshid synchronizes category mastery thresholds to OS-native registries during database writes:
*   **macOS:** `NSUserDefaults` via command line utility bindings (`defaults write`/`read`).
*   **Windows:** Windows Registry (`reg add`/`query` targeting `HKCU\Software\Thabit\Murshid`).
*   **Linux:** Owner-restricted file (`~/.local/share/murshid/.state_backup` with strictly `0600` POSIX permissions).

### Corruption Lifecycle
```text
  Startup -> Connection Open -> Run PRAGMA integrity_check
                                       |
                   +-------------------+-------------------+
                   | OK                                    | Corrupt / NotADatabase
                   v                                       v
             Run Migrations                   Rename to profile.db.corrupt.<ts>
                                                           |
                                                           v
                                                Initialize Fresh DB
                                                           |
                                                           v
                                                Restore Mastery Scores
                                                 from OS Secure Backup
```

---

## 4. Testing & Verification

*   **Concurrency Stress Tests:** Tests verify that transactional rollbacks work correctly upon migration crashes, and that concurrent retries successfully resolve file locks.
*   **Thread-Local State Mocking:** To allow unit tests to run concurrently without corrupting process-wide configuration or registry states, we utilize a `thread_local!` path override variable inside [src/backup.rs](file:///Users/yasir/src/thabit/murshid/src/backup.rs), isolating mock files per test thread.

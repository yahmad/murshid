# Design Plan: Rich Context History Tracking System

This document outlines the design for introducing chronological tracking of user edits and compiler check results into Murshid's SQLite database (`profile.db`). These logged events will be injected into the developer context payload sent to the LLM, giving the Socratic Mentor awareness of the user's recent programming trajectory.

---

## 1. Objectives & Rationale

*   **Trace Programming Trajectory:** To understand the developer's intent and recent struggles, the LLM needs to know what files they recently edited, whether the code compiled, what errors they encountered, and how they progressed.
*   **Persistent & Robust Storage:** All events must be safely recorded in `profile.db` using the existing WAL-mode SQLite database connection and transaction-safe schema migrations.
*   **Proper Pedagogy Integration:** Socratic dialogue and concept mastery state calculations (such as `pedagogy::handle_compile_check_event`) are defined in the codebase but never triggered in the watch loop. We will hook this lifecycle event up correctly to drive Socratic guidance progression.
*   **Minimal Overhead:** The history lookup should be highly efficient, using index-backed chronological queries to fetch a configurable sliding window of recent activities.

---

## 2. SQLite Schema Evolution

We will evolve the database schema to version 2 by introducing a unified chronological history log table.

### Unified vs. Normalized Tables
Instead of keeping separate, fragmented tables for edits and compiler outputs that require heavy SQL `UNION` operations, we will store events in a unified `context_history` table. This approach makes chronological queries simpler and highly performant.

```sql
-- Migration block to schema version 2 in src/db.rs
CREATE TABLE IF NOT EXISTS context_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    event_type TEXT NOT NULL,         -- 'file_edit' | 'compiler_check'
    project_root TEXT NOT NULL,       -- Path to project workspace root
    file_path TEXT NOT NULL,          -- Path to the file involved (relative to workspace)
    success BOOLEAN,                  -- NULL for edits, true/false for compiler checks
    error_code TEXT,                  -- NULL for edits/successful checks, e.g., 'E0382'
    error_message TEXT,               -- NULL for edits/successful checks
    line_number INTEGER,              -- NULL for edits/successful checks
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_history_project_time 
ON context_history(project_root, created_at DESC);
```

### Migration Implementation (`src/db.rs`)
We will increment the database schema version check from version 1 to version 2. If the user's active `PRAGMA user_version` is `< 2`, the migration engine will run:
1.  **Backup creation:** Copy the physical `profile.db` to `profile.db.migration_backup` before modifying the schema.
2.  **Transaction wrapping:** Execute the `CREATE TABLE` and `CREATE INDEX` SQL statements, and update the version flag using `PRAGMA user_version = 2;` inside a transaction.
3.  **Self-Repair:** Rollback, restore the file from the backup, and report the error on failure.

---

## 3. Logging Activity in the Watcher Loop

We will modify the file watch callback in `src/main.rs` to orchestrate chronological event logging.

### Edit and Compile Lifecycle Flow
When a file is modified and saved:
1.  **Resolve Project Path and Workspace Hash:** Retrieve the canonicalized project root path and compute the SHA-256 hash using `pedagogy::sha256(project_root.to_string_lossy().as_bytes())` to replace the hardcoded `"workspace-hash"` placeholder.
2.  **Log File Edit:** Establish a database connection and insert a `'file_edit'` event into the `context_history` table.
3.  **Run Compiler Check:** Execute `CompilerInterceptor::run_check(&project_root, &path)`.
4.  **Log Compile Result:** Insert a `'compiler_check'` event.
    *   On **Success**: Log the event with `success = true`.
    *   On **Failure**: Log the event with `success = false`, recording the primary error code, message, and line number from the first prioritized diagnostic.
5.  **Update Socratic Dialogue & Concept Mastery State:**
    *   Call `pedagogy::handle_compile_check_event` using the resolved workspace hash, file path, error code, and compilation success status.
    *   This will correctly drive mastery adjustments and dialogue scaffold level updates.

---

## 4. Context Payload Enrichment

We will enrich the XML context payload sent to the model provider in `src/context.rs`.

### Querying Recent History
Expose a database query function in `src/db.rs`:
```rust
pub fn get_recent_history(
    conn: &rusqlite::Connection,
    project_root: &Path,
    limit: usize,
) -> rusqlite::Result<Vec<HistoryEvent>>
```
This fetches the last $N$ events (e.g. 5) sorted in ascending order of `created_at` (chronological timeline).

### XML Serialization Format
In `generate_context_payload` (inside `src/context.rs`), we will query the timeline and serialize it as a `<recent_history>` element:

```xml
<developer_code_context>
  <file_path>src/main.rs</file_path>
  <error_line>18</error_line>
  <code_span>
    ...
  </code_span>
  
  <!-- Chronological activity trace -->
  <recent_history>
    <event type="file_edit" file="src/lib.rs" timestamp="2026-07-02 01:21:30" />
    <event type="compiler_check" file="src/lib.rs" timestamp="2026-07-02 01:21:35" success="true" />
    <event type="file_edit" file="src/main.rs" timestamp="2026-07-02 01:23:45" />
    <event type="compiler_check" file="src/main.rs" timestamp="2026-07-02 01:23:50" success="false">
      <diagnostic code="E0308" line="18" message="mismatched types" />
    </event>
  </recent_history>
  
  <referenced_types>
    ...
  </referenced_types>
</developer_code_context>
```

---

## 5. File Modification Blueprint (Summary)

### `src/db.rs`
*   Add `context_history` to DB initialization/migrations.
*   Implement `log_history_event(...)` to record edits and compiler checks.
*   Implement `get_recent_history(...)` to fetch chronological events.
*   Write unit tests to verify migrations and event retrieval.

### `src/main.rs`
*   Replace `"workspace-hash"` placeholder with actual SHA-256 workspace directory hash.
*   Insert `'file_edit'` and `'compiler_check'` logs on watcher triggers.
*   Call `pedagogy::handle_compile_check_event` on compile success/failure to link pedagogy calculations with watcher events.

### `src/context.rs`
*   Integrate history fetching into `generate_context_payload`.
*   Render the `<recent_history>` XML block with sanitized paths and details.
*   Add unit tests to verify correct formatting of history events in context payload.


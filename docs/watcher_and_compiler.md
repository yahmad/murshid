# File System Watcher & Compiler Interceptor Design

This document details the design, constraints, and implementation of Murshid's directory watch loops and asynchronous compiler runner.

---

## 1. Directory Watch Loops & Polling Fallback

Murshid uses a multi-tiered file monitoring design in [watcher.rs](file:///Users/yasir/src/yahmad/murshid/src/watcher.rs) to process code edits recursively.

### Resource Throttling Coordinator
To prevent system crashes due to OS descriptor limits (such as `EMFILE` or `ENOSPC`), the coordinator in [watcher_coordinator.rs](file:///Users/yasir/src/yahmad/murshid/src/watcher_coordinator.rs) tracks active watch threads and file descriptor counts process-wide:
1.  **Native Monitor Mode:** Spawns a native OS loop using the `notify` crate. Requires 1 thread and a descriptor count matching the total files in the workspace.
2.  **Polling Fallback Mode:** Activates automatically if resource limits are exceeded or if native setup fails. Scans the directory tree recursively every 2500ms using a single background thread, using 0 persistent file descriptors.

### Boundary Verification
*   **Symlink Checking:** All monitored files are canonicalized and validated to ensure they resolve within the workspace root.
*   **External Links:** Paths resolving outside the workspace are rejected unless explicitly whitelisted in `watcher.include_external_links`.
*   **Path Exclusions:** Directory traversals completely ignore paths matching `target/`, `.git/`, and `.murshid_experiments/`.
*   **File Size & Line Limits:** Watch event processing immediately skips any `.rs` files exceeding 50KB or 1500 lines to avoid token bloat and performance degradation.

---

## 2. Asynchronous Compiler Interceptor

When a file edit event is validated, the watcher triggers the compiler interceptor in [compiler.rs](file:///Users/yasir/src/yahmad/murshid/src/compiler.rs).

### Synchronous Process Cancellation
To prevent background cargo check threads from accumulating during rapid file saves, the interceptor enforces a strict cancellation lifecycle:
```text
  File Save Event -> Check Active Compiler Process ID
                          |
             +------------+------------+
             | None                    | Some(PID)
             v                         v
       Spawn Cargo Check          Send SIGTERM
                                       |
                                       v
                                 Wait up to 500ms
                                       |
                      +----------------+----------------+
                      | Exited                          | Still Running
                      v                                 v
                Spawn Cargo Check                 Send SIGKILL
                                                        |
                                                        v
                                                  Spawn Cargo Check
```

### isolated Compilation Target
All interceptor runs override `CARGO_TARGET_DIR` to `<project-root>/target/murshid/` to isolate compilation targets, preventing lock conflicts with the developer's main terminal or editor build processes.

### Timeout Guard & Cache Pruning
*   **Compile Lock Timeout:** Enforces a strict 3000ms timeout on compile locks. If blocked longer than 3000ms, the check aborts and returns a timeout diagnostic.
*   **Target Size Pruning:** Runs an asynchronous background evaluation after compilation. If `<project-root>/target/murshid/` size exceeds 5GB, it executes a localized `cargo clean` targeting only the isolated directory.

---

## 3. Diagnostics Parsing & Infrastructure Error Isolation

*   **JSON stream:** Evaluates stdout from `cargo check --message-format=json` and parses compiler diagnostics.
*   **Socratic Prioritization:** Keeps only diagnostics of level `"error"`, sorts them to prioritize errors in the active file, and truncates the list to a maximum of 3 errors to prevent student cognitive overload.
*   **Infrastructure Error Isolation:** Analyzes the exit status and stderr. If cargo check fails due to infrastructure, registry download, or network connectivity issues (e.g. registry timeouts, locked files, missing packages), it marks the run with `is_infra_error = true`, which gates the card pipeline — infra failures never reach the screen/judge stages or the struggle-signal tracking.

---

## 4. Language-Pack Seam (T6/T7)

`compiler.rs` is the Rust pack's diagnostics adapter, mounted as a child module of `pack.rs` (the only code-registration surface). The Go pack's parallel adapter (`go_adapter.rs`) drives `go build`/`go vet` through the same normalized-diagnostics contract, and each pack's `surface.toml` declares its watched file extensions and comment token. Everything above the adapter — quiescence gating, screen/judge pipeline, cards — is language-agnostic.

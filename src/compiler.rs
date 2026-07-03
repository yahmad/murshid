//! The Rust pack's diagnostics adapter (T6 payload 1 / I27): invokes
//! `cargo check --message-format=json` and maps its native output to the
//! engine's [`crate::pack::NormalizedRecord`] shape (I28, amended by C9).
//! This is the ONE piece of code the Rust pack contributes — every other
//! Rust-specific concern lives in `packs/rust/` data files. Together with
//! `pack.rs` (the loader/registry), this module is the seam's other allowed
//! home for "rust"/"cargo" literals.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct CompilerSpan {
    pub file_name: String,
    pub line_start: usize,
    pub line_end: usize,
    pub column_start: usize,
    pub column_end: usize,
    pub text: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct CompilerDiagnostic {
    pub code: Option<String>,
    pub message: String,
    pub spans: Vec<CompilerSpan>,
    pub level: String, // e.g., "error", "warning"
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct CompileOutput {
    pub success: bool,
    pub diagnostics: Vec<CompilerDiagnostic>,
    pub is_infra_error: bool,
}

pub struct CompilerInterceptor {
    active_process: Arc<Mutex<Option<Child>>>,
}

struct LockManager {
    locks: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
}

fn get_lock_manager() -> &'static LockManager {
    static LOCK_MANAGER: OnceLock<LockManager> = OnceLock::new();
    LOCK_MANAGER.get_or_init(|| LockManager {
        locks: Mutex::new(HashMap::new()),
    })
}

pub struct ProjectLockGuard {
    _guard: std::sync::MutexGuard<'static, ()>,
}

fn acquire_project_lock(project_root: &Path, timeout_ms: u128) -> Result<ProjectLockGuard, String> {
    let project_path = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());

    let lock = {
        let mut locks_guard = get_lock_manager().locks.lock().unwrap();
        locks_guard
            .entry(project_path.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    };

    let start = std::time::Instant::now();
    loop {
        if let Ok(guard) = lock.try_lock() {
            let guard_static = unsafe {
                std::mem::transmute::<
                    std::sync::MutexGuard<'_, ()>,
                    std::sync::MutexGuard<'static, ()>,
                >(guard)
            };
            return Ok(ProjectLockGuard {
                _guard: guard_static,
            });
        }
        if start.elapsed().as_millis() >= timeout_ms {
            return Err("Compile lock timeout exceeded".to_string());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

fn determine_is_infra_error(
    status: std::process::ExitStatus,
    stderr: &str,
    has_compilation_errors: bool,
) -> bool {
    if status.success() {
        return false;
    }

    if !has_compilation_errors {
        return true;
    }

    let stderr_lower = stderr.to_lowercase();
    let infra_patterns = [
        "blocking waiting for file lock",
        "failed to resolve",
        "registry",
        "connection timeout",
        "network",
        "download",
        "toolchain mismatch",
        "failed to select a version",
        "could not find",
        "offline",
    ];

    for pattern in &infra_patterns {
        if stderr_lower.contains(pattern) {
            return true;
        }
    }

    false
}

impl Default for CompilerInterceptor {
    fn default() -> Self {
        Self::new()
    }
}

impl CompilerInterceptor {
    pub fn new() -> Self {
        Self {
            active_process: Arc::new(Mutex::new(None)),
        }
    }

    pub fn run_check(
        &self,
        project_root: &Path,
        active_file: &Path,
    ) -> Result<CompileOutput, String> {
        // Enforce 3000ms compile lock timeout
        let _lock = match acquire_project_lock(project_root, 3000) {
            Ok(l) => l,
            Err(_) => {
                return Ok(CompileOutput {
                    success: false,
                    diagnostics: vec![CompilerDiagnostic {
                        code: Some("TIMEOUT".to_string()),
                        message: "Socratic check suspended: compile lock timeout exceeded."
                            .to_string(),
                        spans: vec![],
                        level: "error".to_string(),
                    }],
                    is_infra_error: true,
                });
            }
        };

        // Terminate any previous run
        {
            let mut proc_guard = self.active_process.lock().unwrap();
            if let Some(mut child) = proc_guard.take() {
                terminate_process(&mut child);
            }
        }

        // Start the new cargo check process
        let target_dir = project_root.join("target/murshid");
        let mut cmd = Command::new("cargo");
        cmd.args(["check", "--message-format=json"]);
        cmd.env("CARGO_TARGET_DIR", &target_dir);
        cmd.current_dir(project_root);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        let child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn cargo check: {}", e))?;

        // Save to active process so another thread can terminate it if needed
        {
            let mut proc_guard = self.active_process.lock().unwrap();
            *proc_guard = Some(child);
        }

        // Reacquire child handle from option for wait
        let child_handle = {
            let mut proc_guard = self.active_process.lock().unwrap();
            proc_guard.take()
        };

        if child_handle.is_none() {
            return Err("Process was terminated before execution".to_string());
        }

        let child = child_handle.unwrap();
        let output = child.wait_with_output().map_err(|e| e.to_string())?;

        let stdout_str = String::from_utf8_lossy(&output.stdout);
        let stderr_str = String::from_utf8_lossy(&output.stderr);
        let mut raw_diagnostics = Vec::new();

        for line in stdout_str.lines() {
            if let Ok(cargo_msg) = serde_json::from_str::<CargoMessage>(line) {
                if cargo_msg.reason == "compiler-message" {
                    if let Some(msg) = cargo_msg.message {
                        let spans = msg
                            .spans
                            .into_iter()
                            .map(|s| {
                                let text_joined = s
                                    .text
                                    .into_iter()
                                    .map(|t| t.text)
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                CompilerSpan {
                                    file_name: s.file_name,
                                    line_start: s.line_start,
                                    line_end: s.line_end,
                                    column_start: s.column_start,
                                    column_end: s.column_end,
                                    text: text_joined,
                                }
                            })
                            .collect();

                        raw_diagnostics.push(CompilerDiagnostic {
                            code: msg.code.map(|c| c.code),
                            message: msg.message,
                            spans,
                            level: msg.level,
                        });
                    }
                }
            }
        }

        // Prioritize and filter diagnostics
        let prioritized_errors = prioritize_diagnostics(raw_diagnostics, active_file);
        let has_errors = !prioritized_errors.is_empty();

        // Check for infrastructure/network errors
        let is_infra = determine_is_infra_error(output.status, &stderr_str, has_errors);

        // Asynchronously check and prune cache if target size > 5GB
        check_and_prune_cache(project_root);

        Ok(CompileOutput {
            success: !has_errors && !is_infra,
            diagnostics: prioritized_errors,
            is_infra_error: is_infra,
        })
    }
}

/// The Rust adapter's own build-cache hygiene: `cargo check` writes into
/// `target/murshid` (a dedicated target dir, see `run_check`), so pruning
/// that cache when it grows past 5GB is this adapter's job, not generic
/// engine infra.
fn check_and_prune_cache(project_root: &Path) {
    let project_root_clone = project_root.to_path_buf();
    std::thread::spawn(move || {
        let target_dir = project_root_clone.join("target/murshid");
        if target_dir.exists() {
            let size = crate::watcher_coordinator::get_dir_size(&target_dir);
            // 5GB = 5 * 1024 * 1024 * 1024 bytes
            if size > 5 * 1024 * 1024 * 1024 {
                let _ = std::process::Command::new("cargo")
                    .args(["clean", "--target-dir"])
                    .arg(&target_dir)
                    .current_dir(&project_root_clone)
                    .status();
            }
        }
    });
}

/// Namespaces a rustc/clippy diagnostic code per I28 (e.g. `rust/E0425`).
fn namespaced_rule_id(code: &Option<String>) -> String {
    format!(
        "rust/{}",
        code.clone().unwrap_or_else(|| "unknown".to_string())
    )
}

/// SARIF-style finding-fingerprint (I28/C2): identity for this finding
/// across runs, derived from its namespaced rule id and location.
fn record_fingerprint(rule_id: &str, file: &str, range: &crate::pack::FileRange) -> String {
    let raw = format!(
        "{}|{}|{}|{}|{}|{}",
        rule_id, file, range.line_start, range.column_start, range.line_end, range.column_end
    );
    crate::sha256::sha256_hex(raw.as_bytes())
}

/// Maps one native `cargo check` diagnostic to the engine's normalized
/// record shape (I28, amended by C9): `spans` — the Rust-specific detail
/// beyond file/range — is relayed as the opaque `data` payload, unparsed.
fn normalize_diagnostic(diag: &CompilerDiagnostic) -> crate::pack::NormalizedRecord {
    let file = diag
        .spans
        .first()
        .map(|s| s.file_name.clone())
        .unwrap_or_default();
    let range = diag
        .spans
        .first()
        .map(|s| crate::pack::FileRange {
            line_start: s.line_start,
            line_end: s.line_end,
            column_start: s.column_start,
            column_end: s.column_end,
        })
        .unwrap_or(crate::pack::FileRange {
            line_start: 0,
            line_end: 0,
            column_start: 0,
            column_end: 0,
        });
    let rule_id = namespaced_rule_id(&diag.code);
    let fingerprint = record_fingerprint(&rule_id, &file, &range);

    crate::pack::NormalizedRecord {
        rule_id,
        source_tool: "cargo".to_string(),
        tool_level: diag.level.clone(),
        message: diag.message.clone(),
        file,
        range,
        suggested_fix: None,
        doc_ref: None,
        fingerprint,
        data: serde_json::json!({ "spans": diag.spans }),
    }
}

impl crate::pack::DiagnosticsAdapter for CompilerInterceptor {
    fn run_check(
        &self,
        project_root: &Path,
        active_file: &Path,
    ) -> Result<crate::pack::AdapterCheckOutput, String> {
        let out = CompilerInterceptor::run_check(self, project_root, active_file)?;
        Ok(crate::pack::AdapterCheckOutput {
            success: out.success,
            is_infra_error: out.is_infra_error,
            records: out.diagnostics.iter().map(normalize_diagnostic).collect(),
        })
    }
}

#[cfg(unix)]
fn terminate_process(child: &mut Child) {
    let pid = child.id();
    // Send SIGTERM (15)
    unsafe {
        let _ = libc::kill(pid as libc::pid_t, 15);
    }

    // Wait up to 500ms for exit
    let start = std::time::Instant::now();
    while start.elapsed().as_millis() < 500 {
        match child.try_wait() {
            Ok(Some(_)) => return, // Exited
            _ => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }

    // If still running, send SIGKILL (9)
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(not(unix))]
fn terminate_process(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

pub fn prioritize_diagnostics(
    diagnostics: Vec<CompilerDiagnostic>,
    active_file: &Path,
) -> Vec<CompilerDiagnostic> {
    let mut errors: Vec<CompilerDiagnostic> = diagnostics
        .into_iter()
        .filter(|d| d.level == "error")
        .collect();

    let active_file_str = active_file.to_string_lossy();
    errors.sort_by_key(|d| {
        let has_active_file = d.spans.iter().any(|s| {
            Path::new(&s.file_name).ends_with(active_file) || s.file_name == active_file_str
        });
        if has_active_file {
            0 // priority (comes first)
        } else {
            1
        }
    });

    errors.truncate(3);
    errors
}

// JSON deserialization helpers
#[derive(serde::Deserialize)]
struct CargoMessage {
    reason: String,
    message: Option<DiagnosticMessage>,
}

#[derive(serde::Deserialize)]
struct DiagnosticMessage {
    code: Option<DiagnosticCode>,
    message: String,
    level: String,
    spans: Vec<DiagnosticSpan>,
}

#[derive(serde::Deserialize)]
struct DiagnosticCode {
    code: String,
}

#[derive(serde::Deserialize)]
struct DiagnosticSpan {
    file_name: String,
    line_start: usize,
    line_end: usize,
    column_start: usize,
    column_end: usize,
    text: Vec<TextSpan>,
}

#[derive(serde::Deserialize)]
struct TextSpan {
    text: String,
}

#[cfg(test)]
pub fn parse_cargo_line(line: &str) -> Option<CompilerDiagnostic> {
    if let Ok(cargo_msg) = serde_json::from_str::<CargoMessage>(line) {
        if cargo_msg.reason == "compiler-message" {
            if let Some(msg) = cargo_msg.message {
                let spans = msg
                    .spans
                    .into_iter()
                    .map(|s| {
                        let text_joined = s
                            .text
                            .into_iter()
                            .map(|t| t.text)
                            .collect::<Vec<_>>()
                            .join("\n");
                        CompilerSpan {
                            file_name: s.file_name,
                            line_start: s.line_start,
                            line_end: s.line_end,
                            column_start: s.column_start,
                            column_end: s.column_end,
                            text: text_joined,
                        }
                    })
                    .collect();

                return Some(CompilerDiagnostic {
                    code: msg.code.map(|c| c.code),
                    message: msg.message,
                    spans,
                    level: msg.level,
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_diagnostics_prioritization() {
        let active_file = Path::new("src/main.rs");

        let diag1 = CompilerDiagnostic {
            code: Some("E0001".to_string()),
            message: "Some error in another file".to_string(),
            spans: vec![CompilerSpan {
                file_name: "src/lib.rs".to_string(),
                line_start: 1,
                line_end: 1,
                column_start: 1,
                column_end: 2,
                text: "fn foo() {}".to_string(),
            }],
            level: "error".to_string(),
        };

        let diag2 = CompilerDiagnostic {
            code: Some("E0002".to_string()),
            message: "Some error in active file".to_string(),
            spans: vec![CompilerSpan {
                file_name: "src/main.rs".to_string(),
                line_start: 5,
                line_end: 5,
                column_start: 10,
                column_end: 12,
                text: "let x = y;".to_string(),
            }],
            level: "error".to_string(),
        };

        let diag3 = CompilerDiagnostic {
            code: Some("W0001".to_string()),
            message: "A warning in active file".to_string(),
            spans: vec![CompilerSpan {
                file_name: "src/main.rs".to_string(),
                line_start: 10,
                line_end: 10,
                column_start: 1,
                column_end: 2,
                text: "warning content".to_string(),
            }],
            level: "warning".to_string(),
        };

        let diag4 = CompilerDiagnostic {
            code: Some("E0004".to_string()),
            message: "Another error".to_string(),
            spans: vec![CompilerSpan {
                file_name: "src/main.rs".to_string(),
                line_start: 2,
                line_end: 2,
                column_start: 1,
                column_end: 2,
                text: "err".to_string(),
            }],
            level: "error".to_string(),
        };

        let diag5 = CompilerDiagnostic {
            code: Some("E0005".to_string()),
            message: "Yet another error".to_string(),
            spans: vec![CompilerSpan {
                file_name: "src/lib.rs".to_string(),
                line_start: 10,
                line_end: 10,
                column_start: 1,
                column_end: 2,
                text: "err".to_string(),
            }],
            level: "error".to_string(),
        };

        let result = prioritize_diagnostics(
            vec![diag1.clone(), diag2.clone(), diag3, diag4.clone(), diag5],
            active_file,
        );

        assert_eq!(result.len(), 3);
        assert!(result[0] == diag2 || result[0] == diag4);
        assert!(result[1] == diag2 || result[1] == diag4);
        assert_eq!(result[2], diag1);
    }

    #[test]
    fn test_cargo_json_line_parsing() {
        let json_line = r#"{"reason":"compiler-message","message":{"code":{"code":"E0425"},"level":"error","message":"cannot find value `x` in this scope","spans":[{"file_name":"src/main.rs","line_start":7,"line_end":7,"column_start":5,"column_end":6,"text":[{"text":"    x = 5;"}]}]}}"#;

        let parsed = parse_cargo_line(json_line).unwrap();
        assert_eq!(parsed.code.as_deref(), Some("E0425"));
        assert_eq!(parsed.level, "error");
        assert_eq!(parsed.message, "cannot find value `x` in this scope");
        assert_eq!(parsed.spans.len(), 1);
        assert_eq!(parsed.spans[0].file_name, "src/main.rs");
        assert_eq!(parsed.spans[0].line_start, 7);
        assert_eq!(parsed.spans[0].text, "    x = 5;");
    }

    #[test]
    fn test_compiler_run_check_and_cancel() {
        let temp_dir = std::env::temp_dir();
        let project_dir = temp_dir.join("test_murshid_cargo_check_project");
        let _ = fs::remove_dir_all(&project_dir);
        fs::create_dir_all(&project_dir).unwrap();

        let init_status = Command::new("cargo")
            .arg("init")
            .arg("--bin")
            .current_dir(&project_dir)
            .status()
            .unwrap();
        assert!(init_status.success());

        let interceptor = CompilerInterceptor::new();
        let active_file = project_dir.join("src/main.rs");

        let output1 = interceptor.run_check(&project_dir, &active_file).unwrap();
        assert!(output1.success);
        assert!(output1.diagnostics.is_empty());
        assert!(!output1.is_infra_error);

        fs::write(&active_file, "fn main() { let x: i32 = \"not an int\"; }").unwrap();

        let output2 = interceptor.run_check(&project_dir, &active_file).unwrap();
        assert!(!output2.success);
        assert!(!output2.diagnostics.is_empty());
        assert_eq!(output2.diagnostics[0].level, "error");
        assert!(
            output2.diagnostics[0].spans[0]
                .file_name
                .ends_with("src/main.rs")
        );
        assert!(!output2.is_infra_error);

        let _ = fs::remove_dir_all(&project_dir);
    }

    #[test]
    fn test_compile_lock_timeout() {
        let temp_dir = std::env::temp_dir();
        let project_dir = temp_dir.join("test_murshid_lock_timeout");
        let _ = fs::remove_dir_all(&project_dir);
        fs::create_dir_all(&project_dir).unwrap();

        // Hold the lock in the main thread manually
        let _guard = acquire_project_lock(&project_dir, 100).unwrap();

        let interceptor = CompilerInterceptor::new();
        let active_file = project_dir.join("src/main.rs");

        // Now run_check should time out since the lock is held
        let start = std::time::Instant::now();
        let result = interceptor.run_check(&project_dir, &active_file).unwrap();
        let duration = start.elapsed();

        assert!(!result.success);
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].code.as_deref(), Some("TIMEOUT"));
        assert!(result.is_infra_error);

        // Timeout should take ~3000ms
        assert!(duration.as_millis() >= 3000);

        let _ = fs::remove_dir_all(&project_dir);
    }

    #[test]
    fn test_infra_error_isolation() {
        let status = if cfg!(unix) {
            std::process::Command::new("false").status().unwrap()
        } else {
            std::process::Command::new("cmd")
                .args(["/C", "exit 1"])
                .status()
                .unwrap()
        };

        // Case 1: Compilation errors parsed -> NOT infra error
        assert!(!determine_is_infra_error(
            status,
            "compilation failed",
            true
        ));

        // Case 2: No compilation errors, general command failure -> infra error
        assert!(determine_is_infra_error(
            status,
            "some raw exit error",
            false
        ));

        // Case 3: Specific infra patterns in stderr -> infra error
        assert!(determine_is_infra_error(
            status,
            "blocking waiting for file lock on package...",
            true
        ));
        assert!(determine_is_infra_error(
            status,
            "failed to resolve dependency",
            true
        ));
        assert!(determine_is_infra_error(status, "connection timeout", true));
    }
}

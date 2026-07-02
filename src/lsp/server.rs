use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

pub struct WorkspaceContext {
    pub workspace_root: PathBuf,
    pub build_lock: Arc<Mutex<()>>,
    pub dialogue_history: Arc<Mutex<Vec<String>>>,
    pub diagnostics_manager: Arc<crate::lsp_diagnostics::DiagnosticsManager>,
    pub client_streams: Arc<Mutex<Vec<std::os::unix::net::UnixStream>>>,
    pub watcher: Arc<Mutex<Option<crate::watcher::MurshidWatcher>>>,
}

static WORKSPACES: OnceLock<Mutex<HashMap<PathBuf, Arc<WorkspaceContext>>>> = OnceLock::new();

pub fn get_or_create_workspace(root: PathBuf) -> Arc<WorkspaceContext> {
    let map_lock = WORKSPACES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = map_lock.lock().unwrap();
    map.entry(root.clone())
        .or_insert_with(|| {
            Arc::new(WorkspaceContext {
                workspace_root: root,
                build_lock: Arc::new(Mutex::new(())),
                dialogue_history: Arc::new(Mutex::new(Vec::new())),
                diagnostics_manager: Arc::new(crate::lsp_diagnostics::DiagnosticsManager::new()),
                client_streams: Arc::new(Mutex::new(Vec::new())),
                watcher: Arc::new(Mutex::new(None)),
            })
        })
        .clone()
}

pub fn broadcast_json_rpc(context: &WorkspaceContext, value: serde_json::Value) {
    let mut streams = context.client_streams.lock().unwrap();
    let body = value.to_string();
    let msg = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
    let msg_bytes = msg.as_bytes();

    let mut to_remove = Vec::new();
    for (idx, stream) in streams.iter_mut().enumerate() {
        if stream.write_all(msg_bytes).is_err() || stream.flush().is_err() {
            to_remove.push(idx);
        }
    }
    for idx in to_remove.into_iter().rev() {
        streams.remove(idx);
    }
}

pub fn write_json_rpc(writer: &mut dyn Write, value: serde_json::Value) -> Result<(), std::io::Error> {
    let body = value.to_string();
    let msg = format!("Content-Length: {}\r\n\r\n{}", body.len(), body);
    writer.write_all(msg.as_bytes())?;
    writer.flush()?;
    Ok(())
}

pub fn run_socratic_compile_flow(
    context: &Arc<WorkspaceContext>,
    project_root: &Path,
    active_file: &Path,
) -> Result<(), String> {
    let file_uri = format!(
        "file://{}",
        active_file
            .canonicalize()
            .unwrap_or_else(|_| active_file.to_path_buf())
            .display()
    );

    // Clear previous diagnostics for this file
    context.diagnostics_manager.set_diagnostics(file_uri.clone(), vec![]);

    // 1. Check file size and line count limits using centralized coordinator check
    if crate::watcher_coordinator::should_skip_file(active_file) {
        let diag = crate::lsp_diagnostics::create_special_diagnostic(
            "Socratic assistance suspended: file exceeds maximum line/size thresholds.",
            3, // Information
            "size-limit"
        );
        context.diagnostics_manager.set_diagnostics(file_uri.clone(), vec![diag]);

        let diags = context.diagnostics_manager.get_diagnostics(&file_uri);
        let payload = crate::lsp_diagnostics::format_publish_diagnostics(&file_uri, &diags);
        broadcast_json_rpc(context, payload);
        return Ok(());
    }

    // 2. Execute compiler check
    let interceptor = crate::compiler::CompilerInterceptor::new();
    let compile_output = interceptor
        .run_check(project_root, active_file)
        .map_err(|e| format!("Compiler check failed: {}", e))?;

    // 3. Handle infrastructure/toolchain errors
    if compile_output.is_infra_error {
        let message = if let Some(first_diag) = compile_output.diagnostics.first() {
            &first_diag.message
        } else {
            "Compilation check suspended due to local toolchain environment errors."
        };

        let diag = crate::lsp_diagnostics::create_special_diagnostic(
            message,
            3, // Information
            "infra-error"
        );
        context.diagnostics_manager.set_diagnostics(file_uri.clone(), vec![diag]);

        let diags = context.diagnostics_manager.get_diagnostics(&file_uri);
        let payload = crate::lsp_diagnostics::format_publish_diagnostics(&file_uri, &diags);
        broadcast_json_rpc(context, payload);
        return Ok(());
    }

    // 4. Handle clean compilation (reset/clear diagnostics)
    if compile_output.success {
        context.diagnostics_manager.set_diagnostics(file_uri.clone(), vec![]);
        let payload = crate::lsp_diagnostics::format_publish_diagnostics(&file_uri, &[]);
        broadcast_json_rpc(context, payload);
        return Ok(());
    }

    // 5. Compile check failed - Socratic Orchestration Loop
    let mut lsp_diagnostics = Vec::new();
    let config = crate::config::load_config();

    // Check watcher polling fallback to append polling warning overlay
    let is_polling = {
        let guard = context.watcher.lock().unwrap();
        guard
            .as_ref()
            .map(|w| w.mode == crate::watcher_coordinator::WatchMode::Polling)
            .unwrap_or(false)
    };
    if is_polling {
        let polling_diag = crate::lsp_diagnostics::create_special_diagnostic(
            "Watcher resources exhausted. Downgrading workspace to background polling loop.",
            3, // Information
            "polling-fallback"
        );
        lsp_diagnostics.push(polling_diag);
    }

    for diag in compile_output.diagnostics {
        let code = diag.code.clone().unwrap_or_else(|| "unknown".to_string());

        let mut line_num = 1;
        let mut start_char = 0;
        let mut end_char = 10;

        let active_span = diag.spans.iter()
            .find(|s| {
                let p = std::path::Path::new(&s.file_name);
                p == active_file || p.ends_with(active_file)
            })
            .or_else(|| {
                diag.spans.iter().find(|s| {
                    let p = std::path::Path::new(&s.file_name);
                    p.starts_with(project_root)
                })
            })
            .or(diag.spans.first());

        if let Some(span) = active_span {
            line_num = span.line_start;
            start_char = span.column_start as u32;
            end_char = span.column_end as u32;
        }

        // Pedagogy DB operations
        if let Some(db_path) = crate::db::get_db_path() {
            if let Ok(conn) = crate::db::open_connection(&db_path) {
                let ws_hash = project_root.to_string_lossy().to_string();
                let _ = crate::pedagogy::handle_compile_check_event(
                    &conn,
                    &ws_hash,
                    active_file,
                    &code,
                    false
                );
            }
        }

        // Contact LLM API Provider for Socratic guidance
        let api_keys = crate::credentials::get_api_keys();
        let provider_type = if std::env::var("GEMINI_API_KEY").is_ok()
            || std::env::var("MURSHID_GEMINI_API_KEY").is_ok()
        {
            "gemini"
        } else if std::env::var("ANTHROPIC_API_KEY").is_ok()
            || api_keys.as_ref().and_then(|k| k.claude_api_key.as_ref()).is_some()
        {
            "claude"
        } else {
            "gemini"
        };
        let api_key = if provider_type == "claude" {
            api_keys.as_ref().and_then(|k| k.claude_api_key.clone())
        } else {
            api_keys.as_ref().and_then(|k| k.gemini_api_key.clone())
        };

        let hint_text = if let Some(ref key) = api_key {
            let exclude_patterns = config.watcher.exclude.clone();
            if let Ok(context_payload) = crate::context::generate_context_payload(
                project_root,
                active_file,
                line_num,
                &diag.message,
                &exclude_patterns
            ) {
                match crate::provider::dispatch_debounced(provider_type, &context_payload, Some(key)) {
                    Ok(response) => {
                        let redacted = crate::redactor::redact_markdown_blocks(&response, &[]);
                        crate::redactor::redact_clippy_fixes(&redacted)
                    }
                    Err(e) => {
                        if e.contains("401")
                            || e.contains("403")
                            || e.contains("429")
                            || e.contains("API key not valid")
                            || e.contains("unauthorized")
                        {
                            let warning_diag = crate::lsp_diagnostics::create_special_diagnostic(
                                "Socratic check suspended: invalid API key or connection unauthorized. Verify your credential configuration.",
                                2, // Warning
                                "api-key-error"
                            );
                            lsp_diagnostics.push(warning_diag);
                            break;
                        }
                        format!("Socratic Mentor hint generation failed: {}", e)
                    }
                }
            } else {
                diag.message.clone()
            }
        } else {
            "(Note: Set GEMINI_API_KEY or ANTHROPIC_API_KEY to see Socratic AI mentor guidance)".to_string()
        };

        if lsp_diagnostics.iter().any(|d| d.code.as_deref() == Some("api-key-error")) {
            break;
        }

        let socratic_msg = crate::lsp_code_actions::format_socratic_guidance_fallback(&diag.message, &hint_text);

        let socratic_diag = crate::lsp_diagnostics::Diagnostic {
            range: crate::lsp_diagnostics::Range {
                start: crate::lsp_diagnostics::Position {
                    line: if line_num > 0 { (line_num - 1) as u32 } else { 0 },
                    character: start_char,
                },
                end: crate::lsp_diagnostics::Position {
                    line: if line_num > 0 { (line_num - 1) as u32 } else { 0 },
                    character: end_char,
                },
            },
            severity: Some(4), // Hint
            code: Some(code),
            source: Some("murshid".to_string()),
            message: socratic_msg,
            tags: Some(vec![1]), // Unnecessary / Fade tag
        };

        lsp_diagnostics.push(socratic_diag);
    }

    context.diagnostics_manager.set_diagnostics(file_uri.clone(), lsp_diagnostics);

    let diags = context.diagnostics_manager.get_diagnostics(&file_uri);
    let payload = crate::lsp_diagnostics::format_publish_diagnostics(&file_uri, &diags);
    broadcast_json_rpc(context, payload);

    Ok(())
}

fn handle_lsp_json_rpc(
    context: Arc<WorkspaceContext>,
    msg: serde_json::Value,
    writer: &mut std::os::unix::net::UnixStream,
) -> Result<(), std::io::Error> {
    let method = msg.get("method").and_then(|m| m.as_str());
    let id = msg.get("id");

    if let Some("initialize") = method {
        let resp = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "capabilities": {
                    "textDocumentSync": 1, // Full
                    "codeActionProvider": true,
                    "executeCommandProvider": {
                        "commands": ["murshid.openChatPanel"]
                    }
                }
            }
        });
        write_json_rpc(writer, resp)?;
    } else if let Some("textDocument/didChange") = method {
        if let Some(params) = msg.get("params") {
            if let Some(uri) = params.get("textDocument").and_then(|d| d.get("uri")).and_then(|u| u.as_str()) {
                if let Some(changes) = params.get("contentChanges").and_then(|c| c.as_array()) {
                    if let Some(updated_diags) = context.diagnostics_manager.handle_did_change(uri, changes) {
                        let payload = crate::lsp_diagnostics::format_publish_diagnostics(uri, &updated_diags);
                        broadcast_json_rpc(&context, payload);
                    }
                }
            }
        }
    } else if let Some("textDocument/codeAction") = method {
        if let Some(params) = msg.get("params") {
            let uri_opt = params.get("textDocument").and_then(|d| d.get("uri")).and_then(|u| u.as_str());
            let range_opt = params.get("range").and_then(|r| serde_json::from_value::<crate::lsp_diagnostics::Range>(r.clone()).ok());
            let diags_opt = params.get("context").and_then(|c| c.get("diagnostics")).and_then(|d| d.as_array());

            if let (Some(uri), Some(range), Some(diags_val)) = (uri_opt, range_opt, diags_opt) {
                let mut diagnostics = Vec::new();
                for d_val in diags_val {
                    if let Ok(d) = serde_json::from_value::<crate::lsp_diagnostics::Diagnostic>(d_val.clone()) {
                        diagnostics.push(d);
                    }
                }

                let actions = crate::lsp_code_actions::handle_code_actions_request(uri, &range, &diagnostics);
                let resp = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": actions
                });
                write_json_rpc(writer, resp)?;
            }
        }
    } else if let Some("workspace/executeCommand") = method {
        let resp = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": null
        });
        write_json_rpc(writer, resp)?;
    } else if let Some("shutdown") = method {
        let resp = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": null
        });
        write_json_rpc(writer, resp)?;
    } else if id.is_some() {
        let resp = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": null
        });
        write_json_rpc(writer, resp)?;
    }

    Ok(())
}

/// Binds to a UDS socket. If a stale socket file is present, tests connection and unlinks it.
pub fn bind_uds_socket(path: &Path) -> Result<std::os::unix::net::UnixListener, String> {
    if path.exists() {
        // Test if UDS socket is active
        match std::os::unix::net::UnixStream::connect(path) {
            Ok(_) => {
                return Err("Another daemon instance is already active and listening on the socket".to_string());
            }
            Err(_) => {
                // Connection failed: UDS is stale and can be safely unlinked
                fs::remove_file(path)
                    .map_err(|e| format!("Failed to remove stale UDS socket file: {}", e))?;
            }
        }
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
        }
    }

    let listener = std::os::unix::net::UnixListener::bind(path)
        .map_err(|e| format!("Failed to bind UDS listener: {}", e))?;

    Ok(listener)
}

pub fn handle_connection(stream: std::os::unix::net::UnixStream) -> Result<(), String> {
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|e| format!("Failed to read handshake: {}", e))?;

    let handshake: serde_json::Value = serde_json::from_str(&line)
        .map_err(|e| format!("Invalid handshake JSON: {}", e))?;

    let root_str = handshake.get("workspace_root")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing workspace_root in handshake".to_string())?;
    let _client_pid = handshake.get("client_pid")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "Missing client_pid in handshake".to_string())?;

    let workspace_root = PathBuf::from(root_str);
    let context = get_or_create_workspace(workspace_root.clone());

    // Spawn watcher for this workspace if not already running
    {
        let mut watcher_guard = context.watcher.lock().unwrap();
        if watcher_guard.is_none() {
            let context_clone = context.clone();
            let root_clone = workspace_root.clone();

            let callback = move |path: PathBuf| {
                let _ = run_socratic_compile_flow(&context_clone, &root_clone, &path);
            };

            if let Ok(w) = crate::watcher::start_watching(workspace_root, callback) {
                *watcher_guard = Some(w);
            }
        }
    }

    // Add current stream to the client_streams list
    {
        let mut streams = context.client_streams.lock().unwrap();
        if let Ok(cloned) = stream.try_clone() {
            streams.push(cloned);
        }
    }

    // Simple interaction loop to process messages and verify context isolation
    let mut writer = stream;
    loop {
        let mut msg_line = String::new();
        match reader.read_line(&mut msg_line) {
            Ok(0) => break, // EOF
            Ok(_) => {
                let trimmed = msg_line.trim();
                if trimmed.is_empty() {
                    continue;
                }

                if trimmed.starts_with("Content-Length:") {
                    let len_str = trimmed["Content-Length:".len()..].trim();
                    let content_len: usize = match len_str.parse() {
                        Ok(l) => l,
                        Err(_) => continue,
                    };

                    let mut has_err = false;
                    loop {
                        let mut hdr_line = String::new();
                        if reader.read_line(&mut hdr_line).is_err() {
                            has_err = true;
                            break;
                        }
                        if hdr_line == "\r\n" || hdr_line == "\n" {
                            break;
                        }
                    }
                    if has_err {
                        break;
                    }

                    let mut body_buf = vec![0u8; content_len];
                    if reader.read_exact(&mut body_buf).is_err() {
                        break;
                    }

                    let body_str = match String::from_utf8(body_buf) {
                        Ok(s) => s,
                        Err(_) => continue,
                    };

                    if let Ok(msg) = serde_json::from_str::<serde_json::Value>(&body_str) {
                        if handle_lsp_json_rpc(context.clone(), msg, &mut writer).is_err() {
                            break;
                        }
                    }
                } else {
                    if trimmed.starts_with("add:") {
                        let content = trimmed[4..].to_string();
                        let mut hist = context.dialogue_history.lock().unwrap();
                        hist.push(content);
                        if hist.len() > 3 {
                            hist.remove(0); // keep sliding window of last 3 turns
                        }
                        if writer.write_all(b"added\n").is_err() || writer.flush().is_err() {
                            break;
                        }
                    } else if trimmed.starts_with("watch_path:") {
                        let path_str = trimmed[11..].to_string();
                        let path = PathBuf::from(path_str);
                        crate::watcher::register_watch_path(path);
                        if writer.write_all(b"watched\n").is_err() || writer.flush().is_err() {
                            break;
                        }
                    } else if trimmed == "history" {
                        let hist = context.dialogue_history.lock().unwrap();
                        let resp = format!("{}\n", hist.join(","));
                        if writer.write_all(resp.as_bytes()).is_err() || writer.flush().is_err() {
                            break;
                        }
                    } else {
                        let resp = format!("echo:{}\n", trimmed);
                        if writer.write_all(resp.as_bytes()).is_err() || writer.flush().is_err() {
                            break;
                        }
                    }
                }
            }
            Err(_) => break,
        }
    }

    Ok(())
}

pub fn start_lsp_server(listener: std::os::unix::net::UnixListener) {
    thread::spawn(move || {
        for stream_res in listener.incoming() {
            if let Ok(stream) = stream_res {
                thread::spawn(move || {
                    let _ = handle_connection(stream);
                });
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::net::UnixStream;

    #[test]
    fn test_stale_uds_socket_unlink() {
        let temp_dir = std::env::temp_dir().join(format!("murshid_stale_uds_{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();

        let socket_path = temp_dir.join("lsp.sock");

        // 1. Initial bind should work
        let listener = bind_uds_socket(&socket_path).unwrap();

        // 2. Binding again to active socket should fail
        let listener_fail = bind_uds_socket(&socket_path);
        assert!(listener_fail.is_err());
        assert!(listener_fail.err().unwrap().contains("Another daemon instance"));

        // 3. Drop first listener to make it stale (file still exists but nothing listens).
        // Drain the backlog first: the liveness probe in step 2 left a queued,
        // never-accepted connection, and a connect() racing against a closed-but-
        // undrained listener can transiently succeed, masking staleness.
        listener.set_nonblocking(true).unwrap();
        while listener.accept().is_ok() {}
        drop(listener);
        assert!(socket_path.exists());

        // 4. Binding to stale socket should succeed (tests stale connection and unlinks).
        // Retry briefly: concurrent tests spawn children (cargo check, curl) that can
        // inherit this listener's FD across a fork racing the CLOEXEC flag, keeping the
        // socket alive until the child exits — the liveness probe then sees a live socket.
        let mut listener_stale_success = bind_uds_socket(&socket_path);
        for _ in 0..50 {
            if listener_stale_success.is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
            listener_stale_success = bind_uds_socket(&socket_path);
        }
        assert!(
            listener_stale_success.is_ok(),
            "Expected stale socket unlink and successful rebinding, got: {:?}",
            listener_stale_success.err()
        );

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_workspace_multiplexing_isolation() {
        let temp_dir = std::env::temp_dir().join(format!("murshid_lsp_multiplex_{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();

        let socket_path = temp_dir.join("lsp.sock");
        let listener = bind_uds_socket(&socket_path).unwrap();

        start_lsp_server(listener);

        // Client 1 connects to /workspace/alpha
        let mut client1 = UnixStream::connect(&socket_path).unwrap();
        let handshake1 = serde_json::json!({
            "workspace_root": "/workspace/alpha",
            "client_pid": 1001,
        });
        client1.write_all(format!("{}\n", handshake1).as_bytes()).unwrap();
        client1.flush().unwrap();

        // Client 2 connects to /workspace/beta
        let mut client2 = UnixStream::connect(&socket_path).unwrap();
        let handshake2 = serde_json::json!({
            "workspace_root": "/workspace/beta",
            "client_pid": 1002,
        });
        client2.write_all(format!("{}\n", handshake2).as_bytes()).unwrap();
        client2.flush().unwrap();

        // Client 1 adds a dialogue log
        client1.write_all(b"add:ownership_mistake\n").unwrap();
        client1.flush().unwrap();

        let mut reader1 = BufReader::new(client1.try_clone().unwrap());
        let mut resp1 = String::new();
        reader1.read_line(&mut resp1).unwrap();
        assert_eq!(resp1.trim(), "added");

        // Client 2 adds a different dialogue log
        client2.write_all(b"add:lifetimes_confusion\n").unwrap();
        client2.flush().unwrap();

        let mut reader2 = BufReader::new(client2.try_clone().unwrap());
        let mut resp2 = String::new();
        reader2.read_line(&mut resp2).unwrap();
        assert_eq!(resp2.trim(), "added");

        // Verify history isolation
        // Client 1 history query
        client1.write_all(b"history\n").unwrap();
        client1.flush().unwrap();
        let mut history1 = String::new();
        reader1.read_line(&mut history1).unwrap();
        assert_eq!(history1.trim(), "ownership_mistake");

        // Client 2 history query
        client2.write_all(b"history\n").unwrap();
        client2.flush().unwrap();
        let mut history2 = String::new();
        reader2.read_line(&mut history2).unwrap();
        assert_eq!(history2.trim(), "lifetimes_confusion");

        // Cleanup
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_json_rpc_flow() {
        let temp_dir = std::env::temp_dir().join("murshid_json_rpc_flow");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();

        let socket_path = temp_dir.join("lsp.sock");
        let listener = bind_uds_socket(&socket_path).unwrap();

        start_lsp_server(listener);

        // Connect client
        let mut client = UnixStream::connect(&socket_path).unwrap();
        let handshake = serde_json::json!({
            "workspace_root": temp_dir.to_string_lossy().to_string(),
            "client_pid": 2001,
        });
        client.write_all(format!("{}\n", handshake).as_bytes()).unwrap();
        client.flush().unwrap();

        // Send initialize request in JSON-RPC format
        let init_req = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        });
        let init_req_str = init_req.to_string();
        let msg = format!("Content-Length: {}\r\n\r\n{}", init_req_str.len(), init_req_str);
        client.write_all(msg.as_bytes()).unwrap();
        client.flush().unwrap();

        // Read response
        let mut reader = BufReader::new(client.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(line.starts_with("Content-Length:"));

        // Read empty line
        let mut empty = String::new();
        reader.read_line(&mut empty).unwrap();

        // Read body
        let len: usize = line["Content-Length:".len()..].trim().parse().unwrap();
        let mut body_buf = vec![0u8; len];
        reader.read_exact(&mut body_buf).unwrap();
        let body_str = String::from_utf8(body_buf).unwrap();
        let resp: serde_json::Value = serde_json::from_str(&body_str).unwrap();

        assert_eq!(resp["id"].as_u64(), Some(1));
        assert!(resp["result"]["capabilities"]["codeActionProvider"].as_bool().unwrap());

        // Cleanup
        let _ = fs::remove_dir_all(&temp_dir);
    }
}

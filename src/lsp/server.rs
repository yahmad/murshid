use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

pub struct WorkspaceContext {
    pub workspace_root: PathBuf,
    pub build_lock: Arc<Mutex<()>>,
    pub dialogue_history: Arc<Mutex<Vec<String>>>,
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
            })
        })
        .clone()
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

fn handle_connection(stream: std::os::unix::net::UnixStream) -> Result<(), String> {
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
    let context = get_or_create_workspace(workspace_root);

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

                if trimmed.starts_with("add:") {
                    let content = trimmed[4..].to_string();
                    let mut hist = context.dialogue_history.lock().unwrap();
                    hist.push(content);
                    if hist.len() > 3 {
                        hist.remove(0); // keep sliding window of last 3 turns
                    }
                    writer.write_all(b"added\n").map_err(|e| e.to_string())?;
                } else if trimmed.starts_with("watch_path:") {
                    let path_str = trimmed[11..].to_string();
                    let path = PathBuf::from(path_str);
                    crate::watcher::register_watch_path(path);
                    writer.write_all(b"watched\n").map_err(|e| e.to_string())?;
                } else if trimmed == "history" {
                    let hist = context.dialogue_history.lock().unwrap();
                    let resp = format!("{}\n", hist.join(","));
                    writer.write_all(resp.as_bytes()).map_err(|e| e.to_string())?;
                } else {
                    let resp = format!("echo:{}\n", trimmed);
                    writer.write_all(resp.as_bytes()).map_err(|e| e.to_string())?;
                }
                let _ = writer.flush();
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
        let temp_dir = std::env::temp_dir().join("murshid_stale_uds");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();

        let socket_path = temp_dir.join("lsp.sock");

        // 1. Initial bind should work
        let listener = bind_uds_socket(&socket_path).unwrap();

        // 2. Binding again to active socket should fail
        let listener_fail = bind_uds_socket(&socket_path);
        assert!(listener_fail.is_err());
        assert!(listener_fail.err().unwrap().contains("Another daemon instance"));

        // 3. Drop first listener to make it stale (file still exists but nothing listens)
        drop(listener);
        assert!(socket_path.exists());

        // 4. Binding to stale socket should succeed (tests stale connection and unlinks)
        let listener_stale_success = bind_uds_socket(&socket_path);
        assert!(listener_stale_success.is_ok(), "Expected stale socket unlink and successful rebinding");

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_workspace_multiplexing_isolation() {
        let temp_dir = std::env::temp_dir().join("murshid_lsp_multiplex");
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

        // Client 1 adds a dialog log
        client1.write_all(b"add:ownership_mistake\n").unwrap();
        client1.flush().unwrap();

        let mut reader1 = BufReader::new(client1.try_clone().unwrap());
        let mut resp1 = String::new();
        reader1.read_line(&mut resp1).unwrap();
        assert_eq!(resp1.trim(), "added");

        // Client 2 adds a different dialog log
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
}

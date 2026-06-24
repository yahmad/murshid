use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

fn generate_token() -> String {
    #[cfg(unix)]
    if let Ok(mut file) = fs::File::open("/dev/urandom") {
        let mut bytes = [0u8; 16];
        if file.read_exact(&mut bytes).is_ok() {
            return bytes.iter().map(|b| format!("{:02x}", b)).collect();
        }
    }

    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let mut token = String::new();
    for _ in 0..32 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let val = (seed >> 32) as u32;
        token.push_str(&format!("{:x}", val % 16));
    }
    token
}

fn write_session_file(project_root: &Path, port: u16, token: &str) -> Result<(), String> {
    let murshid_dir = project_root.join(".murshid");
    fs::create_dir_all(&murshid_dir).map_err(|e| e.to_string())?;

    let path = murshid_dir.join("container_session.json");
    let json = serde_json::json!({
        "port": port,
        "token": token
    });

    fs::write(&path, json.to_string()).map_err(|e| e.to_string())?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("Failed to set session file permissions to 0600: {}", e))?;
    }
    Ok(())
}

fn find_container_session_json() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let p = dir.join(".murshid/container_session.json");
        if p.exists() {
            return Some(p);
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

/// Tries to read the container session file and establish connection to the host bridge.
pub fn try_connect_container_bridge() -> Option<TcpStream> {
    let session_path = find_container_session_json()?;
    let content = fs::read_to_string(&session_path).ok()?;
    let val: serde_json::Value = serde_json::from_str(&content).ok()?;
    let port = val.get("port")?.as_u64()? as u16;
    let token = val.get("token")?.as_str()?;

    let mut stream = TcpStream::connect(format!("127.0.0.1:{}", port)).ok()?;
    stream.set_read_timeout(Some(Duration::from_millis(1000))).ok()?;
    stream.set_write_timeout(Some(Duration::from_millis(1000))).ok()?;

    stream.write_all(format!("{}\n", token).as_bytes()).ok()?;
    stream.flush().ok()?;

    let mut reader = BufReader::new(&stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    if line.trim() == "authorized" {
        Some(stream)
    } else {
        None
    }
}

pub struct TcpBridgeServer {
    pub listener: TcpListener,
    pub token: String,
    pub token_valid: Arc<Mutex<bool>>,
}

impl TcpBridgeServer {
    pub fn start(project_root: &Path, uds_path: PathBuf) -> Result<Self, String> {
        let mut listener_opt = None;
        let mut selected_port = 0;
        for port in 8488..=8500 {
            if let Ok(l) = TcpListener::bind(format!("127.0.0.1:{}", port)) {
                listener_opt = Some(l);
                selected_port = port;
                break;
            }
        }

        let listener = listener_opt.ok_or_else(|| "No available ports in range 8488-8500".to_string())?;
        let token = generate_token();

        write_session_file(project_root, selected_port, &token)?;

        let token_valid = Arc::new(Mutex::new(true));
        let token_valid_clone = token_valid.clone();
        let token_clone = token.clone();
        let listener_clone = listener.try_clone().map_err(|e| e.to_string())?;

        thread::spawn(move || {
            for stream_res in listener_clone.incoming() {
                if let Ok(mut client_stream) = stream_res {
                    let token_clone2 = token_clone.clone();
                    let token_valid_clone2 = token_valid_clone.clone();
                    let uds_path_clone2 = uds_path.clone();

                    thread::spawn(move || {
                        let _ = client_stream.set_read_timeout(Some(Duration::from_millis(1000)));
                        let mut reader = BufReader::new(&client_stream);
                        let mut line = String::new();
                        if reader.read_line(&mut line).is_ok() {
                            let received = line.trim();

                            let mut valid = token_valid_clone2.lock().unwrap();
                            if *valid && received == token_clone2 {
                                // Dynamic single-use token consumed
                                *valid = false;
                                drop(valid);

                                if client_stream.write_all(b"authorized\n").is_ok() && client_stream.flush().is_ok() {
                                    if let Ok(uds_stream) = std::os::unix::net::UnixStream::connect(&uds_path_clone2) {
                                        let _ = bridge_streams(client_stream, uds_stream);
                                    }
                                }
                            } else {
                                let _ = client_stream.write_all(b"unauthorized\n");
                            }
                        }
                    });
                }
            }
        });

        Ok(Self {
            listener,
            token,
            token_valid,
        })
    }
}

fn bridge_streams(mut tcp: TcpStream, mut uds: std::os::unix::net::UnixStream) -> Result<(), String> {
    let mut tcp_read = tcp.try_clone().map_err(|e| e.to_string())?;
    let mut uds_read = uds.try_clone().map_err(|e| e.to_string())?;

    let t1 = thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match tcp_read.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if uds.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let mut buf = [0u8; 4096];
    loop {
        match uds_read.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if tcp.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    let _ = t1.join();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_generation_length() {
        let t = generate_token();
        assert_eq!(t.len(), 32);
    }

    #[test]
    fn test_session_file_creation_and_permissions() {
        let temp_dir = std::env::temp_dir().join("murshid_bridge_test");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();

        write_session_file(&temp_dir, 8488, "my-secret-token").unwrap();

        let session_file = temp_dir.join(".murshid/container_session.json");
        assert!(session_file.exists());

        let content = fs::read_to_string(&session_file).unwrap();
        let val: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(val["port"], 8488);
        assert_eq!(val["token"], "my-secret-token");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = fs::metadata(&session_file).unwrap();
            let mode = meta.permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_bridge_authorization_flow() {
        let temp_dir = std::env::temp_dir().join("murshid_bridge_auth");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();

        // 1. Spawn Unix listener (simulating daemon UDS)
        let uds_path = temp_dir.join("lsp.sock");
        let uds_listener = std::os::unix::net::UnixListener::bind(&uds_path).unwrap();
        let uds_handle = thread::spawn(move || {
            if let Ok((mut stream, _)) = uds_listener.accept() {
                let mut buf = [0u8; 10];
                let n = stream.read(&mut buf).unwrap();
                assert_eq!(&buf[..n], b"client_msg");
                stream.write_all(b"server_msg").unwrap();
            }
        });

        // 2. Start TCP Bridge pointing to UDS
        let _bridge = TcpBridgeServer::start(&temp_dir, uds_path).unwrap();

        // 3. Connect client via bridge client logic (mocking find_container_session_json)
        let original_dir = std::env::current_dir().unwrap();
        std::env::set_current_dir(&temp_dir).unwrap();

        let client_opt = try_connect_container_bridge();
        assert!(client_opt.is_some());

        let mut client_stream = client_opt.unwrap();
        client_stream.write_all(b"client_msg").unwrap();
        client_stream.flush().unwrap();

        let mut buf = [0u8; 10];
        let n = client_stream.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"server_msg");

        // 4. Test single-use token invalidation (subsequent connection with same token fails)
        let client_stream2 = try_connect_container_bridge();
        assert!(client_stream2.is_none());

        std::env::set_current_dir(original_dir).unwrap();
        uds_handle.join().unwrap();
        let _ = fs::remove_dir_all(&temp_dir);
    }
}

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

// Windows API declarations for named pipe validation
#[cfg(windows)]
mod win32 {
    pub type HANDLE = *mut std::ffi::c_void;
    pub type BOOL = i32;
    pub type DWORD = u32;
    pub type ULONG = u32;
    pub type PSID = *mut std::ffi::c_void;

    extern "system" {
        pub fn GetCurrentProcess() -> HANDLE;
        pub fn OpenProcess(
            dwDesiredAccess: DWORD,
            bInheritHandle: BOOL,
            dwProcessId: DWORD,
        ) -> HANDLE;
        pub fn OpenProcessToken(
            ProcessHandle: HANDLE,
            DesiredAccess: DWORD,
            TokenHandle: *mut HANDLE,
        ) -> BOOL;
        pub fn GetTokenInformation(
            TokenHandle: HANDLE,
            TokenInformationClass: u32,
            TokenInformation: *mut std::ffi::c_void,
            TokenInformationLength: DWORD,
            ReturnLength: *mut DWORD,
        ) -> BOOL;
        pub fn EqualSid(pSid1: PSID, pSid2: PSID) -> BOOL;
        pub fn CloseHandle(hObject: HANDLE) -> BOOL;
        pub fn GetNamedPipeServerProcessId(
            hNamedPipe: HANDLE,
            ServerProcessId: *mut ULONG,
        ) -> BOOL;
    }
}

/// Resolves the socket path according to spec.
/// Primary: Application Support/murshid/lsp.sock
/// Fallback: /tmp/murshid-<uid>/lsp.sock
pub fn get_socket_path() -> PathBuf {
    if let Some(h) = crate::config::get_home_dir() {
        #[cfg(target_os = "macos")]
        let base = h.join("Library/Application Support/murshid");
        #[cfg(not(target_os = "macos"))]
        let base = h.join(".config/murshid");

        if fs::create_dir_all(&base).is_ok() {
            return base.join("lsp.sock");
        }
    }

    #[cfg(unix)]
    {
        let uid = unsafe { libc::getuid() };
        let base = PathBuf::from(format!("/tmp/murshid-{}", uid));
        let _ = fs::create_dir_all(&base);
        base.join("lsp.sock")
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(".murshid-lsp.sock")
    }
}

/// Verification of Unix Peer credentials to prevent privilege escalation or hijacking.
#[cfg(unix)]
fn verify_peer_credentials(stream: &std::os::unix::net::UnixStream) -> Result<(), String> {
    use std::os::unix::io::AsRawFd;
    let fd = stream.as_raw_fd();
    let current_uid = unsafe { libc::getuid() };

    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
    {
        let mut peer_uid = 0;
        let mut peer_gid = 0;
        if unsafe { libc::getpeereid(fd, &mut peer_uid, &mut peer_gid) } == 0 {
            if peer_uid != current_uid {
                return Err(format!("Peer UID {} does not match current process UID {}", peer_uid, current_uid));
            }
            Ok(())
        } else {
            Err("Failed to get peer credentials via getpeereid".to_string())
        }
    }

    #[cfg(target_os = "linux")]
    {
        let mut ucred = libc::ucred { pid: 0, uid: 0, gid: 0 };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let res = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                &mut ucred as *mut _ as *mut libc::c_void,
                &mut len,
            )
        };
        if res == 0 {
            if ucred.uid != current_uid {
                return Err(format!("Peer UID {} does not match current process UID {}", ucred.uid, current_uid));
            }
            Ok(())
        } else {
            Err("Failed to get peer credentials via SO_PEERCRED".to_string())
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "freebsd", target_os = "linux")))]
    {
        Ok(())
    }
}

#[cfg(windows)]
unsafe fn get_process_owner_sid(process_handle: win32::HANDLE) -> Result<Vec<u8>, String> {
    use win32::*;
    let mut token_handle = std::ptr::null_mut();
    if OpenProcessToken(process_handle, 0x0008, &mut token_handle) == 0 { // TOKEN_QUERY = 0x0008
        return Err("OpenProcessToken failed".to_string());
    }

    let mut req_len = 0;
    // TokenUser = 1
    GetTokenInformation(token_handle, 1, std::ptr::null_mut(), 0, &mut req_len);
    if req_len == 0 {
        CloseHandle(token_handle);
        return Err("GetTokenInformation length query failed".to_string());
    }

    let mut buf = vec![0u8; req_len as usize];
    if GetTokenInformation(token_handle, 1, buf.as_mut_ptr() as *mut _, req_len, &mut req_len) == 0 {
        CloseHandle(token_handle);
        return Err("GetTokenInformation user query failed".to_string());
    }

    CloseHandle(token_handle);
    Ok(buf)
}

/// On Windows, verify that the server side of the named pipe is owned by the current user.
#[cfg(windows)]
fn verify_windows_pipe_server(pipe_file: &std::fs::File) -> Result<(), String> {
    use std::os::windows::io::AsRawHandle;
    use win32::*;

    let pipe_handle = pipe_file.as_raw_handle() as HANDLE;
    unsafe {
        let mut server_pid = 0;
        if GetNamedPipeServerProcessId(pipe_handle, &mut server_pid) == 0 {
            return Err("GetNamedPipeServerProcessId failed".to_string());
        }

        // Open the server process
        // PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
        let server_process = OpenProcess(0x1000, 0, server_pid);
        if server_process.is_null() {
            return Err(format!("OpenProcess failed for PID {}", server_pid));
        }

        let current_process = GetCurrentProcess();

        let client_token_info = match get_process_owner_sid(current_process) {
            Ok(info) => info,
            Err(e) => {
                CloseHandle(server_process);
                return Err(format!("Failed to get client SID: {}", e));
            }
        };

        let server_token_info = match get_process_owner_sid(server_process) {
            Ok(info) => info,
            Err(e) => {
                CloseHandle(server_process);
                return Err(format!("Failed to get server SID: {}", e));
            }
        };

        CloseHandle(server_process);

        let client_sid_ptr = *(client_token_info.as_ptr() as *const PSID);
        let server_sid_ptr = *(server_token_info.as_ptr() as *const PSID);

        if EqualSid(client_sid_ptr, server_sid_ptr) == 0 {
            return Err("Pipe server process owner SID does not match client SID".to_string());
        }
    }
    Ok(())
}

/// Strictly validates target path permissions, owners, and layout to prevent hijacking.
pub fn validate_client_path(path: &Path) -> Result<(), String> {
    let parent = path.parent().ok_or_else(|| "Socket path must have a parent directory".to_string())?;
    
    // Check parent exists and has strictly 0700 permissions
    if !parent.exists() {
        return Err("Parent directory does not exist".to_string());
    }

    let parent_meta = fs::metadata(parent)
        .map_err(|e| format!("Failed to read parent directory metadata: {}", e))?;
    
    if !parent_meta.is_dir() {
        return Err("Parent path is not a directory".to_string());
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = parent_meta.permissions().mode() & 0o777;
        if mode != 0o700 {
            return Err(format!("Security failure: Parent directory permissions are {:o}, must be strictly 0700", mode));
        }

        let current_uid = unsafe { libc::getuid() };
        if parent_meta.uid() != current_uid {
            return Err("Security failure: Parent directory owner does not match current user".to_string());
        }
    }

    // Check socket file itself if it exists (must not be a symlink)
    if path.exists() {
        let link_meta = fs::symlink_metadata(path)
            .map_err(|e| format!("Failed to check socket metadata: {}", e))?;

        if link_meta.file_type().is_symlink() {
            return Err("Security failure: Socket path is a symbolic link".to_string());
        }

        #[cfg(unix)]
        {
            let current_uid = unsafe { libc::getuid() };
            if link_meta.uid() != current_uid {
                return Err("Security failure: Socket file owner does not match current user".to_string());
            }
        }
    }

    Ok(())
}

/// Establishes the secure client connection over UDS or Named Pipe.
pub fn connect_lsp_client(timeout: Duration) -> Result<Box<dyn ReadWrite + Send>, String> {
    if let Some(tcp_stream) = crate::lsp_bridge::try_connect_container_bridge() {
        return Ok(Box::new(tcp_stream));
    }

    let start_time = std::time::Instant::now();
    let socket_path = get_socket_path();

    #[cfg(windows)]
    {
        let pipe_name = r"\\.\pipe\murshid-lsp";
        loop {
            if start_time.elapsed() >= timeout {
                return Err("LSP proxy connection timeout".to_string());
            }

            if let Ok(file) = fs::OpenOptions::new().read(true).write(true).open(pipe_name) {
                if let Err(e) = verify_windows_pipe_server(&file) {
                    return Err(format!("Windows Named Pipe server verification failed: {}", e));
                }
                return Ok(Box::new(file));
            }

            thread::sleep(Duration::from_millis(50));
        }
    }

    #[cfg(unix)]
    {
        validate_client_path(&socket_path)?;

        loop {
            if start_time.elapsed() >= timeout {
                return Err("LSP proxy connection timeout".to_string());
            }

            if let Ok(stream) = std::os::unix::net::UnixStream::connect(&socket_path) {
                if let Err(e) = verify_peer_credentials(&stream) {
                    return Err(format!("Unix peer credentials verification failed: {}", e));
                }
                return Ok(Box::new(stream));
            }

            thread::sleep(Duration::from_millis(50));
        }
    }
}

pub trait ReadWrite: Read + Write + Send {
    fn try_clone_box(&self) -> Result<Box<dyn ReadWrite>, String>;
}

impl ReadWrite for std::fs::File {
    fn try_clone_box(&self) -> Result<Box<dyn ReadWrite>, String> {
        self.try_clone()
            .map(|f| Box::new(f) as Box<dyn ReadWrite>)
            .map_err(|e| e.to_string())
    }
}

#[cfg(unix)]
impl ReadWrite for std::os::unix::net::UnixStream {
    fn try_clone_box(&self) -> Result<Box<dyn ReadWrite>, String> {
        self.try_clone()
            .map(|s| Box::new(s) as Box<dyn ReadWrite>)
            .map_err(|e| e.to_string())
    }
}

impl ReadWrite for std::net::TcpStream {
    fn try_clone_box(&self) -> Result<Box<dyn ReadWrite>, String> {
        self.try_clone()
            .map(|s| Box::new(s) as Box<dyn ReadWrite>)
            .map_err(|e| e.to_string())
    }
}

pub fn run_lsp_proxy() -> Result<(), String> {
    let client_stream = connect_lsp_client(Duration::from_millis(2000))?;
    let mut socket_read = client_stream;
    let mut socket_write = socket_read.try_clone_box().map_err(|e| format!("Failed to clone stream: {}", e))?;

    // Send Socratic daemon handshake
    let client_pid = std::process::id();
    let workspace_root = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .to_string_lossy()
        .to_string();
    let handshake = serde_json::json!({
        "workspace_root": workspace_root,
        "client_pid": client_pid,
    });
    let handshake_str = format!("{}\n", handshake.to_string());
    socket_write.write_all(handshake_str.as_bytes()).map_err(|e| format!("Failed to write handshake: {}", e))?;
    socket_write.flush().map_err(|e| format!("Failed to flush handshake: {}", e))?;

    let mut stdin_handle = io::stdin();
    let mut stdout_handle = io::stdout();

    let t1 = thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match stdin_handle.read(&mut buf) {
                Ok(0) => break, // EOF
                Ok(n) => {
                    if socket_write.write_all(&buf[..n]).is_err() {
                        break;
                    }
                    let _ = socket_write.flush();
                }
                Err(_) => break,
            }
        }
    });

    let mut buf = [0u8; 4096];
    loop {
        match socket_read.read(&mut buf) {
            Ok(0) => break, // EOF
            Ok(n) => {
                if stdout_handle.write_all(&buf[..n]).is_err() {
                    break;
                }
                let _ = stdout_handle.flush();
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
    use std::fs;
    use std::os::unix::net::UnixListener;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn test_get_socket_path() {
        let path = get_socket_path();
        assert!(path.to_string_lossy().contains("lsp.sock"));
    }

    #[test]
    fn test_client_connect_success() {
        let temp_dir = std::env::temp_dir().join("murshid_lsp_success");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();
        fs::set_permissions(&temp_dir, fs::Permissions::from_mode(0o700)).unwrap();

        let socket_path = temp_dir.join("lsp.sock");

        let listener = UnixListener::bind(&socket_path).unwrap();
        let handle = thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buf = [0u8; 10];
                let n = stream.read(&mut buf).unwrap();
                assert_eq!(&buf[..n], b"hello");
                stream.write_all(b"world").unwrap();
            }
        });

        validate_client_path(&socket_path).unwrap();

        // Connect directly via standard UDS stream and check verify_peer_credentials
        let stream = std::os::unix::net::UnixStream::connect(&socket_path).unwrap();
        verify_peer_credentials(&stream).unwrap();

        let mut client_write = stream;
        let mut client_read = client_write.try_clone().unwrap();

        client_write.write_all(b"hello").unwrap();
        client_write.flush().unwrap();

        let mut res = [0u8; 10];
        let n = client_read.read(&mut res).unwrap();
        assert_eq!(&res[..n], b"world");

        handle.join().unwrap();
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_client_reject_symlink() {
        let temp_dir = std::env::temp_dir().join("murshid_lsp_symlink");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();
        fs::set_permissions(&temp_dir, fs::Permissions::from_mode(0o700)).unwrap();

        let socket_path = temp_dir.join("lsp.sock");
        let symlink_path = temp_dir.join("lsp_sym.sock");

        // Bind socket
        let _listener = UnixListener::bind(&socket_path).unwrap();

        // Create symlink
        std::os::unix::fs::symlink(&socket_path, &symlink_path).unwrap();

        // Validate client path on the symlink
        let result = validate_client_path(&symlink_path);
        assert!(result.is_err());
        assert!(result.err().unwrap().contains("symbolic link"));

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_client_reject_wrong_parent_permissions() {
        let temp_dir = std::env::temp_dir().join("murshid_lsp_permissions");
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();
        // Set loose permissions (0755)
        fs::set_permissions(&temp_dir, fs::Permissions::from_mode(0o755)).unwrap();

        let socket_path = temp_dir.join("lsp.sock");

        let result = validate_client_path(&socket_path);
        assert!(result.is_err());
        assert!(result.err().unwrap().contains("must be strictly 0700"));

        let _ = fs::remove_dir_all(&temp_dir);
    }
}

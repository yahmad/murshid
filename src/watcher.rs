use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

pub enum WatcherImpl {
    Native(notify::RecommendedWatcher),
    Polling {
        thread_handle: std::thread::JoinHandle<()>,
        stop_flag: Arc<AtomicBool>,
    },
}

pub struct MurshidWatcher {
    pub inner: WatcherImpl,
    pub mode: crate::watcher_coordinator::WatchMode,
    pub fd_count: u32,
}

impl Drop for MurshidWatcher {
    fn drop(&mut self) {
        if let WatcherImpl::Polling { stop_flag, .. } = &self.inner {
            stop_flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        crate::watcher_coordinator::get_coordinator().release_resources(self.mode, self.fd_count);
    }
}

pub fn is_excluded(path: &Path, exclude_patterns: &[String]) -> bool {
    let path_str = path.to_string_lossy();
    for pattern in exclude_patterns {
        let clean_pattern = pattern.replace("**", "");
        if clean_pattern.is_empty() {
            continue;
        }
        let trimmed_pattern = clean_pattern.trim_matches('/');
        if path.components().any(|c| c.as_os_str() == trimmed_pattern) {
            return true;
        }
        if path_str.contains(&format!("/{}", trimmed_pattern)) || path_str.contains(&format!("{}/", trimmed_pattern)) {
            return true;
        }
    }
    false
}

pub fn is_allowed_path(path: &Path, root: &Path, include_external_links: &[String]) -> bool {
    if let Ok(canon_root) = root.canonicalize() {
        if path.starts_with(&canon_root) {
            return true;
        }
    }
    for link in include_external_links {
        if let Ok(canon_link) = Path::new(link).canonicalize() {
            if path.starts_with(&canon_link) {
                return true;
            }
        }
    }
    false
}

fn scan_rs_files(
    dir: &Path,
    root: &Path,
    exclude: &[String],
    ext_links: &[String],
    files: &mut HashMap<PathBuf, std::time::SystemTime>
) {
    if is_excluded(dir, exclude) {
        return;
    }
    
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if is_excluded(&path, exclude) {
                continue;
            }
            if let Ok(metadata) = entry.metadata() {
                if metadata.is_dir() {
                    scan_rs_files(&path, root, exclude, ext_links, files);
                } else if metadata.is_file() && path.extension().map_or(false, |ext| ext == "rs") {
                    if let Ok(canon_path) = path.canonicalize() {
                        if is_allowed_path(&canon_path, root, ext_links) {
                            if let Ok(mtime) = metadata.modified() {
                                files.insert(canon_path, mtime);
                            }
                        }
                    }
                }
            }
        }
    }
}

fn scan_all_sources(
    root: &Path,
    exclude: &[String],
    ext_links: &[String]
) -> HashMap<PathBuf, std::time::SystemTime> {
    let mut files = HashMap::new();
    scan_rs_files(root, root, exclude, ext_links, &mut files);
    for link in ext_links {
        let link_path = Path::new(link);
        if link_path.exists() {
            scan_rs_files(link_path, root, exclude, ext_links, &mut files);
        }
    }
    files
}

pub fn setup_native_watcher(
    root: &Path,
    callback: Arc<dyn Fn(PathBuf) + Send + Sync + 'static>,
    exclude: &[String],
    ext_links: &[String]
) -> Result<notify::RecommendedWatcher, String> {
    use notify::{Watcher, RecursiveMode};

    let root_buf = root.to_path_buf();
    let exclude_vec = exclude.to_vec();
    let ext_links_vec = ext_links.to_vec();

    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(event) = res {
            for path in event.paths {
                if path.extension().map_or(false, |ext| ext == "rs") {
                    if !is_excluded(&path, &exclude_vec) {
                        if let Ok(canon_path) = path.canonicalize() {
                            if is_allowed_path(&canon_path, &root_buf, &ext_links_vec) {
                                if !crate::watcher_coordinator::should_skip_file(&canon_path) {
                                    callback(canon_path);
                                }
                            }
                        }
                    }
                }
            }
        }
    }).map_err(|e| e.to_string())?;

    watcher.watch(root, RecursiveMode::Recursive).map_err(|e| e.to_string())?;
    
    for link in ext_links {
        let link_path = Path::new(link);
        if link_path.exists() {
            let _ = watcher.watch(link_path, RecursiveMode::Recursive);
        }
    }

    Ok(watcher)
}

pub fn setup_polling_watcher(
    root: PathBuf,
    callback: Arc<dyn Fn(PathBuf) + Send + Sync + 'static>,
    exclude: Vec<String>,
    ext_links: Vec<String>
) -> (std::thread::JoinHandle<()>, Arc<AtomicBool>) {
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_flag_clone = stop_flag.clone();
    
    let thread_handle = std::thread::spawn(move || {
        let mut last_seen = scan_all_sources(&root, &exclude, &ext_links);

        while !stop_flag_clone.load(std::sync::atomic::Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(2500));
            if stop_flag_clone.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }

            let current = scan_all_sources(&root, &exclude, &ext_links);

            for (path, mtime) in &current {
                match last_seen.get(path) {
                    Some(last_mtime) => {
                        if mtime != last_mtime {
                            if !crate::watcher_coordinator::should_skip_file(path) {
                                callback(path.clone());
                            }
                        }
                    }
                    None => {
                        if !crate::watcher_coordinator::should_skip_file(path) {
                            callback(path.clone());
                        }
                    }
                }
            }

            last_seen = current;
        }
    });

    (thread_handle, stop_flag)
}

pub fn start_watching<F>(root: PathBuf, callback: F) -> Result<MurshidWatcher, String>
where
    F: Fn(PathBuf) + Send + Sync + 'static
{
    let config = crate::config::load_config();
    let exclude = config.watcher.exclude.clone();
    let ext_links = config.watcher.include_external_links.clone();
    let callback_arc = Arc::new(callback);

    let fd_count = crate::watcher_coordinator::count_workspace_files(&root, &exclude);
    let force_polling = std::env::var("MURSHID_FORCE_POLLING_WATCHER").is_ok();
    
    if force_polling {
        let mode = crate::watcher_coordinator::get_coordinator().acquire_resources(0);
        let (thread_handle, stop_flag) = setup_polling_watcher(root, callback_arc, exclude, ext_links);
        return Ok(MurshidWatcher {
            inner: WatcherImpl::Polling { thread_handle, stop_flag },
            mode,
            fd_count: 0,
        });
    }

    let mode = crate::watcher_coordinator::get_coordinator().acquire_resources(fd_count);
    match mode {
        crate::watcher_coordinator::WatchMode::Native => {
            let native_res = setup_native_watcher(&root, callback_arc.clone(), &exclude, &ext_links);
            match native_res {
                Ok(w) => Ok(MurshidWatcher {
                    inner: WatcherImpl::Native(w),
                    mode,
                    fd_count,
                }),
                Err(e) => {
                    eprintln!("[WARNING] Native watcher failed to initialize: {}. Falling back to background polling.", e);
                    crate::watcher_coordinator::get_coordinator().release_resources(mode, fd_count);
                    let polling_mode = crate::watcher_coordinator::get_coordinator().acquire_resources(0);
                    let (thread_handle, stop_flag) = setup_polling_watcher(root, callback_arc, exclude, ext_links);
                    Ok(MurshidWatcher {
                        inner: WatcherImpl::Polling { thread_handle, stop_flag },
                        mode: polling_mode,
                        fd_count: 0,
                    })
                }
            }
        }
        crate::watcher_coordinator::WatchMode::Polling => {
            let (thread_handle, stop_flag) = setup_polling_watcher(root, callback_arc, exclude, ext_links);
            Ok(MurshidWatcher {
                inner: WatcherImpl::Polling { thread_handle, stop_flag },
                mode,
                fd_count: 0,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_exclusions() {
        let patterns = vec![
            "**/target/**".to_string(),
            "**/.git/**".to_string(),
            "**/.murshid_experiments/**".to_string(),
        ];
        
        assert!(is_excluded(Path::new("src/target/main.rs"), &patterns));
        assert!(is_excluded(Path::new(".git/config"), &patterns));
        assert!(is_excluded(Path::new(".murshid_experiments/exp1/src/lib.rs"), &patterns));
        assert!(!is_excluded(Path::new("src/main.rs"), &patterns));
    }

    #[test]
    fn test_path_boundaries() {
        let temp_dir = std::env::temp_dir();
        let root = temp_dir.join("murshid_test_root");
        let external = temp_dir.join("murshid_test_external");
        
        let _ = fs::create_dir_all(&root);
        let _ = fs::create_dir_all(&external);

        let root_canon = root.canonicalize().unwrap();
        let external_canon = external.canonicalize().unwrap();

        let in_root = root_canon.join("src/lib.rs");
        let out_root = external_canon.join("other.rs");

        assert!(is_allowed_path(&in_root, &root_canon, &[]));
        assert!(!is_allowed_path(&out_root, &root_canon, &[]));
        
        // Allowed by external links
        assert!(is_allowed_path(&out_root, &root_canon, &[external.to_string_lossy().to_string()]));

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&external);
    }

    #[test]
    fn test_native_watcher_integration() {
        use std::sync::Mutex;
        use std::time::Instant;

        let temp_dir = std::env::temp_dir();
        let watch_root = temp_dir.join("test_murshid_native_watch");
        let _ = fs::remove_dir_all(&watch_root);
        fs::create_dir_all(&watch_root.join("src")).unwrap();

        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();

        // Start native watcher
        let _watcher = start_watching(watch_root.clone(), move |path| {
            events_clone.lock().unwrap().push(path);
        }).unwrap();

        // Create a new .rs file
        let file_path = watch_root.join("src/lib.rs");
        fs::write(&file_path, "fn hello() {}").unwrap();

        // Wait a short moment for OS to dispatch fs event
        let start = Instant::now();
        while start.elapsed().as_secs() < 3 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            if events.lock().unwrap().len() >= 1 {
                break;
            }
        }

        let rec_events = events.lock().unwrap().clone();
        assert!(!rec_events.is_empty(), "Native watcher did not capture the file creation event");
        assert_eq!(rec_events[0].canonicalize().unwrap(), file_path.canonicalize().unwrap());

        // Cleanup
        let _ = fs::remove_dir_all(&watch_root);
    }

    #[test]
    fn test_polling_watcher_integration() {
        use std::sync::Mutex;
        use std::time::Instant;

        let temp_dir = std::env::temp_dir();
        let watch_root = temp_dir.join("test_murshid_polling_watch");
        let _ = fs::remove_dir_all(&watch_root);
        fs::create_dir_all(&watch_root.join("src")).unwrap();

        // 1. Create the file first so it is present in the initial scan
        let file_path = watch_root.join("src/lib.rs");
        fs::write(&file_path, "fn hello() {}").unwrap();

        let events = Arc::new(Mutex::new(Vec::new()));
        let events_clone = events.clone();

        // 2. Create polling watcher directly
        let config = crate::config::load_config();
        let (thread_handle, stop_flag) = setup_polling_watcher(
            watch_root.clone(),
            Arc::new(move |path| {
                events_clone.lock().unwrap().push(path);
            }),
            config.watcher.exclude.clone(),
            config.watcher.include_external_links.clone(),
        );
        let _watcher = MurshidWatcher {
            inner: WatcherImpl::Polling { thread_handle, stop_flag },
            mode: crate::watcher_coordinator::WatchMode::Polling,
            fd_count: 0,
        };

        // 3. Wait a bit to ensure mtime change is registered, then modify the file
        std::thread::sleep(std::time::Duration::from_millis(500));
        fs::write(&file_path, "fn hello() { println!(\"modified\"); }").unwrap();

        // 4. Since the polling watcher runs every 2500ms, we wait for up to 4500ms
        let start = Instant::now();
        while start.elapsed().as_millis() < 4500 {
            std::thread::sleep(std::time::Duration::from_millis(100));
            if events.lock().unwrap().len() >= 1 {
                break;
            }
        }

        let rec_events = events.lock().unwrap().clone();
        assert!(!rec_events.is_empty(), "Polling watcher did not capture the file modification event");
        assert_eq!(rec_events[0].canonicalize().unwrap(), file_path.canonicalize().unwrap());

        // Cleanup
        let _ = fs::remove_dir_all(&watch_root);
    }
}

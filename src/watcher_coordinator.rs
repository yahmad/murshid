//! Admission control for the file-watch backends. Bounds how many files a
//! single sweep may process and skips files that are too large or too
//! long, using compare-and-swap admission loops with saturating release so
//! a burst of saves can never oversubscribe the judging path.

use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchMode {
    Native,
    Polling,
}

pub struct ResourceCoordinator {
    active_threads: AtomicU32,
    active_fds: AtomicU32,
}

pub fn get_coordinator() -> &'static ResourceCoordinator {
    static COORDINATOR: OnceLock<ResourceCoordinator> = OnceLock::new();
    COORDINATOR.get_or_init(|| ResourceCoordinator {
        active_threads: AtomicU32::new(0),
        active_fds: AtomicU32::new(0),
    })
}

impl ResourceCoordinator {
    pub fn acquire_resources(&self, fd_count: u32) -> WatchMode {
        let config = crate::config::load_config();
        let max_threads = config.watcher.max_watch_threads;
        let max_fds = config.watcher.max_watch_fds;

        // T9 req 4: reserve fds with a compare_exchange loop — the previous
        // separate load + fetch_add let two concurrent watchers both pass the
        // limit check and over-admit Native mode beyond max_fds.
        let mut got_fds = false;
        let mut current_fds = self.active_fds.load(Ordering::SeqCst);
        while current_fds + fd_count <= max_fds {
            match self.active_fds.compare_exchange(
                current_fds,
                current_fds + fd_count,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => {
                    got_fds = true;
                    break;
                }
                Err(observed) => current_fds = observed,
            }
        }

        let mut got_thread = false;
        if got_fds {
            let mut current_threads = self.active_threads.load(Ordering::SeqCst);
            while current_threads < max_threads {
                match self.active_threads.compare_exchange(
                    current_threads,
                    current_threads + 1,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => {
                        got_thread = true;
                        break;
                    }
                    Err(observed) => current_threads = observed,
                }
            }
        }

        if got_fds && got_thread {
            WatchMode::Native
        } else {
            if got_fds {
                // Thread slot lost the race: hand the fd reservation back.
                self.active_fds.fetch_sub(fd_count, Ordering::SeqCst);
            }
            self.active_threads.fetch_add(1, Ordering::SeqCst);
            WatchMode::Polling
        }
    }

    pub fn release_resources(&self, mode: WatchMode, fd_count: u32) {
        // T9 req 4: saturating release — a double-release previously wrapped
        // the u32 counters to ~u32::MAX, permanently forcing Polling mode.
        fn saturating_sub_atomic(counter: &AtomicU32, amount: u32) {
            let mut current = counter.load(Ordering::SeqCst);
            loop {
                let next = current.saturating_sub(amount);
                match counter.compare_exchange(current, next, Ordering::SeqCst, Ordering::SeqCst) {
                    Ok(_) => break,
                    Err(observed) => current = observed,
                }
            }
        }
        match mode {
            WatchMode::Native => {
                saturating_sub_atomic(&self.active_threads, 1);
                saturating_sub_atomic(&self.active_fds, fd_count);
            }
            WatchMode::Polling => {
                saturating_sub_atomic(&self.active_threads, 1);
            }
        }
    }

    pub fn active_threads(&self) -> u32 {
        self.active_threads.load(Ordering::SeqCst)
    }

    pub fn active_fds(&self) -> u32 {
        self.active_fds.load(Ordering::SeqCst)
    }

    pub fn reset(&self) {
        self.active_threads.store(0, Ordering::SeqCst);
        self.active_fds.store(0, Ordering::SeqCst);
    }
}

pub fn count_workspace_files(root: &Path, exclude: &[String]) -> u32 {
    let mut count = 0;
    count_files_recursive(root, exclude, &mut count);
    count
}

fn count_files_recursive(dir: &Path, exclude: &[String], count: &mut u32) {
    if crate::watcher::is_excluded(dir, exclude) {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if crate::watcher::is_excluded(&path, exclude) {
                continue;
            }
            if let Ok(metadata) = entry.metadata() {
                if metadata.is_dir() {
                    count_files_recursive(&path, exclude, count);
                } else if metadata.is_file() {
                    *count += 1;
                }
            }
        }
    }
}

pub fn should_skip_file(path: &Path) -> bool {
    if let Ok(metadata) = std::fs::metadata(path) {
        let size = metadata.len();
        if size > 50 * 1024 {
            eprintln!(
                "[INFO] Skipping file {}: size exceeds 50KB limit ({} bytes)",
                path.display(),
                size
            );
            return true;
        }
    }

    if let Ok(content) = std::fs::read_to_string(path) {
        let line_count = content.lines().count();
        if line_count > 1500 {
            eprintln!(
                "[INFO] Skipping file {}: lines exceed 1500 limit ({} lines)",
                path.display(),
                line_count
            );
            return true;
        }
    }

    false
}

pub fn get_dir_size(path: &Path) -> u64 {
    let mut size = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(metadata) = entry.metadata() {
                if metadata.is_dir() {
                    size += get_dir_size(&entry.path());
                } else {
                    size += metadata.len();
                }
            }
        }
    }
    size
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    static TEST_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn test_file_skips_by_size_and_lines() {
        let temp_dir = std::env::temp_dir();
        let small_file = temp_dir.join("test_murshid_small.rs");
        let large_file = temp_dir.join("test_murshid_large.rs");
        let many_lines_file = temp_dir.join("test_murshid_lines.rs");

        // Small file (1KB, 10 lines)
        fs::write(&small_file, "fn foo() {}\n".repeat(10)).unwrap();
        assert!(!should_skip_file(&small_file));

        // Large file (51KB)
        let large_content = vec![0u8; 51 * 1024];
        fs::write(&large_file, large_content).unwrap();
        assert!(should_skip_file(&large_file));

        // Many lines file (1501 lines)
        fs::write(&many_lines_file, "line\n".repeat(1501)).unwrap();
        assert!(should_skip_file(&many_lines_file));

        // Cleanup
        let _ = fs::remove_file(&small_file);
        let _ = fs::remove_file(&large_file);
        let _ = fs::remove_file(&many_lines_file);
    }

    #[test]
    fn test_count_workspace_files() {
        let temp_dir = std::env::temp_dir();
        let watch_root = temp_dir.join("test_murshid_count");
        let _ = fs::remove_dir_all(&watch_root);
        fs::create_dir_all(watch_root.join("src")).unwrap();
        fs::create_dir_all(watch_root.join("target")).unwrap();
        fs::create_dir_all(watch_root.join(".git")).unwrap();

        fs::write(watch_root.join("src/lib.rs"), "pub fn a() {}").unwrap();
        fs::write(watch_root.join("src/main.rs"), "pub fn b() {}").unwrap();
        fs::write(watch_root.join("target/some_build.rs"), "pub fn c() {}").unwrap();
        fs::write(watch_root.join(".git/config"), "some config").unwrap();

        let exclude = vec!["**/target/**".to_string(), "**/.git/**".to_string()];

        let count = count_workspace_files(&watch_root, &exclude);
        // Only src/lib.rs and src/main.rs should be counted (2 files)
        assert_eq!(count, 2);

        let _ = fs::remove_dir_all(&watch_root);
    }

    #[test]
    fn test_resource_coordination_limits() {
        // HOME is process-global: take the crate-wide env lock FIRST (before
        // this module's coordinator mutex, and nothing else takes both, so no
        // ordering cycle) so credentials.rs tests can't see the temp HOME.
        let _env_lock = crate::credentials::env_test_lock();
        let _lock = TEST_MUTEX.lock().unwrap();

        // Redirect HOME to temp dir to load custom test configuration
        let temp_dir = std::env::temp_dir();
        let old_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", temp_dir.to_str().unwrap());
        }

        // Make sure user config path parent directory exists
        let user_config_path = crate::config::resolve_user_config_path().unwrap();
        if let Some(parent) = user_config_path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        // Write custom config with threads limit 2 and fds limit 10
        fs::write(
            &user_config_path,
            "
[watcher]
max_watch_threads = 2
max_watch_fds = 10
",
        )
        .unwrap();

        let coordinator = get_coordinator();
        coordinator.reset();

        // 1st Watcher requests 6 fds: should get Native (as threads=0+1<=2, fds=0+6<=10)
        let mode1 = coordinator.acquire_resources(6);
        assert_eq!(mode1, WatchMode::Native);
        assert_eq!(coordinator.active_threads(), 1);
        assert_eq!(coordinator.active_fds(), 6);

        // 2nd Watcher requests 5 fds: should get Polling (as fds=6+5=11 > 10 fds limit)
        let mode2 = coordinator.acquire_resources(5);
        assert_eq!(mode2, WatchMode::Polling);
        assert_eq!(coordinator.active_threads(), 2);
        assert_eq!(coordinator.active_fds(), 6); // polling uses 0 fds

        // 3rd Watcher requests 1 fd: should get Polling (as threads=2+1=3 > 2 limit)
        let mode3 = coordinator.acquire_resources(1);
        assert_eq!(mode3, WatchMode::Polling);
        assert_eq!(coordinator.active_threads(), 3);

        // Release 1st watcher
        coordinator.release_resources(mode1, 6);
        assert_eq!(coordinator.active_threads(), 2);
        assert_eq!(coordinator.active_fds(), 0);

        // Release others
        coordinator.release_resources(mode2, 5);
        coordinator.release_resources(mode3, 1);
        assert_eq!(coordinator.active_threads(), 0);
        assert_eq!(coordinator.active_fds(), 0);

        // Cleanup
        let _ = fs::remove_file(&user_config_path);
        unsafe {
            if let Some(h) = old_home {
                std::env::set_var("HOME", h);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }
}

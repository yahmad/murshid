use keyring::Entry;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedKeys {
    pub gemini_api_key: Option<String>,
    pub claude_api_key: Option<String>,
}

pub fn get_key_cache() -> &'static Arc<RwLock<Option<CachedKeys>>> {
    static CACHE: OnceLock<Arc<RwLock<Option<CachedKeys>>>> = OnceLock::new();
    CACHE.get_or_init(|| Arc::new(RwLock::new(None)))
}

static MOCK_KEYRING: OnceLock<Mutex<HashMap<(String, String), String>>> = OnceLock::new();

fn get_mock_keyring() -> &'static Mutex<HashMap<(String, String), String>> {
    MOCK_KEYRING.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn get_credential(service: &str, username: &str) -> Result<String, keyring::Error> {
    if std::env::var("MURSHID_TESTING").is_ok() {
        let map = get_mock_keyring().lock().unwrap();
        if let Some(pwd) = map.get(&(service.to_string(), username.to_string())) {
            Ok(pwd.clone())
        } else {
            Err(keyring::Error::NoEntry)
        }
    } else {
        let entry = Entry::new(service, username)?;
        entry.get_password()
    }
}

pub fn set_credential(service: &str, username: &str, password: &str) -> Result<(), keyring::Error> {
    if std::env::var("MURSHID_TESTING").is_ok() {
        let mut map = get_mock_keyring().lock().unwrap();
        map.insert(
            (service.to_string(), username.to_string()),
            password.to_string(),
        );
        Ok(())
    } else {
        let entry = Entry::new(service, username)?;
        entry.set_password(password)
    }
}

pub fn delete_credential(service: &str, username: &str) -> Result<(), keyring::Error> {
    if std::env::var("MURSHID_TESTING").is_ok() {
        let mut map = get_mock_keyring().lock().unwrap();
        if map
            .remove(&(service.to_string(), username.to_string()))
            .is_some()
        {
            Ok(())
        } else {
            Err(keyring::Error::NoEntry)
        }
    } else {
        let entry = Entry::new(service, username)?;
        entry.delete_password()
    }
}

fn keyring_service_name() -> &'static str {
    if std::env::var("MURSHID_TESTING").is_ok() {
        "murshid_test"
    } else {
        "murshid"
    }
}

fn is_no_entry_error(err: &keyring::Error) -> bool {
    matches!(err, keyring::Error::NoEntry)
}

pub fn load_keys_from_source() -> CachedKeys {
    let mut gemini = None;
    let mut claude = None;
    let mut used_env = false;

    // Load Gemini API Key from Keychain
    match get_credential(keyring_service_name(), "gemini_api_key") {
        Ok(pwd) => gemini = Some(pwd),
        Err(e) => {
            if !is_no_entry_error(&e) {
                eprintln!(
                    "[WARNING] Keyring access failed for username gemini_api_key: {}. Verification checks will degrade gracefully.",
                    e
                );
            }
        }
    }

    if gemini.is_none() {
        if let Ok(val) =
            std::env::var("MURSHID_GEMINI_API_KEY").or_else(|_| std::env::var("GEMINI_API_KEY"))
        {
            gemini = Some(val);
            used_env = true;
        }
    }

    // Load Claude API Key from Keychain
    match get_credential(keyring_service_name(), "claude_api_key") {
        Ok(pwd) => claude = Some(pwd),
        Err(e) => {
            if !is_no_entry_error(&e) {
                eprintln!(
                    "[WARNING] Keyring access failed for username claude_api_key: {}. Verification checks will degrade gracefully.",
                    e
                );
            }
        }
    }

    if claude.is_none() {
        if let Ok(val) =
            std::env::var("MURSHID_CLAUDE_API_KEY").or_else(|_| std::env::var("ANTHROPIC_API_KEY"))
        {
            claude = Some(val);
            used_env = true;
        }
    }

    if used_env {
        let config = crate::config::load_config();
        let has_no_warn = std::env::args().any(|arg| arg == "--no-warn");
        if !config.provider.suppress_api_key_warning && !has_no_warn {
            eprintln!(
                "[WARNING] Using plaintext API keys from environment variables. Secure Keychain storage is recommended for production codebases."
            );
        }
    }

    CachedKeys {
        gemini_api_key: gemini,
        claude_api_key: claude,
    }
}

pub fn refresh_cache() -> Result<CachedKeys, String> {
    let keys = load_keys_from_source();
    if let Ok(mut cache) = get_key_cache().write() {
        *cache = Some(keys.clone());
    } else {
        return Err("Failed to acquire write lock on credentials cache".to_string());
    }
    Ok(keys)
}

pub fn get_api_keys() -> Option<CachedKeys> {
    {
        if let Ok(cache) = get_key_cache().read() {
            if let Some(ref keys) = *cache {
                return Some(keys.clone());
            }
        }
    }
    refresh_cache().ok()
}

pub fn set_gemini_key(key: &str) -> Result<(), keyring::Error> {
    set_credential(keyring_service_name(), "gemini_api_key", key)?;
    let _ = refresh_cache();
    Ok(())
}

pub fn set_claude_key(key: &str) -> Result<(), keyring::Error> {
    set_credential(keyring_service_name(), "claude_api_key", key)?;
    let _ = refresh_cache();
    Ok(())
}

pub fn delete_gemini_key() -> Result<(), keyring::Error> {
    delete_credential(keyring_service_name(), "gemini_api_key")?;
    let _ = refresh_cache();
    Ok(())
}

pub fn delete_claude_key() -> Result<(), keyring::Error> {
    delete_credential(keyring_service_name(), "claude_api_key")?;
    let _ = refresh_cache();
    Ok(())
}

#[cfg(unix)]
fn setup_sighup_handler() {
    static SIGHUP_INIT: std::sync::Once = std::sync::Once::new();
    SIGHUP_INIT.call_once(|| unsafe {
        libc::signal(libc::SIGHUP, sighup_handler as usize);
    });
}

#[cfg(unix)]
extern "C" fn sighup_handler(_sig: libc::c_int) {
    std::thread::spawn(|| {
        let _ = refresh_cache();
    });
}

fn spawn_config_watcher() {
    static WATCHER_INIT: std::sync::Once = std::sync::Once::new();
    WATCHER_INIT.call_once(|| {
        std::thread::spawn(|| {
            let path = crate::config::resolve_user_config_path();
            let mut last_mtime = path
                .as_ref()
                .and_then(|p| std::fs::metadata(p).ok())
                .and_then(|m| m.modified().ok());

            loop {
                std::thread::sleep(std::time::Duration::from_millis(500));
                let current_path = crate::config::resolve_user_config_path();
                let current_mtime = current_path
                    .as_ref()
                    .and_then(|p| std::fs::metadata(p).ok())
                    .and_then(|m| m.modified().ok());
                if current_mtime != last_mtime {
                    last_mtime = current_mtime;
                    let _ = refresh_cache();
                }
            }
        });
    });
}

pub fn init() {
    let _ = refresh_cache();
    #[cfg(unix)]
    setup_sighup_handler();
    spawn_config_watcher();
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct TestEnvGuard;
    impl TestEnvGuard {
        fn new() -> Self {
            unsafe {
                std::env::set_var("MURSHID_TESTING", "1");
            }
            // Clean up any test keys that might be left over from crashed runs
            let _ = delete_credential("murshid_test", "gemini_api_key");
            let _ = delete_credential("murshid_test", "claude_api_key");
            Self
        }
    }
    impl Drop for TestEnvGuard {
        fn drop(&mut self) {
            let _ = delete_credential("murshid_test", "gemini_api_key");
            let _ = delete_credential("murshid_test", "claude_api_key");
            unsafe {
                std::env::remove_var("MURSHID_TESTING");
            }
        }
    }

    #[test]
    fn test_env_var_fallback() {
        let _lock = TEST_MUTEX.lock().unwrap();
        let _env_guard = TestEnvGuard::new();

        // Clear environment variables
        unsafe {
            std::env::remove_var("MURSHID_GEMINI_API_KEY");
            std::env::remove_var("GEMINI_API_KEY");
            std::env::remove_var("MURSHID_CLAUDE_API_KEY");
            std::env::remove_var("ANTHROPIC_API_KEY");
        }

        // Try load
        let keys = load_keys_from_source();
        assert!(keys.gemini_api_key.is_none());
        assert!(keys.claude_api_key.is_none());

        // Set env vars
        unsafe {
            std::env::set_var("MURSHID_GEMINI_API_KEY", "env_gemini_test_value");
            std::env::set_var("ANTHROPIC_API_KEY", "env_claude_test_value");
        }

        let keys2 = load_keys_from_source();
        assert_eq!(
            keys2.gemini_api_key.as_deref(),
            Some("env_gemini_test_value")
        );
        assert_eq!(
            keys2.claude_api_key.as_deref(),
            Some("env_claude_test_value")
        );

        // Clean up
        unsafe {
            std::env::remove_var("MURSHID_GEMINI_API_KEY");
            std::env::remove_var("ANTHROPIC_API_KEY");
        }
    }

    #[test]
    fn test_get_api_keys_caching_and_latency() {
        let _lock = TEST_MUTEX.lock().unwrap();
        let _env_guard = TestEnvGuard::new();

        // Mock cache state
        if let Ok(mut cache) = get_key_cache().write() {
            *cache = Some(CachedKeys {
                gemini_api_key: Some("cached_gemini".to_string()),
                claude_api_key: Some("cached_claude".to_string()),
            });
        }

        // Measure latency
        let start = std::time::Instant::now();
        let keys = get_api_keys().unwrap();
        let duration = start.elapsed();

        assert_eq!(keys.gemini_api_key.as_deref(), Some("cached_gemini"));
        assert_eq!(keys.claude_api_key.as_deref(), Some("cached_claude"));

        // Assert latency is well within 50ms (usually under 0.1ms)
        assert!(
            duration.as_millis() < 50,
            "Cache read took too long: {:?}",
            duration
        );

        // Reset cache
        if let Ok(mut cache) = get_key_cache().write() {
            *cache = None;
        }
    }

    #[test]
    fn test_config_modification_reload() {
        let _lock = TEST_MUTEX.lock().unwrap();
        let _env_guard = TestEnvGuard::new();

        // Redirect HOME to temp dir
        let temp_dir = std::env::temp_dir();
        let old_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", temp_dir.to_str().unwrap());
        }

        // Make sure the config file path exists
        let user_config_path = crate::config::resolve_user_config_path().unwrap();
        if let Some(parent) = user_config_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        // Write an initial config
        std::fs::write(
            &user_config_path,
            "
[provider]
api_key_source = \"keychain\"
suppress_api_key_warning = false
",
        )
        .unwrap();

        // Start watch loop (calls init)
        init();

        // Set initial environment values (as keyring is empty)
        unsafe {
            std::env::set_var("GEMINI_API_KEY", "initial_env_val");
        }
        refresh_cache().unwrap();

        let keys = get_api_keys().unwrap();
        assert_eq!(keys.gemini_api_key.as_deref(), Some("initial_env_val"));

        // Now change the environment key
        unsafe {
            std::env::set_var("GEMINI_API_KEY", "updated_env_val");
        }

        // To trigger the config modification watch, we can modify the user config file
        // Wait a brief moment to ensure modification timestamps will differ
        std::thread::sleep(std::time::Duration::from_millis(100));
        std::fs::write(
            &user_config_path,
            "
[provider]
api_key_source = \"keychain\"
suppress_api_key_warning = true
",
        )
        .unwrap();

        // Wait for the polling watcher to detect change (polls every 500ms, 800ms is safe)
        std::thread::sleep(std::time::Duration::from_millis(800));

        // Cache should have reloaded and picked up the updated environment key!
        let keys2 = get_api_keys().unwrap();
        assert_eq!(keys2.gemini_api_key.as_deref(), Some("updated_env_val"));

        // Cleanup
        let _ = std::fs::remove_file(&user_config_path);
        unsafe {
            std::env::remove_var("GEMINI_API_KEY");
            if let Some(h) = old_home {
                std::env::set_var("HOME", h);
            } else {
                std::env::remove_var("HOME");
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn test_sighup_reload() {
        let _lock = TEST_MUTEX.lock().unwrap();
        let _env_guard = TestEnvGuard::new();

        // Set some test env key
        unsafe {
            std::env::set_var("GEMINI_API_KEY", "env_value_before_sighup");
        }
        refresh_cache().unwrap();

        let keys = get_api_keys().unwrap();
        assert_eq!(
            keys.gemini_api_key.as_deref(),
            Some("env_value_before_sighup")
        );

        // Change env key
        unsafe {
            std::env::set_var("GEMINI_API_KEY", "env_value_after_sighup");
        }

        // Trigger SIGHUP signal to ourselves
        unsafe {
            libc::kill(libc::getpid(), libc::SIGHUP);
        }

        // Wait a short moment for the handler thread to run
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Cache should have been updated!
        let keys2 = get_api_keys().unwrap();
        assert_eq!(
            keys2.gemini_api_key.as_deref(),
            Some("env_value_after_sighup")
        );

        // Cleanup
        unsafe {
            std::env::remove_var("GEMINI_API_KEY");
        }
    }

    #[test]
    fn test_keyring_get_set_delete() {
        let _lock = TEST_MUTEX.lock().unwrap();
        let _env_guard = TestEnvGuard::new();

        // Since OS keychain might fail if unlocked/non-interactive, we handle failure gracefully
        // but assert that if they succeed, they are cached correctly.
        let test_key = "keyring_test_key_123";

        match set_gemini_key(test_key) {
            Ok(_) => {
                // If it succeeds, verify it gets loaded
                let keys = load_keys_from_source();
                assert_eq!(keys.gemini_api_key.as_deref(), Some(test_key));

                // Delete it and verify it's gone
                assert!(delete_gemini_key().is_ok());
                let keys_after = load_keys_from_source();
                assert!(keys_after.gemini_api_key.is_none());
            }
            Err(e) => {
                println!(
                    "Skipping keychain write test as OS keyring is unavailable: {}",
                    e
                );
            }
        }
    }
}

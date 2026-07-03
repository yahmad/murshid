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

// T9 req 9(a): under MURSHID_TESTING, scope the keyring service name to this
// process (`murshid_test_<pid>`) instead of a single shared "murshid_test"
// literal. Two `cargo test` runs in different checkouts previously raced on
// the SAME real keychain service, so one process's cleanup could delete
// entries the other process's assertions still depended on. `pub(crate)` so
// `cli::setup` can route through this instead of hardcoding its own literal.
pub(crate) fn keyring_service_name() -> String {
    if std::env::var("MURSHID_TESTING").is_ok() {
        format!("murshid_test_{}", std::process::id())
    } else {
        "murshid".to_string()
    }
}

fn is_no_entry_error(err: &keyring::Error) -> bool {
    matches!(err, keyring::Error::NoEntry)
}

pub fn load_keys_from_source() -> CachedKeys {
    let mut gemini = None;
    let mut claude = None;
    let mut used_env = false;

    let config = crate::config::load_config();
    let use_keychain = config.provider.api_key_source == "keychain"
        && std::env::var("MURSHID_NO_KEYCHAIN").is_err()
        && std::env::var("MURSHID_BYPASS_KEYCHAIN").is_err();

    if use_keychain {
        // Load Gemini API Key from Keychain
        match get_credential(&keyring_service_name(), "gemini_api_key") {
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
    }

    if gemini.is_none() {
        if let Ok(val) =
            std::env::var("MURSHID_GEMINI_API_KEY").or_else(|_| std::env::var("GEMINI_API_KEY"))
        {
            gemini = Some(val);
            used_env = true;
        }
    }

    if use_keychain {
        // Load Claude API Key from Keychain
        match get_credential(&keyring_service_name(), "claude_api_key") {
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
    set_credential(&keyring_service_name(), "gemini_api_key", key)?;
    let _ = refresh_cache();
    Ok(())
}

pub fn set_claude_key(key: &str) -> Result<(), keyring::Error> {
    set_credential(&keyring_service_name(), "claude_api_key", key)?;
    let _ = refresh_cache();
    Ok(())
}

pub fn delete_gemini_key() -> Result<(), keyring::Error> {
    delete_credential(&keyring_service_name(), "gemini_api_key")?;
    let _ = refresh_cache();
    Ok(())
}

pub fn delete_claude_key() -> Result<(), keyring::Error> {
    delete_credential(&keyring_service_name(), "claude_api_key")?;
    let _ = refresh_cache();
    Ok(())
}

// T9 req 3: the SIGHUP-triggered hot-reload machinery (`init()`,
// `setup_sighup_handler`/`sighup_handler`, and the 500ms config-poll thread)
// is deleted rather than fixed. It was never wired up (`credentials::init()`
// had no caller), its signal handler was not async-signal-safe (it called
// `thread::spawn` from inside the handler), and BYOK key rotation can just
// restart the process. `set_*_key`/`delete_*_key` already call
// `refresh_cache()` inline, so nothing depended on the poller.

/// Serializes every test (crate-wide) that mutates process-global env vars
/// (`MURSHID_TESTING`, `*_API_KEY`, `HOME`, …). Env is process-wide, so
/// per-module mutexes cannot prevent cross-module interleaving: a lost race
/// between this module's tests and `cli_setup`'s once routed `run_setup`'s
/// mock keys into the user's REAL keychain service (2026-07-03). Poison is
/// swallowed so one failing test cannot cascade PoisonError through every
/// later env-touching test.
#[cfg(test)]
pub(crate) fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static ENV_TEST_MUTEX: std::sync::Mutex<()> = std::sync::Mutex::new(());
    ENV_TEST_MUTEX.lock().unwrap_or_else(|e| e.into_inner())
}

// NOTE (T9): only env-MUTATING tests take `env_test_lock()`. Reader paths
// (pack/taxonomy loads via `resolve_packs_dir()`) must NOT acquire it — a
// depth-counting "reentrant" wrapper was tried and self-deadlocked, because
// acquisitions through the plain fn above bypass the thread-local depth
// counter, so a reader nested under a plain-locked scope re-locks the same
// non-reentrant mutex. The residual reader-vs-mutator race on
// `MURSHID_PACKS_DIR` is accepted (pre-existing, never observed) and
// documented in the T9 spec addendum.

#[cfg(test)]
mod tests {
    use super::*;

    struct TestEnvGuard;
    impl TestEnvGuard {
        fn new() -> Self {
            unsafe {
                std::env::set_var("MURSHID_TESTING", "1");
            }
            // T9 req 9(a): route through keyring_service_name() (now
            // per-process-unique) instead of the literal "murshid_test", so
            // cleanup targets the same service this process's code under
            // test actually touches.
            let service = keyring_service_name();
            let _ = delete_credential(&service, "gemini_api_key");
            let _ = delete_credential(&service, "claude_api_key");
            Self
        }
    }
    impl Drop for TestEnvGuard {
        fn drop(&mut self) {
            let service = keyring_service_name();
            let _ = delete_credential(&service, "gemini_api_key");
            let _ = delete_credential(&service, "claude_api_key");
            unsafe {
                std::env::remove_var("MURSHID_TESTING");
            }
        }
    }

    #[test]
    fn test_keychain_bypass_with_env_override() {
        let _lock = env_test_lock();
        let _env_guard = TestEnvGuard::new();

        unsafe {
            std::env::set_var("GEMINI_API_KEY", "env_bypass_gemini_test_value");
            std::env::set_var("MURSHID_NO_KEYCHAIN", "1");
        }

        let _ = set_gemini_key("keyring_ignored_val");

        let keys = load_keys_from_source();
        assert_eq!(
            keys.gemini_api_key.as_deref(),
            Some("env_bypass_gemini_test_value")
        );

        unsafe {
            std::env::remove_var("GEMINI_API_KEY");
            std::env::remove_var("MURSHID_NO_KEYCHAIN");
        }
        let _ = delete_gemini_key();
    }

    #[test]
    fn test_env_var_fallback() {
        let _lock = env_test_lock();
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
        let _lock = env_test_lock();
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
    fn test_keyring_get_set_delete() {
        let _lock = env_test_lock();
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

//! BYOK key resolution (C6): an in-process cache over the OS keyring with an
//! environment-variable override and SIGHUP-triggered reload. Ollama and
//! other local providers need no key. Under `cfg!(test)` the keyring is
//! mocked so the suite never touches the real Keychain.

use crate::provider::{OpenAiKind, Provider};
// Only the test-only `env_test_lock` locks a mutex here.
#[cfg(test)]
use crate::sync_ext::LockExt;
use keyring::Entry;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

/// One keyed provider's credential-loading recipe: its keyring username (the
/// stable on-disk identifier — DO NOT rename these, `cli::setup` writes them)
/// and the environment variables it falls back to, in precedence order.
struct KeySpec {
    provider: Provider,
    keyring_username: &'static str,
    env_vars: &'static [&'static str],
}

/// The providers that carry an API key. Gemini and Claude require one; the
/// generic `openai` OpenAI-compatible endpoint MAY carry a bearer key (T8 req
/// 5 / ROADMAP item 6 — previously its keyed auth path was production-dead
/// because `resolve_slot_key` returned `None` for it). Ollama/LM Studio are
/// keyless-local and are deliberately absent. Adding a keyed provider is one
/// entry here — the load loop and the cache are generic over this table.
const KEY_SPECS: &[KeySpec] = &[
    KeySpec {
        provider: Provider::Gemini,
        keyring_username: "gemini_api_key",
        env_vars: &["MURSHID_GEMINI_API_KEY", "GEMINI_API_KEY"],
    },
    KeySpec {
        provider: Provider::Claude,
        keyring_username: "claude_api_key",
        env_vars: &["MURSHID_CLAUDE_API_KEY", "ANTHROPIC_API_KEY"],
    },
    KeySpec {
        provider: Provider::OpenAiCompat(OpenAiKind::OpenAi),
        keyring_username: "openai_api_key",
        env_vars: &["MURSHID_OPENAI_API_KEY", "OPENAI_API_KEY"],
    },
];

/// The BYOK key cache (C6): provider → key. Keyed by [`Provider`] rather than
/// hardcoding one named field per provider, so a new keyed endpoint is a
/// [`KEY_SPECS`] table entry, not a copy-pasted keyring block. A provider with
/// no resolvable key is simply absent from the map.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CachedKeys {
    keys: HashMap<Provider, String>,
    /// Providers whose keyring entry EXISTS but could not be read — a non-
    /// `NoEntry` keyring error, e.g. the macOS keychain ACL denying the murshid
    /// binary. Distinct from "no entry at all": a key IS configured, it's just
    /// unreadable, so the degraded-mode reason can say so instead of the
    /// misleading "no API key configured" (dogfood 2026-07-03).
    unreadable: std::collections::HashSet<Provider>,
}

impl CachedKeys {
    /// The resolved key for `provider`, if any (absent = keyless or unset).
    pub fn get(&self, provider: Provider) -> Option<&str> {
        self.keys.get(&provider).map(String::as_str)
    }

    /// Whether `provider` has a keyring entry that exists but could not be read
    /// (keychain access denied), as opposed to no entry at all.
    pub fn is_unreadable(&self, provider: Provider) -> bool {
        self.unreadable.contains(&provider)
    }
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
    if cfg!(test) || std::env::var("MURSHID_TESTING").is_ok() {
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
    if cfg!(test) || std::env::var("MURSHID_TESTING").is_ok() {
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
    if cfg!(test) || std::env::var("MURSHID_TESTING").is_ok() {
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
    if cfg!(test) || std::env::var("MURSHID_TESTING").is_ok() {
        format!("murshid_test_{}", std::process::id())
    } else {
        "murshid".to_string()
    }
}

fn is_no_entry_error(err: &keyring::Error) -> bool {
    matches!(err, keyring::Error::NoEntry)
}

pub fn load_keys_from_source() -> CachedKeys {
    let config = crate::config::load_config();
    let use_keychain = config.provider.api_key_source == "keychain"
        && std::env::var("MURSHID_NO_KEYCHAIN").is_err()
        && std::env::var("MURSHID_BYPASS_KEYCHAIN").is_err();

    let mut keys = HashMap::new();
    let mut unreadable = std::collections::HashSet::new();
    let mut used_env = false;

    // One loop over KEY_SPECS: keychain first (when enabled), then the
    // ordered env-var fallbacks. Previously two copy-pasted blocks hardcoded
    // gemini/claude; openai now resolves the same way, so its bearer-auth arm
    // is reachable.
    for spec in KEY_SPECS {
        let mut value = None;

        if use_keychain {
            match get_credential(&keyring_service_name(), spec.keyring_username) {
                Ok(pwd) => value = Some(pwd),
                Err(e) => {
                    if !is_no_entry_error(&e) {
                        // A key EXISTS but couldn't be read (e.g. keychain ACL).
                        // Record it so the degraded reason can distinguish this
                        // from a genuinely-missing key.
                        unreadable.insert(spec.provider);
                        eprintln!(
                            "[WARNING] Keyring access failed for username {}: {}. Verification checks will degrade gracefully.",
                            spec.keyring_username, e
                        );
                    }
                }
            }
        }

        if value.is_none() {
            for env_var in spec.env_vars {
                if let Ok(val) = std::env::var(env_var) {
                    value = Some(val);
                    used_env = true;
                    break;
                }
            }
        }

        if let Some(v) = value {
            // An env-var fallback that succeeds overrides an unreadable
            // keychain entry — the slot is usable, so it's not "unreadable".
            unreadable.remove(&spec.provider);
            keys.insert(spec.provider, v);
        }
    }

    if used_env {
        let has_no_warn = std::env::args().any(|arg| arg == "--no-warn");
        if !config.provider.suppress_api_key_warning && !has_no_warn {
            eprintln!(
                "[WARNING] Using plaintext API keys from environment variables. Secure Keychain storage is recommended for production codebases."
            );
        }
    }

    CachedKeys { keys, unreadable }
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
    ENV_TEST_MUTEX.lock_poison_safe()
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
            clear_all_keyring_entries();
            Self
        }
    }
    impl Drop for TestEnvGuard {
        fn drop(&mut self) {
            clear_all_keyring_entries();
            unsafe {
                std::env::remove_var("MURSHID_TESTING");
            }
        }
    }

    /// Deletes every keyed provider's keyring entry (T9 req 9(a): routed
    /// through the per-process-unique `keyring_service_name()`, not the literal
    /// `"murshid_test"`). Iterates `KEY_SPECS` so a newly keyed provider is
    /// cleaned up automatically.
    fn clear_all_keyring_entries() {
        let service = keyring_service_name();
        for spec in KEY_SPECS {
            let _ = delete_credential(&service, spec.keyring_username);
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
            keys.get(Provider::Gemini),
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
        assert!(keys.get(Provider::Gemini).is_none());
        assert!(keys.get(Provider::Claude).is_none());

        // Set env vars
        unsafe {
            std::env::set_var("MURSHID_GEMINI_API_KEY", "env_gemini_test_value");
            std::env::set_var("ANTHROPIC_API_KEY", "env_claude_test_value");
        }

        let keys2 = load_keys_from_source();
        assert_eq!(keys2.get(Provider::Gemini), Some("env_gemini_test_value"));
        assert_eq!(keys2.get(Provider::Claude), Some("env_claude_test_value"));

        // Clean up
        unsafe {
            std::env::remove_var("MURSHID_GEMINI_API_KEY");
            std::env::remove_var("ANTHROPIC_API_KEY");
        }
    }

    /// ROADMAP item 6: a keyed OpenAI-compatible endpoint's bearer key was
    /// production-dead — `resolve_slot_key` hardcoded `None` for `openai`.
    /// Now it resolves through the generic cache, while keyless locals stay
    /// keyless.
    #[test]
    fn test_openai_keyed_endpoint_resolves_a_bearer_key() {
        let _lock = env_test_lock();
        let _env_guard = TestEnvGuard::new();

        unsafe {
            // Force the env path (no keychain read) and provide a bearer key.
            std::env::set_var("MURSHID_NO_KEYCHAIN", "1");
            std::env::set_var("OPENAI_API_KEY", "openai_bearer_test_value");
        }

        let keys = load_keys_from_source();
        assert_eq!(
            keys.get(Provider::OpenAiCompat(OpenAiKind::OpenAi)),
            Some("openai_bearer_test_value")
        );

        // The formerly-dead path: a keyed `openai` slot now carries its key.
        let resolved = crate::resolve_slot_key("openai", &Some(keys));
        assert_eq!(resolved.as_deref(), Some("openai_bearer_test_value"));

        // Keyless locals still resolve to no key.
        assert!(crate::resolve_slot_key("ollama", &Some(CachedKeys::default())).is_none());
        assert!(crate::resolve_slot_key("lmstudio", &Some(CachedKeys::default())).is_none());

        unsafe {
            std::env::remove_var("OPENAI_API_KEY");
            std::env::remove_var("MURSHID_NO_KEYCHAIN");
        }
    }

    /// Dogfood fix: an unreadable keychain entry (a key that exists but the ACL
    /// denied) propagates provider → CachedKeys → ResolvedSlot, so the degraded
    /// reason can distinguish it from a missing key. (The keychain-ACL error
    /// origin itself has no test seam in the mock; this pins the plumbing.)
    #[test]
    fn test_unreadable_flag_propagates_to_resolved_slot() {
        let keys = CachedKeys {
            unreadable: std::collections::HashSet::from([Provider::Gemini]),
            ..Default::default()
        };
        assert!(keys.is_unreadable(Provider::Gemini));
        assert!(!keys.is_unreadable(Provider::Claude));

        let slot = crate::ResolvedSlot::resolve(
            &crate::config::ModelSlotConfig {
                provider: "gemini".to_string(),
                model: "m".to_string(),
                base_url: None,
            },
            &Some(keys),
        );
        assert!(slot.key_unreadable);
        assert!(slot.key.is_none());
    }

    #[test]
    fn test_get_api_keys_caching_and_latency() {
        let _lock = env_test_lock();
        let _env_guard = TestEnvGuard::new();

        // Mock cache state
        if let Ok(mut cache) = get_key_cache().write() {
            *cache = Some(CachedKeys {
                keys: HashMap::from([
                    (Provider::Gemini, "cached_gemini".to_string()),
                    (Provider::Claude, "cached_claude".to_string()),
                ]),
                ..Default::default()
            });
        }

        // Measure latency
        let start = std::time::Instant::now();
        let keys = get_api_keys().unwrap();
        let duration = start.elapsed();

        assert_eq!(keys.get(Provider::Gemini), Some("cached_gemini"));
        assert_eq!(keys.get(Provider::Claude), Some("cached_claude"));

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

        // T13 addendum 8: with the unconditional cfg!(test) mock, `set_gemini_key`
        // below always succeeds — but `load_keys_from_source` only ever READS
        // the keychain (mock included) when `use_keychain` is true.
        //
        // Two distinct reasons the gate can be closed, handled differently:
        //   1. A `MURSHID_NO_KEYCHAIN` / `MURSHID_BYPASS_KEYCHAIN` override in
        //      the *ambient* environment (a legitimate developer/CI config that
        //      disables the OS keychain). The keychain path genuinely can't be
        //      exercised, so SKIP — panicking here would make the whole suite
        //      unrunnable for anyone who sets that var in their shell.
        //   2. `api_key_source` resolving to something other than "keychain".
        //      That's an unexpected test-setup problem, so still ASSERT so a
        //      failure self-diagnoses rather than reporting a confusing
        //      "key not found".
        let config = crate::config::load_config();
        if std::env::var("MURSHID_NO_KEYCHAIN").is_ok()
            || std::env::var("MURSHID_BYPASS_KEYCHAIN").is_ok()
        {
            println!(
                "Skipping keychain path test: disabled by ambient env \
                 (MURSHID_NO_KEYCHAIN={:?}, MURSHID_BYPASS_KEYCHAIN={:?})",
                std::env::var("MURSHID_NO_KEYCHAIN"),
                std::env::var("MURSHID_BYPASS_KEYCHAIN"),
            );
            return;
        }
        assert_eq!(
            config.provider.api_key_source, "keychain",
            "use_keychain gate must be true for this test to exercise the keychain path \
             (api_key_source={:?})",
            config.provider.api_key_source,
        );

        // Since OS keychain might fail if unlocked/non-interactive, we handle failure gracefully
        // but assert that if they succeed, they are cached correctly.
        let test_key = "keyring_test_key_123";

        match set_gemini_key(test_key) {
            Ok(_) => {
                // If it succeeds, verify it gets loaded
                let keys = load_keys_from_source();
                assert_eq!(keys.get(Provider::Gemini), Some(test_key));

                // Delete it and verify it's gone
                assert!(delete_gemini_key().is_ok());
                let keys_after = load_keys_from_source();
                assert!(keys_after.get(Provider::Gemini).is_none());
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

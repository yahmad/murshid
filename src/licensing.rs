use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub const EMBEDDED_PUBLIC_KEY: [u8; 32] = [
    33, 82, 248, 209, 155, 121, 29, 36, 69, 50, 66, 225, 95, 46, 171, 108, 183, 207, 250, 123, 106,
    94, 211, 0, 151, 150, 14, 6, 152, 129, 219, 18,
];

fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    if s.len() % 2 != 0 {
        return Err("Hex string must have an even length".to_string());
    }
    let mut bytes = Vec::with_capacity(s.len() / 2);
    let chars: Vec<char> = s.chars().collect();
    for i in (0..s.len()).step_by(2) {
        let high = chars[i].to_digit(16).ok_or("Invalid hex character")? as u8;
        let low = chars[i + 1].to_digit(16).ok_or("Invalid hex character")? as u8;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

pub fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

fn get_fallback_lease_path() -> PathBuf {
    crate::config::get_home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(".murshid")
        .join("license.lease")
}

fn get_last_checkin_path() -> PathBuf {
    crate::config::get_home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(".murshid")
        .join("last_checkin.txt")
}

#[cfg(unix)]
fn set_file_permissions_0600(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn set_file_permissions_0600(_path: &Path) {}

pub fn write_cached_lease(lease_token: &str) -> Result<(), String> {
    let service_name = if std::env::var("MURSHID_TESTING").is_ok() {
        "murshid_test"
    } else {
        "murshid"
    };
    match crate::credentials::set_credential(service_name, "license_lease", lease_token) {
        Ok(_) => {
            let current_time = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let _ = crate::credentials::set_credential(
                service_name,
                "last_checkin_time",
                &current_time.to_string(),
            );
            return Ok(());
        }
        Err(e) => {
            eprintln!(
                "[WARNING] Keyring storage failed: {}. Falling back to 0600 file cache.",
                e
            );
        }
    }

    let path = get_fallback_lease_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(&path, lease_token).map_err(|e| format!("Failed to write lease file: {}", e))?;
    set_file_permissions_0600(&path);

    let current_time = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let checkin_path = get_last_checkin_path();
    let _ = fs::write(&checkin_path, current_time.to_string());
    set_file_permissions_0600(&checkin_path);

    Ok(())
}

pub fn get_tenant_id_from_configs() -> Option<String> {
    let paths = vec![
        crate::config::get_project_config_path(),
        crate::config::resolve_user_config_path(),
        Some(crate::config::get_system_config_path()),
    ];
    for path in paths.into_iter().flatten() {
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                let parsed = crate::config::parse_toml(&content);
                for sec_map in parsed.values() {
                    if let Some(tenant_id) = sec_map.get("tenant_id") {
                        return Some(tenant_id.trim_matches('"').to_string());
                    }
                }
            }
        }
    }
    None
}

pub fn verify_license(email_or_token: &str, is_online: bool) -> Result<(), String> {
    if let Some(expected_tenant_id) = get_tenant_id_from_configs() {
        let lease_path = Path::new("license.lease");
        let lease_content = if lease_path.exists() {
            fs::read_to_string(lease_path)
                .map_err(|e| format!("Failed to read dark-site lease: {}", e))?
        } else {
            let fallback_path = get_fallback_lease_path();
            if fallback_path.exists() {
                fs::read_to_string(fallback_path)
                    .map_err(|e| format!("Failed to read dark-site lease: {}", e))?
            } else {
                return Err("Dark-site lease file 'license.lease' not found".to_string());
            }
        };

        return verify_dark_site_lease(&lease_content, &expected_tenant_id);
    }

    let service_name = if std::env::var("MURSHID_TESTING").is_ok() {
        "murshid_test"
    } else {
        "murshid"
    };
    let mut lease_token = None;
    match crate::credentials::get_credential(service_name, "license_lease") {
        Ok(t) => lease_token = Some(t),
        Err(_) => {
            let path = get_fallback_lease_path();
            if path.exists() {
                if let Ok(t) = fs::read_to_string(&path) {
                    lease_token = Some(t.trim().to_string());
                }
            }
        }
    }

    let token = lease_token.ok_or_else(|| "No cached lease token found".to_string())?;

    let parts: Vec<&str> = token.split(':').collect();
    if parts.len() != 4 {
        return Err("Invalid lease format".to_string());
    }
    let expiration_str = parts[0];
    let tier = parts[1];
    let account_hash = parts[2];
    let signature_hex = parts[3];

    let msg = format!("{}:{}:{}", expiration_str, tier, account_hash);
    verify_signature_payload(&msg, signature_hex)?;

    let expected_hash = crate::pedagogy::sha256(email_or_token.as_bytes());
    if account_hash != expected_hash {
        return Err("Account email/token hash mismatch".to_string());
    }

    let expiration: u64 = expiration_str
        .parse()
        .map_err(|_| "Invalid expiration timestamp".to_string())?;
    let current_time = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let is_trial = tier == "trial" || tier == "pro_trial";

    if is_online {
        if current_time >= expiration {
            return Err("Subscription license has expired".to_string());
        }
        let _ = crate::credentials::set_credential(
            service_name,
            "last_checkin_time",
            &current_time.to_string(),
        );
        let checkin_path = get_last_checkin_path();
        let _ = fs::write(&checkin_path, current_time.to_string());
        set_file_permissions_0600(&checkin_path);
    } else {
        if is_trial {
            return Err("Trial subscriptions require active internet connection".to_string());
        }

        let mut last_checkin = None;
        if let Ok(t_str) = crate::credentials::get_credential(service_name, "last_checkin_time") {
            if let Ok(t) = t_str.parse::<u64>() {
                last_checkin = Some(t);
            }
        }
        if last_checkin.is_none() {
            let checkin_path = get_last_checkin_path();
            if checkin_path.exists() {
                if let Ok(t_str) = fs::read_to_string(&checkin_path) {
                    if let Ok(t) = t_str.trim().parse::<u64>() {
                        last_checkin = Some(t);
                    }
                }
            }
        }

        let last_checkin_time =
            last_checkin.ok_or_else(|| "No cached last online check-in time found".to_string())?;

        if current_time < last_checkin_time {
            return Err("Clock rollback detected".to_string());
        }

        let elapsed = current_time.saturating_sub(last_checkin_time);
        if elapsed > 14 * 24 * 3600 {
            return Err("Offline grace period of 14 days exceeded".to_string());
        }

        if current_time >= expiration {
            return Err("Subscription license has expired".to_string());
        }
    }

    Ok(())
}

pub fn verify_dark_site_lease(lease_content: &str, expected_tenant_id: &str) -> Result<(), String> {
    let parts: Vec<&str> = lease_content.trim().split(':').collect();
    if parts.len() != 3 {
        return Err("Invalid dark-site lease format".to_string());
    }
    let expiration_str = parts[0];
    let tenant_id = parts[1];
    let signature_hex = parts[2];

    if tenant_id != expected_tenant_id {
        return Err("Tenant ID mismatch in dark-site lease".to_string());
    }

    let msg = format!("{}:{}", expiration_str, tenant_id);
    verify_signature_payload(&msg, signature_hex)?;

    let expiration: u64 = expiration_str
        .parse()
        .map_err(|_| "Invalid expiration timestamp".to_string())?;
    let current_time = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    if current_time >= expiration {
        return Err("Dark-site lease has expired".to_string());
    }

    Ok(())
}

fn verify_signature_payload(msg: &str, signature_hex: &str) -> Result<(), String> {
    let public_key = VerifyingKey::from_bytes(&EMBEDDED_PUBLIC_KEY)
        .map_err(|e| format!("Invalid embedded public key: {}", e))?;

    let signature_bytes = decode_hex(signature_hex)?;
    let signature =
        Signature::from_slice(&signature_bytes).map_err(|e| format!("Invalid signature: {}", e))?;

    public_key
        .verify(msg.as_bytes(), &signature)
        .map_err(|e| format!("Ed25519 signature verification failed: {}", e))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use std::sync::Mutex;

    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    struct TestHomeGuard {
        old_home: Option<String>,
        temp_dir: PathBuf,
    }

    impl TestHomeGuard {
        fn new() -> Self {
            let temp_dir = std::env::temp_dir().join(format!(
                "murshid_license_test_{}",
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_micros()
            ));
            let _ = fs::remove_dir_all(&temp_dir);
            fs::create_dir_all(&temp_dir).unwrap();

            let old_home = std::env::var("HOME").ok();
            unsafe {
                std::env::set_var("HOME", temp_dir.to_str().unwrap());
                std::env::set_var("MURSHID_TESTING", "1");
            }
            Self { old_home, temp_dir }
        }
    }

    impl Drop for TestHomeGuard {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.temp_dir);
            unsafe {
                std::env::remove_var("MURSHID_TESTING");
                if let Some(ref h) = self.old_home {
                    std::env::set_var("HOME", h);
                } else {
                    std::env::remove_var("HOME");
                }
            }
        }
    }

    fn generate_valid_standard_lease(expiration: u64, tier: &str, email_or_token: &str) -> String {
        use ed25519_dalek::Signer;
        let signing_key = SigningKey::from_bytes(&[0x42; 32]);
        let account_hash = crate::pedagogy::sha256(email_or_token.as_bytes());
        let payload = format!("{}:{}:{}", expiration, tier, account_hash);
        let signature = signing_key.sign(payload.as_bytes());
        let signature_hex = encode_hex(&signature.to_bytes());
        format!("{}:{}:{}:{}", expiration, tier, account_hash, signature_hex)
    }

    fn generate_valid_dark_site_lease(expiration: u64, tenant_id: &str) -> String {
        use ed25519_dalek::Signer;
        let signing_key = SigningKey::from_bytes(&[0x42; 32]);
        let payload = format!("{}:{}", expiration, tenant_id);
        let signature = signing_key.sign(payload.as_bytes());
        let signature_hex = encode_hex(&signature.to_bytes());
        format!("{}:{}:{}", expiration, tenant_id, signature_hex)
    }

    fn write_last_checkin_time(last_checkin: u64) {
        let path = get_last_checkin_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, last_checkin.to_string()).unwrap();
    }

    fn write_fallback_lease(content: &str) {
        let path = get_fallback_lease_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, content).unwrap();
    }

    #[test]
    fn test_valid_standard_license_online() {
        let _lock = TEST_MUTEX.lock().unwrap();
        let _guard = TestHomeGuard::new();

        let current_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // 30 days expiration
        let lease =
            generate_valid_standard_lease(current_time + 30 * 24 * 3600, "pro", "user@example.com");
        write_cached_lease(&lease).unwrap();

        let result = verify_license("user@example.com", true);
        assert!(
            result.is_ok(),
            "Expected valid license to verify online: {:?}",
            result
        );
    }

    #[test]
    fn test_expired_license_online() {
        let _lock = TEST_MUTEX.lock().unwrap();
        let _guard = TestHomeGuard::new();

        let current_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Expired 1 hour ago
        let lease = generate_valid_standard_lease(current_time - 3600, "pro", "user@example.com");
        write_cached_lease(&lease).unwrap();

        let result = verify_license("user@example.com", true);
        assert!(result.is_err(), "Expected expired license to fail");
        assert!(result.err().unwrap().contains("expired"));
    }

    #[test]
    fn test_mismatched_email_standard_license() {
        let _lock = TEST_MUTEX.lock().unwrap();
        let _guard = TestHomeGuard::new();

        let current_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let lease =
            generate_valid_standard_lease(current_time + 30 * 24 * 3600, "pro", "user@example.com");
        write_cached_lease(&lease).unwrap();

        let result = verify_license("different@example.com", true);
        assert!(result.is_err());
        assert!(result.err().unwrap().contains("hash mismatch"));
    }

    #[test]
    fn test_paid_grace_period_offline() {
        let _lock = TEST_MUTEX.lock().unwrap();
        let _guard = TestHomeGuard::new();

        let current_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // 30 days expiration, check-in 5 days ago (well within 14-day grace)
        let lease =
            generate_valid_standard_lease(current_time + 30 * 24 * 3600, "pro", "user@example.com");
        write_cached_lease(&lease).unwrap();

        // Overwrite the last checkin time to 5 days ago
        let last_checkin = current_time - 5 * 24 * 3600;
        let service_name = "murshid_test";
        crate::credentials::set_credential(
            service_name,
            "last_checkin_time",
            &last_checkin.to_string(),
        )
        .unwrap();
        write_last_checkin_time(last_checkin);

        let result = verify_license("user@example.com", false);
        assert!(
            result.is_ok(),
            "Expected offline verification to succeed under grace period: {:?}",
            result
        );
    }

    #[test]
    fn test_paid_grace_period_exceeded_offline() {
        let _lock = TEST_MUTEX.lock().unwrap();
        let _guard = TestHomeGuard::new();

        let current_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // 30 days expiration, check-in 15 days ago (grace period is 14 days)
        let lease =
            generate_valid_standard_lease(current_time + 30 * 24 * 3600, "pro", "user@example.com");
        write_cached_lease(&lease).unwrap();

        let last_checkin = current_time - 15 * 24 * 3600;
        let service_name = "murshid_test";
        crate::credentials::set_credential(
            service_name,
            "last_checkin_time",
            &last_checkin.to_string(),
        )
        .unwrap();
        write_last_checkin_time(last_checkin);

        let result = verify_license("user@example.com", false);
        assert!(
            result.is_err(),
            "Expected offline verification to fail if grace exceeded"
        );
        assert!(
            result
                .err()
                .unwrap()
                .contains("grace period of 14 days exceeded")
        );
    }

    #[test]
    fn test_clock_rollback_protection() {
        let _lock = TEST_MUTEX.lock().unwrap();
        let _guard = TestHomeGuard::new();

        let current_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let lease =
            generate_valid_standard_lease(current_time + 30 * 24 * 3600, "pro", "user@example.com");
        write_cached_lease(&lease).unwrap();

        // Checkin time is 1 hour in the future (rolled back clock)
        let last_checkin = current_time + 3600;
        let service_name = "murshid_test";
        crate::credentials::set_credential(
            service_name,
            "last_checkin_time",
            &last_checkin.to_string(),
        )
        .unwrap();
        write_last_checkin_time(last_checkin);

        let result = verify_license("user@example.com", false);
        assert!(result.is_err(), "Expected rollback clock to fail-fast");
        assert!(result.err().unwrap().contains("Clock rollback detected"));
    }

    #[test]
    fn test_trial_requires_active_online() {
        let _lock = TEST_MUTEX.lock().unwrap();
        let _guard = TestHomeGuard::new();

        let current_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Trial license
        let lease = generate_valid_standard_lease(
            current_time + 30 * 24 * 3600,
            "trial",
            "user@example.com",
        );
        write_cached_lease(&lease).unwrap();

        // Verification online should pass
        assert!(verify_license("user@example.com", true).is_ok());

        // Verification offline should fail
        let result = verify_license("user@example.com", false);
        assert!(result.is_err());
        assert!(
            result
                .err()
                .unwrap()
                .contains("Trial subscriptions require active internet connection")
        );
    }

    #[test]
    fn test_dark_site_matching_tenant() {
        let _lock = TEST_MUTEX.lock().unwrap();
        let _guard = TestHomeGuard::new();

        let current_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let lease =
            generate_valid_dark_site_lease(current_time + 30 * 24 * 3600, "enterprise_corp");

        // Write mock config.toml to temporary home
        let user_config_path = crate::config::resolve_user_config_path().unwrap();
        if let Some(parent) = user_config_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(
            &user_config_path,
            "
[licensing]
tenant_id = \"enterprise_corp\"
",
        )
        .unwrap();

        // Write lease to license.lease file in project root (or fallback)
        write_fallback_lease(&lease);

        let result = verify_license("any_user", false);
        assert!(
            result.is_ok(),
            "Expected dark-site lease to verify: {:?}",
            result
        );
    }

    #[test]
    fn test_dark_site_mismatched_tenant() {
        let _lock = TEST_MUTEX.lock().unwrap();
        let _guard = TestHomeGuard::new();

        let current_time = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let lease =
            generate_valid_dark_site_lease(current_time + 30 * 24 * 3600, "enterprise_corp");

        let user_config_path = crate::config::resolve_user_config_path().unwrap();
        if let Some(parent) = user_config_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(
            &user_config_path,
            "
[licensing]
tenant_id = \"other_corp\"
",
        )
        .unwrap();

        write_fallback_lease(&lease);

        let result = verify_license("any_user", false);
        assert!(result.is_err());
        assert!(result.err().unwrap().contains("Tenant ID mismatch"));
    }
}

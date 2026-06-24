//! Team Digest Signature Verifier
//!
//! Validates HMAC-SHA256 signatures on exported team progress digests.
//! Supports key rotation by accepting a comma-separated list of environment
//! variable names, each containing a candidate HMAC secret. Keys are tried
//! in declared order (newest first), and the digest is accepted if any key
//! produces a valid signature.
//!
//! Digests with missing, malformed, or invalid signatures are silently
//! rejected to prevent metric injection or spoofing.

/// Verify a digest's HMAC signature using keys loaded from environment variables.
///
/// `hmac_secret_env_list` is a comma-separated string of environment variable
/// names (e.g. `"MURSHID_HMAC_KEY_V2, MURSHID_HMAC_KEY_V1"`). Each variable
/// that exists in the environment is read and used as a candidate HMAC key.
/// Keys are tried in the order they appear in the list, enabling seamless
/// rotation: place the newest key first so valid digests verify on the first
/// attempt, while digests signed with older keys still pass.
///
/// Returns `Ok(())` if any key validates, `Err` with a descriptive message
/// if no key matches or the JSON is malformed.
pub fn verify_digest_from_env(
    json_content: &str,
    hmac_secret_env_list: &str,
) -> Result<(), String> {
    let mut keys: Vec<Vec<u8>> = Vec::new();
    for env_name in hmac_secret_env_list.split(',') {
        let env_name = env_name.trim();
        if !env_name.is_empty() {
            if let Ok(val) = std::env::var(env_name) {
                keys.push(val.into_bytes());
            }
        }
    }

    if keys.is_empty() {
        return Err("No HMAC keys found in configured environment variables".to_string());
    }

    let key_refs: Vec<&[u8]> = keys.iter().map(|k| k.as_slice()).collect();
    verify_digest_with_keys(json_content, &key_refs)
}

/// Verify a digest's HMAC signature against one or more raw byte keys.
///
/// The verification process:
/// 1. Parses the JSON and extracts the `hmac_signature` field.
/// 2. Rebuilds the canonical (compact, key-sorted) JSON from all remaining fields.
/// 3. Computes HMAC-SHA256 of the canonical JSON with each candidate key.
/// 4. Returns `Ok(())` on the first match, or `Err` if no key validates.
pub fn verify_digest_with_keys(json_content: &str, keys: &[&[u8]]) -> Result<(), String> {
    let parsed: serde_json::Value =
        serde_json::from_str(json_content).map_err(|e| format!("Invalid JSON: {}", e))?;

    let obj = parsed
        .as_object()
        .ok_or_else(|| "Digest JSON is not an object".to_string())?;

    let signature = obj
        .get("hmac_signature")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing hmac_signature field".to_string())?;

    // Reconstruct canonical JSON from all fields except hmac_signature.
    // BTreeMap ensures alphabetical key ordering matches the export step.
    let mut fields: std::collections::BTreeMap<String, serde_json::Value> =
        std::collections::BTreeMap::new();
    for (k, v) in obj {
        if k != "hmac_signature" {
            fields.insert(k.clone(), v.clone());
        }
    }

    let canonical = crate::team_exporter::canonical_json(&fields);

    for key in keys {
        let computed = crate::team_exporter::hmac_sha256(key, canonical.as_bytes());
        if computed == signature {
            return Ok(());
        }
    }

    Err("HMAC signature verification failed against all provided keys".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_verify_valid_digest() {
        let entries = vec![crate::team_exporter::DigestEntry {
            concept_slug: "ownership".to_string(),
            mastery_score: 0.8,
            exposure_count: 5,
        }];

        let (_, content) = crate::team_exporter::export_digest(
            "device1",
            "project1",
            "test@test.com",
            "anonymous",
            "",
            &entries,
            b"my_secret",
        );

        let result = verify_digest_with_keys(&content, &[b"my_secret"]);
        assert!(result.is_ok(), "Expected valid signature: {:?}", result);
    }

    #[test]
    fn test_verify_wrong_key_fails() {
        let entries = vec![];
        let (_, content) = crate::team_exporter::export_digest(
            "d",
            "p",
            "e@e.com",
            "anonymous",
            "",
            &entries,
            b"correct_key",
        );

        let result = verify_digest_with_keys(&content, &[b"wrong_key"]);
        assert!(result.is_err());
        assert!(result.err().unwrap().contains("verification failed"));
    }

    #[test]
    fn test_verify_rotated_keys_priority_order() {
        let entries = vec![crate::team_exporter::DigestEntry {
            concept_slug: "lifetimes".to_string(),
            mastery_score: 0.6,
            exposure_count: 8,
        }];

        // Digest was signed with "old_key"
        let (_, content) = crate::team_exporter::export_digest(
            "d",
            "p",
            "e@e.com",
            "anonymous",
            "",
            &entries,
            b"old_key",
        );

        // Verifier tries "new_key" first, then "old_key" — second key matches
        let result = verify_digest_with_keys(&content, &[b"new_key", b"old_key"]);
        assert!(
            result.is_ok(),
            "Expected rotated key to validate: {:?}",
            result
        );
    }

    #[test]
    fn test_verify_from_env_single_key() {
        unsafe {
            std::env::set_var("MURSHID_TEST_HMAC_SINGLE", "env_secret_value");
        }

        let entries = vec![];
        let (_, content) = crate::team_exporter::export_digest(
            "d",
            "p",
            "e@e.com",
            "anonymous",
            "",
            &entries,
            b"env_secret_value",
        );

        let result = verify_digest_from_env(&content, "MURSHID_TEST_HMAC_SINGLE");
        assert!(
            result.is_ok(),
            "Expected env var key to validate: {:?}",
            result
        );

        unsafe {
            std::env::remove_var("MURSHID_TEST_HMAC_SINGLE");
        }
    }

    #[test]
    fn test_verify_from_env_comma_separated_rotation() {
        unsafe {
            std::env::set_var("MURSHID_TEST_KEY_NEW", "new_rotation_secret");
            std::env::set_var("MURSHID_TEST_KEY_OLD", "old_rotation_secret");
        }

        let entries = vec![];
        // Signed with old key
        let (_, content) = crate::team_exporter::export_digest(
            "d",
            "p",
            "e@e.com",
            "anonymous",
            "",
            &entries,
            b"old_rotation_secret",
        );

        // Verify with comma-separated list (new first, old second)
        let result = verify_digest_from_env(&content, "MURSHID_TEST_KEY_NEW, MURSHID_TEST_KEY_OLD");
        assert!(
            result.is_ok(),
            "Expected second env var to validate: {:?}",
            result
        );

        unsafe {
            std::env::remove_var("MURSHID_TEST_KEY_NEW");
            std::env::remove_var("MURSHID_TEST_KEY_OLD");
        }
    }

    #[test]
    fn test_tampered_digest_fails() {
        let entries = vec![crate::team_exporter::DigestEntry {
            concept_slug: "traits".to_string(),
            mastery_score: 0.5,
            exposure_count: 3,
        }];

        let (_, content) = crate::team_exporter::export_digest(
            "d",
            "p",
            "e@e.com",
            "anonymous",
            "",
            &entries,
            b"secret",
        );

        // Tamper with the mastery score
        let tampered = content.replace("0.5", "0.99");
        let result = verify_digest_with_keys(&tampered, &[b"secret"]);
        assert!(
            result.is_err(),
            "Expected tampered digest to fail verification"
        );
    }

    #[test]
    fn test_missing_signature_field_fails() {
        let json = r#"{"concepts": [], "device_id_hash": "abc"}"#;
        let result = verify_digest_with_keys(json, &[b"key"]);
        assert!(result.is_err());
        assert!(result.err().unwrap().contains("Missing hmac_signature"));
    }

    #[test]
    fn test_invalid_json_fails() {
        let result = verify_digest_with_keys("not valid json at all", &[b"key"]);
        assert!(result.is_err());
        assert!(result.err().unwrap().contains("Invalid JSON"));
    }

    #[test]
    fn test_no_env_keys_found_fails() {
        let result = verify_digest_from_env("{}", "NONEXISTENT_VAR_12345");
        assert!(result.is_err());
        assert!(result.err().unwrap().contains("No HMAC keys found"));
    }
}

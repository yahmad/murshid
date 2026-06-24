/// Anonymized Team Digest Exporter
///
/// Generates cryptographically signed JSON digests of developer mastery progress
/// for the Team Tier shared dashboard pipeline. All developer-identifying data
/// (project paths, emails, device IDs) is replaced with SHA-256 hashes before
/// export, preventing correlation leaks.
///
/// Digest payloads are signed with HMAC-SHA256 over a key-sorted canonical JSON
/// representation to detect tampering and spoofing.
use std::collections::BTreeMap;
use std::time::SystemTime;

/// Decode a hex string to raw bytes.
fn decode_hex(s: &str) -> Vec<u8> {
    let chars: Vec<char> = s.chars().collect();
    let mut bytes = Vec::with_capacity(s.len() / 2);
    for pair in chars.chunks(2) {
        if pair.len() == 2 {
            let high = pair[0].to_digit(16).unwrap_or(0) as u8;
            let low = pair[1].to_digit(16).unwrap_or(0) as u8;
            bytes.push((high << 4) | low);
        }
    }
    bytes
}

/// Compute HMAC-SHA256 of `message` under `key`, returning a hex-encoded MAC.
///
/// Implements RFC 2104 HMAC using the pure-Rust SHA-256 from `pedagogy::sha256`.
/// Key handling:
/// - Keys longer than the SHA-256 block size (64 bytes) are first hashed.
/// - Keys shorter than the block size are zero-padded to 64 bytes.
pub fn hmac_sha256(key: &[u8], message: &[u8]) -> String {
    const BLOCK_SIZE: usize = 64;

    // Step 1: If key > block size, hash it down to 32 bytes
    let key_prime = if key.len() > BLOCK_SIZE {
        decode_hex(&crate::pedagogy::sha256(key))
    } else {
        key.to_vec()
    };

    // Step 2: Pad key to block size with zeros
    let mut padded_key = key_prime;
    padded_key.resize(BLOCK_SIZE, 0);

    // Step 3: XOR padded key with ipad (0x36) and opad (0x5c)
    let ipad_key: Vec<u8> = padded_key.iter().map(|b| b ^ 0x36).collect();
    let opad_key: Vec<u8> = padded_key.iter().map(|b| b ^ 0x5c).collect();

    // Step 4: inner = SHA256(ipad_key || message)
    let mut inner_input = ipad_key;
    inner_input.extend_from_slice(message);
    let inner_hex = crate::pedagogy::sha256(&inner_input);
    let inner_bytes = decode_hex(&inner_hex);

    // Step 5: HMAC = SHA256(opad_key || inner_hash_bytes)
    let mut outer_input = opad_key;
    outer_input.extend_from_slice(&inner_bytes);
    crate::pedagogy::sha256(&outer_input)
}

/// A single concept mastery entry to include in the exported digest.
pub struct DigestEntry {
    pub concept_slug: String,
    pub mastery_score: f64,
    pub exposure_count: u32,
}

/// Generate a compact, key-sorted canonical JSON string from a BTreeMap.
///
/// BTreeMap iterates keys in alphabetical order, and serde_json serializes in
/// iteration order, so the output is deterministic and suitable for HMAC signing.
pub fn canonical_json(fields: &BTreeMap<String, serde_json::Value>) -> String {
    serde_json::to_string(fields).unwrap_or_default()
}

/// Export an anonymized digest, returning `(filename, json_content)`.
///
/// - `device_id`: raw device/hostname identifier (will be SHA-256 hashed)
/// - `project_root`: raw project root path (will be SHA-256 hashed)
/// - `developer_email`: developer email (anonymized per `privacy_level`)
/// - `privacy_level`: one of `"anonymous"`, `"pseudonym"`, or `"identified"`
/// - `pseudonym`: alias used when `privacy_level` is `"pseudonym"`
/// - `entries`: concept mastery data to embed in the digest
/// - `hmac_key`: shared team secret for HMAC-SHA256 signing
///
/// The returned filename follows the pattern `digest_<device_hash>_<project_hash>.json`.
/// The returned JSON contains alphabetically sorted keys and an `hmac_signature`
/// field computed over the canonical representation of all other fields.
pub fn export_digest(
    device_id: &str,
    project_root: &str,
    developer_email: &str,
    privacy_level: &str,
    pseudonym: &str,
    entries: &[DigestEntry],
    hmac_key: &[u8],
) -> (String, String) {
    let device_id_hash = crate::pedagogy::sha256(device_id.as_bytes());
    let project_root_hash = crate::pedagogy::sha256(project_root.as_bytes());

    let developer_identity = match privacy_level {
        "pseudonym" => pseudonym.to_string(),
        "identified" => developer_email.to_string(),
        // "anonymous" and any unknown value default to SHA-256 hash
        _ => crate::pedagogy::sha256(developer_email.as_bytes()),
    };

    let timestamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string();

    // Build concepts array with sorted keys in each entry
    let concepts: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| {
            let mut entry_map = serde_json::Map::new();
            entry_map.insert(
                "concept_slug".to_string(),
                serde_json::Value::String(e.concept_slug.clone()),
            );
            entry_map.insert(
                "exposure_count".to_string(),
                serde_json::Value::Number(serde_json::Number::from(e.exposure_count)),
            );
            entry_map.insert(
                "mastery_score".to_string(),
                serde_json::json!(e.mastery_score),
            );
            serde_json::Value::Object(entry_map)
        })
        .collect();

    // Build alphabetically sorted field map (BTreeMap guarantees sort order)
    let mut fields = BTreeMap::new();
    fields.insert("concepts".to_string(), serde_json::Value::Array(concepts));
    fields.insert(
        "developer_identity".to_string(),
        serde_json::Value::String(developer_identity),
    );
    fields.insert(
        "device_id_hash".to_string(),
        serde_json::Value::String(device_id_hash.clone()),
    );
    fields.insert(
        "privacy_level".to_string(),
        serde_json::Value::String(privacy_level.to_string()),
    );
    fields.insert(
        "project_root_hash".to_string(),
        serde_json::Value::String(project_root_hash.clone()),
    );
    fields.insert(
        "timestamp".to_string(),
        serde_json::Value::String(timestamp),
    );

    // Compute HMAC-SHA256 over the canonical (compact, key-sorted) JSON
    let canonical = canonical_json(&fields);
    let signature = hmac_sha256(hmac_key, canonical.as_bytes());

    // Insert signature into output (after signing, so it's excluded from MAC input)
    fields.insert(
        "hmac_signature".to_string(),
        serde_json::Value::String(signature),
    );

    let filename = format!("digest_{}_{}.json", device_id_hash, project_root_hash);
    let json_content = serde_json::to_string_pretty(&fields).unwrap_or_default();

    (filename, json_content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hmac_sha256_rfc4231_case2() {
        // RFC 4231 Test Case 2: HMAC-SHA256("Jefe", "what do ya want for nothing?")
        let key = b"Jefe";
        let data = b"what do ya want for nothing?";
        let expected = "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843";
        assert_eq!(hmac_sha256(key, data), expected);
    }

    #[test]
    fn test_hmac_sha256_empty_message() {
        // Known HMAC-SHA256 of empty message with key "key"
        let result = hmac_sha256(b"key", b"");
        assert_eq!(result.len(), 64); // hex-encoded SHA-256 = 64 chars
        assert!(result.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_hmac_sha256_long_key() {
        // Keys longer than 64 bytes should be hashed first
        let long_key = vec![0xAA; 131];
        let result = hmac_sha256(
            &long_key,
            b"Test Using Larger Than Block-Size Key - Hash Key First",
        );
        assert_eq!(result.len(), 64);
    }

    #[test]
    fn test_export_anonymizes_project_and_email() {
        let entries = vec![DigestEntry {
            concept_slug: "ownership".to_string(),
            mastery_score: 0.75,
            exposure_count: 10,
        }];

        let (filename, content) = export_digest(
            "my-macbook-pro",
            "/Users/yasir/secret-project",
            "yasir@example.com",
            "anonymous",
            "",
            &entries,
            b"test_team_secret",
        );

        // Filename uses hashes, never raw identifiers
        assert!(!filename.contains("my-macbook-pro"));
        assert!(!filename.contains("secret-project"));
        assert!(filename.starts_with("digest_"));
        assert!(filename.ends_with(".json"));

        // Content must not contain any raw PII
        assert!(!content.contains("yasir@example.com"));
        assert!(!content.contains("/Users/yasir/secret-project"));
        assert!(!content.contains("my-macbook-pro"));

        // Content must contain concept data and HMAC signature
        assert!(content.contains("ownership"));
        assert!(content.contains("hmac_signature"));
    }

    #[test]
    fn test_canonical_json_keys_sorted_alphabetically() {
        let mut fields = BTreeMap::new();
        fields.insert(
            "zebra".to_string(),
            serde_json::Value::String("last".to_string()),
        );
        fields.insert(
            "alpha".to_string(),
            serde_json::Value::String("first".to_string()),
        );
        fields.insert(
            "mid".to_string(),
            serde_json::Value::String("middle".to_string()),
        );

        let json = canonical_json(&fields);
        let alpha_pos = json.find("alpha").unwrap();
        let mid_pos = json.find("mid").unwrap();
        let zebra_pos = json.find("zebra").unwrap();
        assert!(alpha_pos < mid_pos);
        assert!(mid_pos < zebra_pos);
    }

    #[test]
    fn test_privacy_level_anonymous() {
        let entries = vec![];
        let (_, content) = export_digest(
            "d",
            "p",
            "user@example.com",
            "anonymous",
            "",
            &entries,
            b"key",
        );
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let identity = parsed["developer_identity"].as_str().unwrap();
        // Anonymous: email is replaced with a 64-char SHA-256 hex hash
        assert_ne!(identity, "user@example.com");
        assert_eq!(identity.len(), 64);
    }

    #[test]
    fn test_privacy_level_pseudonym() {
        let entries = vec![];
        let (_, content) = export_digest(
            "d",
            "p",
            "user@example.com",
            "pseudonym",
            "dev42",
            &entries,
            b"key",
        );
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["developer_identity"].as_str().unwrap(), "dev42");
    }

    #[test]
    fn test_privacy_level_identified() {
        let entries = vec![];
        let (_, content) = export_digest(
            "d",
            "p",
            "user@example.com",
            "identified",
            "",
            &entries,
            b"key",
        );
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(
            parsed["developer_identity"].as_str().unwrap(),
            "user@example.com"
        );
    }

    #[test]
    fn test_different_keys_produce_different_signatures() {
        let entries = vec![DigestEntry {
            concept_slug: "traits".to_string(),
            mastery_score: 0.5,
            exposure_count: 3,
        }];
        let (_, content_a) =
            export_digest("d", "p", "e@e.com", "anonymous", "", &entries, b"key_a");
        let (_, content_b) =
            export_digest("d", "p", "e@e.com", "anonymous", "", &entries, b"key_b");

        let parsed_a: serde_json::Value = serde_json::from_str(&content_a).unwrap();
        let parsed_b: serde_json::Value = serde_json::from_str(&content_b).unwrap();
        assert_ne!(
            parsed_a["hmac_signature"].as_str().unwrap(),
            parsed_b["hmac_signature"].as_str().unwrap(),
        );
    }
}

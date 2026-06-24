use ed25519_dalek::{Signer, SigningKey};
use std::fs;
use std::time::SystemTime;

pub fn run_container_token() -> Result<(), String> {
    let tier = crate::licensing::get_host_license_tier()?;
    if tier != "pro" && tier != "pro_trial" && tier != "enterprise" {
        return Err("Host does not have a validated Pro or Enterprise license".to_string());
    }

    let expiration = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        + 7 * 24 * 3600; // 7 days

    let msg = format!("{}:{}", tier, expiration);
    let signing_key = SigningKey::from_bytes(&[0x42; 32]);
    let signature = signing_key.sign(msg.as_bytes());
    let signature_hex = crate::licensing::encode_hex(&signature.to_bytes());

    let token = format!("{}:{}:{}", tier, expiration, signature_hex);

    let project_root = crate::licensing::get_project_root();
    let path = project_root.join(".murshid_container_license");

    fs::write(&path, &token).map_err(|e| format!("Failed to write container token file: {}", e))?;

    // Set 0600 permissions
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }

    println!("Container token generated successfully at {:?}", path);
    Ok(())
}

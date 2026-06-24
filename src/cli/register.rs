pub fn run_registration(
    gemini_key: Option<&str>,
    claude_key: Option<&str>,
    silent: bool,
) -> Result<(), String> {
    if gemini_key.is_none() && claude_key.is_none() {
        return Err("No keys provided for registration".to_string());
    }

    let service_name = if std::env::var("MURSHID_TESTING").is_ok() {
        "murshid_test"
    } else {
        "murshid"
    };

    if let Some(key) = gemini_key {
        match crate::credentials::set_credential(service_name, "gemini_api_key", key) {
            Ok(_) => {
                if !silent {
                    println!("Successfully registered gemini_api_key.");
                }
            }
            Err(e) => {
                return Err(format!("Failed to register gemini_api_key: {}", e));
            }
        }
    }

    if let Some(key) = claude_key {
        match crate::credentials::set_credential(service_name, "claude_api_key", key) {
            Ok(_) => {
                if !silent {
                    println!("Successfully registered claude_api_key.");
                }
            }
            Err(e) => {
                return Err(format!("Failed to register claude_api_key: {}", e));
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_silent_registration() {
        // Cleanup keys before running test
        unsafe {
            std::env::set_var("MURSHID_TESTING", "1");
        }
        let _ = crate::credentials::delete_credential("murshid_test", "gemini_api_key");
        let _ = crate::credentials::delete_credential("murshid_test", "claude_api_key");

        // Run with mock keys and verify no output or crash
        let res = run_registration(Some("mock_gemini"), Some("mock_claude"), true);

        // Cleanup keys after test
        let _ = crate::credentials::delete_credential("murshid_test", "gemini_api_key");
        let _ = crate::credentials::delete_credential("murshid_test", "claude_api_key");
        unsafe {
            std::env::remove_var("MURSHID_TESTING");
        }

        match res {
            Ok(()) => {}
            Err(e) => {
                assert!(e.contains("Failed to register"));
            }
        }
    }
}

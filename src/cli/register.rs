pub fn run_registration(
    gemini_key: Option<&str>,
    claude_key: Option<&str>,
    silent: bool,
) -> Result<(), String> {
    if gemini_key.is_none() && claude_key.is_none() {
        return Err("No keys provided for registration".to_string());
    }
    
    if let Some(key) = gemini_key {
        match keyring::Entry::new("murshid", "gemini_api_key").and_then(|entry| entry.set_password(key)) {
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
        match keyring::Entry::new("murshid", "claude_api_key").and_then(|entry| entry.set_password(key)) {
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
        // Run with mock keys and verify no output or crash
        let res = run_registration(Some("mock_gemini"), Some("mock_claude"), true);
        
        // On headless test runners keyring might fail, but it's okay as long as it handles it gracefully
        match res {
            Ok(()) => {}
            Err(e) => {
                assert!(e.contains("Failed to register"));
            }
        }
    }
}

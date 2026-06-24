use std::fs::{self, OpenOptions};
use std::io::{Write, BufRead, BufReader};
use std::path::{Path, PathBuf};

pub const EXIT_OK: i32 = 0;
pub const EXIT_CONFIG_ERROR: i32 = 78;
pub const EXIT_DEPENDENCY_ERROR: i32 = 69;
pub const EXIT_IO_ERROR: i32 = 74;

pub fn get_trace_logs_dir() -> Option<PathBuf> {
    crate::config::get_home_dir().map(|h| {
        #[cfg(target_os = "macos")]
        {
            h.join("Library/Logs/murshid")
        }
        #[cfg(target_os = "windows")]
        {
            h.join("AppData\\Local\\murshid\\Logs")
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            h.join(".cache/murshid/log")
        }
    })
}

pub fn run_setup(project_root: &Path) -> Result<(), String> {
    let env_path = project_root.join(".env");
    let mut gemini_key = None;
    let mut claude_key = None;
    
    if env_path.exists() {
        if let Ok(file) = fs::File::open(&env_path) {
            let reader = BufReader::new(file);
            for line in reader.lines().flatten() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some(pos) = line.find('=') {
                    let key = line[..pos].trim();
                    let val = line[pos + 1..].trim().trim_matches('"').trim_matches('\'').trim().to_string();
                    if key == "GEMINI_API_KEY" || key == "MURSHID_GEMINI_API_KEY" {
                        gemini_key = Some(val);
                    } else if key == "CLAUDE_API_KEY" || key == "ANTHROPIC_API_KEY" || key == "MURSHID_CLAUDE_API_KEY" {
                        claude_key = Some(val);
                    }
                }
            }
        }
    }
    
    if let Some(ref key) = gemini_key {
        if let Err(e) = keyring::Entry::new("murshid", "gemini_api_key").and_then(|entry| entry.set_password(key)) {
            eprintln!("[WARNING] Failed to store gemini_api_key in keyring: {}", e);
        }
    }
    if let Some(ref key) = claude_key {
        if let Err(e) = keyring::Entry::new("murshid", "claude_api_key").and_then(|entry| entry.set_password(key)) {
            eprintln!("[WARNING] Failed to store claude_api_key in keyring: {}", e);
        }
    }
    
    let gitignore_path = project_root.join(".gitignore");
    let pattern = ".murshid/";
    let mut needs_append = true;
    if gitignore_path.exists() {
        if let Ok(content) = fs::read_to_string(&gitignore_path) {
            if content.lines().any(|l| l.trim() == pattern || l.trim() == ".murshid") {
                needs_append = false;
            }
        }
    }
    
    if needs_append {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&gitignore_path)
            .map_err(|e| format!("Failed to open .gitignore: {}", e))?;
            
        writeln!(file, "\n# Murshid local project config\n{}", pattern)
            .map_err(|e| format!("Failed to write to .gitignore: {}", e))?;
    }
    
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_trace_logs_dir() {
        let dir = get_trace_logs_dir();
        assert!(dir.is_some());
    }

    #[test]
    fn test_setup_gitignore_and_env() {
        let temp_dir = std::env::temp_dir();
        let test_root = temp_dir.join("murshid_setup_test");
        let _ = fs::remove_dir_all(&test_root);
        fs::create_dir_all(&test_root).unwrap();

        // Write a mock .env file
        let env_path = test_root.join(".env");
        fs::write(&env_path, "GEMINI_API_KEY=\"mock-gemini-key\"\nCLAUDE_API_KEY='mock-claude-key'\n").unwrap();

        // Write initial .gitignore
        let gitignore_path = test_root.join(".gitignore");
        fs::write(&gitignore_path, "/target\n").unwrap();

        run_setup(&test_root).unwrap();

        // Verify gitignore has been updated
        let updated_gitignore = fs::read_to_string(&gitignore_path).unwrap();
        assert!(updated_gitignore.contains(".murshid/"));

        // Cleanup
        let _ = fs::remove_dir_all(&test_root);
    }
}

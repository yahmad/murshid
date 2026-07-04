//! Progress backup and corruption recovery for the SQLite store: serializes
//! durable concept-memory to a side file (and, on macOS, the defaults
//! system) so a corrupt DB can be rebuilt without losing mastery state.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct ProgressBackup {
    pub concepts: HashMap<String, f64>,
}

#[cfg(target_os = "macos")]
fn write_platform_backup(json: &str) -> Result<(), String> {
    use std::process::Command;
    // T9 req 2: `defaults write` without a type flag sniffs the value and
    // parses a leading '{' as old-style plist syntax, which fails for our
    // JSON payload. `-string` forces it to store the JSON blob verbatim.
    let status = Command::new("defaults")
        .args(["write", "com.thabit.murshid", "progress", "-string", json])
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("defaults write command failed".to_string())
    }
}

#[cfg(target_os = "macos")]
fn read_platform_backup() -> Result<String, String> {
    use std::process::Command;
    let output = Command::new("defaults")
        .args(["read", "com.thabit.murshid", "progress"])
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        let s = String::from_utf8(output.stdout).map_err(|e| e.to_string())?;
        Ok(s.trim().to_string())
    } else {
        Err("defaults read command failed or value not found".to_string())
    }
}

#[cfg(target_os = "windows")]
fn write_platform_backup(json: &str) -> Result<(), String> {
    use std::process::Command;
    let status = Command::new("reg")
        .args([
            "add",
            "HKCU\\Software\\Thabit\\Murshid",
            "/v",
            "progress",
            "/t",
            "REG_SZ",
            "/d",
            json,
            "/f",
        ])
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("reg add command failed".to_string())
    }
}

#[cfg(target_os = "windows")]
fn read_platform_backup() -> Result<String, String> {
    use std::process::Command;
    let output = Command::new("reg")
        .args(["query", "HKCU\\Software\\Thabit\\Murshid", "/v", "progress"])
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        let raw = String::from_utf8(output.stdout).map_err(|e| e.to_string())?;
        if let Some(pos) = raw.find("REG_SZ") {
            let val = raw[pos + 6..].trim().to_string();
            Ok(val)
        } else {
            Err("Failed to parse registry query output".to_string())
        }
    } else {
        Err("reg query command failed or value not found".to_string())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn get_linux_backup_path() -> Option<PathBuf> {
    crate::config::get_home_dir().map(|h| h.join(".local/share/murshid/.state_backup"))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn write_platform_backup(json: &str) -> Result<(), String> {
    let path = get_linux_backup_path().ok_or_else(|| "Failed to find user home dir".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, json).map_err(|e| e.to_string())?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path)
            .map_err(|e| e.to_string())?
            .permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&path, perms).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn read_platform_backup() -> Result<String, String> {
    let path = get_linux_backup_path().ok_or_else(|| "Failed to find user home dir".to_string())?;
    if !path.exists() {
        return Err("Linux state backup file not found".to_string());
    }
    let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    Ok(content.trim().to_string())
}

use std::cell::RefCell;

thread_local! {
    static TEST_BACKUP_PATH: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

pub fn set_test_backup_path(path: Option<PathBuf>) {
    TEST_BACKUP_PATH.with(|p| {
        *p.borrow_mut() = path;
    });
}

pub fn write_backup(backup: &ProgressBackup) -> Result<(), String> {
    let test_path = TEST_BACKUP_PATH.with(|p| p.borrow().clone());
    if let Some(test_path) = test_path {
        let json = serde_json::to_string(backup).map_err(|e| e.to_string())?;
        std::fs::write(&test_path, json).map_err(|e| e.to_string())?;
        return Ok(());
    }

    let json = serde_json::to_string(backup).map_err(|e| e.to_string())?;
    write_platform_backup(&json)
}

pub fn read_backup() -> Result<ProgressBackup, String> {
    let test_path = TEST_BACKUP_PATH.with(|p| p.borrow().clone());
    if let Some(test_path) = test_path {
        if !test_path.exists() {
            return Err("Test backup file not found".to_string());
        }
        let json = std::fs::read_to_string(&test_path).map_err(|e| e.to_string())?;
        let backup: ProgressBackup = serde_json::from_str(&json).map_err(|e| e.to_string())?;
        return Ok(backup);
    }

    let json = read_platform_backup()?;
    let backup: ProgressBackup = serde_json::from_str(&json).map_err(|e| e.to_string())?;
    Ok(backup)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backup_serialization() {
        let temp_dir = std::env::temp_dir();
        let test_file = temp_dir.join("test_murshid_backup.json");

        set_test_backup_path(Some(test_file.clone()));

        let mut concepts = HashMap::new();
        concepts.insert("ownership".to_string(), 0.75);
        concepts.insert("lifetimes".to_string(), 0.30);
        let backup = ProgressBackup { concepts };

        // Write backup
        assert!(write_backup(&backup).is_ok());

        // Read backup
        let loaded = read_backup().unwrap();
        assert_eq!(loaded, backup);

        std::fs::remove_file(&test_file).unwrap();
        set_test_backup_path(None);
    }

    // T9 req 2 acceptance: verifies the real `defaults write ... -string`
    // path round-trips (the missing `-string` flag previously made
    // `defaults write` silently fail to store a leading-'{' JSON blob).
    // Skips gracefully when `defaults` isn't runnable, e.g. non-macOS or a
    // sandboxed environment without the user defaults system.
    #[cfg(target_os = "macos")]
    #[test]
    fn test_backup_roundtrip_via_defaults_command() {
        if std::process::Command::new("defaults")
            .arg("help")
            .output()
            .is_err()
        {
            eprintln!(
                "skipping test_backup_roundtrip_via_defaults_command: `defaults` unavailable"
            );
            return;
        }

        // Preserve whatever was already stored so this test doesn't clobber
        // a real progress backup on the developer's machine.
        let previous = read_platform_backup().ok();

        let mut concepts = HashMap::new();
        concepts.insert("closures".to_string(), 0.42);
        let backup = ProgressBackup { concepts };
        let json = serde_json::to_string(&backup).unwrap();

        assert!(
            write_platform_backup(&json).is_ok(),
            "defaults write should succeed with -string"
        );
        let read_back =
            read_platform_backup().expect("defaults read should return the written value");
        let loaded: ProgressBackup = serde_json::from_str(&read_back).unwrap();
        assert_eq!(loaded, backup);

        // Restore prior state (best effort) so this test leaves no residue.
        match previous {
            Some(prev_json) => {
                let _ = write_platform_backup(&prev_json);
            }
            None => {
                let _ = std::process::Command::new("defaults")
                    .args(["delete", "com.thabit.murshid", "progress"])
                    .status();
            }
        }
    }
}

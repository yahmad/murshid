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
    let status = Command::new("defaults")
        .args(["write", "com.thabit.murshid", "progress", json])
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
}

use rusqlite::{Connection, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::thread::sleep;

pub fn get_db_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        crate::config::get_home_dir().map(|h| h.join("Library/Application Support/murshid/profile.db"))
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var("APPDATA")
            .ok()
            .map(|a| PathBuf::from(a).join("murshid\\profile.db"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        crate::config::get_home_dir().map(|h| h.join(".local/share/murshid/profile.db"))
    }
}

pub fn open_connection<P: AsRef<Path>>(path: P) -> Result<Connection> {
    let conn = Connection::open(path)?;
    
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.busy_timeout(Duration::from_millis(5000))?;
    
    // Catch early database corruption by running integrity check
    let check: String = conn.query_row("PRAGMA integrity_check(1);", [], |row| row.get(0))?;
    if check != "ok" {
        return Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
            Some(format!("Integrity check failed: {}", check)),
        ));
    }
    
    Ok(conn)
}

pub fn execute_with_retry<F, T>(mut f: F) -> Result<T, rusqlite::Error>
where
    F: FnMut() -> Result<T, rusqlite::Error>
{
    let mut backoff = Duration::from_millis(50);
    let max_backoff = Duration::from_millis(5000);
    
    loop {
        match f() {
            Ok(val) => return Ok(val),
            Err(e) => {
                if is_busy_error(&e) {
                    if backoff >= max_backoff {
                        return Err(e);
                    }
                    
                    let jitter_ms = (rand_jitter() % 21) as i64 - 10; // +/-10ms
                    let sleep_duration = Duration::from_millis(
                        (backoff.as_millis() as i64 + jitter_ms).max(1) as u64
                    );
                    
                    sleep(sleep_duration);
                    backoff *= 2;
                } else {
                    return Err(e);
                }
            }
        }
    }
}

fn is_busy_error(err: &rusqlite::Error) -> bool {
    if let rusqlite::Error::SqliteFailure(ffi_err, _) = err {
        ffi_err.code == rusqlite::ErrorCode::DatabaseBusy
            || ffi_err.code == rusqlite::ErrorCode::DatabaseLocked
    } else {
        false
    }
}

fn rand_jitter() -> u32 {
    use std::time::SystemTime;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u32)
        .unwrap_or(42)
}

pub fn initialize_db<P: AsRef<Path>>(path: P) -> Result<Connection, rusqlite::Error> {
    let path_ref = path.as_ref();
    let is_memory = path_ref.to_string_lossy() == ":memory:";
    let was_missing = !is_memory && !path_ref.exists();
    initialize_db_internal(path_ref, was_missing)
}

fn initialize_db_internal(path_ref: &Path, was_missing: bool) -> Result<Connection, rusqlite::Error> {
    let is_memory = path_ref.to_string_lossy() == ":memory:";
    
    if !is_memory {
        if let Some(parent) = path_ref.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                rusqlite::Error::InvalidPath(PathBuf::from(format!("Failed to create directories: {}", e)))
            })?;
        }
    }
    
    let backup_path = if !is_memory {
        Some(path_ref.with_extension("db.migration_backup"))
    } else {
        None
    };
    
    if let Some(bp) = &backup_path {
        if path_ref.exists() {
            fs::copy(path_ref, bp).map_err(|e| {
                rusqlite::Error::InvalidPath(PathBuf::from(format!("Failed to create database backup: {}", e)))
            })?;
        }
    }
    
    let mut conn = match open_connection(path_ref) {
        Ok(c) => c,
        Err(e) => {
            if !is_memory && is_corrupt_error(&e) {
                if let Ok(timestamp) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
                    let corrupt_path = path_ref.with_extension(format!("db.corrupt.{}", timestamp.as_secs()));
                    let _ = fs::rename(path_ref, &corrupt_path);
                    if let Some(ref bp) = backup_path {
                        let _ = fs::remove_file(bp);
                    }
                    return initialize_db_internal(path_ref, true);
                }
            }
            restore_backup_and_cleanup(path_ref, &backup_path);
            return Err(e);
        }
    };
    
    if let Err(e) = run_migrations(&mut conn) {
        drop(conn);
        if !is_memory && is_corrupt_error(&e) {
            if let Ok(timestamp) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
                let corrupt_path = path_ref.with_extension(format!("db.corrupt.{}", timestamp.as_secs()));
                let _ = fs::rename(path_ref, &corrupt_path);
                if let Some(ref bp) = backup_path {
                    let _ = fs::remove_file(bp);
                }
                return initialize_db_internal(path_ref, true);
            }
        }
        restore_backup_and_cleanup(path_ref, &backup_path);
        return Err(e);
    }
    
    if let Some(bp) = &backup_path {
        if bp.exists() {
            let _ = fs::remove_file(bp);
        }
    }
    
    if was_missing {
        let _ = restore_db_from_backup(&conn);
    }
    
    Ok(conn)
}

fn is_corrupt_error(err: &rusqlite::Error) -> bool {
    if let rusqlite::Error::SqliteFailure(ffi_err, _) = err {
        ffi_err.code == rusqlite::ErrorCode::DatabaseCorrupt
            || ffi_err.code == rusqlite::ErrorCode::NotADatabase
    } else {
        false
    }
}

fn restore_backup_and_cleanup(db_path: &Path, backup_path: &Option<PathBuf>) {
    if let Some(bp) = backup_path {
        if bp.exists() {
            let _ = fs::copy(bp, db_path);
            let _ = fs::remove_file(bp);
        }
    }
}

fn run_migrations(conn: &mut Connection) -> Result<(), rusqlite::Error> {
    let mut current_version: i32 = conn.query_row("PRAGMA user_version;", [], |row| row.get(0))?;
    
    if current_version < 1 {
        let tx = conn.transaction()?;
        
        tx.execute(
            "CREATE TABLE IF NOT EXISTS user_profile (
                user_id TEXT PRIMARY KEY,
                user_email_hash TEXT NOT NULL,
                license_status TEXT NOT NULL,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            );",
            [],
        )?;
        
        tx.execute(
            "CREATE TABLE IF NOT EXISTS concepts (
                concept_slug TEXT PRIMARY KEY,
                mastery_score REAL DEFAULT 0.0 CHECK(mastery_score BETWEEN 0.0 AND 1.0),
                exposure_count INTEGER DEFAULT 0,
                consecutive_successes INTEGER DEFAULT 0,
                last_seen TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            );",
            [],
        )?;
        
        tx.execute(
            "CREATE TABLE IF NOT EXISTS compilation_errors (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                error_code TEXT NOT NULL,
                file_path TEXT NOT NULL,
                line_number INTEGER NOT NULL,
                error_message TEXT NOT NULL,
                consecutive_occurrences INTEGER DEFAULT 1,
                escape_hatch_triggered BOOLEAN DEFAULT 0,
                resolved BOOLEAN DEFAULT 0,
                project_root TEXT NOT NULL,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                resolved_at TIMESTAMP
            );",
            [],
        )?;
        
        tx.execute(
            "CREATE TABLE IF NOT EXISTS pedagogical_interactions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                error_log_id INTEGER,
                prompt_tokens INTEGER,
                completion_tokens INTEGER,
                prompt_payload TEXT NOT NULL,
                response_payload TEXT NOT NULL,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY(error_log_id) REFERENCES compilation_errors(id)
            );",
            [],
        )?;
        
        tx.execute(
            "CREATE TABLE IF NOT EXISTS Socratic_bypass_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                duration_seconds INTEGER,
                bypass_reason_code INTEGER NOT NULL,
                workspace_hash TEXT,
                files_modified_count INTEGER,
                activated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            );",
            [],
        )?;
        
        tx.execute(
            "CREATE TABLE IF NOT EXISTS socratic_dialogues (
                workspace_hash TEXT NOT NULL,
                file_path_hash TEXT NOT NULL,
                scaffold_level INTEGER DEFAULT 1,
                consecutive_failures INTEGER DEFAULT 0,
                repetition_count INTEGER DEFAULT 0,
                dialogue_context_hash TEXT,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (workspace_hash, file_path_hash)
            );",
            [],
        )?;
        
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_errors_resolved ON compilation_errors(error_code, resolved);",
            [],
        )?;
        
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_errors_project ON compilation_errors(project_root);",
            [],
        )?;
        
        let core_concepts = vec![
            ("ownership", 0.5),
            ("borrowing", 0.5),
            ("lifetimes", 0.5),
            ("smart_pointers", 0.5),
            ("concurrency", 0.5),
            ("traits", 0.5),
        ];
        
        for (slug, score) in core_concepts {
            tx.execute(
                "INSERT OR IGNORE INTO concepts (concept_slug, mastery_score) VALUES (?1, ?2);",
                rusqlite::params![slug, score],
            )?;
        }
        
        tx.execute("PRAGMA user_version = 1;", [])?;
        tx.commit()?;
        current_version = 1;
    }
    
    let _ = current_version;
    Ok(())
}

pub fn save_backup_from_db(conn: &Connection) -> std::result::Result<(), String> {
    let mut stmt = conn.prepare("SELECT concept_slug, mastery_score FROM concepts;").map_err(|e| e.to_string())?;
    let concepts_iter = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
    }).map_err(|e| e.to_string())?;
    
    let mut concepts = std::collections::HashMap::new();
    for item in concepts_iter {
        if let Ok((slug, score)) = item {
            concepts.insert(slug, score);
        }
    }
    
    let backup = crate::backup::ProgressBackup { concepts };
    crate::backup::write_backup(&backup)
}

pub fn restore_db_from_backup(conn: &Connection) -> std::result::Result<(), String> {
    if let Ok(backup) = crate::backup::read_backup() {
        let _ = execute_with_retry(|| {
            let tx = conn.unchecked_transaction()?;
            for (slug, score) in &backup.concepts {
                tx.execute(
                    "UPDATE concepts SET mastery_score = ?1 WHERE concept_slug = ?2;",
                    rusqlite::params![score, slug],
                )?;
            }
            tx.commit()?;
            Ok(())
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_open_connection_settings() {
        let temp_dir = std::env::temp_dir();
        let db_path = temp_dir.join("test_murshid_settings.db");
        if db_path.exists() {
            let _ = std::fs::remove_file(&db_path);
        }
        
        let conn = initialize_db(&db_path).unwrap();
        
        let journal_mode: String = conn.query_row("PRAGMA journal_mode;", [], |row| row.get(0)).unwrap();
        assert_eq!(journal_mode.to_uppercase(), "WAL");
        
        let synchronous: i32 = conn.query_row("PRAGMA synchronous;", [], |row| row.get(0)).unwrap();
        assert_eq!(synchronous, 1); // 1 = NORMAL
        
        drop(conn);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn test_prepopulated_concepts() {
        let conn = initialize_db(":memory:").unwrap();
        
        let mut stmt = conn.prepare("SELECT concept_slug, mastery_score FROM concepts ORDER BY concept_slug;").unwrap();
        let concepts_iter = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
        }).unwrap();
        
        let mut results = Vec::new();
        for concept in concepts_iter {
            results.push(concept.unwrap());
        }
        
        let expected = vec![
            ("borrowing".to_string(), 0.5),
            ("concurrency".to_string(), 0.5),
            ("lifetimes".to_string(), 0.5),
            ("ownership".to_string(), 0.5),
            ("smart_pointers".to_string(), 0.5),
            ("traits".to_string(), 0.5),
        ];
        assert_eq!(results, expected);
    }

    #[test]
    fn test_migration_failure_rollback() {
        let temp_dir = std::env::temp_dir();
        let db_path = temp_dir.join("test_murshid_rollback.db");
        if db_path.exists() {
            let _ = std::fs::remove_file(&db_path);
        }
        
        let _conn = initialize_db(&db_path).unwrap();
        
        let conn2 = open_connection(&db_path).unwrap();
        let version: i32 = conn2.query_row("PRAGMA user_version;", [], |row| row.get(0)).unwrap();
        assert_eq!(version, 1);
        drop(conn2);
        
        fn run_faulty_migration(conn: &mut Connection) -> Result<(), rusqlite::Error> {
            let tx = conn.transaction()?;
            tx.execute("INSERT INTO non_existent_table_to_fail VALUES (1);", [])?;
            tx.execute("INSERT INTO user_profile (user_id, user_email_hash, license_status) VALUES ('fail', 'fail', 'fail');", [])?;
            tx.execute("PRAGMA user_version = 2;", [])?;
            tx.commit()?;
            Ok(())
        }

        let backup_path = db_path.with_extension("db.migration_backup");
        std::fs::copy(&db_path, &backup_path).unwrap();
        
        let mut conn3 = open_connection(&db_path).unwrap();
        let run_res = run_faulty_migration(&mut conn3);
        assert!(run_res.is_err());
        drop(conn3);
        
        restore_backup_and_cleanup(&db_path, &Some(backup_path));
        
        let conn4 = open_connection(&db_path).unwrap();
        let version: i32 = conn4.query_row("PRAGMA user_version;", [], |row| row.get(0)).unwrap();
        assert_eq!(version, 1);
        
        let count: i32 = conn4.query_row("SELECT count(*) FROM user_profile WHERE user_id = 'fail';", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 0);
        
        drop(conn4);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn test_database_corruption_recovery() {
        let temp_dir = std::env::temp_dir();
        let db_path = temp_dir.join("test_murshid_corrupt.db");
        let backup_file = temp_dir.join("test_murshid_corrupt_backup.json");
        if db_path.exists() {
            let _ = std::fs::remove_file(&db_path);
        }
        if backup_file.exists() {
            let _ = std::fs::remove_file(&backup_file);
        }
        
        crate::backup::set_test_backup_path(Some(backup_file.clone()));
        
        let conn = initialize_db(&db_path).unwrap();
        conn.execute("UPDATE concepts SET mastery_score = 0.95 WHERE concept_slug = 'ownership';", []).unwrap();
        
        save_backup_from_db(&conn).unwrap();
        drop(conn);
        
        std::fs::write(&db_path, b"garbage sqlite file content which is corrupt for sure").unwrap();
        
        let conn2 = initialize_db(&db_path).unwrap();
        
        let score: f64 = conn2.query_row("SELECT mastery_score FROM concepts WHERE concept_slug = 'ownership';", [], |row| row.get(0)).unwrap();
        assert_eq!(score, 0.95);
        
        let mut found_corrupt = false;
        for entry in std::fs::read_dir(&temp_dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("test_murshid_corrupt.db.corrupt.") {
                found_corrupt = true;
                let _ = std::fs::remove_file(entry.path());
            }
        }
        assert!(found_corrupt);
        
        drop(conn2);
        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(&backup_file);
        crate::backup::set_test_backup_path(None);
    }
}

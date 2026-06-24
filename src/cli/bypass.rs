pub fn check_bypass_limits(
    conn: &rusqlite::Connection,
    _max_sessions: u32,
) -> rusqlite::Result<u32> {
    let mut stmt = conn.prepare(
        "SELECT COUNT(*) FROM Socratic_bypass_log 
         WHERE activated_at >= datetime('now', '-7 days')",
    )?;
    let count: u32 = stmt.query_row([], |r| r.get(0))?;
    Ok(count)
}

pub fn log_bypass_session(
    conn: &rusqlite::Connection,
    duration_seconds: u32,
    reason_code: u32,
    workspace_hash: Option<&str>,
    files_modified_count: Option<u32>,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO Socratic_bypass_log (duration_seconds, bypass_reason_code, workspace_hash, files_modified_count, activated_at)
         VALUES (?1, ?2, ?3, ?4, CURRENT_TIMESTAMP)",
        (
            duration_seconds,
            reason_code,
            workspace_hash,
            files_modified_count,
        )
    )?;
    Ok(())
}

pub fn is_bypass_active(conn: &rusqlite::Connection) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare(
        "SELECT duration_seconds, strftime('%s', activated_at) 
         FROM Socratic_bypass_log 
         ORDER BY id DESC LIMIT 1",
    )?;
    let mut rows = stmt.query([])?;
    if let Some(row) = rows.next()? {
        let duration: i64 = row.get(0)?;
        let activated_timestamp_str: String = row.get(1)?;
        let activated_timestamp: i64 = activated_timestamp_str.parse().unwrap_or(0);

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        Ok(activated_timestamp + duration > now)
    } else {
        Ok(false)
    }
}

pub fn run_bypass(
    conn: &rusqlite::Connection,
    duration_seconds: u32,
    reason_code: u32,
    force: bool,
    workspace_hash: Option<&str>,
    files_count: Option<u32>,
) -> Result<(), String> {
    let config = crate::config::load_config();
    let max_sessions = config.pedagogy.max_weekly_bypass_sessions;

    let weekly_count = check_bypass_limits(conn, max_sessions).map_err(|e| e.to_string())?;

    if weekly_count >= max_sessions && !force {
        return Err(format!(
            "Bypass limit reached ({} of {} allowed in 7 days). Use --force to override.",
            weekly_count, max_sessions
        ));
    }

    log_bypass_session(
        conn,
        duration_seconds,
        reason_code,
        workspace_hash,
        files_count,
    )
    .map_err(|e| format!("Failed to log bypass: {}", e))?;

    println!(
        "Bypass active for {} seconds. Socratic mode bypassed.",
        duration_seconds
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bypass_limits_and_logging() {
        let temp_dir = std::env::temp_dir();
        let db_path = temp_dir.join("murshid_bypass_test.db");
        let _ = std::fs::remove_file(&db_path);

        let conn = rusqlite::Connection::open(&db_path).unwrap();

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS Socratic_bypass_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                duration_seconds INTEGER,
                bypass_reason_code INTEGER NOT NULL,
                workspace_hash TEXT,
                files_modified_count INTEGER,
                activated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            );",
        )
        .unwrap();

        // Initially no bypass active
        assert!(!is_bypass_active(&conn).unwrap());

        // Log a bypass session of 100 seconds
        log_bypass_session(&conn, 100, 1, Some("ws"), Some(2)).unwrap();

        // Now it should be active (within 100s window)
        assert!(is_bypass_active(&conn).unwrap());

        let count = check_bypass_limits(&conn, 3).unwrap();
        assert_eq!(count, 1);

        let _ = std::fs::remove_file(&db_path);
    }
}

//! The progress-backup bridge (`concepts` table <-> `crate::backup`)
//! and the `context_history` edit/compile log (`HistoryEvent`).

use super::*;

pub fn save_backup_from_db(conn: &Connection) -> std::result::Result<(), String> {
    let mut stmt = conn
        .prepare("SELECT concept_slug, mastery_score FROM concepts;")
        .map_err(|e| e.to_string())?;
    let concepts_iter = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
        })
        .map_err(|e| e.to_string())?;

    let mut concepts = std::collections::HashMap::new();
    for (slug, score) in concepts_iter.flatten() {
        concepts.insert(slug, score);
    }

    let backup = crate::backup::ProgressBackup { concepts };
    crate::backup::write_backup(&backup)
}

pub fn restore_db_from_backup(conn: &Connection) -> std::result::Result<(), String> {
    if let Ok(backup) = crate::backup::read_backup() {
        // T9 req 5: name the failed recovery operation instead of silently
        // swallowing it via `let _ =`.
        if let Err(e) = execute_with_retry(|| {
            let tx = conn.unchecked_transaction()?;
            for (slug, score) in &backup.concepts {
                tx.execute(
                    "UPDATE concepts SET mastery_score = ?1 WHERE concept_slug = ?2;",
                    rusqlite::params![score, slug],
                )?;
            }
            tx.commit()?;
            Ok(())
        }) {
            eprintln!(
                "Warning: failed to restore db state from progress backup: {}",
                e
            );
        }
    }
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct HistoryEvent {
    pub id: Option<i64>,
    pub event_type: String, // "file_edit" | "compiler_check"
    pub project_root: String,
    pub file_path: String,
    pub success: Option<bool>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub line_number: Option<i64>,
    pub created_at: Option<String>,
}

pub fn log_history_event(conn: &Connection, event: &HistoryEvent) -> Result<(), rusqlite::Error> {
    execute_with_retry(|| {
        conn.execute(
            "INSERT INTO context_history (event_type, project_root, file_path, success, error_code, error_message, line_number)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                event.event_type,
                event.project_root,
                event.file_path,
                event.success,
                event.error_code,
                event.error_message,
                event.line_number,
            ],
        )?;
        Ok(())
    })
}

pub fn get_recent_history(
    conn: &Connection,
    project_root: &str,
    limit: usize,
) -> Result<Vec<HistoryEvent>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT id, event_type, project_root, file_path, success, error_code, error_message, line_number, created_at
         FROM context_history
         WHERE project_root = ?1
         ORDER BY created_at DESC, id DESC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![project_root, limit], |row| {
        Ok(HistoryEvent {
            id: Some(row.get(0)?),
            event_type: row.get(1)?,
            project_root: row.get(2)?,
            file_path: row.get(3)?,
            success: row.get(4)?,
            error_code: row.get(5)?,
            error_message: row.get(6)?,
            line_number: row.get(7)?,
            created_at: Some(row.get(8)?),
        })
    })?;

    let mut events = Vec::new();
    for row in rows {
        events.push(row?);
    }
    events.reverse();
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_context_history() {
        let conn = initialize_db(":memory:").unwrap();

        let event1 = HistoryEvent {
            id: None,
            event_type: "file_edit".to_string(),
            project_root: "/test/project".to_string(),
            file_path: "src/lib.rs".to_string(),
            success: None,
            error_code: None,
            error_message: None,
            line_number: None,
            created_at: None,
        };
        log_history_event(&conn, &event1).unwrap();

        let event2 = HistoryEvent {
            id: None,
            event_type: "compiler_check".to_string(),
            project_root: "/test/project".to_string(),
            file_path: "src/lib.rs".to_string(),
            success: Some(false),
            error_code: Some("E0382".to_string()),
            error_message: Some("use of moved value".to_string()),
            line_number: Some(15),
            created_at: None,
        };
        log_history_event(&conn, &event2).unwrap();

        let history = get_recent_history(&conn, "/test/project", 10).unwrap();
        assert_eq!(history.len(), 2);

        assert_eq!(history[0].event_type, "file_edit");
        assert_eq!(history[0].file_path, "src/lib.rs");
        assert_eq!(history[1].event_type, "compiler_check");
        assert_eq!(history[1].success, Some(false));
        assert_eq!(history[1].error_code.as_deref(), Some("E0382"));
        assert_eq!(history[1].line_number, Some(15));
    }
}

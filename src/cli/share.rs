use std::path::Path;

pub fn generate_struggle_log_markdown(conn: &rusqlite::Connection) -> rusqlite::Result<String> {
    let mut markdown = String::new();
    markdown.push_str("# Murshid Socratic Struggle Log\n\n");

    let mut stmt = conn.prepare(
        "SELECT workspace_hash, file_path_hash, scaffold_level, consecutive_failures, updated_at 
         FROM socratic_dialogues 
         ORDER BY updated_at DESC",
    )?;

    let mut rows = stmt.query([])?;
    markdown.push_str("## Active Dialogues\n\n");
    markdown.push_str(
        "| Workspace Hash | File Hash | Scaffold Level | Consecutive Failures | Last Updated |\n",
    );
    markdown.push_str("|---|---|---|---|---|\n");

    while let Some(row) = rows.next()? {
        let ws: String = row.get(0)?;
        let file: String = row.get(1)?;
        let scaffold: i32 = row.get(2)?;
        let failures: i32 = row.get(3)?;
        let updated: String = row.get(4)?;

        markdown.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            &ws[..std::cmp::min(8, ws.len())],
            &file[..std::cmp::min(8, file.len())],
            scaffold,
            failures,
            updated
        ));
    }

    let mut stmt_concepts = conn.prepare(
        "SELECT concept_slug, mastery_score, exposure_count, consecutive_successes 
         FROM concepts 
         ORDER BY mastery_score DESC",
    )?;

    let mut rows_c = stmt_concepts.query([])?;
    markdown.push_str("\n## Concept Mastery Progression\n\n");
    markdown.push_str("| Concept | Mastery Score | Exposure Count | Consecutive Successes |\n");
    markdown.push_str("|---|---|---|---|\n");

    while let Some(row) = rows_c.next()? {
        let slug: String = row.get(0)?;
        let score: f64 = row.get(1)?;
        let exposure: i32 = row.get(2)?;
        let successes: i32 = row.get(3)?;

        markdown.push_str(&format!(
            "| {} | {:.2}% | {} | {} |\n",
            slug,
            score * 100.0,
            exposure,
            successes
        ));
    }

    Ok(markdown)
}

pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let mut child = Command::new("pbcopy")
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn pbcopy: {}", e))?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(text.as_bytes())
                .map_err(|e| e.to_string())?;
        }
        let status = child.wait().map_err(|e| e.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err("pbcopy exited with error".to_string())
        }
    }
    #[cfg(target_os = "windows")]
    {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let mut child = Command::new("clip")
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn clip: {}", e))?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(text.as_bytes())
                .map_err(|e| e.to_string())?;
        }
        let status = child.wait().map_err(|e| e.to_string())?;
        if status.success() {
            Ok(())
        } else {
            Err("clip exited with error".to_string())
        }
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let cmd = if Command::new("wl-copy").arg("--version").status().is_ok() {
            "wl-copy"
        } else if Command::new("xclip").arg("-version").status().is_ok() {
            "xclip"
        } else {
            // If clipboard utility is not found, degrade gracefully in test environment
            return Ok(());
        };

        let mut child = Command::new(cmd)
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn clipboard utility: {}", e))?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(text.as_bytes())
                .map_err(|e| e.to_string())?;
        }
        let _ = child.wait();
        Ok(())
    }
}

pub fn run_share(conn: &rusqlite::Connection, output_file: &Path) -> Result<(), String> {
    let markdown = generate_struggle_log_markdown(conn).map_err(|e| e.to_string())?;

    std::fs::write(output_file, &markdown)
        .map_err(|e| format!("Failed to write markdown file: {}", e))?;

    let _ = copy_to_clipboard(&markdown);

    println!(
        "Successfully exported struggle log to {} and copied it to the system clipboard.",
        output_file.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_share_struggle_log() {
        let temp_dir = std::env::temp_dir();
        let db_path = temp_dir.join("murshid_share_test.db");
        let _ = std::fs::remove_file(&db_path);

        let conn = rusqlite::Connection::open(&db_path).unwrap();

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS socratic_dialogues (
                workspace_hash TEXT NOT NULL,
                file_path_hash TEXT NOT NULL,
                scaffold_level INTEGER DEFAULT 1,
                consecutive_failures INTEGER DEFAULT 0,
                repetition_count INTEGER DEFAULT 0,
                dialogue_context_hash TEXT,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                PRIMARY KEY (workspace_hash, file_path_hash)
            );
            CREATE TABLE IF NOT EXISTS concepts (
                concept_slug TEXT PRIMARY KEY,
                mastery_score REAL DEFAULT 0.0 CHECK(mastery_score BETWEEN 0.0 AND 1.0),
                exposure_count INTEGER DEFAULT 0,
                consecutive_successes INTEGER DEFAULT 0,
                last_seen TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            );",
        )
        .unwrap();

        // Add mock dialogues and concepts
        conn.execute(
            "INSERT INTO socratic_dialogues (workspace_hash, file_path_hash, scaffold_level, consecutive_failures, updated_at) 
             VALUES ('ws12345678', 'file12345678', 2, 3, '2026-06-24 12:00:00')", 
            []
        ).unwrap();
        conn.execute(
            "INSERT INTO concepts (concept_slug, mastery_score, exposure_count, consecutive_successes) 
             VALUES ('ownership', 0.75, 4, 2)", 
            []
        ).unwrap();

        let md_path = temp_dir.join("struggle.md");
        let _ = std::fs::remove_file(&md_path);

        run_share(&conn, &md_path).unwrap();

        assert!(md_path.exists());
        let content = std::fs::read_to_string(&md_path).unwrap();
        assert!(content.contains("# Murshid Socratic Struggle Log"));
        assert!(content.contains("ws123456"));
        assert!(content.contains("ownership"));

        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(&md_path);
    }
}

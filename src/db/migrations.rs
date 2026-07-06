//! Schema-migration ladder and the resilient connection
//! bootstrap (backup / corruption-recovery around `open_connection` +
//! `run_migrations`). The public entry point is `initialize_db`.

use super::*;

pub fn initialize_db<P: AsRef<Path>>(path: P) -> Result<Connection, rusqlite::Error> {
    let path_ref = path.as_ref();
    let is_memory = path_ref.to_string_lossy() == ":memory:";
    let was_missing = !is_memory && !path_ref.exists();
    initialize_db_internal(path_ref, was_missing)
}

fn initialize_db_internal(
    path_ref: &Path,
    was_missing: bool,
) -> Result<Connection, rusqlite::Error> {
    let is_memory = path_ref.to_string_lossy() == ":memory:";

    if !is_memory {
        if let Some(parent) = path_ref.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                rusqlite::Error::InvalidPath(PathBuf::from(format!(
                    "Failed to create directories: {}",
                    e
                )))
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
                rusqlite::Error::InvalidPath(PathBuf::from(format!(
                    "Failed to create database backup: {}",
                    e
                )))
            })?;
        }
    }

    let mut conn = match open_connection(path_ref) {
        Ok(c) => c,
        Err(e) => {
            if !is_memory && is_corrupt_error(&e) {
                if let Ok(timestamp) =
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
                {
                    let corrupt_path =
                        path_ref.with_extension(format!("db.corrupt.{}", timestamp.as_secs()));
                    // T9 req 5: name the failed recovery operation instead of
                    // silently swallowing it via `let _ =`.
                    if let Err(e) = fs::rename(path_ref, &corrupt_path) {
                        eprintln!(
                            "Warning: failed to rename corrupt db to {}: {}",
                            corrupt_path.display(),
                            e
                        );
                    }
                    if let Some(ref bp) = backup_path {
                        if let Err(e) = fs::remove_file(bp) {
                            eprintln!(
                                "Warning: failed to remove migration backup {}: {}",
                                bp.display(),
                                e
                            );
                        }
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
            if let Ok(timestamp) =
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            {
                let corrupt_path =
                    path_ref.with_extension(format!("db.corrupt.{}", timestamp.as_secs()));
                // T9 req 5: name the failed recovery operation instead of
                // silently swallowing it via `let _ =`.
                if let Err(e) = fs::rename(path_ref, &corrupt_path) {
                    eprintln!(
                        "Warning: failed to rename corrupt db to {}: {}",
                        corrupt_path.display(),
                        e
                    );
                }
                if let Some(ref bp) = backup_path {
                    if let Err(e) = fs::remove_file(bp) {
                        eprintln!(
                            "Warning: failed to remove migration backup {}: {}",
                            bp.display(),
                            e
                        );
                    }
                }
                return initialize_db_internal(path_ref, true);
            }
        }
        restore_backup_and_cleanup(path_ref, &backup_path);
        return Err(e);
    }

    if let Some(bp) = &backup_path {
        if bp.exists() {
            // T9 req 5: name the failed recovery operation instead of
            // silently swallowing it via `let _ =`.
            if let Err(e) = fs::remove_file(bp) {
                eprintln!(
                    "Warning: failed to remove stale migration backup {}: {}",
                    bp.display(),
                    e
                );
            }
        }
    }

    if was_missing {
        if let Err(e) = restore_db_from_backup(&conn) {
            eprintln!("Warning: failed to restore db from progress backup: {}", e);
        }
    }

    // T9 req 2: wire the platform progress backup into the production path
    // (previously only exercised by a test). Migrations having just
    // succeeded is the natural point to snapshot current mastery state.
    // `#[cfg(not(test))]`: dozens of db.rs unit tests open non-memory temp
    // DBs without setting `backup::set_test_backup_path`, which would
    // otherwise route every such test through the REAL platform backup
    // store (e.g. writing to the developer's actual macOS `defaults`
    // domain on every `cargo test` run) — exactly the kind of real-store
    // test pollution req 9 elsewhere eliminates for the keychain.
    #[cfg(not(test))]
    if !is_memory {
        if let Err(e) = save_backup_from_db(&conn) {
            eprintln!(
                "Warning: failed to save progress backup after migrations: {}",
                e
            );
        }
    }

    Ok(conn)
}

pub(crate) fn is_corrupt_error(err: &rusqlite::Error) -> bool {
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
            // T9 req 5: name the failed recovery operation instead of
            // silently swallowing it via `let _ =`.
            if let Err(e) = fs::copy(bp, db_path) {
                eprintln!(
                    "Warning: failed to restore db from migration backup {}: {}",
                    bp.display(),
                    e
                );
            }
            if let Err(e) = fs::remove_file(bp) {
                eprintln!(
                    "Warning: failed to remove migration backup {}: {}",
                    bp.display(),
                    e
                );
            }
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

    if current_version < 2 {
        let tx = conn.transaction()?;

        tx.execute(
            "CREATE TABLE IF NOT EXISTS goals (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                title TEXT NOT NULL,
                description TEXT,
                status TEXT NOT NULL CHECK(status IN ('active', 'completed', 'abandoned')),
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                completed_at TIMESTAMP
            );",
            [],
        )?;

        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_goals_status ON goals(status);",
            [],
        )?;

        tx.execute("PRAGMA user_version = 2;", [])?;
        tx.commit()?;
        current_version = 2;
    }

    if current_version < 3 {
        let tx = conn.transaction()?;

        tx.execute(
            "CREATE TABLE IF NOT EXISTS context_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                event_type TEXT NOT NULL,
                project_root TEXT NOT NULL,
                file_path TEXT NOT NULL,
                success BOOLEAN,
                error_code TEXT,
                error_message TEXT,
                line_number INTEGER,
                created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            );",
            [],
        )?;

        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_history_project_time ON context_history(project_root, created_at DESC);",
            [],
        )?;

        tx.execute("PRAGMA user_version = 3;", [])?;
        tx.commit()?;
        current_version = 3;
    }

    if current_version < 4 {
        let tx = conn.transaction()?;

        // C5 — the store: events + cards (T1 scope; other C5 tables arrive
        // with their own tasks).
        tx.execute(
            "CREATE TABLE IF NOT EXISTS events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ts TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                session_id TEXT NOT NULL,
                kind TEXT NOT NULL,
                payload_json TEXT NOT NULL DEFAULT '{}'
            );",
            [],
        )?;

        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_events_session ON events(session_id, ts);",
            [],
        )?;

        tx.execute(
            "CREATE TABLE IF NOT EXISTS cards (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                concept_id TEXT NOT NULL,
                category TEXT NOT NULL,
                rung_shown TEXT NOT NULL,
                advice_fp TEXT NOT NULL,
                finding_fp TEXT,
                status TEXT NOT NULL,
                created_ts TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                resolved_ts TIMESTAMP
            );",
            [],
        )?;

        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_cards_session ON cards(session_id);",
            [],
        )?;

        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_cards_session_advice_fp ON cards(session_id, advice_fp);",
            [],
        )?;

        tx.execute("PRAGMA user_version = 4;", [])?;
        tx.commit()?;
        current_version = 4;
    }

    if current_version < 5 {
        let tx = conn.transaction()?;

        // T1 fix-round req 9: worked_diff is a mandatory stage-2 leg and must
        // be a real, persisted part of the card record (it renders folded
        // behind the "(fix available — full interaction in T4)" line, but
        // must exist for T4 to unfold). C5 allows adding fields to `cards`.
        tx.execute("ALTER TABLE cards ADD COLUMN worked_diff TEXT;", [])?;

        tx.execute("PRAGMA user_version = 5;", [])?;
        tx.commit()?;
        current_version = 5;
    }

    if current_version < 6 {
        let tx = conn.transaction()?;

        // Founder-ruled cleanup (SPEC.md decision log, "standing
        // implementation authorization"), scoped by specs/FOUNDATIONS-
        // INHERITED.md's explicit leftover list: `license_status` was a
        // licensing remnant from the removed v6.0 register/licensing
        // subsystem (already cut; see git history) and carries no meaning
        // under SPEC v0.5. Dropped here (not in migration 1) because
        // migrations are frozen once shipped.
        tx.execute("ALTER TABLE user_profile DROP COLUMN license_status;", [])?;

        tx.execute("PRAGMA user_version = 6;", [])?;
        tx.commit()?;
        current_version = 6;
    }

    if current_version < 7 {
        let tx = conn.transaction()?;

        // C5 `suppressions` — session-scoped snooze rows (T2 req 8), purged
        // at session end. Extended beyond C5's minimal (advice_fp, scope,
        // expires_ts) with `session_id` (purge scope) and `concept_id`
        // (needed to detect "second not_now on the SAME concept" for the
        // D11(c) tiered-snooze widen, and to match concept-scope rows —
        // C5 explicitly allows adding fields, never removing them). For
        // scope='instance' rows, `advice_fp` is the exact (concept, site)
        // fingerprint; for scope='concept' rows it holds `concept_id` too,
        // so a single equality check on the right column covers both scopes.
        tx.execute(
            "CREATE TABLE IF NOT EXISTS suppressions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                concept_id TEXT NOT NULL,
                advice_fp TEXT NOT NULL,
                scope TEXT NOT NULL CHECK(scope IN ('instance', 'concept')),
                expires_ts TIMESTAMP,
                created_ts TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            );",
            [],
        )?;

        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_suppressions_session ON suppressions(session_id, id);",
            [],
        )?;

        // T2 req 9 regression re-open: "new card row referencing the old" —
        // nullable, additive per C5 ("fields may be added").
        tx.execute(
            "ALTER TABLE cards ADD COLUMN regresses_card_id INTEGER;",
            [],
        )?;

        // FOUNDATIONS-INHERITED.md known-leftover cleanup: `Socratic_bypass_log`
        // was the removed v6.0 bypass subsystem's writer table (cli/bypass.rs
        // was cut 2026-07-03); dropped here on this migration touching db.rs.
        tx.execute("DROP TABLE IF EXISTS Socratic_bypass_log;", [])?;

        tx.execute("PRAGMA user_version = 7;", [])?;
        tx.commit()?;
        current_version = 7;
    }

    if current_version < 8 {
        let tx = conn.transaction()?;

        // Review fix: cheap, do-now indexes. `advice_fp` backs every T2
        // dedup/ledger/regression lookup (find_ledger_card,
        // card_exists_with_advice_fp); `(category, id)` backs the
        // auto-throttle action-rate window query
        // (recent_card_statuses_for_category's `ORDER BY id DESC LIMIT`).
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_cards_advice_fp ON cards(advice_fp);",
            [],
        )?;
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_cards_category_id ON cards(category, id);",
            [],
        )?;

        tx.execute("PRAGMA user_version = 8;", [])?;
        tx.commit()?;
        current_version = 8;
    }

    if current_version < 9 {
        let tx = conn.transaction()?;

        // D13(c): goal storage moved to the user-editable `.murshid/goal`
        // file (T3 reshapes cli/goal.rs); the `goals` table (migration 2)
        // is dead now, same cleanup precedent as Socratic_bypass_log.
        tx.execute("DROP TABLE IF EXISTS goals;", [])?;

        // T3 req 13: struggle-offer decline suppression needs a third scope
        // (`offer-concept`) that survives the normal session-end purge for
        // 7 days. SQLite can't ALTER a CHECK constraint, so the table is
        // recreated with the widened constraint and its data copied over.
        tx.execute(
            "CREATE TABLE suppressions_new (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                concept_id TEXT NOT NULL,
                advice_fp TEXT NOT NULL,
                scope TEXT NOT NULL CHECK(scope IN ('instance', 'concept', 'offer-concept')),
                expires_ts TIMESTAMP,
                created_ts TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            );",
            [],
        )?;
        tx.execute(
            "INSERT INTO suppressions_new (id, session_id, concept_id, advice_fp, scope, expires_ts, created_ts)
             SELECT id, session_id, concept_id, advice_fp, scope, expires_ts, created_ts FROM suppressions;",
            [],
        )?;
        tx.execute("DROP TABLE suppressions;", [])?;
        tx.execute("ALTER TABLE suppressions_new RENAME TO suppressions;", [])?;
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_suppressions_session ON suppressions(session_id, id);",
            [],
        )?;

        tx.execute("PRAGMA user_version = 9;", [])?;
        tx.commit()?;
        current_version = 9;
    }

    if current_version < 10 {
        let tx = conn.transaction()?;

        // C5 `threads(card_id, turn_no, role, content, ts)` (T4 reqs 5-8,
        // D20): the normative minimal schema, verbatim.
        tx.execute(
            "CREATE TABLE IF NOT EXISTS threads (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                card_id INTEGER NOT NULL,
                turn_no INTEGER NOT NULL,
                role TEXT NOT NULL CHECK(role IN ('user', 'assistant')),
                content TEXT NOT NULL,
                ts TIMESTAMP DEFAULT CURRENT_TIMESTAMP
            );",
            [],
        )?;
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_threads_card ON threads(card_id, turn_no);",
            [],
        )?;

        // T4 req 1's mechanical applied-detection re-checks the card's site
        // at a LATER quiescence diff — `cards` (C5) carries no site pointer
        // today (only the opaque `advice_fp` hash), so a nullable
        // (file, line) pair is added, additive per C5 ("fields may be
        // added; these may not be removed").
        tx.execute("ALTER TABLE cards ADD COLUMN site_file TEXT;", [])?;
        tx.execute("ALTER TABLE cards ADD COLUMN site_line INTEGER;", [])?;

        tx.execute("PRAGMA user_version = 10;", [])?;
        tx.commit()?;
        current_version = 10;
    }

    if current_version < 11 {
        let tx = conn.transaction()?;

        // T5 req 1 / C5 `concept_memory(concept_id PK, p_mastery,
        // help_level, last_encounter_ts, last_outcome, lapse_count,
        // embedding BLOB NULL)` — the normative minimal schema, plus three
        // additive columns (C5: "fields may be added") this task needs:
        // `fade_announced_ts` makes I24's "announced once, including across
        // sessions" a durable DB fact instead of a session-local flag;
        // `pass_streak` backs the fade line's "applied N times straight";
        // `retrieval_skips` backs req 7's ×2-per-skip re-eligibility
        // backoff.
        // `last_encounter_ts`/`fade_announced_ts` are declared TEXT, not
        // TIMESTAMP: this app stores them as decimal Unix-epoch-seconds
        // strings (see `now_epoch_secs_string`), and SQLite's NUMERIC
        // column-affinity rule (which "TIMESTAMP" falls under, matching
        // neither INT/CHAR/CLOB/TEXT/REAL/FLOA/DOUB) would silently coerce
        // an all-digit TEXT value to an INTEGER storage class on insert —
        // TEXT affinity keeps the round-trip exact.
        tx.execute(
            "CREATE TABLE IF NOT EXISTS concept_memory (
                concept_id TEXT PRIMARY KEY,
                p_mastery REAL NOT NULL,
                help_level INTEGER NOT NULL DEFAULT 0,
                last_encounter_ts TEXT,
                last_outcome TEXT,
                lapse_count INTEGER NOT NULL DEFAULT 0,
                embedding BLOB,
                fade_announced_ts TEXT,
                pass_streak INTEGER NOT NULL DEFAULT 0,
                retrieval_skips INTEGER NOT NULL DEFAULT 0
            );",
            [],
        )?;

        tx.execute("PRAGMA user_version = 11;", [])?;
        tx.commit()?;
        current_version = 11;
    }

    if current_version < 12 {
        let tx = conn.transaction()?;

        // Hot-path reads filter `events` by `kind` — `all_check_result_points`,
        // `count_declined_offers_for_concept`, `latest_throttle_action`,
        // `retrieval_questions_asked_this_session`, and the `kind='encounter'`
        // scans — but migration 4 indexed only `(session_id, ts)`. Index
        // `(kind, id)` so those `WHERE kind = ? ... ORDER BY id` reads use the
        // index instead of a full table scan whose cost grows with lifetime
        // history.
        tx.execute(
            "CREATE INDEX IF NOT EXISTS idx_events_kind ON events(kind, id);",
            [],
        )?;

        tx.execute("PRAGMA user_version = 12;", [])?;
        tx.commit()?;
        current_version = 12;
    }

    if current_version < 13 {
        let tx = conn.transaction()?;

        // T15 HISTORY view (founder decision 2026-07-06, "persist the card
        // body"): the `cards` table already carries every metadata column a
        // history list needs (concept/category/status/rung/timestamps) plus
        // `worked_diff`, but never the card's own prose (why/rule/grounding
        // quote/doc ref/concept name) — those were rendered once and
        // discarded. Nullable/additive: old rows read back `NULL` (`card::
        // card_detail` degrades to "(card text not recorded)" for them, per
        // C5's "fields may be added; these may not be removed").
        tx.execute("ALTER TABLE cards ADD COLUMN card_body_json TEXT;", [])?;

        tx.execute("PRAGMA user_version = 13;", [])?;
        tx.commit()?;
        current_version = 13;
    }

    let _ = current_version;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::*;

    #[test]
    fn test_open_connection_settings() {
        let temp_dir = std::env::temp_dir();
        let db_path = temp_dir.join("test_murshid_settings.db");
        if db_path.exists() {
            let _ = std::fs::remove_file(&db_path);
        }

        let conn = initialize_db(&db_path).unwrap();

        let journal_mode: String = conn
            .query_row("PRAGMA journal_mode;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode.to_uppercase(), "WAL");

        let synchronous: i32 = conn
            .query_row("PRAGMA synchronous;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(synchronous, 1); // 1 = NORMAL

        drop(conn);
        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn test_prepopulated_concepts() {
        let conn = initialize_db(":memory:").unwrap();

        let mut stmt = conn
            .prepare("SELECT concept_slug, mastery_score FROM concepts ORDER BY concept_slug;")
            .unwrap();
        let concepts_iter = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
            })
            .unwrap();

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
        let version: i32 = conn2
            .query_row("PRAGMA user_version;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 13);
        drop(conn2);

        fn run_faulty_migration(conn: &mut Connection) -> Result<(), rusqlite::Error> {
            let tx = conn.transaction()?;
            tx.execute("INSERT INTO non_existent_table_to_fail VALUES (1);", [])?;
            tx.execute(
                "INSERT INTO user_profile (user_id, user_email_hash) VALUES ('fail', 'fail');",
                [],
            )?;
            tx.execute("PRAGMA user_version = 12;", [])?;
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
        let version: i32 = conn4
            .query_row("PRAGMA user_version;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 13);

        let count: i32 = conn4
            .query_row(
                "SELECT count(*) FROM user_profile WHERE user_id = 'fail';",
                [],
                |row| row.get(0),
            )
            .unwrap();
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
        conn.execute(
            "UPDATE concepts SET mastery_score = 0.95 WHERE concept_slug = 'ownership';",
            [],
        )
        .unwrap();

        save_backup_from_db(&conn).unwrap();
        drop(conn);

        std::fs::write(
            &db_path,
            b"garbage sqlite file content which is corrupt for sure",
        )
        .unwrap();

        let conn2 = initialize_db(&db_path).unwrap();

        let score: f64 = conn2
            .query_row(
                "SELECT mastery_score FROM concepts WHERE concept_slug = 'ownership';",
                [],
                |row| row.get(0),
            )
            .unwrap();
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

    #[test]
    fn test_c5_events_and_cards_tables_exist() {
        let conn = initialize_db(":memory:").unwrap();

        // events(id, ts, session_id, kind, payload_json)
        let mut stmt = conn.prepare("PRAGMA table_info(events);").unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .flatten()
            .collect();
        for expected in ["id", "ts", "session_id", "kind", "payload_json"] {
            assert!(
                cols.contains(&expected.to_string()),
                "events missing column {}",
                expected
            );
        }

        // cards(id, session_id, concept_id, category, rung_shown, advice_fp, finding_fp?, status, created_ts, resolved_ts?)
        let mut stmt2 = conn.prepare("PRAGMA table_info(cards);").unwrap();
        let cols2: Vec<String> = stmt2
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .flatten()
            .collect();
        for expected in [
            "id",
            "session_id",
            "concept_id",
            "category",
            "rung_shown",
            "advice_fp",
            "finding_fp",
            "status",
            "created_ts",
            "resolved_ts",
        ] {
            assert!(
                cols2.contains(&expected.to_string()),
                "cards missing column {}",
                expected
            );
        }
    }

    #[test]
    fn test_suppressions_and_socratic_bypass_log_migration() {
        let conn = initialize_db(":memory:").unwrap();
        let mut stmt = conn.prepare("PRAGMA table_info(suppressions);").unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .flatten()
            .collect();
        for expected in [
            "id",
            "session_id",
            "concept_id",
            "advice_fp",
            "scope",
            "expires_ts",
        ] {
            assert!(
                cols.contains(&expected.to_string()),
                "suppressions missing column {}",
                expected
            );
        }

        // FOUNDATIONS-INHERITED.md known-leftover cleanup.
        let table_exists: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='Socratic_bypass_log'",
            [],
            |row| row.get(0),
        )
        .unwrap();
        assert_eq!(table_exists, 0, "Socratic_bypass_log must be dropped");
    }

    #[test]
    fn test_migration_8_adds_cards_indexes() {
        let conn = initialize_db(":memory:").unwrap();
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'cards';")
            .unwrap();
        let names: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .flatten()
            .collect();
        assert!(
            names.contains(&"idx_cards_advice_fp".to_string()),
            "missing idx_cards_advice_fp: {:?}",
            names
        );
        assert!(
            names.contains(&"idx_cards_category_id".to_string()),
            "missing idx_cards_category_id: {:?}",
            names
        );

        let version: i32 = conn
            .query_row("PRAGMA user_version;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, 13);
    }

    #[test]
    fn test_migration_9_drops_goals_and_widens_suppression_scope() {
        let conn = initialize_db(":memory:").unwrap();

        let goals_exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='goals'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(goals_exists, 0, "goals table must be dropped");

        // offer-concept scope must now be insertable (CHECK constraint).
        insert_offer_suppression(&conn, "sess1", "borrow-vs-clone", 9_999_999_999).unwrap();
        assert!(is_offer_suppressed(&conn, "borrow-vs-clone", 0).unwrap());
    }

    #[test]
    fn test_migration_13_adds_nullable_card_body_json_column() {
        let conn = initialize_db(":memory:").unwrap();

        let version: i32 = conn
            .query_row("PRAGMA user_version;", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 13);

        let mut stmt = conn.prepare("PRAGMA table_info(cards);").unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .flatten()
            .collect();
        assert!(
            cols.contains(&"card_body_json".to_string()),
            "cards missing column card_body_json"
        );

        // Back-compat: a pre-migration-shaped row (no body written) reads
        // back as NULL, never an error.
        insert_card(&conn, &make_card("sess1", "c", "fp1", "shown")).unwrap();
        let body: Option<String> = conn
            .query_row(
                "SELECT card_body_json FROM cards WHERE advice_fp = 'fp1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(body, None);
    }

    #[test]
    fn test_migration_11_concept_memory_schema() {
        let conn = initialize_db(":memory:").unwrap();

        let version: i32 = conn
            .query_row("PRAGMA user_version;", [], |r| r.get(0))
            .unwrap();
        assert!(version >= 11, "concept_memory migration must have run");

        let mut stmt = conn.prepare("PRAGMA table_info(concept_memory);").unwrap();
        let cols: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .flatten()
            .collect();
        for expected in [
            "concept_id",
            "p_mastery",
            "help_level",
            "last_encounter_ts",
            "last_outcome",
            "lapse_count",
            "embedding",
            "fade_announced_ts",
            "pass_streak",
            "retrieval_skips",
        ] {
            assert!(
                cols.contains(&expected.to_string()),
                "concept_memory missing column {}",
                expected
            );
        }

        // C5: one row per taxonomy slug — concept_id is the primary key.
        let row = ConceptMemoryRow {
            concept_id: "c1".to_string(),
            p_mastery: 0.2,
            help_level: 0,
            last_encounter_ts: None,
            last_outcome: None,
            lapse_count: 0,
            fade_announced_ts: None,
            pass_streak: 0,
            retrieval_skips: 0,
        };
        upsert_concept_memory(&conn, &row).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM concept_memory WHERE concept_id = 'c1'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }
}

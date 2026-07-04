//! Unit tests for the persistence layer. Moved verbatim from the original
//! single-file `db.rs`; `use super::*` resolves against the `db` module's
//! re-export facade, so every public data-access fn is in scope.

use super::*;

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
    assert_eq!(version, 11);
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
    assert_eq!(version, 11);

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
fn test_log_event_and_query() {
    let conn = initialize_db(":memory:").unwrap();
    let event = EventRecord {
        id: None,
        session_id: "01ARZ3TEST".to_string(),
        kind: "session_start".to_string(),
        payload_json: "{}".to_string(),
        ts: None,
    };
    let id = log_event(&conn, &event).unwrap();
    assert!(id > 0);

    let events = get_events_for_session(&conn, "01ARZ3TEST").unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, "session_start");
}

#[test]
fn test_insert_card_and_dedup_by_advice_fp() {
    let conn = initialize_db(":memory:").unwrap();
    let card = CardRecord {
        id: None,
        session_id: "sess1".to_string(),
        concept_id: "borrow-vs-clone".to_string(),
        category: "best-practice".to_string(),
        rung_shown: "R2".to_string(),
        advice_fp: "abc123".to_string(),
        finding_fp: None,
        status: "shown".to_string(),
        created_ts: None,
        resolved_ts: None,
        worked_diff: Some("- old\n+ new".to_string()),
        regresses_card_id: None,
        site_file: None,
        site_line: None,
    };
    let id = insert_card(&conn, &card).unwrap();
    assert!(id > 0);

    assert!(card_exists_with_advice_fp(&conn, "sess1", "abc123").unwrap());
    assert!(!card_exists_with_advice_fp(&conn, "sess1", "other").unwrap());
    assert!(!card_exists_with_advice_fp(&conn, "sess2", "abc123").unwrap());

    update_card_status(&conn, id, "got_it").unwrap();
    let status: String = conn
        .query_row(
            "SELECT status FROM cards WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "got_it");
    let resolved_ts: Option<String> = conn
        .query_row(
            "SELECT resolved_ts FROM cards WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(resolved_ts.is_some());
}

#[test]
fn test_expire_unresolved_cards_at_session_end() {
    let conn = initialize_db(":memory:").unwrap();

    let make_card = |advice_fp: &str, status: &str| CardRecord {
        id: None,
        session_id: "sess1".to_string(),
        concept_id: "borrow-vs-clone".to_string(),
        category: "best-practice".to_string(),
        rung_shown: "R2".to_string(),
        advice_fp: advice_fp.to_string(),
        finding_fp: None,
        status: status.to_string(),
        created_ts: None,
        resolved_ts: None,
        worked_diff: None,
        regresses_card_id: None,
        site_file: None,
        site_line: None,
    };

    let shown_id = insert_card(&conn, &make_card("fp-shown", "shown")).unwrap();
    let got_it_id = insert_card(&conn, &make_card("fp-resolved", "got_it")).unwrap();

    // A card in a different session must not be touched.
    let mut other_session_card = make_card("fp-other-session", "shown");
    other_session_card.session_id = "sess2".to_string();
    let other_session_id = insert_card(&conn, &other_session_card).unwrap();

    let expired_count = expire_unresolved_cards(&conn, "sess1").unwrap();
    assert_eq!(expired_count, 1);

    let status: String = conn
        .query_row(
            "SELECT status FROM cards WHERE id = ?1",
            rusqlite::params![shown_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status, "expired");
    let resolved_ts: Option<String> = conn
        .query_row(
            "SELECT resolved_ts FROM cards WHERE id = ?1",
            rusqlite::params![shown_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(resolved_ts.is_some());

    // Already-resolved cards are untouched.
    let status2: String = conn
        .query_row(
            "SELECT status FROM cards WHERE id = ?1",
            rusqlite::params![got_it_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status2, "got_it");

    // Other sessions are untouched.
    let status3: String = conn
        .query_row(
            "SELECT status FROM cards WHERE id = ?1",
            rusqlite::params![other_session_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(status3, "shown");

    // Idempotent: a second call finds nothing left to expire.
    assert_eq!(expire_unresolved_cards(&conn, "sess1").unwrap(), 0);
}

fn make_card(session_id: &str, concept_id: &str, advice_fp: &str, status: &str) -> CardRecord {
    CardRecord {
        id: None,
        session_id: session_id.to_string(),
        concept_id: concept_id.to_string(),
        category: "idiom".to_string(),
        rung_shown: "R2".to_string(),
        advice_fp: advice_fp.to_string(),
        finding_fp: None,
        status: status.to_string(),
        created_ts: None,
        resolved_ts: None,
        worked_diff: None,
        regresses_card_id: None,
        site_file: None,
        site_line: None,
    }
}

// --- T2 req 5/9: cross-session ledger dedup + regression re-open ---

#[test]
fn test_cross_session_ledger_dedup_via_reopened_db() {
    let temp_dir = std::env::temp_dir();
    let db_path = temp_dir.join("test_murshid_t2_ledger.db");
    let _ = std::fs::remove_file(&db_path);

    {
        let conn = initialize_db(&db_path).unwrap();
        insert_card(
            &conn,
            &make_card("sess1", "borrow-vs-clone", "fp-1", "applied"),
        )
        .unwrap();
    }

    // Reopen the db as a fresh connection/process would in a new session.
    let conn2 = initialize_db(&db_path).unwrap();
    let found = find_ledger_card(&conn2, "fp-1").unwrap();
    assert!(found.is_some(), "ledger dedup must see across sessions");
    let (_, status) = found.unwrap();
    assert_eq!(status, "applied");
    assert!(is_regression_eligible(&status));

    // A not_useful/got_it card blocks re-creation but is NOT
    // regression-eligible.
    insert_card(
        &conn2,
        &make_card("sess1", "string-vs-str", "fp-2", "not_useful"),
    )
    .unwrap();
    let (_, status2) = find_ledger_card(&conn2, "fp-2").unwrap().unwrap();
    assert!(!is_regression_eligible(&status2));

    // Unknown advice-fp: no ledger entry.
    assert!(find_ledger_card(&conn2, "fp-unknown").unwrap().is_none());

    let _ = std::fs::remove_file(&db_path);
}

#[test]
fn test_regression_reopen_references_old_card() {
    let conn = initialize_db(":memory:").unwrap();
    let old_id = insert_card(
        &conn,
        &make_card("sess1", "borrow-vs-clone", "fp-1", "resolved"),
    )
    .unwrap();

    let (found_id, status) = find_ledger_card(&conn, "fp-1").unwrap().unwrap();
    assert_eq!(found_id, old_id);
    assert!(is_regression_eligible(&status));

    let mut regressed = make_card("sess2", "borrow-vs-clone", "fp-1", "shown");
    regressed.regresses_card_id = Some(old_id);
    let new_id = insert_card(&conn, &regressed).unwrap();
    assert_ne!(new_id, old_id);

    let stored_ref: Option<i64> = conn
        .query_row(
            "SELECT regresses_card_id FROM cards WHERE id = ?1",
            rusqlite::params![new_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored_ref, Some(old_id));
}

// --- T2 req 7: concept cooldown ---

#[test]
fn test_concept_shown_this_session() {
    let conn = initialize_db(":memory:").unwrap();
    assert!(!concept_shown_this_session(&conn, "sess1", "borrow-vs-clone").unwrap());

    // A queued (never pushed) card does not count as "shown".
    insert_card(
        &conn,
        &make_card("sess1", "borrow-vs-clone", "fp-1", "queued"),
    )
    .unwrap();
    assert!(!concept_shown_this_session(&conn, "sess1", "borrow-vs-clone").unwrap());

    insert_card(
        &conn,
        &make_card("sess1", "borrow-vs-clone", "fp-2", "shown"),
    )
    .unwrap();
    assert!(concept_shown_this_session(&conn, "sess1", "borrow-vs-clone").unwrap());

    // Different session unaffected.
    assert!(!concept_shown_this_session(&conn, "sess2", "borrow-vs-clone").unwrap());
}

// --- T2 req 8: suppressions (tiered snooze, cap, purge) ---

#[test]
fn test_suppression_instance_and_concept_scope_matching() {
    let conn = initialize_db(":memory:").unwrap();
    insert_suppression(&conn, "sess1", "borrow-vs-clone", "fp-1", "instance").unwrap();

    assert!(is_suppressed(&conn, "sess1", "borrow-vs-clone", "fp-1").unwrap());
    // A different site of the same concept is NOT instance-suppressed.
    assert!(!is_suppressed(&conn, "sess1", "borrow-vs-clone", "fp-2").unwrap());

    insert_suppression(
        &conn,
        "sess1",
        "borrow-vs-clone",
        "borrow-vs-clone",
        "concept",
    )
    .unwrap();
    // Now every site of the concept is suppressed.
    assert!(is_suppressed(&conn, "sess1", "borrow-vs-clone", "fp-2").unwrap());
    assert!(is_suppressed(&conn, "sess1", "borrow-vs-clone", "fp-3").unwrap());
    // A different concept is unaffected.
    assert!(!is_suppressed(&conn, "sess1", "string-vs-str", "fp-4").unwrap());
}

#[test]
fn test_suppression_cap_expiry_oldest_first() {
    let conn = initialize_db(":memory:").unwrap();
    for i in 0..55 {
        insert_suppression(
            &conn,
            "sess1",
            "borrow-vs-clone",
            &format!("fp-{}", i),
            "instance",
        )
        .unwrap();
        enforce_suppression_cap(&conn, "sess1").unwrap();
    }

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM suppressions WHERE session_id = 'sess1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 50, "live cap is 50");

    // Oldest (fp-0..fp-4) expired first; newest (fp-54) survives.
    assert!(!is_suppressed(&conn, "sess1", "borrow-vs-clone", "fp-0").unwrap());
    assert!(is_suppressed(&conn, "sess1", "borrow-vs-clone", "fp-54").unwrap());
}

/// Review fix (req 13): an `offer-concept` suppression must survive
/// `enforce_suppression_cap` even under heavy instance-scope churn in
/// the same session — it's neither counted against the 50-row cap nor
/// itself evictable by it.
#[test]
fn test_enforce_suppression_cap_never_evicts_offer_concept_rows() {
    let conn = initialize_db(":memory:").unwrap();
    insert_offer_suppression(&conn, "sess1", "borrow-vs-clone", 9_999_999_999).unwrap();

    for i in 0..55 {
        insert_suppression(
            &conn,
            "sess1",
            "iterator-chains",
            &format!("fp-{}", i),
            "instance",
        )
        .unwrap();
        enforce_suppression_cap(&conn, "sess1").unwrap();
    }

    assert!(
        is_offer_suppressed(&conn, "borrow-vs-clone", 0).unwrap(),
        "the offer-concept row must survive heavy instance-scope churn"
    );

    let instance_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM suppressions WHERE session_id = 'sess1' AND scope = 'instance'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        instance_count, 50,
        "instance cap unaffected by the offer-concept row"
    );
}

#[test]
fn test_purge_suppressions_at_session_end() {
    let conn = initialize_db(":memory:").unwrap();
    insert_suppression(&conn, "sess1", "borrow-vs-clone", "fp-1", "instance").unwrap();
    insert_suppression(&conn, "sess2", "borrow-vs-clone", "fp-2", "instance").unwrap();

    let purged = purge_suppressions_for_session(&conn, "sess1").unwrap();
    assert_eq!(purged, 1);
    assert!(!is_suppressed(&conn, "sess1", "borrow-vs-clone", "fp-1").unwrap());
    // Other sessions untouched.
    assert!(is_suppressed(&conn, "sess2", "borrow-vs-clone", "fp-2").unwrap());
}

#[test]
fn test_count_instance_snoozes_for_concept() {
    let conn = initialize_db(":memory:").unwrap();
    assert_eq!(
        count_instance_snoozes_for_concept(&conn, "sess1", "borrow-vs-clone").unwrap(),
        0
    );
    insert_suppression(&conn, "sess1", "borrow-vs-clone", "fp-1", "instance").unwrap();
    assert_eq!(
        count_instance_snoozes_for_concept(&conn, "sess1", "borrow-vs-clone").unwrap(),
        1
    );
    // A concept-scope row doesn't count toward the instance tally.
    insert_suppression(
        &conn,
        "sess1",
        "borrow-vs-clone",
        "borrow-vs-clone",
        "concept",
    )
    .unwrap();
    assert_eq!(
        count_instance_snoozes_for_concept(&conn, "sess1", "borrow-vs-clone").unwrap(),
        1
    );
}

// --- T2 req 10: throttle history ---

#[test]
fn test_recent_card_statuses_for_category_excludes_queued() {
    let conn = initialize_db(":memory:").unwrap();
    let mut c1 = make_card("sess1", "c1", "fp-1", "applied");
    c1.category = "idiom".to_string();
    insert_card(&conn, &c1).unwrap();
    let mut c2 = make_card("sess1", "c2", "fp-2", "queued");
    c2.category = "idiom".to_string();
    insert_card(&conn, &c2).unwrap();
    let mut c3 = make_card("sess1", "c3", "fp-3", "not_useful");
    c3.category = "architecture".to_string();
    insert_card(&conn, &c3).unwrap();

    let statuses = recent_card_statuses_for_category(&conn, "idiom", 20).unwrap();
    assert_eq!(statuses, vec!["applied".to_string()]);
}

#[test]
fn test_latest_throttle_action_reads_most_recent_matching_category() {
    let conn = initialize_db(":memory:").unwrap();
    assert_eq!(latest_throttle_action(&conn, "idiom").unwrap(), None);

    log_event(
        &conn,
        &EventRecord {
            id: None,
            session_id: "sess1".to_string(),
            kind: "throttle_change".to_string(),
            payload_json: serde_json::json!({"category": "idiom", "action": "throttled"})
                .to_string(),
            ts: None,
        },
    )
    .unwrap();
    log_event(
        &conn,
        &EventRecord {
            id: None,
            session_id: "sess1".to_string(),
            kind: "throttle_change".to_string(),
            payload_json: serde_json::json!({"category": "bug", "action": "throttled"}).to_string(),
            ts: None,
        },
    )
    .unwrap();

    assert_eq!(
        latest_throttle_action(&conn, "idiom").unwrap(),
        Some("throttled".to_string())
    );

    log_event(
        &conn,
        &EventRecord {
            id: None,
            session_id: "sess1".to_string(),
            kind: "throttle_change".to_string(),
            payload_json: serde_json::json!({"category": "idiom", "action": "unthrottled"})
                .to_string(),
            ts: None,
        },
    )
    .unwrap();
    assert_eq!(
        latest_throttle_action(&conn, "idiom").unwrap(),
        Some("unthrottled".to_string())
    );
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

/// Review fix: migration 8 adds the `cards(advice_fp)` and
/// `cards(category, id)` indexes.
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
    assert_eq!(version, 11);
}

/// T3 migration 9: `goals` (D13(c) obsoletes it) is dropped, and
/// `suppressions.scope` now accepts `offer-concept` (req 13).
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

/// T3 req 6/12: the bookend's card counts and "concepts taught" must
/// exclude `struggle-offer` rows (those exist for EFP accounting on the
/// offer line itself, not real advice cards).
#[test]
fn test_bookend_queries_exclude_struggle_offer_rows() {
    let conn = initialize_db(":memory:").unwrap();
    let session_id = "sess1";

    // A real taught card.
    let mut real_card = make_card(session_id, "borrow-vs-clone", "fp-1", "applied");
    real_card.category = "idiom".to_string();
    insert_card(&conn, &real_card).unwrap();

    // A struggle-offer row (not a real card).
    let mut offer_card = make_card(
        session_id,
        "E0308",
        "struggle-offer:error-streak:E0308",
        "applied",
    );
    offer_card.category = "struggle-offer".to_string();
    insert_card(&conn, &offer_card).unwrap();

    assert_eq!(bookend_shown_count(&conn, session_id).unwrap(), 1);
    assert_eq!(bookend_applied_count(&conn, session_id).unwrap(), 1);
    assert_eq!(
        concepts_taught_this_session(&conn, session_id).unwrap(),
        vec!["borrow-vs-clone".to_string()],
        "E0308 (the offer's pseudo-concept) must not appear as a taught concept"
    );
}

/// T2 acceptance: "event-log completeness for one full scenario" — walks
/// one session through queue -> pull -> snooze-widen -> throttle -> end,
/// exercising every T2 event kind against a real db, and asserts the
/// full event trail is there in order.
#[test]
fn test_t2_event_log_completeness_full_scenario() {
    let conn = initialize_db(":memory:").unwrap();
    let session_id = "sess-t2-scenario";

    log_event(
        &conn,
        &EventRecord {
            id: None,
            session_id: session_id.to_string(),
            kind: "session_start".to_string(),
            payload_json: "{}".to_string(),
            ts: None,
        },
    )
    .unwrap();

    // req 3: a candidate queues instead of being pushed.
    let queued_id = insert_card(
        &conn,
        &make_card(session_id, "iterator-chains", "fp-queue-1", "queued"),
    )
    .unwrap();
    log_event(
        &conn,
        &EventRecord {
            id: None,
            session_id: session_id.to_string(),
            kind: "card_queued".to_string(),
            payload_json: serde_json::json!({"concept": "iterator-chains"}).to_string(),
            ts: None,
        },
    )
    .unwrap();

    // req 3: pulled from the queue via `m` -> becomes shown.
    update_card_status(&conn, queued_id, "shown").unwrap();
    log_event(
        &conn,
        &EventRecord {
            id: None,
            session_id: session_id.to_string(),
            kind: "card_shown".to_string(),
            payload_json:
                serde_json::json!({"concept": "iterator-chains", "pulled_from_queue": true})
                    .to_string(),
            ts: None,
        },
    )
    .unwrap();

    // req 8: first not_now (instance), then a second on the same
    // concept for a different card widens to concept scope.
    update_card_status(&conn, queued_id, "not_now").unwrap();
    insert_suppression(
        &conn,
        session_id,
        "iterator-chains",
        "fp-queue-1",
        "instance",
    )
    .unwrap();
    log_event(
            &conn,
            &EventRecord {
                id: None,
                session_id: session_id.to_string(),
                kind: "card_response".to_string(),
                payload_json: serde_json::json!({"verb": "not_now", "concept": "iterator-chains", "widened": false})
                    .to_string(),
                ts: None,
            },
        )
        .unwrap();

    let other_id = insert_card(
        &conn,
        &make_card(session_id, "iterator-chains", "fp-other-site", "shown"),
    )
    .unwrap();
    update_card_status(&conn, other_id, "not_now").unwrap();
    let prior = count_instance_snoozes_for_concept(&conn, session_id, "iterator-chains").unwrap();
    assert_eq!(prior, 1);
    assert_eq!(
        crate::suppression::tiered_snooze_scope(prior),
        crate::suppression::SnoozeScope::Concept
    );
    insert_suppression(
        &conn,
        session_id,
        "iterator-chains",
        "iterator-chains",
        "concept",
    )
    .unwrap();
    enforce_suppression_cap(&conn, session_id).unwrap();
    log_event(
            &conn,
            &EventRecord {
                id: None,
                session_id: session_id.to_string(),
                kind: "card_response".to_string(),
                payload_json: serde_json::json!({"verb": "not_now", "concept": "iterator-chains", "widened": true})
                    .to_string(),
                ts: None,
            },
        )
        .unwrap();

    // req 10: a category trips the throttle.
    log_event(
        &conn,
        &EventRecord {
            id: None,
            session_id: session_id.to_string(),
            kind: "throttle_change".to_string(),
            payload_json: serde_json::json!({"category": "idiom", "action": "throttled"})
                .to_string(),
            ts: None,
        },
    )
    .unwrap();

    // Session end: expire, purge suppressions.
    expire_unresolved_cards(&conn, session_id).unwrap();
    let purged = purge_suppressions_for_session(&conn, session_id).unwrap();
    assert_eq!(purged, 2, "both the instance and concept snooze rows purge");
    log_event(
        &conn,
        &EventRecord {
            id: None,
            session_id: session_id.to_string(),
            kind: "session_end".to_string(),
            payload_json: serde_json::json!({"expired_cards": 0}).to_string(),
            ts: None,
        },
    )
    .unwrap();

    let events = get_events_for_session(&conn, session_id).unwrap();
    let kinds: Vec<&str> = events.iter().map(|e| e.kind.as_str()).collect();
    assert_eq!(
        kinds,
        vec![
            "session_start",
            "card_queued",
            "card_shown",
            "card_response",
            "card_response",
            "throttle_change",
            "session_end",
        ]
    );

    // The suppressions table is empty post-purge (queue/snoozes die at
    // session end, C2).
    let live_suppressions: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM suppressions WHERE session_id = ?1",
            rusqlite::params![session_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(live_suppressions, 0);
}

// --- T4 reqs 3-4: rung updates + site re-check ---

#[test]
fn test_update_card_rung_and_card_site_roundtrip() {
    let conn = initialize_db(":memory:").unwrap();
    let mut card = make_card("sess1", "borrow-vs-clone", "fp1", "shown");
    card.site_file = Some("src/main.rs".to_string());
    card.site_line = Some(42);
    let id = insert_card(&conn, &card).unwrap();

    assert_eq!(
        card_site(&conn, id).unwrap(),
        Some(("src/main.rs".to_string(), 42))
    );

    update_card_rung(&conn, id, "R3").unwrap();
    let rung: String = conn
        .query_row("SELECT rung_shown FROM cards WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(rung, "R3");
}

// --- T4 req 11: slot contention re-queue never stamps resolved_ts ---

#[test]
fn test_requeue_card_sets_queued_and_never_stamps_resolved_ts() {
    let conn = initialize_db(":memory:").unwrap();
    let id = insert_card(&conn, &make_card("sess1", "c", "fp1", "shown")).unwrap();
    requeue_card(&conn, id).unwrap();

    let (status, resolved_ts): (String, Option<String>) = conn
        .query_row(
            "SELECT status, resolved_ts FROM cards WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "queued");
    assert!(
        resolved_ts.is_none(),
        "a re-queued (displaced) card is not resolved"
    );
}

#[test]
fn test_card_site_none_when_not_set() {
    let conn = initialize_db(":memory:").unwrap();
    let id = insert_card(&conn, &make_card("sess1", "c", "fp1", "shown")).unwrap();
    assert_eq!(card_site(&conn, id).unwrap(), None);
}

// --- T4 reqs 5-8: threads ---

#[test]
fn test_insert_and_get_thread_messages_in_turn_order() {
    let conn = initialize_db(":memory:").unwrap();
    let card_id = insert_card(&conn, &make_card("sess1", "c", "fp1", "shown")).unwrap();

    insert_thread_message(
        &conn,
        &ThreadMessage {
            id: None,
            card_id,
            turn_no: 1,
            role: "user".to_string(),
            content: "why does this need a clone?".to_string(),
            ts: None,
        },
    )
    .unwrap();
    insert_thread_message(
        &conn,
        &ThreadMessage {
            id: None,
            card_id,
            turn_no: 1,
            role: "assistant".to_string(),
            content: "because...".to_string(),
            ts: None,
        },
    )
    .unwrap();

    let msgs = get_thread_messages(&conn, card_id).unwrap();
    assert_eq!(msgs.len(), 2);
    assert_eq!(msgs[0].role, "user");
    assert_eq!(msgs[1].role, "assistant");
    assert_eq!(thread_user_turn_count(&conn, card_id).unwrap(), 1);
}

#[test]
fn test_thread_user_turn_count_only_counts_user_role() {
    let conn = initialize_db(":memory:").unwrap();
    let card_id = insert_card(&conn, &make_card("sess1", "c", "fp1", "shown")).unwrap();
    for turn in 1..=3 {
        insert_thread_message(
            &conn,
            &ThreadMessage {
                id: None,
                card_id,
                turn_no: turn,
                role: "user".to_string(),
                content: "q".to_string(),
                ts: None,
            },
        )
        .unwrap();
        insert_thread_message(
            &conn,
            &ThreadMessage {
                id: None,
                card_id,
                turn_no: turn,
                role: "assistant".to_string(),
                content: "a".to_string(),
                ts: None,
            },
        )
        .unwrap();
    }
    assert_eq!(thread_user_turn_count(&conn, card_id).unwrap(), 3);
}

#[test]
fn test_unresolved_thread_concepts_excludes_terminal_cards() {
    let conn = initialize_db(":memory:").unwrap();
    let unresolved_id = insert_card(
        &conn,
        &make_card("sess1", "borrow-vs-clone", "fp1", "shown"),
    )
    .unwrap();
    let resolved_id = insert_card(
        &conn,
        &make_card("sess1", "string-vs-str", "fp2", "applied"),
    )
    .unwrap();

    for id in [unresolved_id, resolved_id] {
        insert_thread_message(
            &conn,
            &ThreadMessage {
                id: None,
                card_id: id,
                turn_no: 1,
                role: "user".to_string(),
                content: "q".to_string(),
                ts: None,
            },
        )
        .unwrap();
    }

    let unresolved = unresolved_thread_concepts(&conn, "sess1").unwrap();
    assert_eq!(unresolved, vec!["borrow-vs-clone".to_string()]);
}

#[test]
fn test_unresolved_thread_concepts_ignores_cards_without_threads() {
    let conn = initialize_db(":memory:").unwrap();
    insert_card(
        &conn,
        &make_card("sess1", "borrow-vs-clone", "fp1", "shown"),
    )
    .unwrap();
    assert!(
        unresolved_thread_concepts(&conn, "sess1")
            .unwrap()
            .is_empty()
    );
}

// --- T4 reqs 9-11: murshid-comments ---

#[test]
fn test_clear_suppressions_for_concept_clears_session_and_offer_scoped() {
    let conn = initialize_db(":memory:").unwrap();
    insert_suppression(
        &conn,
        "sess1",
        "borrow-vs-clone",
        "borrow-vs-clone",
        "concept",
    )
    .unwrap();
    insert_offer_suppression(&conn, "sess-old", "borrow-vs-clone", 9_999_999_999).unwrap();
    insert_suppression(&conn, "sess1", "string-vs-str", "string-vs-str", "concept").unwrap();

    let cleared = clear_suppressions_for_concept(&conn, "sess1", "borrow-vs-clone").unwrap();
    assert_eq!(
        cleared, 2,
        "both the session-scoped and offer-concept rows clear"
    );

    assert!(!is_suppressed(&conn, "sess1", "borrow-vs-clone", "borrow-vs-clone").unwrap());
    assert!(!is_offer_suppressed(&conn, "borrow-vs-clone", 0).unwrap());
    // Unrelated concept's suppression survives.
    assert!(is_suppressed(&conn, "sess1", "string-vs-str", "string-vs-str").unwrap());
}

/// T4 req 10 hygiene: an answered-but-unremoved murshid-comment (its
/// `cards` row reached a ledger-blocking status, e.g. `got_it`) never
/// re-triggers — the same mechanism T2 proved for ordinary cards
/// (`find_ledger_card`), keyed on the comment's own
/// `(comment_text_hash, site)` advice-fp instead of `(concept, site)`.
#[test]
fn test_answered_comment_never_retriggers_via_ledger_dedup() {
    let conn = initialize_db(":memory:").unwrap();
    let src = "fn foo() {\n    let x = y.clone();\n}\n";
    let site =
        crate::site::compute_site("src/lib.rs", src, 2, &crate::pack::GrammarSpec::default())
            .unwrap();
    let comment_fp =
        crate::comment::comment_advice_fingerprint("why does this need a clone?", &site);

    // Not yet answered: no ledger entry.
    assert!(find_ledger_card(&conn, &comment_fp).unwrap().is_none());

    let mut answered = make_card("sess1", "borrow-vs-clone", &comment_fp, "got_it");
    answered.category = COMMENT_ASK_CATEGORY.to_string();
    insert_card(&conn, &answered).unwrap();

    // The exact same comment (same text, same site) resolves to the
    // same fingerprint and is now permanently ledger-blocked.
    let comment_fp_again =
        crate::comment::comment_advice_fingerprint("why does this need a clone?", &site);
    assert_eq!(comment_fp, comment_fp_again);
    let ledger = find_ledger_card(&conn, &comment_fp_again).unwrap();
    assert!(ledger.is_some(), "answered comment must be ledger-blocked");
    assert_eq!(ledger.unwrap().1, "got_it");
}

#[test]
fn test_unresolved_comment_ask_concepts() {
    let conn = initialize_db(":memory:").unwrap();
    let mut unresolved = make_card("sess1", "borrow-vs-clone", "fp1", "shown");
    unresolved.category = COMMENT_ASK_CATEGORY.to_string();
    insert_card(&conn, &unresolved).unwrap();

    let mut resolved = make_card("sess1", "string-vs-str", "fp2", "got_it");
    resolved.category = COMMENT_ASK_CATEGORY.to_string();
    insert_card(&conn, &resolved).unwrap();

    // A normal (non-comment-ask) shown card must not leak in.
    insert_card(
        &conn,
        &make_card("sess1", "iterator-chains", "fp3", "shown"),
    )
    .unwrap();

    let unresolved_concepts = unresolved_comment_ask_concepts(&conn, "sess1").unwrap();
    assert_eq!(unresolved_concepts, vec!["borrow-vs-clone".to_string()]);
}

// --- T4 reqs 9-13: EFP-exempt categories excluded from the bookend ---

#[test]
fn test_bookend_queries_exclude_comment_ask_and_review_categories() {
    let conn = initialize_db(":memory:").unwrap();
    let mut comment_card = make_card("sess1", "borrow-vs-clone", "fp1", "applied");
    comment_card.category = COMMENT_ASK_CATEGORY.to_string();
    insert_card(&conn, &comment_card).unwrap();

    let mut review_card = make_card("sess1", "string-vs-str", "fp2", "shown");
    review_card.category = REVIEW_CATEGORY.to_string();
    insert_card(&conn, &review_card).unwrap();

    assert_eq!(bookend_shown_count(&conn, "sess1").unwrap(), 0);
    assert_eq!(bookend_applied_count(&conn, "sess1").unwrap(), 0);
    assert!(
        concepts_taught_this_session(&conn, "sess1")
            .unwrap()
            .is_empty()
    );
}

// --- T13 req 1 / T5 req 7: last-session natural-encounter gate ---

fn log_encounter(conn: &Connection, session_id: &str, concept: &str, source: &str) {
    log_event(
        conn,
        &EventRecord {
            id: None,
            session_id: session_id.to_string(),
            kind: "encounter".to_string(),
            payload_json: serde_json::json!({
                "concept": concept,
                "grade": "pass",
                "source": source,
            })
            .to_string(),
            ts: None,
        },
    )
    .unwrap();
}

#[test]
fn test_concepts_encountered_last_session_reads_the_immediately_prior_session() {
    let conn = initialize_db(":memory:").unwrap();
    // Session ids are ULID-shaped and lexically sortable — earlier
    // sessions sort before later ones (see session::generate_session_id).
    log_encounter(&conn, "00000000000000000000000001", "c1", "detection");
    log_encounter(&conn, "00000000000000000000000002", "c2", "detection");

    let last = concepts_encountered_last_session(&conn, "00000000000000000000000003").unwrap();
    assert_eq!(last, std::collections::HashSet::from(["c2".to_string()]));
}

#[test]
fn test_concepts_encountered_last_session_excludes_retrieval_sourced_encounters() {
    let conn = initialize_db(":memory:").unwrap();
    log_encounter(&conn, "00000000000000000000000001", "c1", "retrieval");

    let last = concepts_encountered_last_session(&conn, "00000000000000000000000002").unwrap();
    assert!(
        last.is_empty(),
        "a retrieval-question encounter is not a NATURAL one"
    );
}

#[test]
fn test_concepts_encountered_last_session_empty_when_no_prior_session() {
    let conn = initialize_db(":memory:").unwrap();
    let last = concepts_encountered_last_session(&conn, "00000000000000000000000001").unwrap();
    assert!(last.is_empty());
}

// --- T13 req 5: concept_memory migration-11 schema ---

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

// --- T13 req 6: insert_thread_message pairs with a `thread_msg` event ---

/// `insert_thread_message` itself only performs the `threads` INSERT —
/// the paired `thread_msg` event is logged by the call site (see
/// `watch/keys.rs`'s thread-turn handler), immediately after, once per
/// insert. This test pins that data-layer contract so a docstring/
/// behavior drift here is caught even though the emission itself lives
/// one layer up.
#[test]
fn test_thread_turn_pairs_insert_with_a_thread_msg_event() {
    let conn = initialize_db(":memory:").unwrap();
    let card_id = insert_card(
        &conn,
        &make_card("sess1", "borrow-vs-clone", "fp1", "shown"),
    )
    .unwrap();

    insert_thread_message(
        &conn,
        &ThreadMessage {
            id: None,
            card_id,
            turn_no: 1,
            role: "user".to_string(),
            content: "why does this need a clone?".to_string(),
            ts: None,
        },
    )
    .unwrap();
    log_event(
        &conn,
        &EventRecord {
            id: None,
            session_id: "sess1".to_string(),
            kind: "thread_msg".to_string(),
            payload_json: serde_json::json!({
                "card_id": card_id,
                "role": "user",
                "turn_no": 1,
            })
            .to_string(),
            ts: None,
        },
    )
    .unwrap();

    let events = get_events_for_session(&conn, "sess1").unwrap();
    let thread_events: Vec<_> = events.iter().filter(|e| e.kind == "thread_msg").collect();
    assert_eq!(
        thread_events.len(),
        1,
        "one thread_msg event per thread-message insert"
    );
    assert!(thread_events[0].payload_json.contains("\"role\":\"user\""));
}

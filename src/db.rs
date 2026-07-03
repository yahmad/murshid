use rusqlite::{Connection, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::Duration;

pub fn get_db_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        crate::config::get_home_dir()
            .map(|h| h.join("Library/Application Support/murshid/profile.db"))
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
    F: FnMut() -> Result<T, rusqlite::Error>,
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
                        (backoff.as_millis() as i64 + jitter_ms).max(1) as u64,
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
            if let Ok(timestamp) =
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            {
                let corrupt_path =
                    path_ref.with_extension(format!("db.corrupt.{}", timestamp.as_secs()));
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

    let _ = current_version;
    Ok(())
}

/// T4 (comment-asks, req 9-11; solicited review, req 12-13): both surfaces
/// are pull-priced/solicited and EFP-exempt (C3) — same treatment as T3's
/// `struggle-offer`. Cards stored under these `category` values are excluded
/// from EFP/throttle windows (they're simply never in the fixed category
/// list `main.rs` iterates) and from the bookend's "concepts taught"/counts,
/// mirrored below.
const EFP_EXEMPT_CATEGORIES: [&str; 3] = ["struggle-offer", "comment-ask", "review"];

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct EventRecord {
    pub id: Option<i64>,
    pub session_id: String,
    pub kind: String,
    pub payload_json: String,
    pub ts: Option<String>,
}

/// C5 `events` — append-only. Kinds are the C5-enumerated vocabulary, plus
/// T1's `judge_drop`, and T2's `card_queued`/`card_aggregated` (additive,
/// same rationale as `judge_drop`: `kind` isn't restricted to the C5 list —
/// `card_response`/`throttle_change` cover snooze/throttle transitions using
/// the C5-enumerated kinds directly, per req 11).
pub fn log_event(conn: &Connection, event: &EventRecord) -> Result<i64, rusqlite::Error> {
    execute_with_retry(|| {
        conn.execute(
            "INSERT INTO events (session_id, kind, payload_json) VALUES (?1, ?2, ?3)",
            rusqlite::params![event.session_id, event.kind, event.payload_json],
        )?;
        Ok(conn.last_insert_rowid())
    })
}

pub fn get_events_for_session(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<EventRecord>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT id, session_id, kind, payload_json, ts FROM events WHERE session_id = ?1 ORDER BY id ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![session_id], |row| {
        Ok(EventRecord {
            id: Some(row.get(0)?),
            session_id: row.get(1)?,
            kind: row.get(2)?,
            payload_json: row.get(3)?,
            ts: Some(row.get(4)?),
        })
    })?;
    let mut events = Vec::new();
    for row in rows {
        events.push(row?);
    }
    Ok(events)
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct CardRecord {
    pub id: Option<i64>,
    pub session_id: String,
    pub concept_id: String,
    pub category: String,
    pub rung_shown: String,
    pub advice_fp: String,
    pub finding_fp: Option<String>,
    pub status: String,
    pub created_ts: Option<String>,
    pub resolved_ts: Option<String>,
    /// T1 fix-round req 9: the worked diff is a mandatory stage-2 leg and
    /// must be persisted, not just validated then discarded, so the folded
    /// "(fix available — full interaction in T4)" line has something real
    /// behind it.
    pub worked_diff: Option<String>,
    /// T2 req 9 regression re-open: set when this card row re-opens a prior
    /// `applied`/`resolved` card whose advice-fp regressed; `None` for an
    /// ordinary first-time card.
    pub regresses_card_id: Option<i64>,
    /// T4 req 1: the (file, line) the card's site was computed at, so a
    /// later quiescence pass can mechanically re-check whether the flagged
    /// pattern is still there. `None` for cards with no site (e.g. struggle
    /// offers).
    pub site_file: Option<String>,
    pub site_line: Option<i64>,
}

/// C5 `cards` — one row per shown OR queued card (T2 req 3: a queued card is
/// persisted with `status='queued'` so cross-save/cross-session dedup sees
/// it without needing a second store); `status` tracks the response verb
/// (C3 enum) plus T2's `queued`.
pub fn insert_card(conn: &Connection, card: &CardRecord) -> Result<i64, rusqlite::Error> {
    execute_with_retry(|| {
        conn.execute(
            "INSERT INTO cards (session_id, concept_id, category, rung_shown, advice_fp, finding_fp, status, worked_diff, regresses_card_id, site_file, site_line)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                card.session_id,
                card.concept_id,
                card.category,
                card.rung_shown,
                card.advice_fp,
                card.finding_fp,
                card.status,
                card.worked_diff,
                card.regresses_card_id,
                card.site_file,
                card.site_line,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    })
}

/// Updates a card's lifecycle status (C3 response verb / `expired`); any
/// non-`shown` status stamps `resolved_ts`.
pub fn update_card_status(
    conn: &Connection,
    card_id: i64,
    status: &str,
) -> Result<(), rusqlite::Error> {
    execute_with_retry(|| {
        conn.execute(
            "UPDATE cards SET status = ?1,
                resolved_ts = CASE WHEN ?1 != 'shown' THEN CURRENT_TIMESTAMP ELSE resolved_ts END
             WHERE id = ?2",
            rusqlite::params![status, card_id],
        )?;
        Ok(())
    })
}

/// T4 req 11 / C7 slot contention: a displaced pushed card returns to the
/// queue head — its `cards` row goes back to `status='queued'`. Deliberately
/// NOT [`update_card_status`]: that helper stamps `resolved_ts` on every
/// non-`shown` status, but `queued` is not a resolution (the card is still
/// live, just off-screen again).
pub fn requeue_card(conn: &Connection, card_id: i64) -> Result<(), rusqlite::Error> {
    execute_with_retry(|| {
        conn.execute(
            "UPDATE cards SET status = 'queued' WHERE id = ?1",
            rusqlite::params![card_id],
        )?;
        Ok(())
    })
}

/// T1 req 10 / C3: "no interaction by session end ⇒ expired". Marks every
/// still-`shown` card in `session_id` as `expired` and returns how many were
/// affected, so the caller can log the `session_end` event alongside it.
pub fn expire_unresolved_cards(
    conn: &Connection,
    session_id: &str,
) -> Result<usize, rusqlite::Error> {
    execute_with_retry(|| {
        conn.execute(
            "UPDATE cards SET status = 'expired', resolved_ts = CURRENT_TIMESTAMP
             WHERE session_id = ?1 AND status = 'shown'",
            rusqlite::params![session_id],
        )
    })
}

/// T1 req 8 / C2: within a session, identical (concept, site) advice-
/// fingerprints are never judged or shown twice.
pub fn card_exists_with_advice_fp(
    conn: &Connection,
    session_id: &str,
    advice_fp: &str,
) -> Result<bool, rusqlite::Error> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM cards WHERE session_id = ?1 AND advice_fp = ?2",
        rusqlite::params![session_id, advice_fp],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// C3/T2 req 5: statuses that make a card row a permanent member of the
/// I3 never-re-raise ledger (`queued`/`shown`/`expired` are not terminal in
/// this sense).
const LEDGER_STATUSES: [&str; 4] = ["applied", "got_it", "not_useful", "resolved"];

/// T2 req 9: the subset of ledger statuses a regression is allowed to
/// re-open (misuse re-opens *taught* advice, not a dismissed-as-unhelpful
/// one).
const REGRESSION_ELIGIBLE_STATUSES: [&str; 2] = ["applied", "resolved"];

/// T2 req 5/9: the most recent ledger-blocking card (any session) whose
/// advice-fp matches, if any — `(card_id, status)`.
pub fn find_ledger_card(
    conn: &Connection,
    advice_fp: &str,
) -> Result<Option<(i64, String)>, rusqlite::Error> {
    let placeholders = LEDGER_STATUSES
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT id, status FROM cards WHERE advice_fp = ?1 AND status IN ({}) ORDER BY id DESC LIMIT 1",
        placeholders
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&advice_fp];
    for s in LEDGER_STATUSES.iter() {
        params.push(s);
    }
    let mut rows = stmt.query(params.as_slice())?;
    if let Some(row) = rows.next()? {
        Ok(Some((row.get(0)?, row.get(1)?)))
    } else {
        Ok(None)
    }
}

/// T2 req 9: whether a ledger-blocked card's status is regression-eligible
/// (`applied`/`resolved`, not `got_it`/`not_useful`).
pub fn is_regression_eligible(status: &str) -> bool {
    REGRESSION_ELIGIBLE_STATUSES.contains(&status)
}

/// T3 consolidation (from the T2 re-review): the shared status-set constant
/// for "statuses meaning the user actually saw the card" — everything
/// except `queued` (never reached the screen) and `collapsed` (folded into
/// a sibling card's aggregation before it ever reached the screen). Used by
/// both [`concept_shown_this_session`] (cooldown gate) and
/// [`recent_card_statuses_for_category`] (EFP/throttle window), which used
/// to disagree on `collapsed`; T3's bookend "shown" count uses it too.
pub const SEEN_STATUSES: [&str; 7] = [
    "shown",
    "applied",
    "escalated",
    "got_it",
    "not_now",
    "not_useful",
    "expired",
];

/// T2 req 7 / C8 concept cooldown: has any card actually shipped (pushed —
/// i.e. reached the screen) for `concept_id` in this session already?
pub fn concept_shown_this_session(
    conn: &Connection,
    session_id: &str,
    concept_id: &str,
) -> Result<bool, rusqlite::Error> {
    let placeholders = SEEN_STATUSES.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT COUNT(*) FROM cards WHERE session_id = ? AND concept_id = ? AND status IN ({})",
        placeholders
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&session_id, &concept_id];
    for s in SEEN_STATUSES.iter() {
        params.push(s);
    }
    let count: i64 = stmt.query_row(params.as_slice(), |row| row.get(0))?;
    Ok(count > 0)
}

/// T2 req 8 (D11(c) tiered snooze) / C5 `suppressions`. For `scope='instance'`
/// rows, `advice_fp` is the exact (concept, site) fingerprint; for
/// `scope='concept'` rows it holds `concept_id` too, so a single equality
/// check on the matching column covers either scope (see the migration-7
/// comment for why).
pub fn insert_suppression(
    conn: &Connection,
    session_id: &str,
    concept_id: &str,
    advice_fp: &str,
    scope: &str,
) -> Result<i64, rusqlite::Error> {
    execute_with_retry(|| {
        conn.execute(
            "INSERT INTO suppressions (session_id, concept_id, advice_fp, scope) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![session_id, concept_id, advice_fp, scope],
        )?;
        Ok(conn.last_insert_rowid())
    })
}

/// T2 req 8: how many `instance`-scope not_now snoozes exist for `concept_id`
/// in this session so far — the tier-widening trigger is "a second not_now
/// on the SAME concept".
pub fn count_instance_snoozes_for_concept(
    conn: &Connection,
    session_id: &str,
    concept_id: &str,
) -> Result<u32, rusqlite::Error> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM suppressions WHERE session_id = ?1 AND concept_id = ?2 AND scope = 'instance'",
        rusqlite::params![session_id, concept_id],
        |row| row.get(0),
    )?;
    Ok(count as u32)
}

/// T2 req 8: whether `advice_fp`/`concept_id` is currently snoozed this
/// session, at either scope.
pub fn is_suppressed(
    conn: &Connection,
    session_id: &str,
    concept_id: &str,
    advice_fp: &str,
) -> Result<bool, rusqlite::Error> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM suppressions WHERE session_id = ?1
            AND ((scope = 'instance' AND advice_fp = ?2) OR (scope = 'concept' AND concept_id = ?3))",
        rusqlite::params![session_id, advice_fp, concept_id],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// T2 req 8 / C12: live cap 50, oldest expire first. Trims `session_id`'s
/// suppression rows down to the 50 most recent after an insert. Review fix
/// (mirrors `purge_suppressions_for_session`): `offer-concept` rows (req
/// 13's 7-day cross-session persistence) are excluded from both the count
/// and the eviction — a busy session's instance/concept snoozes must never
/// be able to evict a struggle-offer suppression, and an offer-concept row
/// must never itself count against another session's 50-row cap.
pub fn enforce_suppression_cap(conn: &Connection, session_id: &str) -> Result<(), rusqlite::Error> {
    execute_with_retry(|| {
        conn.execute(
            "DELETE FROM suppressions WHERE session_id = ?1 AND scope != 'offer-concept' AND id NOT IN (
                SELECT id FROM suppressions WHERE session_id = ?1 AND scope != 'offer-concept' ORDER BY id DESC LIMIT 50
             )",
            rusqlite::params![session_id],
        )?;
        Ok(())
    })
}

/// T2 req 3/8 / C2: the pull queue and all snoozes die at session end.
/// T3 req 13's `offer-concept` suppression rows are the deliberate
/// exception (spec-mandated 7-day cross-session persistence) — excluded
/// from this purge so a struggle-offer suppression outlives the session
/// that created it.
pub fn purge_suppressions_for_session(
    conn: &Connection,
    session_id: &str,
) -> Result<usize, rusqlite::Error> {
    execute_with_retry(|| {
        conn.execute(
            "DELETE FROM suppressions WHERE session_id = ?1 AND scope != 'offer-concept'",
            rusqlite::params![session_id],
        )
    })
}

/// T2 req 10 / C3: the last `limit` counted (i.e. actually shown, not merely
/// queued) card statuses for `category`, most-recent-first, across all
/// sessions — the input to the action-rate/throttle computation. Uses the
/// same [`SEEN_STATUSES`] constant as [`concept_shown_this_session`].
pub fn recent_card_statuses_for_category(
    conn: &Connection,
    category: &str,
    limit: u32,
) -> Result<Vec<String>, rusqlite::Error> {
    let placeholders = SEEN_STATUSES.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT status FROM cards WHERE category = ? AND status IN ({}) ORDER BY id DESC LIMIT ?",
        placeholders
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&category];
    for s in SEEN_STATUSES.iter() {
        params.push(s);
    }
    params.push(&limit);
    let rows = stmt.query_map(params.as_slice(), |row| row.get(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

// --- T3 req 6 / T4 reqs 9-13: session-end bookend support ---
//
// All four queries below exclude EFP-exempt category rows
// (`EFP_EXEMPT_CATEGORIES`: struggle-offer, comment-ask, review): those
// `cards` rows exist purely for their own accounting (or, for comment-ask/
// review, are logged EFP-exempt per C3/D17/D18), not real pushed/pulled
// advice cards — counting them here would pollute "concepts taught" with
// raw E-codes/comment snippets/review digest entries instead of taxonomy
// concept names surfaced through the normal ladder.
fn efp_exempt_category_clause() -> String {
    let quoted: Vec<String> = EFP_EXEMPT_CATEGORIES
        .iter()
        .map(|c| format!("'{}'", c))
        .collect();
    format!("category NOT IN ({})", quoted.join(","))
}

/// req 6: total cards that actually reached the screen this session.
pub fn bookend_shown_count(conn: &Connection, session_id: &str) -> Result<usize, rusqlite::Error> {
    let placeholders = SEEN_STATUSES.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT COUNT(*) FROM cards WHERE session_id = ? AND {} AND status IN ({})",
        efp_exempt_category_clause(),
        placeholders
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&session_id];
    for s in SEEN_STATUSES.iter() {
        params.push(s);
    }
    let count: i64 = stmt.query_row(params.as_slice(), |row| row.get(0))?;
    Ok(count as usize)
}

/// req 6: cards actually applied this session.
pub fn bookend_applied_count(conn: &Connection, session_id: &str) -> Result<usize, rusqlite::Error> {
    let sql = format!(
        "SELECT COUNT(*) FROM cards WHERE session_id = ?1 AND {} AND status = 'applied'",
        efp_exempt_category_clause()
    );
    let count: i64 = conn.query_row(&sql, rusqlite::params![session_id], |row| row.get(0))?;
    Ok(count as usize)
}

/// req 6: cards still sitting in the queue, never shown, as the session ends.
pub fn bookend_queued_unshown_count(
    conn: &Connection,
    session_id: &str,
) -> Result<usize, rusqlite::Error> {
    let sql = format!(
        "SELECT COUNT(*) FROM cards WHERE session_id = ?1 AND {} AND status = 'queued'",
        efp_exempt_category_clause()
    );
    let count: i64 = conn.query_row(&sql, rusqlite::params![session_id], |row| row.get(0))?;
    Ok(count as usize)
}

/// req 6: distinct concept slugs actually taught (reached the screen) this
/// session, in first-shown order — names only, per D14's bookend rule; the
/// caller maps slugs to human names via the pack taxonomy.
pub fn concepts_taught_this_session(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<String>, rusqlite::Error> {
    let placeholders = SEEN_STATUSES.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let sql = format!(
        "SELECT concept_id FROM cards WHERE session_id = ? AND {} AND status IN ({}) GROUP BY concept_id ORDER BY MIN(id) ASC",
        efp_exempt_category_clause(),
        placeholders
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&session_id];
    for s in SEEN_STATUSES.iter() {
        params.push(s);
    }
    let rows = stmt.query_map(params.as_slice(), |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

// --- T3 reqs 7-8: struggle-signal baseline support ---

/// req 8: every `check_result` event (across all sessions — the user's own
/// history) as `struggle::CheckResultPoint`s, chronological.
pub fn all_check_result_points(
    conn: &Connection,
) -> Result<Vec<crate::struggle::CheckResultPoint>, rusqlite::Error> {
    let mut stmt =
        conn.prepare("SELECT payload_json FROM events WHERE kind = 'check_result' ORDER BY id ASC")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        let payload = row?;
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&payload) {
            let success = v.get("success").and_then(|s| s.as_bool());
            let ts_ms = v.get("ts_ms").and_then(|t| t.as_u64());
            if let (Some(success), Some(ts_ms)) = (success, ts_ms) {
                out.push(crate::struggle::CheckResultPoint {
                    success,
                    ts_ms: ts_ms as u128,
                });
            }
        }
    }
    Ok(out)
}

// --- T3 reqs 12-13: struggle-offer decline persistence ---

/// req 13: `offer-concept`-scoped suppression, `expires_ts` an epoch-
/// seconds absolute deadline (7 days out from the second decline).
pub fn insert_offer_suppression(
    conn: &Connection,
    session_id: &str,
    concept_id: &str,
    expires_ts_epoch_secs: i64,
) -> Result<i64, rusqlite::Error> {
    execute_with_retry(|| {
        conn.execute(
            "INSERT INTO suppressions (session_id, concept_id, advice_fp, scope, expires_ts) VALUES (?1, ?2, ?2, 'offer-concept', ?3)",
            rusqlite::params![session_id, concept_id, expires_ts_epoch_secs],
        )?;
        Ok(conn.last_insert_rowid())
    })
}

/// req 13: whether `concept_id`'s offers are currently suppressed (a live,
/// unexpired `offer-concept` row exists).
pub fn is_offer_suppressed(
    conn: &Connection,
    concept_id: &str,
    now_epoch_secs: i64,
) -> Result<bool, rusqlite::Error> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM suppressions WHERE scope = 'offer-concept' AND concept_id = ?1 AND (expires_ts IS NULL OR expires_ts > ?2)",
        rusqlite::params![concept_id, now_epoch_secs],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// req 13: how many DECLINED `prompt_response` events exist (across all
/// sessions) for `concept_id` — the cross-session decline count that
/// triggers the 7-day `offer-concept` suppression on the second decline.
pub fn count_declined_offers_for_concept(
    conn: &Connection,
    concept_id: &str,
) -> Result<u32, rusqlite::Error> {
    let mut stmt =
        conn.prepare("SELECT payload_json FROM events WHERE kind = 'prompt_response'")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut count = 0u32;
    for row in rows {
        let payload = row?;
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&payload) {
            if v.get("concept").and_then(|c| c.as_str()) == Some(concept_id)
                && v.get("verb").and_then(|a| a.as_str()) == Some("declined")
            {
                count += 1;
            }
        }
    }
    Ok(count)
}

/// T2 req 10 / C5: throttle state is "computed from events, never stored" —
/// this returns the most recently logged `throttle_change` action
/// (`"throttled"`/`"unthrottled"`) for `category`, if any, so the caller can
/// decide whether a freshly-computed trip is actually a *transition* worth
/// logging again.
pub fn latest_throttle_action(
    conn: &Connection,
    category: &str,
) -> Result<Option<String>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT payload_json FROM events WHERE kind = 'throttle_change' ORDER BY id DESC",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    for row in rows {
        let payload = row?;
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&payload) {
            if v.get("category").and_then(|c| c.as_str()) == Some(category) {
                return Ok(v
                    .get("action")
                    .and_then(|a| a.as_str())
                    .map(|s| s.to_string()));
            }
        }
    }
    Ok(None)
}

// --- T4 req 3-4 / C4: rung-shown updates (escalation + R3 reveal logging) ---

/// req 3/4: updates a card's `rung_shown` (C4). Called on every escalation
/// step AND on the R3 reveal itself, so click-through gaming is visible in
/// the data (I19) — the caller logs the paired `card_response` event with
/// verb `escalated` and the from/to rungs.
pub fn update_card_rung(conn: &Connection, card_id: i64, rung: &str) -> Result<(), rusqlite::Error> {
    execute_with_retry(|| {
        conn.execute(
            "UPDATE cards SET rung_shown = ?1 WHERE id = ?2",
            rusqlite::params![rung, card_id],
        )?;
        Ok(())
    })
}

/// req 1: the card's stored site (file, line), if any — the input to the
/// mechanical applied-detection re-check.
pub fn card_site(
    conn: &Connection,
    card_id: i64,
) -> Result<Option<(String, i64)>, rusqlite::Error> {
    conn.query_row(
        "SELECT site_file, site_line FROM cards WHERE id = ?1",
        rusqlite::params![card_id],
        |row| {
            let file: Option<String> = row.get(0)?;
            let line: Option<i64> = row.get(1)?;
            Ok(file.zip(line))
        },
    )
}

// --- T4 reqs 5-8 / C5 `threads`, D20: card-anchored follow-up threads ---

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ThreadMessage {
    pub id: Option<i64>,
    pub card_id: i64,
    pub turn_no: i64,
    pub role: String, // "user" | "assistant"
    pub content: String,
    pub ts: Option<String>,
}

/// req 7: appends one thread turn (C5 `threads(card_id, turn_no, role,
/// content, ts)`), and logs the paired `thread_msg` event (C5 kind).
pub fn insert_thread_message(
    conn: &Connection,
    msg: &ThreadMessage,
) -> Result<i64, rusqlite::Error> {
    execute_with_retry(|| {
        conn.execute(
            "INSERT INTO threads (card_id, turn_no, role, content) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![msg.card_id, msg.turn_no, msg.role, msg.content],
        )?;
        Ok(conn.last_insert_rowid())
    })
}

/// req 5-7: the full transcript for one card, in turn order — the anchor-
/// scoped "thread history" leg of the thread-context payload.
pub fn get_thread_messages(
    conn: &Connection,
    card_id: i64,
) -> Result<Vec<ThreadMessage>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT id, card_id, turn_no, role, content, ts FROM threads WHERE card_id = ?1 ORDER BY turn_no ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![card_id], |row| {
        Ok(ThreadMessage {
            id: Some(row.get(0)?),
            card_id: row.get(1)?,
            turn_no: row.get(2)?,
            role: row.get(3)?,
            content: row.get(4)?,
            ts: Some(row.get(5)?),
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// req 5 / C12: how many USER turns this card's thread has had so far — the
/// input to the 5-turn cap.
pub fn thread_user_turn_count(conn: &Connection, card_id: i64) -> Result<u32, rusqlite::Error> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM threads WHERE card_id = ?1 AND role = 'user'",
        rusqlite::params![card_id],
        |row| row.get(0),
    )?;
    Ok(count as u32)
}

/// C3's terminal card-lifecycle statuses — a card with none of these hasn't
/// gotten a "terminal response" yet (T4 reqs 7/10's bookend "unresolved"
/// definition). `escalated`/`shown`/`queued`/`collapsed` are all non-
/// terminal (an escalation is an interim event, not a lifecycle verb).
pub const TERMINAL_STATUSES: [&str; 5] =
    ["applied", "got_it", "not_now", "not_useful", "expired"];

fn non_terminal_clause() -> String {
    let quoted: Vec<String> = TERMINAL_STATUSES.iter().map(|s| format!("'{}'", s)).collect();
    format!("status NOT IN ({})", quoted.join(","))
}

/// req 7: concept names (via `concept_id`) of cards this session that have
/// at least one thread message AND no terminal C3 response yet — the
/// bookend's "unresolved threads" list.
pub fn unresolved_thread_concepts(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<String>, rusqlite::Error> {
    let sql = format!(
        "SELECT DISTINCT c.concept_id FROM cards c
         JOIN threads t ON t.card_id = c.id
         WHERE c.session_id = ?1 AND {}
         ORDER BY c.id ASC",
        non_terminal_clause()
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![session_id], |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

// --- T4 reqs 9-11 / D17: murshid-comments (direct asks) ---

/// req 9: comment-ask cards are stored under this `category` — pull-priced,
/// EFP-exempt (see `EFP_EXEMPT_CATEGORIES`), mirroring `offer::OFFER_CATEGORY`.
pub const COMMENT_ASK_CATEGORY: &str = "comment-ask";

/// req 10: a direct ask on a concept clears that concept's suppressions —
/// both session-scoped snooze tiers (`instance`/`concept`) AND the
/// cross-session `offer-concept` decline suppression ("asking trumps 'not
/// now'"). Session-scoped rows are matched by `session_id`; `offer-concept`
/// rows are cleared regardless of session (they're cross-session by design).
pub fn clear_suppressions_for_concept(
    conn: &Connection,
    session_id: &str,
    concept_id: &str,
) -> Result<usize, rusqlite::Error> {
    execute_with_retry(|| {
        conn.execute(
            "DELETE FROM suppressions WHERE concept_id = ?1 AND (scope = 'offer-concept' OR session_id = ?2)",
            rusqlite::params![concept_id, session_id],
        )
    })
}

/// req 10: concept names of cards this session in the `comment-ask` category
/// with no terminal response yet — the bookend's "unresolved murshid-
/// comments" list.
pub fn unresolved_comment_ask_concepts(
    conn: &Connection,
    session_id: &str,
) -> Result<Vec<String>, rusqlite::Error> {
    let sql = format!(
        "SELECT concept_id FROM cards WHERE session_id = ?1 AND category = '{}' AND {} ORDER BY id ASC",
        COMMENT_ASK_CATEGORY,
        non_terminal_clause()
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![session_id], |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

// --- T4 req 12 / D18: solicited review ---

/// req 12: review digest cards are stored under this `category` — solicited,
/// EFP-exempt (see `EFP_EXEMPT_CATEGORIES`).
pub const REVIEW_CATEGORY: &str = "review";

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
        assert_eq!(version, 10);
        drop(conn2);

        fn run_faulty_migration(conn: &mut Connection) -> Result<(), rusqlite::Error> {
            let tx = conn.transaction()?;
            tx.execute("INSERT INTO non_existent_table_to_fail VALUES (1);", [])?;
            tx.execute(
                "INSERT INTO user_profile (user_id, user_email_hash) VALUES ('fail', 'fail');",
                [],
            )?;
            tx.execute("PRAGMA user_version = 11;", [])?;
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
        assert_eq!(version, 10);

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
        assert_eq!(instance_count, 50, "instance cap unaffected by the offer-concept row");
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
                payload_json: serde_json::json!({"category": "bug", "action": "throttled"})
                    .to_string(),
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
        assert_eq!(version, 10);
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
        let mut offer_card = make_card(session_id, "E0308", "struggle-offer:error-streak:E0308", "applied");
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
        let prior =
            count_instance_snoozes_for_concept(&conn, session_id, "iterator-chains").unwrap();
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
            .query_row("SELECT rung_shown FROM cards WHERE id = ?1", [id], |r| r.get(0))
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
        let unresolved_id = insert_card(&conn, &make_card("sess1", "borrow-vs-clone", "fp1", "shown")).unwrap();
        let resolved_id = insert_card(&conn, &make_card("sess1", "string-vs-str", "fp2", "applied")).unwrap();

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
        insert_card(&conn, &make_card("sess1", "borrow-vs-clone", "fp1", "shown")).unwrap();
        assert!(unresolved_thread_concepts(&conn, "sess1").unwrap().is_empty());
    }

    // --- T4 reqs 9-11: murshid-comments ---

    #[test]
    fn test_clear_suppressions_for_concept_clears_session_and_offer_scoped() {
        let conn = initialize_db(":memory:").unwrap();
        insert_suppression(&conn, "sess1", "borrow-vs-clone", "borrow-vs-clone", "concept").unwrap();
        insert_offer_suppression(&conn, "sess-old", "borrow-vs-clone", 9_999_999_999).unwrap();
        insert_suppression(&conn, "sess1", "string-vs-str", "string-vs-str", "concept").unwrap();

        let cleared = clear_suppressions_for_concept(&conn, "sess1", "borrow-vs-clone").unwrap();
        assert_eq!(cleared, 2, "both the session-scoped and offer-concept rows clear");

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
        let site = crate::site::compute_site("src/lib.rs", src, 2).unwrap();
        let comment_fp = crate::comment::comment_advice_fingerprint(
            "why does this need a clone?",
            &site,
        );

        // Not yet answered: no ledger entry.
        assert!(find_ledger_card(&conn, &comment_fp).unwrap().is_none());

        let mut answered = make_card("sess1", "borrow-vs-clone", &comment_fp, "got_it");
        answered.category = COMMENT_ASK_CATEGORY.to_string();
        insert_card(&conn, &answered).unwrap();

        // The exact same comment (same text, same site) resolves to the
        // same fingerprint and is now permanently ledger-blocked.
        let comment_fp_again = crate::comment::comment_advice_fingerprint(
            "why does this need a clone?",
            &site,
        );
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
        insert_card(&conn, &make_card("sess1", "iterator-chains", "fp3", "shown")).unwrap();

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
        assert!(concepts_taught_this_session(&conn, "sess1").unwrap().is_empty());
    }
}

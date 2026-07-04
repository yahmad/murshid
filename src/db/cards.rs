//! The `cards` table: the `CardRecord` row, lifecycle status/rung
//! transitions, the I3 never-re-raise ledger, and the dedup / cooldown /
//! open-card gate queries.

use super::*;

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
    let placeholders = SEEN_STATUSES
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
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

// --- T4 req 3-4 / C4: rung-shown updates (escalation + R3 reveal logging) ---

/// req 3/4: updates a card's `rung_shown` (C4). Called on every escalation
/// step AND on the R3 reveal itself, so click-through gaming is visible in
/// the data (I19) — the caller logs the paired `card_response` event with
/// verb `escalated` and the from/to rungs.
pub fn update_card_rung(
    conn: &Connection,
    card_id: i64,
    rung: &str,
) -> Result<(), rusqlite::Error> {
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

/// req 3's guard: statuses meaning a card is currently OPEN (awaiting a
/// response) — a stage-1 application detection at a site with an open card
/// is never accepted as `pass` evidence (avoids double-counting with req 4's
/// `hard`/`applied` path once the open card itself resolves).
const OPEN_CARD_STATUSES: [&str; 2] = ["shown", "queued"];

/// req 3: whether an OPEN (not yet resolved) card exists for this exact
/// advice-fingerprint, this session.
pub fn has_open_card_at_advice_fp(
    conn: &Connection,
    session_id: &str,
    advice_fp: &str,
) -> Result<bool, rusqlite::Error> {
    let placeholders = OPEN_CARD_STATUSES
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT COUNT(*) FROM cards WHERE session_id = ? AND advice_fp = ? AND status IN ({})",
        placeholders
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&session_id, &advice_fp];
    for s in OPEN_CARD_STATUSES.iter() {
        params.push(s);
    }
    let count: i64 = stmt.query_row(params.as_slice(), |row| row.get(0))?;
    Ok(count > 0)
}

/// req 3's "on a concept previously taught (any prior card exists for it)":
/// whether `concept_id` has ever actually reached the screen (any session),
/// using the same [`SEEN_STATUSES`] definition of "taught" as the T2
/// cooldown gate.
pub fn concept_has_any_prior_card(
    conn: &Connection,
    concept_id: &str,
) -> Result<bool, rusqlite::Error> {
    let placeholders = SEEN_STATUSES
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT COUNT(*) FROM cards WHERE concept_id = ? AND status IN ({})",
        placeholders
    );
    let mut stmt = conn.prepare(&sql)?;
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&concept_id];
    for s in SEEN_STATUSES.iter() {
        params.push(s);
    }
    let count: i64 = stmt.query_row(params.as_slice(), |row| row.get(0))?;
    Ok(count > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::*;

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
}

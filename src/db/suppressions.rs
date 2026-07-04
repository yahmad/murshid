//! The `suppressions` table (C5): the tiered snooze scopes
//! (`instance` / `concept`), the cross-session `offer-concept` decline
//! persistence, the live cap, and session-end purge.

use super::*;
use crate::suppression::SnoozeScope;

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
    scope: SnoozeScope,
) -> Result<i64, rusqlite::Error> {
    execute_with_retry(|| {
        conn.execute(
            "INSERT INTO suppressions (session_id, concept_id, advice_fp, scope) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![session_id, concept_id, advice_fp, scope.as_str()],
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_suppression_instance_and_concept_scope_matching() {
        let conn = initialize_db(":memory:").unwrap();
        insert_suppression(
            &conn,
            "sess1",
            "borrow-vs-clone",
            "fp-1",
            SnoozeScope::Instance,
        )
        .unwrap();

        assert!(is_suppressed(&conn, "sess1", "borrow-vs-clone", "fp-1").unwrap());
        // A different site of the same concept is NOT instance-suppressed.
        assert!(!is_suppressed(&conn, "sess1", "borrow-vs-clone", "fp-2").unwrap());

        insert_suppression(
            &conn,
            "sess1",
            "borrow-vs-clone",
            "borrow-vs-clone",
            SnoozeScope::Concept,
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
                SnoozeScope::Instance,
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
                SnoozeScope::Instance,
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
        insert_suppression(
            &conn,
            "sess1",
            "borrow-vs-clone",
            "fp-1",
            SnoozeScope::Instance,
        )
        .unwrap();
        insert_suppression(
            &conn,
            "sess2",
            "borrow-vs-clone",
            "fp-2",
            SnoozeScope::Instance,
        )
        .unwrap();

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
        insert_suppression(
            &conn,
            "sess1",
            "borrow-vs-clone",
            "fp-1",
            SnoozeScope::Instance,
        )
        .unwrap();
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
            SnoozeScope::Concept,
        )
        .unwrap();
        assert_eq!(
            count_instance_snoozes_for_concept(&conn, "sess1", "borrow-vs-clone").unwrap(),
            1
        );
    }

    #[test]
    fn test_clear_suppressions_for_concept_clears_session_and_offer_scoped() {
        let conn = initialize_db(":memory:").unwrap();
        insert_suppression(
            &conn,
            "sess1",
            "borrow-vs-clone",
            "borrow-vs-clone",
            SnoozeScope::Concept,
        )
        .unwrap();
        insert_offer_suppression(&conn, "sess-old", "borrow-vs-clone", 9_999_999_999).unwrap();
        insert_suppression(
            &conn,
            "sess1",
            "string-vs-str",
            "string-vs-str",
            SnoozeScope::Concept,
        )
        .unwrap();

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
}

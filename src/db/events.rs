//! The append-only `events` spine and the read-side queries derived
//! from it: event logging, session-end bookend counts, struggle-signal
//! baselines, throttle/decline history, and cross-session encounter lookups.

use super::*;

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
    execute_with_retry(|| log_event_stmt(conn, event))
}

/// Plain (non-retrying) core of [`log_event`] — for use inside a
/// [`super::with_tx`] closure, which owns its own retry across the whole
/// transaction.
pub(crate) fn log_event_stmt(
    conn: &Connection,
    event: &EventRecord,
) -> Result<i64, rusqlite::Error> {
    conn.execute(
        "INSERT INTO events (session_id, kind, payload_json) VALUES (?1, ?2, ?3)",
        rusqlite::params![event.session_id, event.kind, event.payload_json],
    )?;
    Ok(conn.last_insert_rowid())
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

/// T2 req 10 / C3: the last `limit` counted (i.e. actually shown, not merely
/// queued) card statuses for `category`, most-recent-first, across all
/// sessions — the input to the action-rate/throttle computation. Uses the
/// same [`SEEN_STATUSES`] constant as [`concept_shown_this_session`].
pub fn recent_card_statuses_for_category(
    conn: &Connection,
    category: &str,
    limit: u32,
) -> Result<Vec<CardStatus>, rusqlite::Error> {
    let placeholders = SEEN_STATUSES
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT status FROM cards WHERE category = ? AND status IN ({}) ORDER BY id DESC LIMIT ?",
        placeholders
    );
    let mut stmt = conn.prepare(&sql)?;
    let status_strs: Vec<&str> = SEEN_STATUSES.iter().map(|s| s.as_str()).collect();
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&category];
    for s in status_strs.iter() {
        params.push(s);
    }
    params.push(&limit);
    let rows = stmt.query_map(params.as_slice(), |row| row.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows {
        // Every value in play here was written via `update_card_status`'s
        // `CardStatus::as_str()`, and the SQL above already restricts to
        // `SEEN_STATUSES`, so this always parses — but degrade (skip, no
        // panic) rather than trust that invariant blindly.
        if let Some(status) = CardStatus::parse(&row?) {
            out.push(status);
        }
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
    let placeholders = SEEN_STATUSES
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT COUNT(*) FROM cards WHERE session_id = ? AND {} AND status IN ({})",
        efp_exempt_category_clause(),
        placeholders
    );
    let mut stmt = conn.prepare(&sql)?;
    let status_strs: Vec<&str> = SEEN_STATUSES.iter().map(|s| s.as_str()).collect();
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&session_id];
    for s in status_strs.iter() {
        params.push(s);
    }
    let count: i64 = stmt.query_row(params.as_slice(), |row| row.get(0))?;
    Ok(count as usize)
}

/// req 6: cards actually applied this session.
pub fn bookend_applied_count(
    conn: &Connection,
    session_id: &str,
) -> Result<usize, rusqlite::Error> {
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
    let placeholders = SEEN_STATUSES
        .iter()
        .map(|_| "?")
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT concept_id FROM cards WHERE session_id = ? AND {} AND status IN ({}) GROUP BY concept_id ORDER BY MIN(id) ASC",
        efp_exempt_category_clause(),
        placeholders
    );
    let mut stmt = conn.prepare(&sql)?;
    let status_strs: Vec<&str> = SEEN_STATUSES.iter().map(|s| s.as_str()).collect();
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&session_id];
    for s in status_strs.iter() {
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
    let mut stmt = conn
        .prepare("SELECT payload_json FROM events WHERE kind = 'check_result' ORDER BY id ASC")?;
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

/// req 7/C12: how many D22 retrieval questions (`encounter` events with
/// `source: "retrieval"`) have already been asked this session — the ≤2/
/// session cap's input.
pub fn retrieval_questions_asked_this_session(
    conn: &Connection,
    session_id: &str,
) -> Result<u32, rusqlite::Error> {
    let mut stmt = conn
        .prepare("SELECT payload_json FROM events WHERE session_id = ?1 AND kind = 'encounter'")?;
    let rows = stmt.query_map(rusqlite::params![session_id], |row| row.get::<_, String>(0))?;
    let mut count = 0u32;
    for row in rows {
        let payload = row?;
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&payload) {
            if v.get("source").and_then(|s| s.as_str())
                == Some(crate::memory::EvidenceSource::Retrieval.as_str())
            {
                count += 1;
            }
        }
    }
    Ok(count)
}

/// T13 req 1 / T5 req 7: concept slugs with at least one NATURAL (i.e. NOT
/// `source: "retrieval"`) `encounter` event in the session immediately
/// before `current_session_id` — feeds the "skip concepts naturally
/// encountered in the last session" retrieval gate. Sessions are ordered by
/// their ULID-shaped session id (a 48-bit millisecond timestamp prefix
/// encoded in a lexically-monotonic Crockford base32 alphabet, so plain
/// string ordering matches chronological order — see
/// `session::generate_session_id`). Returns an empty set when there is no
/// prior session (e.g. this is the very first session ever).
pub fn concepts_encountered_last_session(
    conn: &Connection,
    current_session_id: &str,
) -> Result<std::collections::HashSet<String>, rusqlite::Error> {
    let last_session_id: Option<String> = conn
        .query_row(
            "SELECT session_id FROM events WHERE session_id < ?1 ORDER BY session_id DESC LIMIT 1",
            rusqlite::params![current_session_id],
            |row| row.get(0),
        )
        .optional()?;

    let Some(last_session_id) = last_session_id else {
        return Ok(std::collections::HashSet::new());
    };

    let mut stmt = conn
        .prepare("SELECT payload_json FROM events WHERE session_id = ?1 AND kind = 'encounter'")?;
    let rows = stmt.query_map(rusqlite::params![last_session_id], |row| {
        row.get::<_, String>(0)
    })?;
    let mut concepts = std::collections::HashSet::new();
    for row in rows {
        let payload = row?;
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&payload) {
            // A retrieval-question encounter (pass/hard/fail/skip) is the
            // gate's OWN mechanism, not a "natural" one — never counts here.
            if v.get("source").and_then(|s| s.as_str())
                == Some(crate::memory::EvidenceSource::Retrieval.as_str())
            {
                continue;
            }
            if let Some(concept) = v.get("concept").and_then(|c| c.as_str()) {
                concepts.insert(concept.to_string());
            }
        }
    }
    Ok(concepts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::*;

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
        assert_eq!(statuses, vec![CardStatus::Applied]);
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
        update_card_status(&conn, queued_id, CardStatus::Shown).unwrap();
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
        update_card_status(&conn, queued_id, CardStatus::NotNow).unwrap();
        insert_suppression(
            &conn,
            session_id,
            "iterator-chains",
            "fp-queue-1",
            crate::suppression::SnoozeScope::Instance,
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
        update_card_status(&conn, other_id, CardStatus::NotNow).unwrap();
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
            crate::suppression::SnoozeScope::Concept,
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
}

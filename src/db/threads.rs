//! Card-anchored follow-up threads (C5 `threads`, D20), plus the
//! `comment-ask` / `review` card categories and the bookend's
//! unresolved-thread / unresolved-comment queries.

use super::*;

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
/// content, ts)`). T13 req 6 correction: this function does NOT itself log
/// the paired `thread_msg` event (C5 kind) — the call site does, immediately
/// after, once per insert (see `watch/keys.rs`'s thread-turn handler, and
/// `tests::test_thread_turn_pairs_insert_with_a_thread_msg_event` below for
/// the pinned contract). The two inserts stay separate on purpose: the
/// dispatch that produces the content can fail independently of the event
/// log, and mutation order (turns land before the event is logged) matters
/// to callers.
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

/// Redesign R5 (ask mode): appends one ask-mode exchange — a user question
/// plus murshid's prose answer — as a single paired turn, at the next
/// turn_no (`thread_user_turn_count`+1, so both rows share it, mirroring the
/// existing user/assistant-share-a-turn_no shape this table already uses).
/// Reuses [`insert_thread_message`]'s insert-then-log-event pairing
/// convention (see its doc) for BOTH turns: user insert, its `thread_msg`
/// event, assistant insert, its `thread_msg` event — never wrapped in one
/// transaction (matching the existing convention that these pairs stay
/// separate), but always user-then-assistant so a partial failure never
/// leaves an orphaned assistant turn with no question above it.
pub fn append_ask_turn(
    conn: &Connection,
    session_id: &str,
    card_id: i64,
    question: &str,
    answer: &str,
) -> Result<(), rusqlite::Error> {
    let turn_no = thread_user_turn_count(conn, card_id)? as i64 + 1;

    insert_thread_message(
        conn,
        &ThreadMessage {
            id: None,
            card_id,
            turn_no,
            role: "user".to_string(),
            content: question.to_string(),
            ts: None,
        },
    )?;
    log_event(
        conn,
        &EventRecord {
            id: None,
            session_id: session_id.to_string(),
            kind: "thread_msg".to_string(),
            payload_json: serde_json::json!({
                "card_id": card_id,
                "role": "user",
                "turn_no": turn_no,
            })
            .to_string(),
            ts: None,
        },
    )?;

    insert_thread_message(
        conn,
        &ThreadMessage {
            id: None,
            card_id,
            turn_no,
            role: "assistant".to_string(),
            content: answer.to_string(),
            ts: None,
        },
    )?;
    log_event(
        conn,
        &EventRecord {
            id: None,
            session_id: session_id.to_string(),
            kind: "thread_msg".to_string(),
            payload_json: serde_json::json!({
                "card_id": card_id,
                "role": "assistant",
                "turn_no": turn_no,
            })
            .to_string(),
            ts: None,
        },
    )?;

    Ok(())
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
pub const TERMINAL_STATUSES: [CardStatus; 5] = [
    CardStatus::Applied,
    CardStatus::GotIt,
    CardStatus::NotNow,
    CardStatus::NotUseful,
    CardStatus::Expired,
];

fn non_terminal_clause() -> String {
    let quoted: Vec<String> = TERMINAL_STATUSES
        .iter()
        .map(|s| format!("'{}'", s.as_str()))
        .collect();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::*;

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

    // --- redesign R5 (ask mode): append_ask_turn ---

    #[test]
    fn test_append_ask_turn_writes_user_then_assistant_with_matching_turn_no() {
        let conn = initialize_db(":memory:").unwrap();
        let card_id = insert_card(
            &conn,
            &make_card("sess1", "borrow-vs-clone", "fp1", "shown"),
        )
        .unwrap();

        append_ask_turn(
            &conn,
            "sess1",
            card_id,
            "why does &mut fix this but & doesn't?",
            "because a shared reference can't mutate the value it points to.",
        )
        .unwrap();

        let msgs = get_thread_messages(&conn, card_id).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "user");
        assert_eq!(
            msgs[0].content,
            "why does &mut fix this but & doesn't?"
        );
        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(
            msgs[1].content,
            "because a shared reference can't mutate the value it points to."
        );
        assert_eq!(msgs[0].turn_no, 1);
        assert_eq!(msgs[1].turn_no, 1, "user and assistant share one turn_no");

        let events = get_events_for_session(&conn, "sess1").unwrap();
        let thread_events: Vec<_> = events.iter().filter(|e| e.kind == "thread_msg").collect();
        assert_eq!(thread_events.len(), 2, "one event per inserted turn");
    }

    #[test]
    fn test_append_ask_turn_increments_turn_no_across_exchanges() {
        let conn = initialize_db(":memory:").unwrap();
        let card_id = insert_card(
            &conn,
            &make_card("sess1", "borrow-vs-clone", "fp1", "shown"),
        )
        .unwrap();

        append_ask_turn(&conn, "sess1", card_id, "q1", "a1").unwrap();
        append_ask_turn(&conn, "sess1", card_id, "q2", "a2").unwrap();

        let msgs = get_thread_messages(&conn, card_id).unwrap();
        assert_eq!(msgs.len(), 4);
        assert_eq!(thread_user_turn_count(&conn, card_id).unwrap(), 2);
        assert_eq!(msgs[2].turn_no, 2);
        assert_eq!(msgs[2].role, "user");
        assert_eq!(msgs[2].content, "q2");
        assert_eq!(msgs[3].role, "assistant");
        assert_eq!(msgs[3].content, "a2");
    }

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
}

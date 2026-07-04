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
pub const TERMINAL_STATUSES: [&str; 5] = ["applied", "got_it", "not_now", "not_useful", "expired"];

fn non_terminal_clause() -> String {
    let quoted: Vec<String> = TERMINAL_STATUSES
        .iter()
        .map(|s| format!("'{}'", s))
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

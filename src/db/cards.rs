//! The `cards` table: the `CardRecord` row, lifecycle status/rung
//! transitions, the I3 never-re-raise ledger, and the dedup / cooldown /
//! open-card gate queries.

use super::*;

/// C3's closed set of `cards.status` values — the response verbs
/// ([`crate::response::ResponseVerb`]) plus the additional lifecycle
/// statuses (`shown`/`queued`/`resolved`/`collapsed`) that never reach a
/// keystroke. Same idiom as `ladder::Rung`/`bkt::Grade`: `as_str` for the
/// exact on-disk TEXT, `parse` for the reverse — unknown input is `None`,
/// never a panic. The DB read/write path converts `CardStatus <-> &str`
/// only at the rusqlite boundary; the on-disk TEXT is unchanged by this
/// type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardStatus {
    Shown,
    Queued,
    Applied,
    Escalated,
    GotIt,
    NotNow,
    NotUseful,
    Expired,
    Resolved,
    Collapsed,
}

impl CardStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            CardStatus::Shown => "shown",
            CardStatus::Queued => "queued",
            CardStatus::Applied => "applied",
            CardStatus::Escalated => "escalated",
            CardStatus::GotIt => "got_it",
            CardStatus::NotNow => "not_now",
            CardStatus::NotUseful => "not_useful",
            CardStatus::Expired => "expired",
            CardStatus::Resolved => "resolved",
            CardStatus::Collapsed => "collapsed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "shown" => Some(CardStatus::Shown),
            "queued" => Some(CardStatus::Queued),
            "applied" => Some(CardStatus::Applied),
            "escalated" => Some(CardStatus::Escalated),
            "got_it" => Some(CardStatus::GotIt),
            "not_now" => Some(CardStatus::NotNow),
            "not_useful" => Some(CardStatus::NotUseful),
            "expired" => Some(CardStatus::Expired),
            "resolved" => Some(CardStatus::Resolved),
            "collapsed" => Some(CardStatus::Collapsed),
            _ => None,
        }
    }
}

/// Every C3 response verb is also a valid card status — the reverse isn't
/// true (`shown`/`queued`/`resolved`/`collapsed` aren't response verbs), so
/// this is a one-way `From`, not a shared enum.
impl From<crate::response::ResponseVerb> for CardStatus {
    fn from(verb: crate::response::ResponseVerb) -> Self {
        use crate::response::ResponseVerb;
        match verb {
            ResponseVerb::Applied => CardStatus::Applied,
            ResponseVerb::Escalated => CardStatus::Escalated,
            ResponseVerb::GotIt => CardStatus::GotIt,
            ResponseVerb::NotNow => CardStatus::NotNow,
            ResponseVerb::NotUseful => CardStatus::NotUseful,
            ResponseVerb::Expired => CardStatus::Expired,
        }
    }
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
    /// T15 HISTORY view (founder decision 2026-07-06): the card's own prose
    /// (concept name, grounding quote, why, rule, doc ref, category),
    /// serialized via [`PersistedCardBody`]/[`card_body_json`] — so a later
    /// HISTORY re-read shows the FULL original card, not just these
    /// metadata columns. `None` for cards with no display body at all
    /// (struggle-offer cards, which are a bare prompt with no `Card`) and
    /// for every pre-migration row.
    pub card_body_json: Option<String>,
}

/// T15 HISTORY view: the card's display prose, persisted at insert time
/// alongside the metadata columns already on `cards`. Deliberately
/// duplicates `category` (already its own `cards` column) so a single JSON
/// blob is a complete, self-contained render input for
/// `history_detail_lines` without a second lookup.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct PersistedCardBody {
    pub concept_name: String,
    pub grounding_quote: String,
    pub why: String,
    pub rule: String,
    pub doc_ref: String,
    pub category: String,
}

impl PersistedCardBody {
    pub fn from_card(card: &crate::card::Card, category: &str) -> Self {
        PersistedCardBody {
            concept_name: card.concept_name.clone(),
            grounding_quote: card.grounding_quote.clone(),
            why: card.why.clone(),
            rule: card.rule.clone(),
            doc_ref: card.doc_ref.clone(),
            category: category.to_string(),
        }
    }
}

/// Serializes a card's display body for the `cards.card_body_json` column.
/// `None` only if serialization itself fails (never expected in practice for
/// this plain-string struct) — propagated rather than silently dropping the
/// body or panicking.
pub fn card_body_json(card: &crate::card::Card, category: &str) -> Option<String> {
    serde_json::to_string(&PersistedCardBody::from_card(card, category)).ok()
}

/// C5 `cards` — one row per shown OR queued card (T2 req 3: a queued card is
/// persisted with `status='queued'` so cross-save/cross-session dedup sees
/// it without needing a second store); `status` tracks the response verb
/// (C3 enum) plus T2's `queued`.
pub fn insert_card(conn: &Connection, card: &CardRecord) -> Result<i64, rusqlite::Error> {
    execute_with_retry(|| insert_card_stmt(conn, card))
}

/// Plain (non-retrying) core of [`insert_card`] — for use inside a
/// [`with_tx`] closure, which owns its own retry across the whole
/// transaction.
pub(crate) fn insert_card_stmt(
    conn: &Connection,
    card: &CardRecord,
) -> Result<i64, rusqlite::Error> {
    conn.execute(
        "INSERT INTO cards (session_id, concept_id, category, rung_shown, advice_fp, finding_fp, status, worked_diff, regresses_card_id, site_file, site_line, card_body_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
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
            card.card_body_json,
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Updates a card's lifecycle status (C3 response verb / `expired`); any
/// non-`shown` status stamps `resolved_ts`.
pub fn update_card_status(
    conn: &Connection,
    card_id: i64,
    status: CardStatus,
) -> Result<(), rusqlite::Error> {
    execute_with_retry(|| update_card_status_stmt(conn, card_id, status))
}

/// Plain (non-retrying) core of [`update_card_status`] — for use inside a
/// [`with_tx`] closure, which owns its own retry across the whole
/// transaction.
pub(crate) fn update_card_status_stmt(
    conn: &Connection,
    card_id: i64,
    status: CardStatus,
) -> Result<(), rusqlite::Error> {
    conn.execute(
        "UPDATE cards SET status = ?1,
            resolved_ts = CASE WHEN ?1 != 'shown' THEN CURRENT_TIMESTAMP ELSE resolved_ts END
         WHERE id = ?2",
        rusqlite::params![status.as_str(), card_id],
    )?;
    Ok(())
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

/// T17 R2 (crux gaps #1/#6): the cross-session "shown, then silently
/// ignored" tally for one exact advice-fp — every `expired` card row ever
/// recorded for it (any session). `expired` is deliberately NOT one of
/// [`LEDGER_STATUSES`] (a single ignored show is not a rejection), but K
/// silent ignores in a row IS a signal worth suppressing on — this is the
/// query [`crate::suppression::hint_is_suppressed`] thresholds against.
pub fn count_expired_for_advice_fp(
    conn: &Connection,
    advice_fp: &str,
) -> Result<u32, rusqlite::Error> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM cards WHERE advice_fp = ?1 AND status = 'expired'",
        rusqlite::params![advice_fp],
        |row| row.get(0),
    )?;
    Ok(count as u32)
}

/// C3/T2 req 5: statuses that make a card row a permanent member of the
/// I3 never-re-raise ledger (`queued`/`shown`/`expired` are not terminal in
/// this sense).
const LEDGER_STATUSES: [CardStatus; 4] = [
    CardStatus::Applied,
    CardStatus::GotIt,
    CardStatus::NotUseful,
    CardStatus::Resolved,
];

/// T2 req 9: the subset of ledger statuses a regression is allowed to
/// re-open (misuse re-opens *taught* advice, not a dismissed-as-unhelpful
/// one).
const REGRESSION_ELIGIBLE_STATUSES: [CardStatus; 2] = [CardStatus::Applied, CardStatus::Resolved];

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
    let status_strs: Vec<&str> = LEDGER_STATUSES.iter().map(|s| s.as_str()).collect();
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&advice_fp];
    for s in status_strs.iter() {
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
/// (`applied`/`resolved`, not `got_it`/`not_useful`). `status` is the raw
/// on-disk TEXT (from [`find_ledger_card`]); an unparseable value degrades
/// to `false`, same as the old plain-string `contains` check would for any
/// string outside the eligible set — never a panic.
pub fn is_regression_eligible(status: &str) -> bool {
    match CardStatus::parse(status) {
        Some(cs) => REGRESSION_ELIGIBLE_STATUSES.contains(&cs),
        None => false,
    }
}

/// T3 consolidation (from the T2 re-review): the shared status-set constant
/// for "statuses meaning the user actually saw the card" — everything
/// except `queued` (never reached the screen) and `collapsed` (folded into
/// a sibling card's aggregation before it ever reached the screen). Used by
/// both [`concept_shown_this_session`] (cooldown gate) and
/// [`recent_card_statuses_for_category`] (EFP/throttle window), which used
/// to disagree on `collapsed`; T3's bookend "shown" count uses it too.
pub const SEEN_STATUSES: [CardStatus; 7] = [
    CardStatus::Shown,
    CardStatus::Applied,
    CardStatus::Escalated,
    CardStatus::GotIt,
    CardStatus::NotNow,
    CardStatus::NotUseful,
    CardStatus::Expired,
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
    let status_strs: Vec<&str> = SEEN_STATUSES.iter().map(|s| s.as_str()).collect();
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&session_id, &concept_id];
    for s in status_strs.iter() {
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
const OPEN_CARD_STATUSES: [CardStatus; 2] = [CardStatus::Shown, CardStatus::Queued];

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
    let status_strs: Vec<&str> = OPEN_CARD_STATUSES.iter().map(|s| s.as_str()).collect();
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&session_id, &advice_fp];
    for s in status_strs.iter() {
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
    let status_strs: Vec<&str> = SEEN_STATUSES.iter().map(|s| s.as_str()).collect();
    let mut params: Vec<&dyn rusqlite::ToSql> = vec![&concept_id];
    for s in status_strs.iter() {
        params.push(s);
    }
    let count: i64 = stmt.query_row(params.as_slice(), |row| row.get(0))?;
    Ok(count > 0)
}

// --- T15 HISTORY view (founder decision 2026-07-06): the `h` overlay's
// windowed, cross-session list + per-card detail. ---

/// One row in the HISTORY overlay's list — cross-session, newest first
/// (`id DESC`, matching the codebase's existing "recent" ordering
/// convention). Excludes `queued`/`collapsed`: a queued card never reached
/// the screen, and a collapsed one folded into a sibling card's aggregation
/// before it did either — neither is something the user ever actually saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardListRow {
    pub id: i64,
    pub concept_id: String,
    pub category: String,
    pub status: String,
    pub rung_shown: String,
    pub created_ts: Option<String>,
    pub resolved_ts: Option<String>,
    pub thread_turns: i64,
    pub has_worked_diff: bool,
}

/// The HISTORY list's rows, newest first, bounded to `limit` — cross-session
/// (no `session_id` filter) per the founder's "recent cards, bounded ~50
/// newest" decision; the caller (the TUI) supplies the exact bound.
pub fn recent_cards(conn: &Connection, limit: i64) -> Result<Vec<CardListRow>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT id, concept_id, category, status, rung_shown, created_ts, resolved_ts,
                (SELECT COUNT(*) FROM threads WHERE card_id = cards.id) AS thread_turns,
                worked_diff IS NOT NULL AS has_worked_diff
         FROM cards
         WHERE status NOT IN ('queued', 'collapsed')
         ORDER BY id DESC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map(rusqlite::params![limit], |row| {
        Ok(CardListRow {
            id: row.get(0)?,
            concept_id: row.get(1)?,
            category: row.get(2)?,
            status: row.get(3)?,
            rung_shown: row.get(4)?,
            created_ts: row.get(5)?,
            resolved_ts: row.get(6)?,
            thread_turns: row.get(7)?,
            has_worked_diff: row.get(8)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// The HISTORY detail view's full render input: the row's metadata +
/// `worked_diff` + the deserialized [`PersistedCardBody`] (`None` for a
/// pre-migration row or a card with no display body, e.g. a struggle offer —
/// the detail view degrades to metadata + worked_diff + thread only, per the
/// founder's "tolerate NULL" requirement).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardDetail {
    pub id: i64,
    pub concept_id: String,
    pub category: String,
    pub status: String,
    pub rung_shown: String,
    pub created_ts: Option<String>,
    pub resolved_ts: Option<String>,
    pub worked_diff: Option<String>,
    pub body: Option<PersistedCardBody>,
}

/// One card's full HISTORY detail, by id. `Ok(None)` when no card with that
/// id exists; a malformed/unparseable `card_body_json` degrades `body` to
/// `None` rather than an error (same "never trust stored TEXT blindly"
/// posture the rest of this view layer already follows).
pub fn card_detail(conn: &Connection, card_id: i64) -> Result<Option<CardDetail>, rusqlite::Error> {
    conn.query_row(
        "SELECT id, concept_id, category, status, rung_shown, created_ts, resolved_ts, worked_diff, card_body_json
         FROM cards WHERE id = ?1",
        rusqlite::params![card_id],
        |row| {
            let card_body_json: Option<String> = row.get(8)?;
            let body = card_body_json.and_then(|s| serde_json::from_str(&s).ok());
            Ok(CardDetail {
                id: row.get(0)?,
                concept_id: row.get(1)?,
                category: row.get(2)?,
                status: row.get(3)?,
                rung_shown: row.get(4)?,
                created_ts: row.get(5)?,
                resolved_ts: row.get(6)?,
                worked_diff: row.get(7)?,
                body,
            })
        },
    )
    .optional()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::*;

    /// ROADMAP item 8: exhaustive parse(as_str(x)) == x over every variant, so
    /// the stored `cards.status` TEXT always decodes back to the same enum.
    #[test]
    fn test_card_status_round_trips_through_str() {
        for status in [
            CardStatus::Shown,
            CardStatus::Queued,
            CardStatus::Applied,
            CardStatus::Escalated,
            CardStatus::GotIt,
            CardStatus::NotNow,
            CardStatus::NotUseful,
            CardStatus::Expired,
            CardStatus::Resolved,
            CardStatus::Collapsed,
        ] {
            assert_eq!(CardStatus::parse(status.as_str()), Some(status));
        }
        assert_eq!(CardStatus::parse("nonsense"), None);
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
            card_body_json: None,
        };
        let id = insert_card(&conn, &card).unwrap();
        assert!(id > 0);

        assert!(card_exists_with_advice_fp(&conn, "sess1", "abc123").unwrap());
        assert!(!card_exists_with_advice_fp(&conn, "sess1", "other").unwrap());
        assert!(!card_exists_with_advice_fp(&conn, "sess2", "abc123").unwrap());

        update_card_status(&conn, id, CardStatus::GotIt).unwrap();
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
            card_body_json: None,
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

    // --- with_tx atomicity (the cards<->events desync bug fix) ---

    #[test]
    fn test_with_tx_commits_both_writes_on_success() {
        let conn = initialize_db(":memory:").unwrap();
        let id = insert_card(
            &conn,
            &make_card("sess1", "borrow-vs-clone", "fp-1", "shown"),
        )
        .unwrap();

        let result = with_tx(&conn, |tx| {
            update_card_status_stmt(tx, id, CardStatus::GotIt)?;
            log_event_stmt(
                tx,
                &EventRecord {
                    id: None,
                    session_id: "sess1".to_string(),
                    kind: "card_response".to_string(),
                    payload_json: "{}".to_string(),
                    ts: None,
                },
            )?;
            Ok(())
        });
        assert!(result.is_ok());

        let status: String = conn
            .query_row(
                "SELECT status FROM cards WHERE id = ?1",
                rusqlite::params![id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "got_it", "the status update must commit");

        let events = get_events_for_session(&conn, "sess1").unwrap();
        assert_eq!(events.len(), 1, "the event must commit alongside it");
        assert_eq!(events[0].kind, "card_response");
    }

    #[test]
    fn test_with_tx_rolls_back_first_write_when_second_fails() {
        let conn = initialize_db(":memory:").unwrap();
        let id = insert_card(
            &conn,
            &make_card("sess1", "borrow-vs-clone", "fp-1", "shown"),
        )
        .unwrap();

        // The second statement is deliberately bad (a nonexistent table) so
        // it fails after the first (a real, otherwise-valid) status update
        // has already run inside the same, still-open transaction.
        let result: Result<(), rusqlite::Error> = with_tx(&conn, |tx| {
            update_card_status_stmt(tx, id, CardStatus::GotIt)?;
            tx.execute("INSERT INTO this_table_does_not_exist (x) VALUES (1)", [])?;
            Ok(())
        });
        assert!(result.is_err());

        let status: String = conn
            .query_row(
                "SELECT status FROM cards WHERE id = ?1",
                rusqlite::params![id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            status, "shown",
            "the first write must roll back when the second fails — cards and \
             events must never desync from a partial transaction"
        );

        let events = get_events_for_session(&conn, "sess1").unwrap();
        assert!(
            events.is_empty(),
            "no event should have been logged either, on rollback"
        );
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

    // --- T15 HISTORY view: recent_cards / card_detail ---

    fn sample_card_body() -> crate::card::Card {
        crate::card::Card {
            concept_name: "Borrow vs. clone".to_string(),
            file: "src/main.rs".to_string(),
            line: 42,
            grounding_quote: "person.name.clone()".to_string(),
            why: "why".to_string(),
            rule: "rule".to_string(),
            doc_ref: "ref".to_string(),
            worked_diff: "diff".to_string(),
            additional_anchors: Vec::new(),
            overflow_site_count: 0,
        }
    }

    #[test]
    fn test_recent_cards_newest_first_excludes_queued_and_collapsed_counts_threads() {
        let conn = initialize_db(":memory:").unwrap();

        let shown_id = insert_card(&conn, &make_card("sess1", "c1", "fp1", "shown")).unwrap();
        insert_card(&conn, &make_card("sess1", "c2", "fp2", "queued")).unwrap();
        insert_card(&conn, &make_card("sess1", "c3", "fp3", "collapsed")).unwrap();
        // Cross-session: a card from a different session must still appear.
        let cross_session_id =
            insert_card(&conn, &make_card("sess2", "c4", "fp4", "applied")).unwrap();

        insert_thread_message(
            &conn,
            &ThreadMessage {
                id: None,
                card_id: shown_id,
                turn_no: 1,
                role: "user".to_string(),
                content: "why?".to_string(),
                ts: None,
            },
        )
        .unwrap();
        insert_thread_message(
            &conn,
            &ThreadMessage {
                id: None,
                card_id: shown_id,
                turn_no: 1,
                role: "assistant".to_string(),
                content: "because...".to_string(),
                ts: None,
            },
        )
        .unwrap();

        let rows = recent_cards(&conn, 50).unwrap();
        let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
        assert_eq!(
            ids,
            vec![cross_session_id, shown_id],
            "newest first, cross-session, queued/collapsed excluded"
        );

        let shown_row = rows.iter().find(|r| r.id == shown_id).unwrap();
        assert_eq!(shown_row.thread_turns, 2);
        assert!(!shown_row.has_worked_diff);
    }

    #[test]
    fn test_recent_cards_respects_limit() {
        let conn = initialize_db(":memory:").unwrap();
        for i in 0..5 {
            insert_card(
                &conn,
                &make_card("sess1", "c", &format!("fp{}", i), "shown"),
            )
            .unwrap();
        }
        let rows = recent_cards(&conn, 2).unwrap();
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn test_card_detail_round_trips_a_persisted_body() {
        let conn = initialize_db(":memory:").unwrap();
        let body_card = sample_card_body();
        let mut card = make_card("sess1", "borrow-vs-clone", "fp1", "shown");
        card.worked_diff = Some(body_card.worked_diff.clone());
        card.card_body_json = card_body_json(&body_card, "idiom");
        let id = insert_card(&conn, &card).unwrap();

        let detail = card_detail(&conn, id).unwrap().unwrap();
        assert_eq!(detail.id, id);
        assert_eq!(detail.worked_diff.as_deref(), Some("diff"));
        let body = detail.body.expect("body must round-trip");
        assert_eq!(body.concept_name, "Borrow vs. clone");
        assert_eq!(body.grounding_quote, "person.name.clone()");
        assert_eq!(body.why, "why");
        assert_eq!(body.rule, "rule");
        assert_eq!(body.doc_ref, "ref");
        assert_eq!(body.category, "idiom");
    }

    #[test]
    fn test_card_detail_handles_null_body_gracefully() {
        let conn = initialize_db(":memory:").unwrap();
        // `make_card` leaves `card_body_json: None` — simulates a
        // pre-migration row that never had a body written.
        let id = insert_card(&conn, &make_card("sess1", "c", "fp1", "shown")).unwrap();

        let detail = card_detail(&conn, id).unwrap().unwrap();
        assert!(detail.body.is_none());
    }

    #[test]
    fn test_card_detail_none_for_unknown_id() {
        let conn = initialize_db(":memory:").unwrap();
        assert!(card_detail(&conn, 999).unwrap().is_none());
    }

    // --- T17 R2: cross-session shown-and-ignored tally ---

    #[test]
    fn test_count_expired_for_advice_fp_is_cross_session_and_expired_only() {
        let conn = initialize_db(":memory:").unwrap();
        assert_eq!(count_expired_for_advice_fp(&conn, "fp-1").unwrap(), 0);

        insert_card(&conn, &make_card("sess1", "c1", "fp-1", "expired")).unwrap();
        insert_card(&conn, &make_card("sess2", "c1", "fp-1", "expired")).unwrap();
        // A shown-but-not-yet-resolved row doesn't count as ignored.
        insert_card(&conn, &make_card("sess3", "c1", "fp-1", "shown")).unwrap();
        // A positively-resolved row for the SAME fp doesn't count either.
        insert_card(&conn, &make_card("sess4", "c1", "fp-1", "got_it")).unwrap();
        // A different fp is unaffected.
        insert_card(&conn, &make_card("sess1", "c1", "fp-2", "expired")).unwrap();

        assert_eq!(
            count_expired_for_advice_fp(&conn, "fp-1").unwrap(),
            2,
            "counts only 'expired' rows, across every session, for this exact fp"
        );
    }
}

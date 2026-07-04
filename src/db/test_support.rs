//! Shared test fixtures for the `db` submodule unit tests.

use super::*;

pub(crate) fn make_card(
    session_id: &str,
    concept_id: &str,
    advice_fp: &str,
    status: &str,
) -> CardRecord {
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

pub(crate) fn log_encounter(conn: &Connection, session_id: &str, concept: &str, source: &str) {
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

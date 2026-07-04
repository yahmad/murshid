//! The C5 `concept_memory` row — the BKT learner model —
//! and its lazy read / upsert / list access, plus the epoch-seconds
//! timestamp helper the memory columns are stored with.

use super::*;

// --- T5 req 1-9 / C5 `concept_memory`: the BKT learner-model row ---

/// `concept_memory.last_encounter_ts` (and req 7's skip bookkeeping) are
/// stored as decimal Unix-epoch-seconds strings rather than SQLite's
/// `CURRENT_TIMESTAMP` text format — stdlib-only (no chrono, C10) and
/// trivially round-trips through `str::parse` for req 6's elapsed-time
/// staleness math, with no datetime parser to hand-write.
pub fn now_epoch_secs_string() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ConceptMemoryRow {
    pub concept_id: String,
    pub p_mastery: f64,
    pub help_level: i32,
    pub last_encounter_ts: Option<String>,
    pub last_outcome: Option<String>,
    pub lapse_count: i32,
    /// I24: durable "already announced" fact — checked across sessions, not
    /// a session-local flag.
    pub fade_announced_ts: Option<String>,
    /// The fade line's "applied N times straight" — consecutive `pass`
    /// grades, reset on `fail`, unaffected by `hard` (help was still shown).
    pub pass_streak: i32,
    /// req 7: how many consecutive retrieval questions for this concept
    /// have been skipped — the ×2-per-skip backoff input.
    pub retrieval_skips: i32,
}

/// req 1: lazily reads a concept's memory row, if one exists yet.
pub fn get_concept_memory(
    conn: &Connection,
    concept_id: &str,
) -> Result<Option<ConceptMemoryRow>, rusqlite::Error> {
    conn.query_row(
        "SELECT concept_id, p_mastery, help_level, last_encounter_ts, last_outcome,
                lapse_count, fade_announced_ts, pass_streak, retrieval_skips
         FROM concept_memory WHERE concept_id = ?1",
        rusqlite::params![concept_id],
        |row| {
            Ok(ConceptMemoryRow {
                concept_id: row.get(0)?,
                p_mastery: row.get(1)?,
                help_level: row.get(2)?,
                last_encounter_ts: row.get(3)?,
                last_outcome: row.get(4)?,
                lapse_count: row.get(5)?,
                fade_announced_ts: row.get(6)?,
                pass_streak: row.get(7)?,
                retrieval_skips: row.get(8)?,
            })
        },
    )
    .optional()
}

/// req 1/2: creates-or-updates a concept's full memory row (`INSERT ... ON
/// CONFLICT DO UPDATE`, so lazy first-encounter creation and every
/// subsequent evidence update share one call).
pub fn upsert_concept_memory(
    conn: &Connection,
    row: &ConceptMemoryRow,
) -> Result<(), rusqlite::Error> {
    execute_with_retry(|| {
        conn.execute(
            "INSERT INTO concept_memory
                (concept_id, p_mastery, help_level, last_encounter_ts, last_outcome,
                 lapse_count, fade_announced_ts, pass_streak, retrieval_skips)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(concept_id) DO UPDATE SET
                p_mastery = excluded.p_mastery,
                help_level = excluded.help_level,
                last_encounter_ts = excluded.last_encounter_ts,
                last_outcome = excluded.last_outcome,
                lapse_count = excluded.lapse_count,
                fade_announced_ts = excluded.fade_announced_ts,
                pass_streak = excluded.pass_streak,
                retrieval_skips = excluded.retrieval_skips",
            rusqlite::params![
                row.concept_id,
                row.p_mastery,
                row.help_level,
                row.last_encounter_ts,
                row.last_outcome,
                row.lapse_count,
                row.fade_announced_ts,
                row.pass_streak,
                row.retrieval_skips,
            ],
        )?;
        Ok(())
    })
}

/// req 9: every concept_memory row — the `murshid progress` meter's data
/// source (zero-row/never-encountered concepts are the taxonomy minus this
/// list, handled by the caller).
pub fn list_concept_memory(conn: &Connection) -> Result<Vec<ConceptMemoryRow>, rusqlite::Error> {
    let mut stmt = conn.prepare(
        "SELECT concept_id, p_mastery, help_level, last_encounter_ts, last_outcome,
                lapse_count, fade_announced_ts, pass_streak, retrieval_skips
         FROM concept_memory",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(ConceptMemoryRow {
            concept_id: row.get(0)?,
            p_mastery: row.get(1)?,
            help_level: row.get(2)?,
            last_encounter_ts: row.get(3)?,
            last_outcome: row.get(4)?,
            lapse_count: row.get(5)?,
            fade_announced_ts: row.get(6)?,
            pass_streak: row.get(7)?,
            retrieval_skips: row.get(8)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_row(concept_id: &str) -> ConceptMemoryRow {
        ConceptMemoryRow {
            concept_id: concept_id.to_string(),
            p_mastery: 0.3,
            help_level: 1,
            last_encounter_ts: Some("1000".to_string()),
            last_outcome: Some("pass".to_string()),
            lapse_count: 0,
            fade_announced_ts: None,
            pass_streak: 1,
            retrieval_skips: 0,
        }
    }

    #[test]
    fn test_upsert_and_get_concept_memory_creates_row() {
        let conn = initialize_db(":memory:").unwrap();
        let row = make_row("borrow-vs-clone");
        upsert_concept_memory(&conn, &row).unwrap();

        let fetched = get_concept_memory(&conn, "borrow-vs-clone").unwrap();
        assert_eq!(fetched, Some(row));
    }

    #[test]
    fn test_upsert_concept_memory_overwrites_on_conflict() {
        let conn = initialize_db(":memory:").unwrap();
        let original = make_row("borrow-vs-clone");
        upsert_concept_memory(&conn, &original).unwrap();

        let updated = ConceptMemoryRow {
            concept_id: "borrow-vs-clone".to_string(),
            p_mastery: 0.85,
            help_level: 3,
            last_encounter_ts: Some("2000".to_string()),
            last_outcome: Some("fail".to_string()),
            lapse_count: 2,
            fade_announced_ts: Some("2001".to_string()),
            pass_streak: 0,
            retrieval_skips: 4,
        };
        upsert_concept_memory(&conn, &updated).unwrap();

        let fetched = get_concept_memory(&conn, "borrow-vs-clone").unwrap();
        assert_eq!(fetched, Some(updated));

        // Still exactly one row for this concept_id — the second upsert
        // updated in place rather than inserting a duplicate.
        let all = list_concept_memory(&conn).unwrap();
        assert_eq!(all.len(), 1);
    }

    #[test]
    fn test_get_concept_memory_none_when_never_upserted() {
        let conn = initialize_db(":memory:").unwrap();
        assert_eq!(get_concept_memory(&conn, "never-seen").unwrap(), None);
    }

    #[test]
    fn test_list_concept_memory_round_trips_multiple_rows() {
        let conn = initialize_db(":memory:").unwrap();
        let row_a = make_row("borrow-vs-clone");
        let row_b = make_row("string-vs-str");
        let row_c = make_row("iterator-vs-loop");
        upsert_concept_memory(&conn, &row_a).unwrap();
        upsert_concept_memory(&conn, &row_b).unwrap();
        upsert_concept_memory(&conn, &row_c).unwrap();

        let mut all = list_concept_memory(&conn).unwrap();
        assert_eq!(all.len(), 3);
        all.sort_by(|a, b| a.concept_id.cmp(&b.concept_id));

        let mut expected = vec![row_a, row_b, row_c];
        expected.sort_by(|a, b| a.concept_id.cmp(&b.concept_id));
        assert_eq!(all, expected);
    }
}

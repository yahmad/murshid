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

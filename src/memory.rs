//! T5 — the memory model's orchestration layer: lazy `concept_memory` row
//! creation (req 1), the pure-BKT-then-persist evidence pipeline (req 2/3),
//! entry-rung resolution (req 4), fade/level-down detection (req 5), and the
//! req 10 no-update guard. `db.rs` owns the SQL; `bkt.rs`/`ladder.rs` own the
//! pure math; this module glues them for `main.rs`.

use crate::bkt::{self, Grade};
use crate::db::{self, ConceptMemoryRow};
use crate::ladder;

/// req 1: a brand-new concept's row — seeded from the category's C12 prior,
/// never persisted until a real encounter happens (a mere entry-rung *read*
/// must not fabricate history).
pub fn default_row(concept_id: &str, category: &str) -> ConceptMemoryRow {
    ConceptMemoryRow {
        concept_id: concept_id.to_string(),
        p_mastery: bkt::priors_for_category(category).p_l0,
        help_level: 0,
        last_encounter_ts: None,
        last_outcome: None,
        lapse_count: 0,
        fade_announced_ts: None,
        pass_streak: 0,
        retrieval_skips: 0,
    }
}

/// req 1: reads a concept's row, or the (unpersisted) default if it has
/// never been encountered.
pub fn read_or_default(
    conn: &rusqlite::Connection,
    concept_id: &str,
    category: &str,
) -> Result<ConceptMemoryRow, rusqlite::Error> {
    match db::get_concept_memory(conn, concept_id)? {
        Some(row) => Ok(row),
        None => Ok(default_row(concept_id, category)),
    }
}

/// req 4: read-only entry-rung resolution — never records evidence, never
/// creates a row. `None` is silence (I18/C4: concept mastered, no card).
pub fn entry_rung_for(
    conn: &rusqlite::Connection,
    concept_id: &str,
    category: &str,
    directness: ladder::Directness,
) -> Result<Option<ladder::Rung>, rusqlite::Error> {
    let row = read_or_default(conn, concept_id, category)?;
    let last_outcome = row.last_outcome.as_deref().and_then(Grade::parse);
    Ok(ladder::compose_entry_rung(
        row.p_mastery,
        last_outcome,
        directness,
    ))
}

/// D18: a concept counts as "below mastery" when it has no recorded
/// mastery yet, or its current `p_mastery` hasn't crossed the C4 gate.
pub fn is_below_mastery(
    conn: &rusqlite::Connection,
    concept_id: &str,
    category: &str,
) -> Result<bool, rusqlite::Error> {
    let row = read_or_default(conn, concept_id, category)?;
    Ok(!bkt::is_mastered(row.p_mastery))
}

/// req 3's dual guard for accepting a stage-1 `pass` detection: the concept
/// must currently be below mastery, AND there must be no OPEN card at the
/// exact (concept, site) advice-fingerprint this session — the guard
/// against double-counting with req 4's `hard` grade once that open card
/// itself resolves via applied-detection.
pub fn detection_accepted(
    conn: &rusqlite::Connection,
    session_id: &str,
    concept_id: &str,
    category: &str,
    advice_fp: &str,
) -> Result<bool, rusqlite::Error> {
    if !is_below_mastery(conn, concept_id, category)? {
        return Ok(false);
    }
    if db::has_open_card_at_advice_fp(conn, session_id, advice_fp)? {
        return Ok(false);
    }
    Ok(true)
}

/// T5 support for T4 req 13 / D18: every taxonomy concept currently below
/// mastery — feeds `murshid review`'s prompt (D18 parity: "prioritize
/// teaching next") in place of the static placeholder list. (T5's own
/// requirement list is 1-10; req 13 belongs to T4-card-interaction.md's
/// solicited-review requirement, which this memory-model addition wires
/// the real list into.)
pub fn below_mastery_concepts(
    conn: &rusqlite::Connection,
    taxonomy: &[crate::pack::TaxonomyConcept],
) -> Vec<String> {
    taxonomy
        .iter()
        .filter(|c| is_below_mastery(conn, &c.slug, &c.category).unwrap_or(true))
        .map(|c| c.slug.clone())
        .collect()
}

/// req 2/5: the outcome of recording one evidence encounter — what the
/// caller needs to decide what to print (I24 fade/level-down lines are
/// engine text, not this module's concern) and what entry rung to use next.
#[derive(Debug, Clone, PartialEq)]
pub struct EncounterOutcome {
    pub row: ConceptMemoryRow,
    /// I24: true only the FIRST time this concept ever crosses the mastery
    /// gate (checked against the durable `fade_announced_ts`, not a
    /// session-local flag — survives across sessions).
    pub crossed_into_mastery: bool,
    /// req 5: true when this fail-grade encounter just dropped a
    /// previously-mastered concept back below the gate.
    pub leveled_down: bool,
}

/// req 2/3 — records one evidence encounter: pure BKT update, Wood
/// help_level update, fade/level-down detection, then persists (mutation
/// order: the pure computation happens first; the DB write is the last
/// step, and the caller only announces after this function returns Ok).
/// Also logs the C5 `encounter` event (grade + source, per req 3's closing
/// line) — logged only after the row write succeeds.
pub fn record_encounter(
    conn: &rusqlite::Connection,
    session_id: &str,
    concept_id: &str,
    category: &str,
    grade: Grade,
    source: &str,
) -> Result<EncounterOutcome, rusqlite::Error> {
    let existing = read_or_default(conn, concept_id, category)?;
    let priors = bkt::priors_for_category(category);

    let was_mastered = bkt::is_mastered(existing.p_mastery);
    let new_p = bkt::bkt_update(existing.p_mastery, grade, &priors);
    let new_help_level = bkt::update_help_level(existing.help_level, grade);
    let new_pass_streak = match grade {
        Grade::Pass => existing.pass_streak + 1,
        Grade::Fail => 0,
        Grade::Hard => existing.pass_streak,
    };
    let is_mastered_now = bkt::is_mastered(new_p);

    let crossed_into_mastery = is_mastered_now && existing.fade_announced_ts.is_none();
    let leveled_down = was_mastered && !is_mastered_now;

    let now = db::now_epoch_secs_string();
    let new_row = ConceptMemoryRow {
        concept_id: concept_id.to_string(),
        p_mastery: new_p,
        help_level: new_help_level,
        last_encounter_ts: Some(now.clone()),
        last_outcome: Some(grade.as_str().to_string()),
        lapse_count: existing.lapse_count + if leveled_down { 1 } else { 0 },
        fade_announced_ts: if crossed_into_mastery {
            Some(now)
        } else {
            existing.fade_announced_ts.clone()
        },
        pass_streak: new_pass_streak,
        retrieval_skips: if source == "retrieval" {
            existing.retrieval_skips
        } else {
            0
        },
    };

    db::upsert_concept_memory(conn, &new_row)?;

    let _ = db::log_event(
        conn,
        &db::EventRecord {
            id: None,
            session_id: session_id.to_string(),
            kind: "encounter".to_string(),
            payload_json: serde_json::json!({
                "concept": concept_id,
                "grade": grade.as_str(),
                "source": source,
            })
            .to_string(),
            ts: None,
        },
    );

    if crossed_into_mastery {
        let _ = db::log_event(
            conn,
            &db::EventRecord {
                id: None,
                session_id: session_id.to_string(),
                kind: "fade".to_string(),
                payload_json: serde_json::json!({
                    "concept": concept_id,
                    "pass_streak": new_row.pass_streak,
                })
                .to_string(),
                ts: None,
            },
        );
    }

    Ok(EncounterOutcome {
        row: new_row,
        crossed_into_mastery,
        leveled_down,
    })
}

/// req 7 / I23: a skipped retrieval question is explicitly NOT an
/// observation — `p_mastery`, `help_level`, and `last_outcome` are left
/// untouched. Only the ×2-per-skip backoff counter (`retrieval_skips`)
/// updates, plus the `encounter` event (grade `skip`, source `retrieval`)
/// so req 7's ≤2/session cap can still count it.
pub fn record_retrieval_skip(
    conn: &rusqlite::Connection,
    session_id: &str,
    concept_id: &str,
    category: &str,
) -> Result<i32, rusqlite::Error> {
    let mut row = read_or_default(conn, concept_id, category)?;
    row.retrieval_skips = crate::retrieval::on_skip(row.retrieval_skips);
    db::upsert_concept_memory(conn, &row)?;

    let _ = db::log_event(
        conn,
        &db::EventRecord {
            id: None,
            session_id: session_id.to_string(),
            kind: "encounter".to_string(),
            payload_json: serde_json::json!({
                "concept": concept_id,
                "grade": "skip",
                "source": "retrieval",
            })
            .to_string(),
            ts: None,
        },
    );

    Ok(row.retrieval_skips)
}

/// req 10 / I22/I23 guard: the ONLY C3 card-response verb that is evidence
/// is `applied` (T4's applied-detection -> req 3's `hard` grade). Re-shows,
/// escalations, `got_it`/`not_now`/`not_useful`, queue browsing, and thread
/// turns are never evidence — none of those call sites even construct a
/// `Grade`, but this function is the single, testable source of truth for
/// the one call site (the response-key handler) that does.
pub fn should_record_evidence_for_response(verb: &str) -> Option<Grade> {
    match verb {
        "applied" => Some(Grade::Hard),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> rusqlite::Connection {
        db::initialize_db(":memory:").unwrap()
    }

    /// Passes a concept just up to (never far past) the mastery gate — a
    /// fixed large loop count overshoots into p so close to 1.0 that a
    /// single subsequent fail can no longer drop it back under the 0.95
    /// gate (BKT's fail-branch pulls p toward `p_S`, but starting near 1
    /// that pull isn't enough); stopping at the FIRST crossing keeps the
    /// regression tests meaningful.
    fn pass_until_mastered(conn: &rusqlite::Connection, concept: &str, category: &str) {
        for _ in 0..50 {
            let outcome =
                record_encounter(conn, "sess1", concept, category, Grade::Pass, "detection")
                    .unwrap();
            if bkt::is_mastered(outcome.row.p_mastery) {
                return;
            }
        }
        panic!("concept never crossed the mastery gate within 50 passes");
    }

    // --- req 1: lazy creation ---

    #[test]
    fn test_read_or_default_never_persists_until_a_real_encounter() {
        let c = conn();
        let row = read_or_default(&c, "borrow-vs-clone", "idiom").unwrap();
        assert_eq!(row.p_mastery, bkt::IDIOM_PRIORS.p_l0);
        assert!(
            db::get_concept_memory(&c, "borrow-vs-clone")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_record_encounter_creates_the_row_with_category_priors() {
        let c = conn();
        let outcome = record_encounter(
            &c,
            "sess1",
            "borrow-vs-clone",
            "idiom",
            Grade::Pass,
            "detection",
        )
        .unwrap();
        assert!(
            outcome.row.p_mastery > bkt::IDIOM_PRIORS.p_l0,
            "a pass must raise p above the prior"
        );
        let persisted = db::get_concept_memory(&c, "borrow-vs-clone")
            .unwrap()
            .unwrap();
        assert_eq!(persisted.p_mastery, outcome.row.p_mastery);
    }

    // --- req 2/4: hard is BKT-correct but no downward help_level shift ---

    #[test]
    fn test_hard_grade_updates_bkt_like_pass_but_leaves_help_level_unchanged() {
        let c = conn();
        // Seed help_level up first via a fail.
        record_encounter(&c, "sess1", "c1", "idiom", Grade::Fail, "detection").unwrap();
        let after_fail = db::get_concept_memory(&c, "c1").unwrap().unwrap();
        assert_eq!(after_fail.help_level, 1);

        let outcome =
            record_encounter(&c, "sess1", "c1", "idiom", Grade::Hard, "site_recheck").unwrap();
        assert_eq!(outcome.row.help_level, 1, "hard must not shift help_level");
        // But the BKT math still treats it as correct: p must have risen.
        assert!(outcome.row.p_mastery > after_fail.p_mastery);
    }

    // --- req 5: fade announced exactly once, incl. across "sessions" ---

    /// T13 req 4: the original version of this test reused a single
    /// in-memory connection throughout, so it never actually exercised a
    /// closed-then-reopened database — `fade_announced_ts`'s durability
    /// claim ("checked against the durable... not a session-local flag —
    /// survives across sessions") was untested against a genuine close/
    /// reopen. This version is file-backed: the connection used for the
    /// first mastery crossing is fully dropped (closing the file) before a
    /// brand-new connection reopens the same file and keeps going.
    #[test]
    fn test_fade_announced_exactly_once_across_reopened_connections() {
        let temp_dir = std::env::temp_dir();
        let db_path = temp_dir.join("test_murshid_t13_fade_reopen.db");
        let _ = std::fs::remove_file(&db_path);

        let mut p = bkt::IDIOM_PRIORS.p_l0;
        let mut crossed_count = 0;
        {
            let c = db::initialize_db(&db_path).unwrap();
            for _ in 0..30 {
                let outcome =
                    record_encounter(&c, "sess1", "c1", "idiom", Grade::Pass, "detection").unwrap();
                p = outcome.row.p_mastery;
                if outcome.crossed_into_mastery {
                    crossed_count += 1;
                }
            }
        } // `c` dropped here — the file-backed connection is genuinely closed.
        assert!(bkt::is_mastered(p));
        assert_eq!(crossed_count, 1, "fade must announce exactly once");

        // A brand-new connection to the SAME file — a real reopen, not
        // connection reuse — must still never re-announce.
        {
            let c2 = db::open_connection(&db_path).unwrap();
            for _ in 0..5 {
                let outcome =
                    record_encounter(&c2, "sess1", "c1", "idiom", Grade::Pass, "detection")
                        .unwrap();
                assert!(
                    !outcome.crossed_into_mastery,
                    "must never re-announce after a genuine close/reopen"
                );
            }
        }

        let _ = std::fs::remove_file(&db_path);
    }

    #[test]
    fn test_fade_never_reannounces_after_a_relapse_and_remastery() {
        let c = conn();
        pass_until_mastered(&c, "c1", "idiom");
        let mastered = db::get_concept_memory(&c, "c1").unwrap().unwrap();
        assert!(bkt::is_mastered(mastered.p_mastery));

        // Relapse.
        let regress = record_encounter(&c, "sess1", "c1", "idiom", Grade::Fail, "misuse").unwrap();
        assert!(regress.leveled_down);
        assert!(!bkt::is_mastered(regress.row.p_mastery));

        // Re-master.
        let mut crossed_again = false;
        for _ in 0..30 {
            let outcome =
                record_encounter(&c, "sess1", "c1", "idiom", Grade::Pass, "detection").unwrap();
            if outcome.crossed_into_mastery {
                crossed_again = true;
            }
        }
        assert!(
            !crossed_again,
            "the fade line is a one-time announcement, ever"
        );
    }

    // --- req 5: regression level-down ---

    #[test]
    fn test_regression_level_down_only_fires_when_previously_mastered() {
        let c = conn();
        // Not mastered yet: a fail is not a "level-down".
        let first_fail =
            record_encounter(&c, "sess1", "c1", "idiom", Grade::Fail, "misuse").unwrap();
        assert!(!first_fail.leveled_down);

        pass_until_mastered(&c, "c1", "idiom");
        let regress = record_encounter(&c, "sess1", "c1", "idiom", Grade::Fail, "misuse").unwrap();
        assert!(regress.leveled_down);
        assert_eq!(regress.row.lapse_count, 1);
    }

    // --- req 4: entry rung resolution end to end ---

    #[test]
    fn test_entry_rung_for_new_concept_is_r3_low_mastery() {
        let c = conn();
        // idiom p_L0=0.20 < 0.5 -> R3 band, no prior outcome, balanced knob.
        let rung = entry_rung_for(&c, "c1", "idiom", ladder::Directness::Balanced).unwrap();
        assert_eq!(rung, Some(ladder::Rung::R3));
    }

    #[test]
    fn test_entry_rung_silences_once_mastered() {
        let c = conn();
        for _ in 0..30 {
            record_encounter(&c, "sess1", "c1", "idiom", Grade::Pass, "detection").unwrap();
        }
        let rung = entry_rung_for(&c, "c1", "idiom", ladder::Directness::Balanced).unwrap();
        assert_eq!(rung, None, "mastered concept -> silence");
    }

    /// Acceptance: "regression level-down re-enables cards" — a mastered
    /// (silenced) concept that regresses via a fail encounter must resolve
    /// to a real, non-silence entry rung again on the very next read.
    #[test]
    fn test_regression_re_enables_cards() {
        let c = conn();
        pass_until_mastered(&c, "c1", "idiom");
        assert_eq!(
            entry_rung_for(&c, "c1", "idiom", ladder::Directness::Balanced).unwrap(),
            None,
            "mastered -> silence before the regression"
        );

        let regress = record_encounter(&c, "sess1", "c1", "idiom", Grade::Fail, "misuse").unwrap();
        assert!(regress.leveled_down);

        let rung_after = entry_rung_for(&c, "c1", "idiom", ladder::Directness::Balanced).unwrap();
        assert!(
            rung_after.is_some(),
            "a regressed concept must re-enable cards, not stay silent"
        );
    }

    // --- req 3's dual guard: below-mastery + no open card ---

    fn insert_open_card(
        conn: &rusqlite::Connection,
        session_id: &str,
        concept_id: &str,
        advice_fp: &str,
    ) {
        insert_open_card_with_status(conn, session_id, concept_id, advice_fp, "shown");
    }

    fn insert_open_card_with_status(
        conn: &rusqlite::Connection,
        session_id: &str,
        concept_id: &str,
        advice_fp: &str,
        status: &str,
    ) {
        db::insert_card(
            conn,
            &db::CardRecord {
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
            },
        )
        .unwrap();
    }

    #[test]
    fn test_detection_accepted_true_when_below_mastery_and_no_open_card() {
        let c = conn();
        assert!(detection_accepted(&c, "sess1", "c1", "idiom", "fp-1").unwrap());
    }

    /// Acceptance: "detection-guard (open card at site => no pass double-
    /// count)" — an OPEN card at the exact advice-fp blocks the detection.
    #[test]
    fn test_detection_guard_blocks_when_open_card_at_same_site() {
        let c = conn();
        insert_open_card(&c, "sess1", "c1", "fp-1");
        assert!(
            !detection_accepted(&c, "sess1", "c1", "idiom", "fp-1").unwrap(),
            "an open card at this exact site must block the pass detection"
        );
        // A DIFFERENT site (advice-fp) for the same concept is unaffected.
        assert!(detection_accepted(&c, "sess1", "c1", "idiom", "fp-2").unwrap());
    }

    /// T13 req 5: `db::OPEN_CARD_STATUSES` is `["shown", "queued"]` — the
    /// guard must block on a QUEUED card too, not just a shown one.
    #[test]
    fn test_detection_guard_blocks_when_open_card_status_is_queued() {
        let c = conn();
        insert_open_card_with_status(&c, "sess1", "c1", "fp-1", "queued");
        assert!(
            !detection_accepted(&c, "sess1", "c1", "idiom", "fp-1").unwrap(),
            "a QUEUED card at this exact site must also block the pass detection"
        );
    }

    #[test]
    fn test_detection_guard_does_not_block_on_a_resolved_card() {
        let c = conn();
        insert_open_card(&c, "sess1", "c1", "fp-1");
        db::update_card_status(&c, 1, "got_it").unwrap();
        assert!(
            detection_accepted(&c, "sess1", "c1", "idiom", "fp-1").unwrap(),
            "a RESOLVED card is not 'open' — the guard is about awaiting-response cards only"
        );
    }

    /// Acceptance: "below-mastery-only detection acceptance" — a mastered
    /// concept's detection is never accepted, even with no open card.
    #[test]
    fn test_detection_guard_blocks_when_mastered() {
        let c = conn();
        pass_until_mastered(&c, "c1", "idiom");
        assert!(
            !detection_accepted(&c, "sess1", "c1", "idiom", "fp-1").unwrap(),
            "a mastered concept's application is not below-mastery evidence"
        );
    }

    // --- T4 req 13 / D18: below-mastery list ---

    #[test]
    fn test_below_mastery_concepts_excludes_only_mastered_ones() {
        let c = conn();
        let taxonomy = vec![
            crate::pack::TaxonomyConcept {
                slug: "c1".to_string(),
                name: "C1".to_string(),
                category: "idiom".to_string(),
            },
            crate::pack::TaxonomyConcept {
                slug: "c2".to_string(),
                name: "C2".to_string(),
                category: "idiom".to_string(),
            },
        ];
        for _ in 0..30 {
            record_encounter(&c, "sess1", "c1", "idiom", Grade::Pass, "detection").unwrap();
        }
        let below = below_mastery_concepts(&c, &taxonomy);
        assert_eq!(below, vec!["c2".to_string()]);
    }

    // --- req 10: no-update guard ---

    #[test]
    fn test_should_record_evidence_only_for_applied_response() {
        assert_eq!(
            should_record_evidence_for_response("applied"),
            Some(Grade::Hard)
        );
        for verb in [
            "got_it",
            "not_now",
            "not_useful",
            "escalated",
            "queued",
            "shown",
            "expired",
        ] {
            assert_eq!(
                should_record_evidence_for_response(verb),
                None,
                "{} must never be evidence",
                verb
            );
        }
    }

    // --- req 7/10: a skipped retrieval question is NOT an observation ---

    #[test]
    fn test_record_retrieval_skip_never_touches_bkt_state() {
        let c = conn();
        // Seed a real encounter first so there's mastery state to protect.
        let seeded =
            record_encounter(&c, "sess1", "c1", "idiom", Grade::Pass, "detection").unwrap();

        let skips = record_retrieval_skip(&c, "sess1", "c1", "idiom").unwrap();
        assert_eq!(skips, 1);

        let after = db::get_concept_memory(&c, "c1").unwrap().unwrap();
        assert_eq!(
            after.p_mastery, seeded.row.p_mastery,
            "skip must not move p_mastery"
        );
        assert_eq!(
            after.help_level, seeded.row.help_level,
            "skip must not move help_level"
        );
        assert_eq!(
            after.last_outcome, seeded.row.last_outcome,
            "skip must not touch last_outcome"
        );
        assert_eq!(after.retrieval_skips, 1);
    }

    #[test]
    fn test_record_retrieval_skip_increments_across_repeated_skips() {
        let c = conn();
        record_encounter(&c, "sess1", "c1", "idiom", Grade::Pass, "detection").unwrap();
        record_retrieval_skip(&c, "sess1", "c1", "idiom").unwrap();
        let skips2 = record_retrieval_skip(&c, "sess1", "c1", "idiom").unwrap();
        assert_eq!(skips2, 2);
    }
}

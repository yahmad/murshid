//! T5 req 7-8 / D22, C12 — retrieval practice: scarcity-triggered recall
//! questions at session boundaries, capped at 2/session, judge-graded
//! pass/hard/fail against the canon entry, sanitized, pull-priced.

use crate::bkt::Grade;
use crate::db::ConceptMemoryRow;
use crate::pack::CanonEntry;

/// C12: at most 2 recall questions per session.
pub const MAX_PER_SESSION: u32 = 2;

/// req 7: one stale, eligible concept, paired with its canon entry.
#[derive(Debug, Clone, PartialEq)]
pub struct RecallCandidate {
    pub concept_id: String,
    pub category: String,
}

/// req 7: selects up to `cap_remaining` stale-and-eligible concepts (req 6's
/// staleness gate + its ×2-per-skip backoff), from lowest-p-mastery-worth-
/// retaining first isn't mandated — insertion order (taxonomy order via the
/// caller) is preserved, simplest defensible tie-break.
pub fn select_stale_concepts(
    rows: &[(ConceptMemoryRow, String)], // (row, category)
    now_epoch_secs: u64,
    cap_remaining: u32,
) -> Vec<RecallCandidate> {
    if cap_remaining == 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (row, category) in rows {
        if out.len() as u32 >= cap_remaining {
            break;
        }
        let Some(last) = row.last_encounter_ts.as_deref().and_then(|s| s.parse::<u64>().ok()) else {
            continue; // never encountered -> nothing to retrieve yet
        };
        let elapsed = std::time::Duration::from_secs(now_epoch_secs.saturating_sub(last));
        if crate::staleness::is_stale(category, row.p_mastery, elapsed, row.retrieval_skips as u32) {
            out.push(RecallCandidate {
                concept_id: row.concept_id.clone(),
                category: category.clone(),
            });
        }
    }
    out
}

/// req 7: the R0-form recall question, generated from the canon entry
/// ("how would you rewrite X without cloning?" style) — no LLM call needed
/// to ask it, only to grade the answer (req 8).
pub fn build_recall_question(entry: &CanonEntry) -> String {
    format!(
        "recall check \u{2014} {}: how would you rewrite this to {}?",
        entry.concept,
        entry.use_instead.trim_end_matches('.').to_lowercase()
    )
}

/// req 7: skippable by keypress or 30s timeout; skip = no observation
/// (I23), but backs off the concept's re-eligibility ×2 (staleness.rs).
pub fn on_skip(current_skips: i32) -> i32 {
    current_skips.saturating_add(1)
}

/// req 8 / C6: sanitized judge-grading prompt — one call, pull-priced.
/// Grades the typed answer against the canon entry -> pass/hard/fail + a
/// one-line feedback string.
pub fn build_grading_prompt(question: &str, entry: &CanonEntry, typed_answer: &str) -> String {
    let mut s = String::new();
    s.push_str("Grade this recall answer (D22) against the canon entry.\n\n");
    s.push_str(&format!("Question: {}\n", question));
    s.push_str(&format!("Canon \u{2014} what it does: {}\n", entry.what_it_does));
    s.push_str(&format!("Canon \u{2014} use instead: {}\n", entry.use_instead));
    s.push_str(&format!("Learner's answer: {}\n\n", typed_answer));
    s.push_str(
        "Respond as JSON: {\"grade\": \"pass\"|\"hard\"|\"fail\", \"feedback\": \"one line\"}\n",
    );
    crate::sanitizer::sanitize_diagnostics(&s)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrievalGrade {
    pub grade_str: String,
    pub feedback: String,
}

#[derive(serde::Deserialize)]
struct RawGrade {
    grade: Option<String>,
    feedback: Option<String>,
}

/// req 8: parses the judge's grading response. Any malformed/missing leg is
/// a drop (never guesses a grade), mirroring the stage-2 contract's
/// fail-closed discipline.
pub fn parse_grading_response(raw: &str) -> Result<RetrievalGrade, String> {
    let parsed: RawGrade =
        serde_json::from_str(raw).map_err(|e| format!("recall grading parse error: {}", e))?;
    let grade_str = parsed
        .grade
        .filter(|g| Grade::parse(g).is_some())
        .ok_or_else(|| "missing/invalid grade".to_string())?;
    let feedback = parsed.feedback.unwrap_or_default();
    Ok(RetrievalGrade { grade_str, feedback })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canon_entry() -> CanonEntry {
        CanonEntry {
            id: "borrow-vs-clone".to_string(),
            concept: "Borrow vs. clone".to_string(),
            what_it_does: "warns on an avoidable clone".to_string(),
            why_is_this_bad: "allocates and copies data a borrow would serve".to_string(),
            example: "fn f(s: String)".to_string(),
            use_instead: "Take &str instead.".to_string(),
            refs: vec![],
            source_rule_ids: vec![],
        }
    }

    fn row(concept: &str, p: f64, last_encounter_secs_ago: u64, now: u64, skips: i32) -> ConceptMemoryRow {
        ConceptMemoryRow {
            concept_id: concept.to_string(),
            p_mastery: p,
            help_level: 0,
            last_encounter_ts: Some((now - last_encounter_secs_ago).to_string()),
            last_outcome: Some("pass".to_string()),
            lapse_count: 0,
            fade_announced_ts: None,
            pass_streak: 3,
            retrieval_skips: skips,
        }
    }

    // --- req 7: selection + cap ---

    #[test]
    fn test_select_stale_concepts_respects_cap() {
        let now = 10_000_000u64;
        let window = crate::staleness::staleness_window("idiom").unwrap().as_secs();
        let rows = vec![
            (row("c1", 0.9, window + 10, now, 0), "idiom".to_string()),
            (row("c2", 0.9, window + 10, now, 0), "idiom".to_string()),
            (row("c3", 0.9, window + 10, now, 0), "idiom".to_string()),
        ];
        let selected = select_stale_concepts(&rows, now, 2);
        assert_eq!(selected.len(), 2, "capped at MAX_PER_SESSION-equivalent remaining");
    }

    #[test]
    fn test_select_stale_concepts_zero_cap_selects_nothing() {
        let now = 10_000_000u64;
        let window = crate::staleness::staleness_window("idiom").unwrap().as_secs();
        let rows = vec![(row("c1", 0.9, window + 10, now, 0), "idiom".to_string())];
        assert!(select_stale_concepts(&rows, now, 0).is_empty());
    }

    #[test]
    fn test_select_stale_concepts_excludes_never_encountered() {
        let now = 10_000_000u64;
        let mut r = row("c1", 0.9, 0, now, 0);
        r.last_encounter_ts = None;
        let selected = select_stale_concepts(&[(r, "idiom".to_string())], now, 2);
        assert!(selected.is_empty());
    }

    #[test]
    fn test_select_stale_concepts_excludes_fresh_concepts() {
        let now = 10_000_000u64;
        let rows = vec![(row("c1", 0.9, 60, now, 0), "idiom".to_string())]; // encountered a minute ago
        assert!(select_stale_concepts(&rows, now, 2).is_empty());
    }

    // --- req 7: question generation ---

    #[test]
    fn test_build_recall_question_contains_concept_and_use_instead() {
        let q = build_recall_question(&canon_entry());
        assert!(q.contains("Borrow vs. clone"));
        assert!(q.contains("take &str instead"));
        assert!(q.starts_with("recall check"));
    }

    // --- req 7: skip backoff ---

    #[test]
    fn test_on_skip_increments() {
        assert_eq!(on_skip(0), 1);
        assert_eq!(on_skip(2), 3);
    }

    // --- req 8: grading prompt + parsing fixtures (pass/hard/fail) ---

    #[test]
    fn test_build_grading_prompt_contains_question_canon_and_answer() {
        let prompt = build_grading_prompt("how would you rewrite this?", &canon_entry(), "use &str");
        assert!(prompt.contains("how would you rewrite this?"));
        assert!(prompt.contains("Take &str instead."));
        assert!(prompt.contains("use &str"));
    }

    #[test]
    fn test_parse_grading_response_pass_fixture() {
        let raw = r#"{"grade": "pass", "feedback": "exactly right."}"#;
        let g = parse_grading_response(raw).unwrap();
        assert_eq!(g.grade_str, "pass");
        assert_eq!(g.feedback, "exactly right.");
    }

    #[test]
    fn test_parse_grading_response_hard_fixture() {
        let raw = r#"{"grade": "hard", "feedback": "close, but you needed the hint about lifetimes."}"#;
        let g = parse_grading_response(raw).unwrap();
        assert_eq!(g.grade_str, "hard");
    }

    #[test]
    fn test_parse_grading_response_fail_fixture() {
        let raw = r#"{"grade": "fail", "feedback": "that would still clone."}"#;
        let g = parse_grading_response(raw).unwrap();
        assert_eq!(g.grade_str, "fail");
    }

    #[test]
    fn test_parse_grading_response_rejects_invalid_grade() {
        let raw = r#"{"grade": "maybe", "feedback": "?"}"#;
        assert!(parse_grading_response(raw).is_err());
    }

    #[test]
    fn test_parse_grading_response_rejects_missing_grade() {
        let raw = r#"{"feedback": "no grade field"}"#;
        assert!(parse_grading_response(raw).is_err());
    }
}

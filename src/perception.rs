//! T16b — the perception pass: a cheap-model pass that reasons over the
//! accumulated session diff, the edit-log churn, a mechanical signal
//! summary, the goal, and the tracked below-mastery concept slugs, and
//! emits a structured judgment `{stuck, confidence, site_hint, concept,
//! one_line_evidence}`. Pure prompt builder + pure parser only — the LIVE
//! dispatch (`watch::sweep::run_perception_pass`) stores the result as a
//! [`PerceptionCandidate`] for the SAME offer gate
//! (`watch::offers::run_poll_loop`) every other evidence type flows
//! through. Perception itself never writes a notice/card/offer directly —
//! the anti-Clippy invariant (D15/C12 amendment, new T16b invariant): it
//! only ever *proposes* a candidate the gate may still reject.

use serde::Deserialize;

use crate::diff::{DiffOp, Hunk};
use crate::editlog::ChurnSummary;
use crate::pack::TaxonomyConcept;

/// A compact summary of the mechanical struggle signals — the same ones
/// `struggle::ErrorStreak`/`RedStreak`/help-comment detection already
/// track — fed into the perception prompt alongside the diff/churn, per
/// the T16b spec's `{... + a signal summary (error/red streaks, recent
/// check history, touch cadence) + ...}`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SignalSummary {
    /// `ErrorStreak`'s current code + count (C12 signal 1).
    pub error_code: Option<String>,
    pub error_count: u32,
    /// `RedStreak`'s current minutes-in-red (C12 signal 2).
    pub red_minutes: u64,
    /// A still-live fresh help-flavored comment, if any (C12 signal 3).
    pub help_comment: Option<String>,
    /// T3 req 4 `DriftTracking`'s touch cadence: how many of the recent
    /// touches fall inside the goal's file cluster vs the total recent
    /// touch count.
    pub touches_in_cluster: usize,
    pub touches_total: usize,
}

/// Renders a diff's hunks as plain `+`/`-` lines — mirrors
/// `pipeline::build_stage1_prompt`'s hunk rendering, kept local (no
/// judge/card-context changes per T16b's scope) rather than shared, since
/// the two prompts otherwise diverge (no framing/taxonomy-slug-list leg
/// here).
fn render_hunks(hunks: &[Hunk]) -> String {
    let mut s = String::new();
    for hunk in hunks {
        s.push_str("--- hunk ---\n");
        for op in &hunk.ops {
            match op {
                DiffOp::Added { new_line, text } => {
                    s.push_str(&format!("+{} {}\n", new_line, text))
                }
                DiffOp::Removed { old_line, text } => {
                    s.push_str(&format!("-{} {}\n", old_line, text))
                }
                DiffOp::Context { .. } => {}
            }
        }
    }
    s
}

/// T16b: builds the perception prompt — the accumulated session diff, the
/// edit-log churn summary, the mechanical signal summary, the goal, and
/// the forced-choice below-mastery concept slug list — plus the
/// structured-output instruction. Pure (no I/O, no dispatch).
pub fn build_perception_prompt(
    rel_file: &str,
    hunks: &[Hunk],
    churn: &ChurnSummary,
    signals: &SignalSummary,
    goal: Option<&str>,
    concepts: &[String],
) -> String {
    let mut s = String::new();
    s.push_str(
        "You are watching one developer's live coding session for signs they are stuck.\n\n",
    );
    s.push_str(&format!("File: {}\n\n", rel_file));
    s.push_str("Accumulated session diff:\n");
    s.push_str(&render_hunks(hunks));
    s.push('\n');
    s.push_str(&format!(
        "Edit-log churn: {} edit(s) in the last {} min, {} distinct file state(s), {} revert(s) detected.\n",
        churn.edits_in_window,
        crate::editlog::CHURN_WINDOW.as_secs() / 60,
        churn.distinct_states,
        churn.revert_count,
    ));
    s.push_str(&format!(
        "Signals: same-error streak = {} (code: {}), time in red = {} min, recent help-seeking comment: {}, touches in goal cluster: {}/{}.\n",
        signals.error_count,
        signals.error_code.as_deref().unwrap_or("none"),
        signals.red_minutes,
        signals.help_comment.as_deref().unwrap_or("none"),
        signals.touches_in_cluster,
        signals.touches_total,
    ));
    s.push_str(&format!("Goal: {}\n", goal.unwrap_or("(none set)")));
    s.push_str(
        "Concepts currently below mastery (forced choice for `concept`, or null if none fit): ",
    );
    s.push_str(&concepts.join(", "));
    s.push('\n');
    s.push_str(
        "\nRespond with exactly one JSON object and no other text: \
         {\"stuck\": bool, \"confidence\": 0.0-1.0, \"site_hint\": string or null, \
         \"concept\": one slug from the list above or null, \"one_line_evidence\": string}. \
         `one_line_evidence` must read as a single lowercase clause naming the concept and \
         site, e.g. \"you're circling ownership in parse_config\" — no leading capital, no \
         trailing period; it will be appended to \"... \\u2014 hint? [y/N]\" verbatim.\n",
    );
    s
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PerceptionRaw {
    stuck: Option<bool>,
    confidence: Option<f64>,
    site_hint: Option<String>,
    concept: Option<String>,
    one_line_evidence: Option<String>,
}

/// The perception pass's structured output — the candidate judgment fed
/// (via [`PerceptionCandidate`]) to the offer gate. A malformed/
/// unparseable response parses to the safe default (`stuck: false`,
/// `confidence: 0.0`) — never a fabricated positive.
#[derive(Debug, Clone, PartialEq)]
pub struct PerceptionOutput {
    pub stuck: bool,
    pub confidence: f64,
    pub site_hint: Option<String>,
    pub concept: Option<String>,
    pub one_line_evidence: String,
}

impl Default for PerceptionOutput {
    fn default() -> Self {
        Self {
            stuck: false,
            confidence: 0.0,
            site_hint: None,
            concept: None,
            one_line_evidence: String::new(),
        }
    }
}

/// Best-effort extraction of a single JSON object from model output that
/// may be fenced (```json ... ```) or padded with prose — mirrors
/// `judge::extract_json_object` (kept local rather than shared: no
/// judge/card-context changes per T16b's scope).
fn extract_json_object(raw: &str) -> &str {
    let s = raw.trim();
    match (s.find('{'), s.rfind('}')) {
        (Some(start), Some(end)) if start <= end => &s[start..=end],
        _ => s,
    }
}

/// Parses the perception pass's raw model output. `taxonomy` backs the
/// forced-choice validation of `concept` (mirrors `pack::is_valid_slug`'s
/// use in the judge contract) — a hallucinated/non-taxonomy slug is
/// dropped to `None` rather than trusted verbatim. Malformed JSON yields
/// the whole safe default; a `confidence` outside `0.0..=1.0` is clamped
/// rather than rejecting an otherwise well-formed response.
pub fn parse_perception_output(raw: &str, taxonomy: &[TaxonomyConcept]) -> PerceptionOutput {
    let json = extract_json_object(raw);
    let Ok(parsed) = serde_json::from_str::<PerceptionRaw>(json) else {
        return PerceptionOutput::default();
    };
    let confidence = parsed.confidence.unwrap_or(0.0).clamp(0.0, 1.0);
    let concept = parsed
        .concept
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .filter(|c| crate::pack::is_valid_slug(taxonomy, c));
    let site_hint = parsed
        .site_hint
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    PerceptionOutput {
        stuck: parsed.stuck.unwrap_or(false),
        confidence,
        site_hint,
        concept,
        one_line_evidence: parsed.one_line_evidence.unwrap_or_default(),
    }
}

/// T16b: perception's live candidate proposal — read by the SAME offer
/// gate (`watch::offers::run_poll_loop`) every other evidence type flows
/// through; perception itself never writes a notice/card/offer directly.
/// Only ever constructed when a pass judged `stuck` AND named a concept —
/// a stuck-but-conceptless or not-stuck judgment produces no candidate at
/// all (I11: the offer must be concept-named, so there is nothing to
/// propose either way).
#[derive(Debug, Clone, PartialEq)]
pub struct PerceptionCandidate {
    pub concept: String,
    pub evidence_line: String,
    pub confidence: f64,
    pub site_file: std::path::PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::diff_lines;

    fn taxonomy() -> Vec<TaxonomyConcept> {
        vec![TaxonomyConcept {
            slug: "ownership".to_string(),
            name: "Ownership".to_string(),
            category: crate::pack::Category::Idiom,
        }]
    }

    // --- prompt builder ---

    #[test]
    fn test_build_perception_prompt_includes_diff_churn_signals_goal_and_concepts() {
        let hunks = diff_lines("fn a() {}\n", "fn a() { let x = 1; }\n");
        let churn = ChurnSummary {
            edits_in_window: 3,
            distinct_states: 2,
            revert_count: 1,
        };
        let signals = SignalSummary {
            error_code: Some("E0308".to_string()),
            error_count: 3,
            red_minutes: 12,
            help_comment: Some("why does this need a clone?".to_string()),
            touches_in_cluster: 2,
            touches_total: 5,
        };
        let prompt = build_perception_prompt(
            "src/lib.rs",
            &hunks,
            &churn,
            &signals,
            Some("ship the parser"),
            &["ownership".to_string(), "borrow-vs-clone".to_string()],
        );

        assert!(prompt.contains("src/lib.rs"));
        assert!(prompt.contains("let x = 1"), "diff hunk content present");
        assert!(prompt.contains("3 edit(s)"));
        assert!(prompt.contains("1 revert(s)"));
        assert!(prompt.contains("E0308"));
        assert!(prompt.contains("12 min"));
        assert!(prompt.contains("why does this need a clone?"));
        assert!(prompt.contains("ship the parser"));
        assert!(prompt.contains("ownership, borrow-vs-clone"));
        assert!(prompt.contains("\"stuck\""));
        assert!(prompt.contains("\"confidence\""));
        assert!(prompt.contains("\"concept\""));
        assert!(prompt.contains("\"one_line_evidence\""));
    }

    #[test]
    fn test_build_perception_prompt_handles_no_goal_and_no_signals() {
        let prompt = build_perception_prompt(
            "a.rs",
            &[],
            &ChurnSummary::default(),
            &SignalSummary::default(),
            None,
            &[],
        );
        assert!(prompt.contains("(none set)"));
        assert!(prompt.contains("code: none"));
        assert!(prompt.contains("recent help-seeking comment: none"));
    }

    // --- output parser ---

    #[test]
    fn test_parse_well_formed_output() {
        let raw = r#"{"stuck": true, "confidence": 0.9, "site_hint": "parse_config", "concept": "ownership", "one_line_evidence": "you're circling ownership in parse_config"}"#;
        let out = parse_perception_output(raw, &taxonomy());
        assert!(out.stuck);
        assert_eq!(out.confidence, 0.9);
        assert_eq!(out.site_hint.as_deref(), Some("parse_config"));
        assert_eq!(out.concept.as_deref(), Some("ownership"));
        assert_eq!(
            out.one_line_evidence,
            "you're circling ownership in parse_config"
        );
    }

    #[test]
    fn test_parse_tolerates_markdown_fence_and_prose() {
        let raw = "sure, here you go:\n```json\n{\"stuck\": false, \"confidence\": 0.2, \"site_hint\": null, \"concept\": null, \"one_line_evidence\": \"\"}\n```\nhope that helps!";
        let out = parse_perception_output(raw, &taxonomy());
        assert!(!out.stuck);
        assert_eq!(out.confidence, 0.2);
        assert_eq!(out.site_hint, None);
        assert_eq!(out.concept, None);
    }

    #[test]
    fn test_parse_malformed_json_yields_safe_default() {
        let out = parse_perception_output("not json at all", &taxonomy());
        assert_eq!(out, PerceptionOutput::default());
        assert!(!out.stuck);
        assert_eq!(out.confidence, 0.0);
    }

    #[test]
    fn test_parse_clamps_out_of_range_confidence() {
        let raw =
            r#"{"stuck": true, "confidence": 1.7, "concept": null, "one_line_evidence": "x"}"#;
        let out = parse_perception_output(raw, &taxonomy());
        assert_eq!(out.confidence, 1.0);

        let raw_negative =
            r#"{"stuck": true, "confidence": -0.3, "concept": null, "one_line_evidence": "x"}"#;
        let out_negative = parse_perception_output(raw_negative, &taxonomy());
        assert_eq!(out_negative.confidence, 0.0);
    }

    #[test]
    fn test_parse_drops_non_taxonomy_concept_to_none() {
        let raw = r#"{"stuck": true, "confidence": 0.8, "concept": "made-up-slug", "one_line_evidence": "x"}"#;
        let out = parse_perception_output(raw, &taxonomy());
        assert_eq!(
            out.concept, None,
            "a hallucinated slug outside the forced-choice list must not be trusted verbatim"
        );
    }

    #[test]
    fn test_parse_missing_fields_default_safely() {
        let out = parse_perception_output("{}", &taxonomy());
        assert!(!out.stuck);
        assert_eq!(out.confidence, 0.0);
        assert_eq!(out.concept, None);
        assert_eq!(out.one_line_evidence, "");
    }
}

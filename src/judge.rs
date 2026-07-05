//! Two-stage judge (D9/C6) — stage-1 screen, stage-2 judge with strict
//! output-contract validation, and degraded mode.

use serde::Deserialize;

// --- Stage 1 (screen) ---

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Stage1Candidate {
    pub site_hint: String,
    pub slugs: Vec<String>,
}

/// Parses the stage-1 (screen) model's JSON output: an array of
/// `{site_hint, slugs[]}` candidate moments. Delegates to
/// [`parse_stage1_full`] and drops the detections leg.
pub fn parse_stage1_output(raw: &str) -> Result<Vec<Stage1Candidate>, String> {
    parse_stage1_full(raw).map(|(candidates, _)| candidates)
}

/// T5 req 3 / C6: one stage-1 positive-application detection — a below-
/// mastery concept the screen model saw APPLIED (not misused) at a site,
/// paired via `site_hint` the same way a [`Stage1Candidate`] is.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Stage1Detection {
    pub site_hint: String,
    pub concept: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct Stage1Object {
    #[serde(default)]
    candidates: Vec<Stage1Candidate>,
    #[serde(default)]
    application_detections: Vec<Stage1Detection>,
}

/// T5 req 3 / C6: stage-1's dual output — candidate teaching moments AND
/// positive-application detections for below-mastery concepts, in the same
/// call ("the dual output is how implicit review costs zero extra calls").
/// Accepts either the T1-era bare array (candidates only, no detections) or
/// the T5 object form `{candidates: [...], application_detections: [...]}`
/// — backward compatible with every T1-T4 fixture and dispatch.
pub fn parse_stage1_full(
    raw: &str,
) -> Result<(Vec<Stage1Candidate>, Vec<Stage1Detection>), String> {
    if let Ok(candidates) = serde_json::from_str::<Vec<Stage1Candidate>>(raw) {
        return Ok((candidates, Vec::new()));
    }
    let obj: Stage1Object =
        serde_json::from_str(raw).map_err(|e| format!("stage1 output parse error: {}", e))?;
    Ok((obj.candidates, obj.application_detections))
}

// --- Stage 2 (judge) ---

#[derive(Debug, Clone, Deserialize, Default)]
pub struct Stage2Raw {
    pub concept: Option<String>,
    pub grounding_quote: Option<String>,
    pub why: Option<String>,
    pub rule: Option<String>,
    pub worked_diff: Option<String>,
    pub category: Option<String>,
    pub likely_bug: Option<bool>,
    /// C6 strict-mode-only extra leg (D10): "stage-2 must additionally
    /// produce a concrete failure scenario". Not part of T1 req 7's base
    /// 7-field contract (so its absence never drops a non-bug candidate);
    /// only consulted by [`strict_mode_passed`] when `likely_bug=true`.
    #[serde(default)]
    pub failure_scenario: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage2Card {
    pub concept: String,
    pub grounding_quote: String,
    pub why: String,
    pub rule: String,
    pub worked_diff: String,
    pub category: String,
    pub likely_bug: bool,
    pub failure_scenario: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JudgeDropReason {
    /// T14 req 2: every leg absent/empty (the model's `{}`, per
    /// `packs/*/prompts/stage2.md`'s "If you cannot ground the finding,
    /// respond with `{}` and no other text") — the correct "not a teaching
    /// moment" decline, NOT a contract failure. Distinct from
    /// `MissingLeg`, which is a *partial* response (at least one leg
    /// present) missing a required field.
    Declined,
    MissingLeg(String),
    NonTaxonomyConcept(String),
    UnverifiableQuote,
    ParseError(String),
}

impl JudgeDropReason {
    pub fn reason_tag(&self) -> &'static str {
        match self {
            JudgeDropReason::Declined => "declined",
            JudgeDropReason::MissingLeg(_) => "missing_leg",
            JudgeDropReason::NonTaxonomyConcept(_) => "non_taxonomy_concept",
            JudgeDropReason::UnverifiableQuote => "unverifiable_quote",
            JudgeDropReason::ParseError(_) => "parse_error",
        }
    }

    pub fn detail(&self) -> String {
        match self {
            JudgeDropReason::Declined => String::new(),
            JudgeDropReason::MissingLeg(f) => f.clone(),
            JudgeDropReason::NonTaxonomyConcept(c) => c.clone(),
            JudgeDropReason::UnverifiableQuote => String::new(),
            JudgeDropReason::ParseError(e) => e.clone(),
        }
    }

    /// T14 req 2: true for the correct "chose not to teach" outcome, so a
    /// caller can log a distinct event kind (`judge_declined`) instead of
    /// the failure-flavored `judge_drop`, separating "declined" from
    /// "failed the contract" in the outcome-rate read (T14 req 3).
    pub fn is_declined(&self) -> bool {
        matches!(self, JudgeDropReason::Declined)
    }
}

/// Parses the raw stage-2 (judge) model JSON text into its (possibly
/// partial) field set.
pub fn parse_stage2_output(raw: &str) -> Result<Stage2Raw, JudgeDropReason> {
    // Dogfood 2026-07-05: gemini-3.5-flash (and many models) return the JSON
    // wrapped in a ```json … ``` markdown fence or with surrounding prose,
    // despite the prompt asking for a bare object — so a verbatim parse fails
    // with "expected value at line 1 column 1" and the (good) card is dropped.
    // Extract the outermost `{ … }` span first. rfind('}') lands on the
    // object's own closing brace: a fence close (```), trailing prose, or
    // whitespace after it contains no `}`, and any `}` inside a string value
    // (e.g. the worked_diff) precedes the real close. `{}` (a decline) still
    // extracts to `{}`, preserving the declined-vs-failed distinction.
    let json = extract_json_object(raw);
    serde_json::from_str(json).map_err(|e| JudgeDropReason::ParseError(e.to_string()))
}

/// Best-effort extraction of a single JSON object from model output that may
/// be fenced (```json … ```) or padded with prose — the span from the first
/// `{` to the last `}`. Returns the trimmed input unchanged if no braces are
/// found (so a genuinely empty/garbage response still yields a parse error).
fn extract_json_object(raw: &str) -> &str {
    let s = raw.trim();
    match (s.find('{'), s.rfind('}')) {
        (Some(start), Some(end)) if start <= end => &s[start..=end],
        _ => s,
    }
}

fn non_empty(opt: &Option<String>) -> Option<String> {
    opt.as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// T14 req 2: every leg absent/empty — the model's deliberate `{}` decline,
/// not a partial (some legs present) contract failure. Checked against the
/// same `non_empty` emptiness test every per-field validation below uses,
/// so "empty" means the same thing in both places.
fn stage2_all_legs_empty(raw: &Stage2Raw) -> bool {
    non_empty(&raw.concept).is_none()
        && non_empty(&raw.grounding_quote).is_none()
        && non_empty(&raw.why).is_none()
        && non_empty(&raw.rule).is_none()
        && non_empty(&raw.worked_diff).is_none()
        && non_empty(&raw.category).is_none()
        && raw.likely_bug.is_none()
        && non_empty(&raw.failure_scenario).is_none()
}

/// T1 req 7 / C6: validates the stage-2 output contract. `concept` must be a
/// taxonomy slug; `grounding_quote` must appear verbatim in `file_content`;
/// every leg must be present. Any failure drops the candidate. T14 req 2:
/// an all-empty raw response (the pack prompt's instructed `{}` decline) is
/// checked FIRST and reported as `Declined`, distinct from a partial
/// response that's actually missing a required leg.
pub fn validate_stage2_output(
    raw: &Stage2Raw,
    taxonomy: &[crate::pack::TaxonomyConcept],
    file_content: &str,
) -> Result<Stage2Card, JudgeDropReason> {
    if stage2_all_legs_empty(raw) {
        return Err(JudgeDropReason::Declined);
    }

    let concept = non_empty(&raw.concept)
        .ok_or_else(|| JudgeDropReason::MissingLeg("concept".to_string()))?;
    let grounding_quote = non_empty(&raw.grounding_quote)
        .ok_or_else(|| JudgeDropReason::MissingLeg("grounding_quote".to_string()))?;
    let why = non_empty(&raw.why).ok_or_else(|| JudgeDropReason::MissingLeg("why".to_string()))?;
    let rule =
        non_empty(&raw.rule).ok_or_else(|| JudgeDropReason::MissingLeg("rule".to_string()))?;
    let worked_diff = non_empty(&raw.worked_diff)
        .ok_or_else(|| JudgeDropReason::MissingLeg("worked_diff".to_string()))?;
    let category = non_empty(&raw.category)
        .ok_or_else(|| JudgeDropReason::MissingLeg("category".to_string()))?;
    let likely_bug = raw
        .likely_bug
        .ok_or_else(|| JudgeDropReason::MissingLeg("likely_bug".to_string()))?;

    if !crate::pack::is_valid_slug(taxonomy, &concept) {
        return Err(JudgeDropReason::NonTaxonomyConcept(concept));
    }

    if !file_content.contains(&grounding_quote) {
        return Err(JudgeDropReason::UnverifiableQuote);
    }

    Ok(Stage2Card {
        concept,
        grounding_quote,
        why,
        rule,
        worked_diff,
        category,
        likely_bug,
        failure_scenario: non_empty(&raw.failure_scenario),
    })
}

/// C6 strict mode (D10's bug exemption): `first` is the shown card's sample;
/// `second` is an independent re-sample dispatched only when
/// `first.likely_bug`. Passes only when both agree on `likely_bug` AND the
/// shown sample carries a concrete failure scenario. A failed/unparseable
/// second sample is the caller's problem to represent as `None`-equivalent
/// (i.e. simply don't call this) — this function assumes both were
/// obtained successfully.
pub fn strict_mode_passed(first: &Stage2Card, second: &Stage2Card) -> bool {
    first.likely_bug
        && second.likely_bug
        && first
            .failure_scenario
            .as_ref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
}

/// Builds the `judge_drop` event payload (T1 req 7: "log `judge_drop` in
/// payload"). NOTE: `judge_drop` is not in C5's enumerated `events.kind`
/// vocabulary; see the T1 implementation notes for why this is treated as an
/// additive, not conflicting, extension.
pub fn judge_drop_payload(reason: &JudgeDropReason, site_hint: &str) -> serde_json::Value {
    serde_json::json!({
        "reason": reason.reason_tag(),
        "detail": reason.detail(),
        "site_hint": site_hint,
    })
}

// --- Degraded mode (C6) ---

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JudgeMode {
    Active,
    Degraded { reason: String },
}

/// T8 req 5: providers whose slot needs no key at all — the shared OpenAI-
/// compatible local-provider arm (`provider.rs`'s `"ollama" | "lmstudio" |
/// "openai"`). Generalizes the original Ollama-only special case.
fn is_keyless_provider(provider: &str) -> bool {
    crate::provider::Provider::parse(provider)
        .map(|p| p.is_keyless())
        .unwrap_or(false)
}

/// The key situation for one model slot — separates a genuinely-absent key from
/// a key that EXISTS but is unreadable (keychain ACL), so the degraded-mode
/// reason can tell the user which it is (dogfood 2026-07-03).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyStatus {
    /// A non-empty key is available.
    Present,
    /// No key and no keyring entry — genuinely unconfigured.
    Absent,
    /// A keyring entry exists but could not be read (keychain access denied).
    Unreadable,
}

impl KeyStatus {
    /// Derives the status from a resolved key and whether this provider's
    /// keyring read failed with an access (non-"no entry") error.
    pub fn resolve(key: Option<&str>, keyring_unreadable: bool) -> Self {
        if key.map(|k| !k.is_empty()).unwrap_or(false) {
            KeyStatus::Present
        } else if keyring_unreadable {
            KeyStatus::Unreadable
        } else {
            KeyStatus::Absent
        }
    }
}

/// A model slot has what it needs to dispatch: either a present key, or its
/// provider is keyless (local, no key required) — req 5's "make Ollama [and
/// friends] selectable".
fn slot_ready(provider: &str, status: KeyStatus) -> bool {
    is_keyless_provider(provider) || status == KeyStatus::Present
}

/// C6 degraded mode: no usable key for either model slot (unless that slot's
/// provider is keyless — T8: Ollama, LM Studio, or a generic OpenAI-
/// compatible local endpoint, none of which need one), or (by construction
/// of the caller) a provider error, drops the pipeline to observe-only.
///
/// The reason distinguishes an unreadable keychain entry from a missing one:
/// a key that exists but fails a macOS keychain ACL read otherwise reports the
/// misleading "no API key configured" (dogfood 2026-07-03).
pub fn determine_judge_mode(
    screen_provider: &str,
    screen_status: KeyStatus,
    judge_provider: &str,
    judge_status: KeyStatus,
) -> JudgeMode {
    if slot_ready(screen_provider, screen_status) && slot_ready(judge_provider, judge_status) {
        return JudgeMode::Active;
    }

    // If a blocking slot's key exists but is unreadable, say so — that's a
    // different (and actionable) failure than no key at all.
    let unreadable_blocks = (!slot_ready(screen_provider, screen_status)
        && screen_status == KeyStatus::Unreadable)
        || (!slot_ready(judge_provider, judge_status) && judge_status == KeyStatus::Unreadable);

    let reason = if unreadable_blocks {
        "an API key exists in the keychain but could not be read (access denied) — unlock the keychain, or re-add the key via `murshid setup`".to_string()
    } else {
        "no API key configured for the model seam (screen/judge)".to_string()
    };
    JudgeMode::Degraded { reason }
}

/// The pane status line shown in degraded mode (T1 req 13): explains why,
/// never crashes.
pub fn degraded_status_line(reason: &str) -> String {
    format!(
        "[murshid] degraded mode: {} — observing only (events still recorded, no cards).",
        reason
    )
}

/// Wraps a stage dispatch call so a provider failure degrades gracefully
/// instead of propagating a panic. `dispatch` is expected to be
/// `provider::dispatch_debounced` (or a test double for it).
pub fn safe_dispatch<F>(dispatch: F) -> Result<String, JudgeMode>
where
    F: FnOnce() -> Result<String, String>,
{
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(dispatch)) {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(e)) => Err(JudgeMode::Degraded { reason: e }),
        Err(_) => Err(JudgeMode::Degraded {
            reason: "provider call panicked".to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn taxonomy() -> Vec<crate::pack::TaxonomyConcept> {
        // T9 req 9 addendum: see pipeline.rs::tests::taxonomy() for why this
        // takes the crate-wide env lock.
        let _lock = crate::credentials::env_test_lock();
        crate::pack::load_taxonomy(&crate::pack::default_pack_dir()).unwrap()
    }

    // --- stage 1 ---

    #[test]
    fn test_parse_stage1_output_fixture() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/stage1_response.json");
        let raw = std::fs::read_to_string(&path).unwrap();
        let candidates = parse_stage1_output(&raw).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].site_hint, "fn print_name");
        assert_eq!(candidates[0].slugs, vec!["borrow-vs-clone".to_string()]);
    }

    #[test]
    fn test_parse_stage1_output_ignores_application_detections_field() {
        let raw = r#"[{"site_hint": "fn foo", "slugs": ["borrow-vs-clone"], "application_detections": ["something"]}]"#;
        let candidates = parse_stage1_output(raw).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].slugs, vec!["borrow-vs-clone".to_string()]);
    }

    // --- T5 req 3 / C6: stage-1 dual output ---

    #[test]
    fn test_parse_stage1_full_bare_array_is_candidates_only_no_detections() {
        let raw = r#"[{"site_hint": "fn foo", "slugs": ["borrow-vs-clone"]}]"#;
        let (candidates, detections) = parse_stage1_full(raw).unwrap();
        assert_eq!(candidates.len(), 1);
        assert!(detections.is_empty());
    }

    #[test]
    fn test_parse_stage1_full_object_form_carries_both_legs() {
        let raw = r#"{
            "candidates": [{"site_hint": "fn foo", "slugs": ["borrow-vs-clone"]}],
            "application_detections": [{"site_hint": "fn bar", "concept": "option-combinators"}]
        }"#;
        let (candidates, detections) = parse_stage1_full(raw).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(detections.len(), 1);
        assert_eq!(detections[0].concept, "option-combinators");
        assert_eq!(detections[0].site_hint, "fn bar");
    }

    #[test]
    fn test_parse_stage1_full_object_form_empty_detections_is_fine() {
        let raw = r#"{"candidates": [], "application_detections": []}"#;
        let (candidates, detections) = parse_stage1_full(raw).unwrap();
        assert!(candidates.is_empty());
        assert!(detections.is_empty());
    }

    // --- stage 2: contract validation ---

    #[test]
    fn test_validate_stage2_output_valid_fixture() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/stage2_response_valid.json");
        let raw_text = std::fs::read_to_string(&path).unwrap();
        let raw = parse_stage2_output(&raw_text).unwrap();

        let file_content = "fn print_name(name: String) { println!(\"{}\", name); }\nprint_name(person.name.clone());\n";
        let card = validate_stage2_output(&raw, &taxonomy(), file_content).unwrap();
        assert_eq!(card.concept, "borrow-vs-clone");
        assert!(file_content.contains(&card.grounding_quote));
    }

    #[test]
    fn test_drop_on_missing_leg() {
        let raw = Stage2Raw {
            concept: Some("borrow-vs-clone".to_string()),
            grounding_quote: Some("person.name.clone()".to_string()),
            why: Some("why text".to_string()),
            rule: Some("rule text".to_string()),
            worked_diff: None, // missing leg
            category: Some("best-practice".to_string()),
            likely_bug: Some(false),
            failure_scenario: None,
        };
        let file_content = "print_name(person.name.clone());\n";
        let result = validate_stage2_output(&raw, &taxonomy(), file_content);
        assert_eq!(
            result,
            Err(JudgeDropReason::MissingLeg("worked_diff".to_string()))
        );
    }

    #[test]
    fn test_drop_on_non_taxonomy_concept() {
        let raw = Stage2Raw {
            concept: Some("not-a-real-concept".to_string()),
            grounding_quote: Some("person.name.clone()".to_string()),
            why: Some("why text".to_string()),
            rule: Some("rule text".to_string()),
            worked_diff: Some("diff text".to_string()),
            category: Some("best-practice".to_string()),
            likely_bug: Some(false),
            failure_scenario: None,
        };
        let file_content = "print_name(person.name.clone());\n";
        let result = validate_stage2_output(&raw, &taxonomy(), file_content);
        assert_eq!(
            result,
            Err(JudgeDropReason::NonTaxonomyConcept(
                "not-a-real-concept".to_string()
            ))
        );
    }

    #[test]
    fn test_drop_on_unverifiable_quote() {
        let raw = Stage2Raw {
            concept: Some("borrow-vs-clone".to_string()),
            grounding_quote: Some("this text is not in the file".to_string()),
            why: Some("why text".to_string()),
            rule: Some("rule text".to_string()),
            worked_diff: Some("diff text".to_string()),
            category: Some("best-practice".to_string()),
            likely_bug: Some(false),
            failure_scenario: None,
        };
        let file_content = "print_name(person.name.clone());\n";
        let result = validate_stage2_output(&raw, &taxonomy(), file_content);
        assert_eq!(result, Err(JudgeDropReason::UnverifiableQuote));
    }

    // --- T14 req 2: declined ({}) vs contract-failure (partial) vs parse_error ---

    #[test]
    fn test_validate_stage2_output_empty_object_is_declined() {
        let raw = parse_stage2_output("{}").unwrap();
        let file_content = "print_name(person.name.clone());\n";
        let result = validate_stage2_output(&raw, &taxonomy(), file_content);
        assert_eq!(result, Err(JudgeDropReason::Declined));
    }

    #[test]
    fn test_validate_stage2_output_partial_missing_concept_is_contract_failure_not_declined() {
        // Every OTHER leg present, only `concept` missing — a genuine
        // partial/broken response, not the model's `{}` decline.
        let raw = Stage2Raw {
            concept: None,
            grounding_quote: Some("person.name.clone()".to_string()),
            why: Some("why text".to_string()),
            rule: Some("rule text".to_string()),
            worked_diff: Some("diff text".to_string()),
            category: Some("best-practice".to_string()),
            likely_bug: Some(false),
            failure_scenario: None,
        };
        let file_content = "print_name(person.name.clone());\n";
        let result = validate_stage2_output(&raw, &taxonomy(), file_content);
        assert_eq!(
            result,
            Err(JudgeDropReason::MissingLeg("concept".to_string()))
        );
    }

    #[test]
    fn test_parse_stage2_output_malformed_is_parse_error_unchanged() {
        let result = parse_stage2_output("{not valid json");
        assert!(matches!(result, Err(JudgeDropReason::ParseError(_))));
    }

    #[test]
    fn test_parse_stage2_output_strips_markdown_json_fence() {
        // Dogfood 2026-07-05: the exact shape gemini-3.5-flash returned (fenced
        // JSON whose worked_diff even contains a nested ```rust block) must
        // parse — before the fix this was dropped as a parse_error.
        let raw = "```json\n{\n  \"concept\": \"string-vs-str\",\n  \"why\": \"w\",\n  \"worked_diff\": \"```rust\\n-a\\n+b\\n```\"\n}\n```";
        let parsed = parse_stage2_output(raw).expect("fenced JSON should parse");
        assert_eq!(parsed.concept.as_deref(), Some("string-vs-str"));
        assert_eq!(parsed.worked_diff.as_deref(), Some("```rust\n-a\n+b\n```"));
    }

    #[test]
    fn test_parse_stage2_output_tolerates_surrounding_prose() {
        let raw = "Here is the card:\n{\"concept\":\"c\"}\nHope that helps!";
        let parsed = parse_stage2_output(raw).expect("prose-wrapped JSON should parse");
        assert_eq!(parsed.concept.as_deref(), Some("c"));
    }

    #[test]
    fn test_parse_stage2_output_empty_object_still_parses_to_decline() {
        // The extract must not turn a genuine `{}` decline into a parse error.
        let parsed = parse_stage2_output("{}").expect("{} parses");
        assert!(parsed.concept.is_none());
    }

    #[test]
    fn test_judge_drop_reason_is_declined_discriminator() {
        assert!(JudgeDropReason::Declined.is_declined());
        assert!(!JudgeDropReason::MissingLeg("concept".to_string()).is_declined());
        assert!(!JudgeDropReason::NonTaxonomyConcept("x".to_string()).is_declined());
        assert!(!JudgeDropReason::UnverifiableQuote.is_declined());
        assert!(!JudgeDropReason::ParseError("x".to_string()).is_declined());
    }

    #[test]
    fn test_judge_drop_payload_shape() {
        let payload = judge_drop_payload(&JudgeDropReason::UnverifiableQuote, "fn foo");
        assert_eq!(payload["reason"], "unverifiable_quote");
        assert_eq!(payload["site_hint"], "fn foo");
    }

    // --- strict mode (C6/D10) ---

    fn bug_card(failure_scenario: Option<&str>) -> Stage2Card {
        Stage2Card {
            concept: "borrow-vs-clone".to_string(),
            grounding_quote: "person.name.clone()".to_string(),
            why: "why".to_string(),
            rule: "rule".to_string(),
            worked_diff: "diff".to_string(),
            category: "bug".to_string(),
            likely_bug: true,
            failure_scenario: failure_scenario.map(|s| s.to_string()),
        }
    }

    #[test]
    fn test_strict_mode_passes_when_both_agree_and_scenario_present() {
        let first = bug_card(Some(
            "passing None where the caller expects Some panics at runtime",
        ));
        let second = bug_card(Some("independent sample, also concrete"));
        assert!(strict_mode_passed(&first, &second));
    }

    #[test]
    fn test_strict_mode_fails_on_disagreement() {
        let first = bug_card(Some("concrete scenario"));
        let mut second = bug_card(Some("concrete scenario"));
        second.likely_bug = false;
        assert!(!strict_mode_passed(&first, &second));
    }

    #[test]
    fn test_strict_mode_fails_without_concrete_failure_scenario() {
        let first = bug_card(None);
        let second = bug_card(Some("concrete scenario"));
        assert!(!strict_mode_passed(&first, &second));
    }

    #[test]
    fn test_strict_mode_fails_when_first_sample_not_likely_bug() {
        let mut first = bug_card(Some("concrete scenario"));
        first.likely_bug = false;
        let second = bug_card(Some("concrete scenario"));
        assert!(!strict_mode_passed(&first, &second));
    }

    // --- degraded mode ---

    #[test]
    fn test_degraded_mode_when_no_keys() {
        let mode = determine_judge_mode("gemini", KeyStatus::Absent, "claude", KeyStatus::Absent);
        assert!(matches!(mode, JudgeMode::Degraded { .. }));
    }

    #[test]
    fn test_active_mode_when_both_keys_present() {
        let mode = determine_judge_mode("gemini", KeyStatus::Present, "claude", KeyStatus::Present);
        assert_eq!(mode, JudgeMode::Active);
    }

    #[test]
    fn test_active_mode_for_ollama_slot_without_key() {
        // req 5: Ollama is local and needs no key.
        let mode = determine_judge_mode("ollama", KeyStatus::Absent, "claude", KeyStatus::Present);
        assert_eq!(mode, JudgeMode::Active);
    }

    // --- T8 req 5: slot_ready generalizes beyond Ollama ---

    #[test]
    fn test_active_mode_for_lmstudio_slot_without_key() {
        let mode =
            determine_judge_mode("lmstudio", KeyStatus::Absent, "claude", KeyStatus::Present);
        assert_eq!(mode, JudgeMode::Active);
    }

    #[test]
    fn test_active_mode_for_openai_compat_slot_without_key() {
        // req 5: a generic OpenAI-compatible local endpoint is keyless too
        // (out of scope: hosted OpenAI with a real API key).
        let mode = determine_judge_mode("openai", KeyStatus::Absent, "claude", KeyStatus::Present);
        assert_eq!(mode, JudgeMode::Active);
    }

    #[test]
    fn test_degraded_when_non_ollama_slot_missing_key_even_if_other_is_ollama() {
        let mode = determine_judge_mode("ollama", KeyStatus::Absent, "claude", KeyStatus::Absent);
        assert!(matches!(mode, JudgeMode::Degraded { .. }));
    }

    #[test]
    fn test_key_status_resolve_distinguishes_absent_from_unreadable() {
        assert_eq!(KeyStatus::resolve(Some("k"), false), KeyStatus::Present);
        assert_eq!(KeyStatus::resolve(Some("k"), true), KeyStatus::Present); // usable key wins
        assert_eq!(KeyStatus::resolve(None, false), KeyStatus::Absent);
        assert_eq!(KeyStatus::resolve(Some(""), false), KeyStatus::Absent);
        assert_eq!(KeyStatus::resolve(None, true), KeyStatus::Unreadable);
    }

    #[test]
    fn test_unreadable_key_gets_a_distinct_actionable_reason() {
        // A key that EXISTS but can't be read must not report "no key configured".
        let unreadable = determine_judge_mode(
            "gemini",
            KeyStatus::Unreadable,
            "claude",
            KeyStatus::Present,
        );
        let JudgeMode::Degraded { reason } = unreadable else {
            panic!("expected degraded mode");
        };
        assert!(reason.contains("could not be read"), "reason was: {reason}");
        assert!(!reason.contains("no API key configured"));

        // A genuinely-absent key keeps the generic reason.
        let absent =
            determine_judge_mode("gemini", KeyStatus::Absent, "claude", KeyStatus::Present);
        let JudgeMode::Degraded { reason } = absent else {
            panic!("expected degraded mode");
        };
        assert!(
            reason.contains("no API key configured"),
            "reason was: {reason}"
        );
    }

    #[test]
    fn test_degraded_status_line_explains_why() {
        let line = degraded_status_line("no API key configured");
        assert!(line.contains("no API key configured"));
        assert!(line.contains("degraded mode"));
    }

    #[test]
    fn test_safe_dispatch_never_panics_on_provider_error() {
        let result = safe_dispatch(|| Err::<String, String>("provider unreachable".to_string()));
        assert!(matches!(result, Err(JudgeMode::Degraded { .. })));
    }

    #[test]
    fn test_safe_dispatch_never_panics_on_panicking_provider() {
        let result: Result<String, JudgeMode> = safe_dispatch(|| -> Result<String, String> {
            panic!("simulated provider crash");
        });
        assert!(matches!(result, Err(JudgeMode::Degraded { .. })));
    }
}

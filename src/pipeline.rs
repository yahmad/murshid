//! Orchestration glue: session-diff hunks -> two-stage judge -> one R2 card
//! (T1 vertical slice). Dispatch is injected so this is testable against
//! recorded fixture responses, never a live call, per the T1 acceptance
//! note on LLM calls in tests.

use crate::diff::{DiffOp, Hunk};
use crate::pack::{CanonEntry, TaxonomyConcept};

/// Builds the stage-1 (screen) prompt: session-diff hunks + one line of
/// enclosing context per hunk + the taxonomy slug list (T1 req 6), sanitized
/// per req 5.
pub fn build_stage1_prompt(rel_file: &str, hunks: &[Hunk], taxonomy: &[TaxonomyConcept]) -> String {
    let mut s = String::new();
    s.push_str("Screen this Rust session diff for teaching moments.\n\n");
    s.push_str(&format!("File: {}\n\n", rel_file));

    for hunk in hunks {
        s.push_str("--- hunk ---\n");
        if let Some(ctx) = hunk.ops.iter().find_map(|op| match op {
            DiffOp::Context { text, .. } => Some(text.clone()),
            _ => None,
        }) {
            s.push_str(&format!("context: {}\n", ctx));
        }
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

    s.push_str("\nTaxonomy slugs: ");
    s.push_str(
        &taxonomy
            .iter()
            .map(|c| c.slug.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    );
    s.push('\n');

    crate::sanitizer::sanitize_diagnostics(&s)
}

/// Builds the stage-2 (judge) prompt: one candidate + full enclosing item +
/// the matching canon entries (T1 req 7), sanitized per req 5.
pub fn build_stage2_prompt(
    candidate_slugs: &[String],
    enclosing_item_text: &str,
    canon: &[CanonEntry],
) -> String {
    let mut s = String::new();
    s.push_str("Judge this candidate teaching moment.\n\n");
    s.push_str("Enclosing item:\n");
    s.push_str(enclosing_item_text);
    s.push_str("\n\nCanon entries:\n");
    for slug in candidate_slugs {
        if let Some(entry) = crate::pack::find_canon_for_concept(canon, slug) {
            s.push_str(&format!(
                "- {} :: {}\n  why: {}\n  use instead: {}\n",
                entry.concept, entry.what_it_does, entry.why_is_this_bad, entry.use_instead
            ));
        }
    }
    crate::sanitizer::sanitize_diagnostics(&s)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JudgeOutcome {
    pub card: Option<crate::card::Card>,
    pub stage2: Option<crate::judge::Stage2Card>,
    pub drop_reason: Option<crate::judge::JudgeDropReason>,
    /// C6/D10: whether a `likely_bug` card cleared strict mode (second
    /// independent sample agreed + concrete failure scenario), i.e. may
    /// bypass the push budget. Always `false` for non-bug cards.
    pub strict_mode_passed: bool,
    /// req 8/req-4-fix: true when every one of stage-1's candidate slugs at
    /// this site had already been judged/shown this session, so stage 2 was
    /// never dispatched.
    pub deduped: bool,
}

impl JudgeOutcome {
    fn empty() -> Self {
        Self {
            card: None,
            stage2: None,
            drop_reason: None,
            strict_mode_passed: false,
            deduped: false,
        }
    }
}

/// Runs stage 1 -> (dedup check) -> stage 2 -> contract validation -> (strict
/// mode re-sample) over one changed file's session-diff hunks, producing at
/// most one card. `dispatch_stage1`/`dispatch_stage2` are injected so this
/// never makes a live call in tests; `already_judged` reports whether an
/// advice-fingerprint (`concept`, site) has already been judged/shown this
/// session (T1 req 8) — it's checked *before* stage 2 is dispatched so
/// repeat encounters at a known site don't re-burn the strong model.
#[allow(clippy::too_many_arguments)]
pub fn judge_hunks(
    rel_file: &str,
    hunks: &[Hunk],
    current_content: &str,
    taxonomy: &[TaxonomyConcept],
    canon: &[CanonEntry],
    already_judged: impl Fn(&str) -> bool,
    dispatch_stage1: impl Fn(&str) -> Result<String, String>,
    dispatch_stage2: impl Fn(&str) -> Result<String, String>,
) -> Result<JudgeOutcome, String> {
    if hunks.is_empty() {
        return Ok(JudgeOutcome::empty());
    }

    let stage1_prompt = build_stage1_prompt(rel_file, hunks, taxonomy);
    let stage1_raw = dispatch_stage1(&stage1_prompt)?;
    let candidates = crate::judge::parse_stage1_output(&stage1_raw)?;

    let Some(candidate) = candidates.into_iter().next() else {
        return Ok(JudgeOutcome::empty());
    };

    let changed_lines = crate::diff::changed_line_numbers(hunks);
    let Some(&line) = changed_lines.first() else {
        return Ok(JudgeOutcome::empty());
    };

    // req 8 / req-4-fix: dedup before dispatching stage 2. If every
    // candidate slug is already judged at this site, skip re-screening.
    if !candidate.slugs.is_empty() {
        if let Some(site) = crate::site::compute_site(rel_file, current_content, line) {
            let all_known = candidate
                .slugs
                .iter()
                .all(|slug| already_judged(&crate::site::advice_fingerprint(slug, &site)));
            if all_known {
                let mut outcome = JudgeOutcome::empty();
                outcome.deduped = true;
                return Ok(outcome);
            }
        }
    }

    let enclosing_text = crate::site::enclosing_item_text(current_content, line)
        .unwrap_or_else(|| current_content.to_string());

    let stage2_prompt = build_stage2_prompt(&candidate.slugs, &enclosing_text, canon);
    let stage2_raw = dispatch_stage2(&stage2_prompt)?;

    let raw = match crate::judge::parse_stage2_output(&stage2_raw) {
        Ok(r) => r,
        Err(reason) => {
            let mut outcome = JudgeOutcome::empty();
            outcome.drop_reason = Some(reason);
            return Ok(outcome);
        }
    };

    let stage2_card = match crate::judge::validate_stage2_output(&raw, taxonomy, current_content) {
        Ok(c) => c,
        Err(reason) => {
            let mut outcome = JudgeOutcome::empty();
            outcome.drop_reason = Some(reason);
            return Ok(outcome);
        }
    };

    // C6 strict mode (D10): only for likely_bug cards, dispatch a second
    // independent stage-2 sample; disagreement or a failed/unparseable
    // re-sample just means "not budget-exempt", never drops the card.
    let strict_mode_passed = if stage2_card.likely_bug {
        dispatch_stage2(&stage2_prompt)
            .ok()
            .and_then(|second_raw| crate::judge::parse_stage2_output(&second_raw).ok())
            .and_then(|second_parsed| {
                crate::judge::validate_stage2_output(&second_parsed, taxonomy, current_content).ok()
            })
            .map(|second_card| crate::judge::strict_mode_passed(&stage2_card, &second_card))
            .unwrap_or(false)
    } else {
        false
    };

    let canon_entry = crate::pack::find_canon_for_concept(canon, &stage2_card.concept);
    let doc_ref = canon_entry
        .and_then(|e| e.refs.first().cloned())
        .unwrap_or_default();
    let concept_name = taxonomy
        .iter()
        .find(|c| c.slug == stage2_card.concept)
        .map(|c| c.name.clone())
        .unwrap_or_else(|| stage2_card.concept.clone());

    let card = crate::card::Card {
        concept_name,
        file: rel_file.to_string(),
        line,
        grounding_quote: stage2_card.grounding_quote.clone(),
        why: stage2_card.why.clone(),
        rule: stage2_card.rule.clone(),
        doc_ref,
        worked_diff: stage2_card.worked_diff.clone(),
    };

    Ok(JudgeOutcome {
        card: Some(card),
        stage2: Some(stage2_card),
        drop_reason: None,
        strict_mode_passed,
        deduped: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn taxonomy() -> Vec<TaxonomyConcept> {
        crate::pack::load_taxonomy(&crate::pack::default_pack_dir()).unwrap()
    }

    fn canon() -> Vec<CanonEntry> {
        crate::pack::load_canon(&crate::pack::default_pack_dir()).unwrap()
    }

    fn fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        std::fs::read_to_string(&path).unwrap()
    }

    #[test]
    fn test_build_stage1_prompt_contains_hunks_and_taxonomy() {
        let old = "fn print_name(name: String) { println!(\"{}\", name); }\n";
        let new = "fn print_name(name: String) { println!(\"{}\", name); }\nprint_name(person.name.clone());\n";
        let hunks = crate::diff::diff_lines(old, new);
        let prompt = build_stage1_prompt("src/main.rs", &hunks, &taxonomy());
        assert!(prompt.contains("File: src/main.rs"));
        assert!(prompt.contains("person.name.clone()"));
        assert!(prompt.contains("borrow-vs-clone"));
    }

    /// The automated stand-in for T1's manual dogfood check: an introduced
    /// `.clone()`-where-borrow-works diff, run through the full stage1 ->
    /// stage2 -> validation pipeline against recorded fixture responses,
    /// produces one card naming `borrow-vs-clone` with a verbatim grounding
    /// quote.
    #[test]
    fn test_judge_hunks_full_flow_produces_borrow_vs_clone_card() {
        let old = "fn print_name(name: String) { println!(\"{}\", name); }\n";
        let new = "fn print_name(name: String) { println!(\"{}\", name); }\nprint_name(person.name.clone());\n";
        let hunks = crate::diff::diff_lines(old, new);

        let stage1_fixture = fixture("stage1_response.json");
        let stage2_fixture = fixture("stage2_response_valid.json");

        let outcome = judge_hunks(
            "src/main.rs",
            &hunks,
            new,
            &taxonomy(),
            &canon(),
            |_fp| false,
            |_prompt| Ok(stage1_fixture.clone()),
            |_prompt| Ok(stage2_fixture.clone()),
        )
        .unwrap();

        assert!(outcome.drop_reason.is_none());
        assert!(!outcome.deduped);
        let card = outcome.card.expect("expected a card");
        assert_eq!(card.concept_name, "Borrow vs. clone");
        assert!(new.contains(&card.grounding_quote));
        assert_eq!(card.file, "src/main.rs");
        // req 9: worked_diff must be carried through, not discarded.
        assert!(!card.worked_diff.is_empty());
        // Not a likely_bug candidate in this fixture, so strict mode never engages.
        assert!(!outcome.strict_mode_passed);
    }

    #[test]
    fn test_judge_hunks_reports_drop_reason_on_invalid_stage2() {
        let old = "fn print_name(name: String) { println!(\"{}\", name); }\n";
        let new = "fn print_name(name: String) { println!(\"{}\", name); }\nprint_name(person.name.clone());\n";
        let hunks = crate::diff::diff_lines(old, new);

        let stage1_fixture = fixture("stage1_response.json");
        let bad_stage2 = r#"{"concept": "not-a-real-concept", "grounding_quote": "person.name.clone()", "why": "x", "rule": "y", "worked_diff": "z", "category": "idiom", "likely_bug": false}"#.to_string();

        let outcome = judge_hunks(
            "src/main.rs",
            &hunks,
            new,
            &taxonomy(),
            &canon(),
            |_fp| false,
            |_prompt| Ok(stage1_fixture.clone()),
            |_prompt| Ok(bad_stage2.clone()),
        )
        .unwrap();

        assert!(outcome.card.is_none());
        assert!(matches!(
            outcome.drop_reason,
            Some(crate::judge::JudgeDropReason::NonTaxonomyConcept(_))
        ));
    }

    #[test]
    fn test_judge_hunks_none_when_no_hunks() {
        let outcome = judge_hunks(
            "src/main.rs",
            &[],
            "fn a() {}\n",
            &taxonomy(),
            &canon(),
            |_fp| false,
            |_| Ok("[]".to_string()),
            |_| Ok("{}".to_string()),
        )
        .unwrap();
        assert!(outcome.card.is_none());
        assert!(outcome.drop_reason.is_none());
    }

    #[test]
    fn test_judge_hunks_none_when_stage1_has_no_candidates() {
        let old = "fn a() {}\n";
        let new = "fn a() { let x = 1; }\n";
        let hunks = crate::diff::diff_lines(old, new);

        let outcome = judge_hunks(
            "src/main.rs",
            &hunks,
            new,
            &taxonomy(),
            &canon(),
            |_fp| false,
            |_| Ok("[]".to_string()),
            |_| Ok("{}".to_string()),
        )
        .unwrap();
        assert!(outcome.card.is_none());
    }

    // --- req 8 / req-4-fix: dedup before dispatching stage 2 ---

    #[test]
    fn test_judge_hunks_skips_stage2_when_all_candidate_slugs_already_judged() {
        let old = "fn print_name(name: String) { println!(\"{}\", name); }\n\nfn caller() {\n}\n";
        let new = "fn print_name(name: String) { println!(\"{}\", name); }\n\nfn caller() {\n    print_name(person.name.clone());\n}\n";
        let hunks = crate::diff::diff_lines(old, new);

        let stage1_fixture = fixture("stage1_response.json"); // candidate slugs = ["borrow-vs-clone"]
        let stage2_calls = std::cell::Cell::new(0);

        let outcome = judge_hunks(
            "src/main.rs",
            &hunks,
            new,
            &taxonomy(),
            &canon(),
            |_fp| true, // every advice-fp is already judged
            |_prompt| Ok(stage1_fixture.clone()),
            |_prompt| {
                stage2_calls.set(stage2_calls.get() + 1);
                Ok("{}".to_string())
            },
        )
        .unwrap();

        assert!(outcome.deduped);
        assert!(outcome.card.is_none());
        assert_eq!(
            stage2_calls.get(),
            0,
            "stage 2 must never be dispatched for a fully-known site"
        );
    }

    #[test]
    fn test_judge_hunks_still_dispatches_stage2_when_site_unknown() {
        let old = "fn print_name(name: String) { println!(\"{}\", name); }\n";
        let new = "fn print_name(name: String) { println!(\"{}\", name); }\nprint_name(person.name.clone());\n";
        let hunks = crate::diff::diff_lines(old, new);

        let stage1_fixture = fixture("stage1_response.json");
        let stage2_fixture = fixture("stage2_response_valid.json");

        let outcome = judge_hunks(
            "src/main.rs",
            &hunks,
            new,
            &taxonomy(),
            &canon(),
            |_fp| false, // nothing known yet
            |_prompt| Ok(stage1_fixture.clone()),
            |_prompt| Ok(stage2_fixture.clone()),
        )
        .unwrap();

        assert!(!outcome.deduped);
        assert!(outcome.card.is_some());
    }

    // --- C6/D10 strict mode: second independent stage-2 sample ---

    #[test]
    fn test_judge_hunks_strict_mode_passes_when_second_sample_agrees() {
        let old = "fn maybe_get() -> Option<i32> { None }\n";
        let new = "fn maybe_get() -> Option<i32> { None }\nlet v = maybe_get().unwrap();\n";
        let hunks = crate::diff::diff_lines(old, new);

        let stage1_fixture =
            r#"[{"site_hint": "let v", "slugs": ["option-combinators"]}]"#.to_string();
        let stage2_fixture = r#"{
            "concept": "option-combinators",
            "grounding_quote": "maybe_get().unwrap()",
            "why": "unwrap panics on None.",
            "rule": "Use map/unwrap_or instead of unwrap.",
            "worked_diff": "- .unwrap()\n+ .unwrap_or(0)",
            "category": "bug",
            "likely_bug": true,
            "failure_scenario": "maybe_get() returns None here, so unwrap() panics at runtime."
        }"#
        .to_string();

        let call_count = std::cell::Cell::new(0);
        let outcome = judge_hunks(
            "src/main.rs",
            &hunks,
            new,
            &taxonomy(),
            &canon(),
            |_fp| false,
            |_prompt| Ok(stage1_fixture.clone()),
            |_prompt| {
                call_count.set(call_count.get() + 1);
                Ok(stage2_fixture.clone())
            },
        )
        .unwrap();

        assert_eq!(
            call_count.get(),
            2,
            "likely_bug must trigger a second independent sample"
        );
        assert!(outcome.strict_mode_passed);
        assert!(outcome.card.is_some());
    }

    #[test]
    fn test_judge_hunks_strict_mode_fails_when_second_sample_disagrees() {
        let old = "fn maybe_get() -> Option<i32> { None }\n";
        let new = "fn maybe_get() -> Option<i32> { None }\nlet v = maybe_get().unwrap();\n";
        let hunks = crate::diff::diff_lines(old, new);

        let stage1_fixture =
            r#"[{"site_hint": "let v", "slugs": ["option-combinators"]}]"#.to_string();
        let bug_fixture = r#"{
            "concept": "option-combinators",
            "grounding_quote": "maybe_get().unwrap()",
            "why": "unwrap panics on None.",
            "rule": "Use map/unwrap_or instead of unwrap.",
            "worked_diff": "- .unwrap()\n+ .unwrap_or(0)",
            "category": "bug",
            "likely_bug": true,
            "failure_scenario": "maybe_get() returns None here, so unwrap() panics at runtime."
        }"#
        .to_string();
        let not_bug_fixture = r#"{
            "concept": "option-combinators",
            "grounding_quote": "maybe_get().unwrap()",
            "why": "unwrap panics on None.",
            "rule": "Use map/unwrap_or instead of unwrap.",
            "worked_diff": "- .unwrap()\n+ .unwrap_or(0)",
            "category": "idiom",
            "likely_bug": false
        }"#
        .to_string();

        let call_count = std::cell::Cell::new(0);
        let outcome = judge_hunks(
            "src/main.rs",
            &hunks,
            new,
            &taxonomy(),
            &canon(),
            |_fp| false,
            |_prompt| Ok(stage1_fixture.clone()),
            |_prompt| {
                let n = call_count.get();
                call_count.set(n + 1);
                if n == 0 {
                    Ok(bug_fixture.clone())
                } else {
                    Ok(not_bug_fixture.clone())
                }
            },
        )
        .unwrap();

        assert_eq!(call_count.get(), 2);
        assert!(
            !outcome.strict_mode_passed,
            "disagreement must not be budget-exempt"
        );
        // The card still ships — disagreement only removes the bypass, per C6.
        assert!(outcome.card.is_some());
    }

    #[test]
    fn test_judge_hunks_no_strict_mode_dispatch_for_non_bug_card() {
        let old = "fn print_name(name: String) { println!(\"{}\", name); }\n";
        let new = "fn print_name(name: String) { println!(\"{}\", name); }\nprint_name(person.name.clone());\n";
        let hunks = crate::diff::diff_lines(old, new);

        let stage1_fixture = fixture("stage1_response.json");
        let stage2_fixture = fixture("stage2_response_valid.json"); // likely_bug: false

        let call_count = std::cell::Cell::new(0);
        let outcome = judge_hunks(
            "src/main.rs",
            &hunks,
            new,
            &taxonomy(),
            &canon(),
            |_fp| false,
            |_prompt| Ok(stage1_fixture.clone()),
            |_prompt| {
                call_count.set(call_count.get() + 1);
                Ok(stage2_fixture.clone())
            },
        )
        .unwrap();

        assert_eq!(
            call_count.get(),
            1,
            "non-bug cards never trigger a second stage-2 sample"
        );
        assert!(!outcome.strict_mode_passed);
    }
}

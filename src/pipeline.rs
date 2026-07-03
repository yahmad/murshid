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
}

/// Runs stage 1 -> stage 2 -> contract validation over one changed file's
/// session-diff hunks, producing at most one card. `dispatch_stage1`/
/// `dispatch_stage2` are injected so this never makes a live call in tests.
#[allow(clippy::too_many_arguments)]
pub fn judge_hunks(
    rel_file: &str,
    hunks: &[Hunk],
    current_content: &str,
    taxonomy: &[TaxonomyConcept],
    canon: &[CanonEntry],
    dispatch_stage1: impl FnOnce(&str) -> Result<String, String>,
    dispatch_stage2: impl FnOnce(&str) -> Result<String, String>,
) -> Result<JudgeOutcome, String> {
    if hunks.is_empty() {
        return Ok(JudgeOutcome {
            card: None,
            stage2: None,
            drop_reason: None,
        });
    }

    let stage1_prompt = build_stage1_prompt(rel_file, hunks, taxonomy);
    let stage1_raw = dispatch_stage1(&stage1_prompt)?;
    let candidates = crate::judge::parse_stage1_output(&stage1_raw)?;

    let Some(candidate) = candidates.into_iter().next() else {
        return Ok(JudgeOutcome {
            card: None,
            stage2: None,
            drop_reason: None,
        });
    };

    let changed_lines = crate::diff::changed_line_numbers(hunks);
    let Some(&line) = changed_lines.first() else {
        return Ok(JudgeOutcome {
            card: None,
            stage2: None,
            drop_reason: None,
        });
    };

    let enclosing_text = crate::site::enclosing_item_text(current_content, line)
        .unwrap_or_else(|| current_content.to_string());

    let stage2_prompt = build_stage2_prompt(&candidate.slugs, &enclosing_text, canon);
    let stage2_raw = dispatch_stage2(&stage2_prompt)?;

    let raw = match crate::judge::parse_stage2_output(&stage2_raw) {
        Ok(r) => r,
        Err(reason) => {
            return Ok(JudgeOutcome {
                card: None,
                stage2: None,
                drop_reason: Some(reason),
            });
        }
    };

    let stage2_card = match crate::judge::validate_stage2_output(&raw, taxonomy, current_content) {
        Ok(c) => c,
        Err(reason) => {
            return Ok(JudgeOutcome {
                card: None,
                stage2: None,
                drop_reason: Some(reason),
            });
        }
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
    };

    Ok(JudgeOutcome {
        card: Some(card),
        stage2: Some(stage2_card),
        drop_reason: None,
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
            |_prompt| Ok(stage1_fixture.clone()),
            |_prompt| Ok(stage2_fixture.clone()),
        )
        .unwrap();

        assert!(outcome.drop_reason.is_none());
        let card = outcome.card.expect("expected a card");
        assert_eq!(card.concept_name, "Borrow vs. clone");
        assert!(new.contains(&card.grounding_quote));
        assert_eq!(card.file, "src/main.rs");
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
            |_| Ok("[]".to_string()),
            |_| Ok("{}".to_string()),
        )
        .unwrap();
        assert!(outcome.card.is_none());
    }
}

//! T4 reqs 12-13 / D18, C6, C12 — solicited review: `murshid review`
//! batches screen->judge over the full session diff in one pass, producing
//! a relevance-ranked digest (top 3 + "N more queued"). Architecture-
//! category advice is allowed here regardless of the detent floor (D18:
//! "this surface is its home") — the caller simply never applies
//! `noise::floor_excludes` to review candidates; there is no floor gate in
//! this module to bypass.

use crate::card::Card;

/// req 12/13: top-3 digest cap (C12).
pub const DIGEST_TOP_N: usize = 3;

/// req 13: static below-mastery concept-slug placeholder until T5 wires the
/// real BKT-derived list. The D18 parity assertion requires the review
/// prompt to carry SOME below-mastery list, not nothing — a bare "critique
/// this diff" prompt (goal-blind, mastery-blind) is the spec violation this
/// guards against.
pub const BELOW_MASTERY_PLACEHOLDER: &[&str] = &[
    "borrow-vs-clone",
    "option-combinators",
    "question-mark-propagation",
];

/// req 12: one judged review candidate, pre-digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewFinding {
    pub concept_id: String,
    pub category: String,
    pub card: Card,
}

use crate::queue::category_rank;

fn goal_relevance_rank(
    goal_cluster_dirs: &std::collections::HashSet<String>,
    goal_text: &str,
    finding: &ReviewFinding,
) -> u8 {
    if crate::goal::is_goal_relevant(
        goal_cluster_dirs,
        goal_text,
        &finding.card.file,
        &finding.card.concept_name,
    ) {
        0
    } else {
        1
    }
}

/// req 12: relevance-ranked digest of top 3 cards + a count of the rest —
/// "memory-aware ranking teaches the next concept, not random nits" (D18).
/// Memory-awareness proper is T5; this task ranks by goal-relevance then
/// category (same shape as the pull queue's C7 ordering, minus the
/// throttle/age terms that don't apply to a one-shot batched pass).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewDigest {
    pub top: Vec<Card>,
    pub more_queued: usize,
}

pub fn rank_and_digest(
    mut findings: Vec<ReviewFinding>,
    goal_cluster_dirs: &std::collections::HashSet<String>,
    goal_text: &str,
) -> ReviewDigest {
    findings.sort_by(|a, b| {
        goal_relevance_rank(goal_cluster_dirs, goal_text, a)
            .cmp(&goal_relevance_rank(goal_cluster_dirs, goal_text, b))
            .then(category_rank(&a.category).cmp(&category_rank(&b.category)))
    });
    let more_queued = findings.len().saturating_sub(DIGEST_TOP_N);
    let top = findings.into_iter().take(DIGEST_TOP_N).map(|f| f.card).collect();
    ReviewDigest { top, more_queued }
}

/// req 12: builds the review's stage-2 judge prompt — reuses the normal
/// card output contract (`judge::validate_stage2_output`) but MUST carry
/// the goal text and the below-mastery concept list (D18 parity assertion):
/// a prompt indistinguishable from a free "critique my diff" persona fails
/// the ruling.
pub fn build_review_prompt(
    goal_text: &str,
    below_mastery_concepts: &[&str],
    candidate_slugs: &[String],
    enclosing_item_text: &str,
    canon: &[crate::pack::CanonEntry],
) -> String {
    let mut s = String::new();
    s.push_str("Solicited review (`murshid review`, D18): critique this diff as ranked\n");
    s.push_str("teaching moments, not a generic \"critique my diff\" pass. Architecture-\n");
    s.push_str("grade advice is welcome here regardless of the usual push floor.\n\n");
    s.push_str(&format!(
        "Session goal: {}\n",
        if goal_text.trim().is_empty() {
            "(none set)"
        } else {
            goal_text
        }
    ));
    s.push_str("Below-mastery concepts to prioritize teaching next: ");
    s.push_str(&below_mastery_concepts.join(", "));
    s.push_str("\n\n");
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

/// req 12: the one-line offer surfaced at commit detection and in the
/// bookend (never auto-runs).
pub const REVIEW_OFFER_LINE: &str =
    "how would you have done this better? \u{2014} murshid review";

/// Resolves which changed line a stage-1 candidate's `site_hint` most
/// likely refers to (a candidate's hint is typically the enclosing item's
/// own header, e.g. `"fn print_name"` — see the `stage1_response.json`
/// fixture). Delegates to the shared [`crate::diff::resolve_site_hint_line`]
/// (T5 reuses it for application-detection resolution).
fn resolve_candidate_line(hunks: &[crate::diff::Hunk], site_hint: &str, fallback: usize) -> usize {
    crate::diff::resolve_site_hint_line(hunks, site_hint, fallback)
}

/// req 12: judges one changed file's hunks for `murshid review` — the
/// batched screen->judge pass. Unlike the normal pipeline (which stops at
/// the FIRST stage-1 candidate, one card per file), review surfaces EVERY
/// candidate stage-1 finds so the digest has real material to rank across
/// the whole diff. `dispatch_stage1`/`dispatch_stage2` are injected so this
/// never makes a live call in tests, mirroring `pipeline::judge_hunks`.
///
/// Fix (review): the enclosing-item anchor (line + text) is resolved PER
/// CANDIDATE via `resolve_candidate_line`, not once from the file's first
/// changed line and reused for every candidate — a multi-candidate file
/// used to give every candidate after the first the WRONG anchor.
#[allow(clippy::too_many_arguments)]
pub fn judge_review_hunks(
    rel_file: &str,
    hunks: &[crate::diff::Hunk],
    current_content: &str,
    taxonomy: &[crate::pack::TaxonomyConcept],
    canon: &[crate::pack::CanonEntry],
    grammar: &crate::pack::GrammarSpec,
    stage1_framing: &str,
    goal_text: &str,
    below_mastery_concepts: &[&str],
    dispatch_stage1: impl Fn(&str) -> Result<String, String>,
    dispatch_stage2: impl Fn(&str) -> Result<String, String>,
) -> Vec<ReviewFinding> {
    if hunks.is_empty() {
        return Vec::new();
    }

    let stage1_prompt = crate::pipeline::build_stage1_prompt(stage1_framing, rel_file, hunks, taxonomy);
    let Ok(stage1_raw) = dispatch_stage1(&stage1_prompt) else {
        return Vec::new();
    };
    let Ok(candidates) = crate::judge::parse_stage1_output(&stage1_raw) else {
        return Vec::new();
    };

    let changed_lines = crate::diff::changed_line_numbers(hunks);
    let Some(&fallback_line) = changed_lines.first() else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for candidate in candidates {
        let line = resolve_candidate_line(hunks, &candidate.site_hint, fallback_line);
        let enclosing_text = crate::site::enclosing_item_text(current_content, line, grammar)
            .unwrap_or_else(|| current_content.to_string());

        let stage2_prompt = build_review_prompt(
            goal_text,
            below_mastery_concepts,
            &candidate.slugs,
            &enclosing_text,
            canon,
        );
        let Ok(stage2_raw) = dispatch_stage2(&stage2_prompt) else {
            continue;
        };
        let Ok(raw) = crate::judge::parse_stage2_output(&stage2_raw) else {
            continue;
        };
        let Ok(stage2_card) = crate::judge::validate_stage2_output(&raw, taxonomy, current_content)
        else {
            continue;
        };

        let canon_entry = crate::pack::find_canon_for_concept(canon, &stage2_card.concept);
        let doc_ref = canon_entry.and_then(|e| e.refs.first().cloned()).unwrap_or_default();
        let concept_name = taxonomy
            .iter()
            .find(|c| c.slug == stage2_card.concept)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| stage2_card.concept.clone());

        let card = Card {
            concept_name,
            file: rel_file.to_string(),
            line,
            grounding_quote: stage2_card.grounding_quote.clone(),
            why: stage2_card.why.clone(),
            rule: stage2_card.rule.clone(),
            doc_ref,
            worked_diff: stage2_card.worked_diff.clone(),
            additional_anchors: Vec::new(),
            overflow_site_count: 0,
        };

        out.push(ReviewFinding {
            concept_id: stage2_card.concept.clone(),
            category: stage2_card.category.clone(),
            card,
        });
    }
    out
}

/// req 12: renders the digest — top cards (I21 minimal rendering) plus the
/// "N more queued" trailer.
pub fn render_review_digest(digest: &ReviewDigest) -> String {
    let mut out = String::new();
    if digest.top.is_empty() {
        out.push_str("(nothing to review \u{2014} diff is clean)\n");
        return out;
    }
    for card in &digest.top {
        out.push_str(&crate::card::render_card(card, 0));
        out.push('\n');
    }
    if digest.more_queued > 0 {
        out.push_str(&format!("{} more queued\n", digest.more_queued));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(name: &str, file: &str) -> Card {
        Card {
            concept_name: name.to_string(),
            file: file.to_string(),
            line: 1,
            grounding_quote: "q".to_string(),
            why: "why".to_string(),
            rule: "rule".to_string(),
            doc_ref: "ref".to_string(),
            worked_diff: "diff".to_string(),
            additional_anchors: Vec::new(),
            overflow_site_count: 0,
        }
    }

    fn finding(concept: &str, category: &str, file: &str) -> ReviewFinding {
        ReviewFinding {
            concept_id: concept.to_string(),
            category: category.to_string(),
            card: card(concept, file),
        }
    }

    fn no_goal() -> std::collections::HashSet<String> {
        std::collections::HashSet::new()
    }

    // --- req 12/13: ranking + digest cap ---

    #[test]
    fn test_rank_and_digest_orders_by_category_rank() {
        let findings = vec![
            finding("arch1", "architecture", "a.rs"),
            finding("bug1", "bug", "b.rs"),
            finding("idiom1", "idiom", "c.rs"),
        ];
        let digest = rank_and_digest(findings, &no_goal(), "");
        let names: Vec<&str> = digest.top.iter().map(|c| c.concept_name.as_str()).collect();
        assert_eq!(names, vec!["bug1", "idiom1", "arch1"]);
    }

    #[test]
    fn test_rank_and_digest_caps_at_three_and_counts_rest() {
        let findings = vec![
            finding("a", "bug", "a.rs"),
            finding("b", "bug", "b.rs"),
            finding("c", "idiom", "c.rs"),
            finding("d", "architecture", "d.rs"),
            finding("e", "architecture", "e.rs"),
        ];
        let digest = rank_and_digest(findings, &no_goal(), "");
        assert_eq!(digest.top.len(), 3);
        assert_eq!(digest.more_queued, 2);
    }

    #[test]
    fn test_rank_and_digest_goal_relevance_precedes_category() {
        let mut arch = finding("arch_goal", "architecture", "src/auth/login.rs");
        arch.card.file = "src/auth/login.rs".to_string();
        let bug = finding("unrelated_bug", "bug", "b.rs");

        let mut cluster = std::collections::HashSet::new();
        cluster.insert("src/auth".to_string());

        let digest = rank_and_digest(vec![bug, arch], &cluster, "");
        assert_eq!(digest.top[0].concept_name, "arch_goal");
    }

    #[test]
    fn test_rank_and_digest_empty_is_empty() {
        let digest = rank_and_digest(vec![], &no_goal(), "");
        assert!(digest.top.is_empty());
        assert_eq!(digest.more_queued, 0);
    }

    // --- req 13: D18 parity assertion — prompt carries goal + below-mastery ---

    #[test]
    fn test_build_review_prompt_contains_goal_text() {
        let prompt = build_review_prompt(
            "fix auth timeout",
            BELOW_MASTERY_PLACEHOLDER,
            &["borrow-vs-clone".to_string()],
            "fn foo() {}",
            &[],
        );
        assert!(prompt.contains("fix auth timeout"));
    }

    #[test]
    fn test_build_review_prompt_contains_below_mastery_placeholder() {
        let prompt = build_review_prompt(
            "",
            BELOW_MASTERY_PLACEHOLDER,
            &[],
            "fn foo() {}",
            &[],
        );
        for concept in BELOW_MASTERY_PLACEHOLDER {
            assert!(
                prompt.contains(concept),
                "missing below-mastery concept {} \u{2014} bare 'critique this diff' prompt is a D18 parity failure",
                concept
            );
        }
    }

    #[test]
    fn test_build_review_prompt_no_goal_states_none_set_not_silently_blank() {
        let prompt = build_review_prompt("", BELOW_MASTERY_PLACEHOLDER, &[], "fn a(){}", &[]);
        assert!(prompt.contains("(none set)"));
    }

    // --- rendering ---

    #[test]
    fn test_render_review_digest_shows_more_queued_trailer() {
        let digest = ReviewDigest {
            top: vec![card("a", "a.rs")],
            more_queued: 4,
        };
        let rendered = render_review_digest(&digest);
        assert!(rendered.contains("4 more queued"));
    }

    #[test]
    fn test_render_review_digest_empty_says_nothing_to_review() {
        let digest = ReviewDigest { top: vec![], more_queued: 0 };
        let rendered = render_review_digest(&digest);
        assert!(rendered.contains("nothing to review"));
    }

    // --- req 12: judge_review_hunks (batched screen->judge over fixtures) ---

    fn taxonomy() -> Vec<crate::pack::TaxonomyConcept> {
        // T9 req 9 addendum: see pipeline.rs::tests::taxonomy() for why this
        // takes the crate-wide env lock.
        let _lock = crate::credentials::env_test_lock();
        crate::pack::load_taxonomy(&crate::pack::default_pack_dir()).unwrap()
    }

    fn canon() -> Vec<crate::pack::CanonEntry> {
        let _lock = crate::credentials::env_test_lock();
        crate::pack::load_canon(&crate::pack::default_pack_dir()).unwrap()
    }

    fn grammar() -> crate::pack::GrammarSpec {
        crate::pack::GrammarSpec::default()
    }

    fn fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        std::fs::read_to_string(&path).unwrap()
    }

    #[test]
    fn test_judge_review_hunks_produces_a_finding_from_fixtures() {
        let old = "fn print_name(name: String) { println!(\"{}\", name); }\n";
        let new = "fn print_name(name: String) { println!(\"{}\", name); }\nprint_name(person.name.clone());\n";
        let hunks = crate::diff::diff_lines(old, new);

        let stage1_fixture = fixture("stage1_response.json");
        let stage2_fixture = fixture("stage2_response_valid.json");

        let findings = judge_review_hunks(
            "src/main.rs",
            &hunks,
            new,
            &taxonomy(),
            &canon(),
            &grammar(),
            "",
            "fix auth timeout",
            BELOW_MASTERY_PLACEHOLDER,
            |_prompt| Ok(stage1_fixture.clone()),
            |_prompt| Ok(stage2_fixture.clone()),
        );

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].concept_id, "borrow-vs-clone");
        assert_eq!(findings[0].card.file, "src/main.rs");
    }

    /// Fix (review anchors): a multi-candidate file must resolve EACH
    /// candidate's own anchor line, not reuse the first candidate's line
    /// for every subsequent one.
    #[test]
    fn test_judge_review_hunks_resolves_each_candidates_own_anchor() {
        let old = "fn print_name(name: String) { println!(\"{}\", name); }\n\nfn other(v: Option<i32>) {\n    let y = 1;\n}\n";
        let new = "fn print_name(name: String) { println!(\"{}\", name); }\nprint_name(person.name.clone());\n\nfn other(v: Option<i32>) {\n    let y = 1;\n    let x = v.unwrap();\n}\n";
        let hunks = crate::diff::diff_lines(old, new);

        // Two stage-1 candidates whose site_hints point at two DIFFERENT
        // changed lines.
        let stage1_fixture = r#"[
            {"site_hint": "person.name.clone()", "slugs": ["borrow-vs-clone"]},
            {"site_hint": "v.unwrap()", "slugs": ["option-combinators"]}
        ]"#
        .to_string();

        let stage2_borrow = fixture("stage2_response_valid.json");
        let stage2_option = r#"{
            "concept": "option-combinators",
            "grounding_quote": "v.unwrap()",
            "why": "unwrap panics on None.",
            "rule": "Use map/unwrap_or instead of unwrap.",
            "worked_diff": "- v.unwrap()\n+ v.unwrap_or(0)",
            "category": "bug",
            "likely_bug": true
        }"#
        .to_string();

        let findings = judge_review_hunks(
            "src/main.rs",
            &hunks,
            new,
            &taxonomy(),
            &canon(),
            &grammar(),
            "",
            "",
            BELOW_MASTERY_PLACEHOLDER,
            |_prompt| Ok(stage1_fixture.clone()),
            |prompt: &str| {
                // Route on the CANDIDATE-SPECIFIC canon-entry line, not the
                // static below-mastery placeholder (which always lists
                // "option-combinators" regardless of which candidate this
                // prompt is for — a naive `contains("option-combinators")`
                // check would match both candidates' prompts).
                if prompt.contains("- option-combinators ::") {
                    Ok(stage2_option.clone())
                } else {
                    Ok(stage2_borrow.clone())
                }
            },
        );

        assert_eq!(findings.len(), 2);
        let borrow = findings
            .iter()
            .find(|f| f.concept_id == "borrow-vs-clone")
            .expect("borrow-vs-clone finding");
        let option = findings
            .iter()
            .find(|f| f.concept_id == "option-combinators")
            .expect("option-combinators finding");
        assert_ne!(
            borrow.card.line, option.card.line,
            "secondary candidate must get its own anchor line, not the first candidate's"
        );
        // The option-combinators candidate's own grounding quote must be
        // verifiable on ITS resolved line's content, not smuggled in via
        // the wrong candidate's anchor.
        assert_eq!(option.card.grounding_quote, "v.unwrap()");
    }

    #[test]
    fn test_judge_review_hunks_empty_when_no_hunks() {
        let findings = judge_review_hunks(
            "src/main.rs",
            &[],
            "fn a() {}\n",
            &taxonomy(),
            &canon(),
            &grammar(),
            "",
            "",
            BELOW_MASTERY_PLACEHOLDER,
            |_| Ok("[]".to_string()),
            |_| Ok("{}".to_string()),
        );
        assert!(findings.is_empty());
    }
}

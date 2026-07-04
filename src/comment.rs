//! T4 reqs 9-11 / D17, C2 — murshid-addressed comments: the highest-
//! precision "ask anything, in-flow" channel. A comment matching the pack's
//! address token (`comment_token` + `address_token`, e.g. `// murshid:`) is
//! a DIRECT ask: it skips the offer stage AND the screen stage entirely and
//! is answered as a normal card at the next quiescence moment.

/// req 9: strips the address token (case-insensitive) from an already
/// comment-token-stripped body, returning the question text if it matches.
pub fn strip_address_token<'a>(body: &'a str, address_token: &str) -> Option<&'a str> {
    let trimmed = body.trim_start();
    let lower = trimmed.to_lowercase();
    if lower.starts_with(&address_token.to_lowercase()) {
        Some(trimmed[address_token.len()..].trim())
    } else {
        None
    }
}

/// req 9: scans a diff's newly-ADDED lines for fresh murshid-addressed
/// comments (I1: advice/asks attach only to added lines), returning
/// `(line, question text)` for each — the line is needed to compute the
/// enclosing item's site (req 10's `(comment_text_hash, site)` advice-fp).
/// Reuses `struggle::strip_comment_token`'s pattern: the generic comment-
/// token strip runs first (most-specific-first per repo convention — the
/// address-token check only fires on lines that are already recognized as
/// comments), so a `// murshid: ...` line is never confused with a plain
/// `//` comment mentioning the word "murshid".
pub fn find_fresh_murshid_comments(
    hunks: &[crate::diff::Hunk],
    comment_token: &str,
    address_token: &str,
) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for hunk in hunks {
        for op in &hunk.ops {
            if let crate::diff::DiffOp::Added { new_line, text } = op {
                let trimmed = text.trim_start();
                if trimmed.starts_with(comment_token) {
                    let body = crate::struggle::strip_comment_token(trimmed, comment_token);
                    if let Some(question) = strip_address_token(body, address_token) {
                        if !question.is_empty() {
                            out.push((*new_line, question.to_string()));
                        }
                    }
                }
            }
        }
    }
    out
}

/// req 10: `advice_fp = (comment_text_hash, site)` — reuses
/// `site::advice_fingerprint`'s `(concept, site)` shape with the comment's
/// own text hash standing in for the concept, per C2's stated D17 semantics.
pub fn comment_advice_fingerprint(comment_text: &str, site: &crate::site::Site) -> String {
    let comment_hash = crate::sha256::sha256_hex(comment_text.as_bytes());
    crate::site::advice_fingerprint(&comment_hash, site)
}

/// req 9: builds the direct-ask judge prompt — skips stage 1 (screen)
/// entirely and asks the judge to answer the question about the enclosing
/// item, reusing the same output contract as a normal card
/// (`judge::validate_stage2_output`) so it renders through the normal card
/// pipeline. Sanitized per the non-negotiable mandate.
pub fn build_comment_ask_prompt(
    question: &str,
    enclosing_item_text: &str,
    taxonomy: &[crate::pack::TaxonomyConcept],
) -> String {
    let mut s = String::new();
    s.push_str("A developer left a comment addressed directly to you (the mentor) in their\n");
    s.push_str("code, asking a question. Answer it as a normal card: name the concept it's\n");
    s.push_str("really about (forced choice against the taxonomy below), a verdict+why, the\n");
    s.push_str("transferable rule, and a worked diff if a concrete fix applies.\n\n");
    s.push_str(&format!("Question: {}\n\n", question));
    s.push_str("Enclosing item:\n");
    s.push_str(enclosing_item_text);
    s.push_str("\n\nTaxonomy slugs: ");
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

/// req 10: the card's note that the answered comment can be deleted (the
/// watcher never edits user code itself).
pub const DELETE_COMMENT_NOTE: &str =
    "(you can delete that comment now \u{2014} it's been answered)";

#[cfg(test)]
mod tests {
    use super::*;

    // --- req 9: address-token detection ---

    #[test]
    fn test_strip_address_token_matches_case_insensitive() {
        assert_eq!(
            strip_address_token("murshid: how do I avoid this clone?", "murshid:"),
            Some("how do I avoid this clone?")
        );
        assert_eq!(
            strip_address_token("MURSHID: why?", "murshid:"),
            Some("why?")
        );
    }

    #[test]
    fn test_strip_address_token_no_match_is_none() {
        assert_eq!(strip_address_token("just a comment", "murshid:"), None);
    }

    #[test]
    fn test_find_fresh_murshid_comments_default_rust_pack_token() {
        let old = "fn a() {}\n";
        let new = "fn a() {\n    // murshid: how do I avoid this clone?\n}\n";
        let hunks = crate::diff::diff_lines(old, new);
        let found = find_fresh_murshid_comments(&hunks, "//", "murshid:");
        assert_eq!(found, vec![(2, "how do I avoid this clone?".to_string())]);
    }

    /// Acceptance: a synthetic non-`//` pack token still works (pack-
    /// agnosticism, D17's "comment syntax per language pack").
    #[test]
    fn test_find_fresh_murshid_comments_synthetic_non_slash_slash_pack() {
        let old = "func a() {}\n";
        let new = "func a() {\n    # mentor: why does this need a lock?\n}\n";
        let hunks = crate::diff::diff_lines(old, new);
        let found = find_fresh_murshid_comments(&hunks, "#", "mentor:");
        assert_eq!(found, vec![(2, "why does this need a lock?".to_string())]);
    }

    #[test]
    fn test_find_fresh_murshid_comments_only_scans_added_lines() {
        let old = "fn a() {\n    // murshid: pre-existing\n}\n";
        let new = "fn a() {\n    // murshid: pre-existing\n    let x = 1;\n}\n";
        let hunks = crate::diff::diff_lines(old, new);
        let found = find_fresh_murshid_comments(&hunks, "//", "murshid:");
        assert!(found.is_empty());
    }

    /// Ordering: a plain comment that merely mentions the address word
    /// without the comment-token-then-address-token shape must not match —
    /// the comment-token strip runs first, most-specific-first.
    #[test]
    fn test_plain_comment_mentioning_murshid_is_not_a_direct_ask() {
        let old = "fn a() {}\n";
        let new = "fn a() {\n    // murshid helped me understand this\n}\n";
        let hunks = crate::diff::diff_lines(old, new);
        let found = find_fresh_murshid_comments(&hunks, "//", "murshid:");
        assert!(found.is_empty());
    }

    // --- req 10: advice fingerprint identity ---

    #[test]
    fn test_comment_advice_fingerprint_stable_for_same_text_and_site() {
        let src = "fn foo() {\n    let x = y.clone();\n}\n";
        let site =
            crate::site::compute_site("src/lib.rs", src, 2, &crate::pack::GrammarSpec::default())
                .unwrap();
        let fp1 = comment_advice_fingerprint("why does this need a clone?", &site);
        let fp2 = comment_advice_fingerprint("why does this need a clone?", &site);
        assert_eq!(fp1, fp2);
    }

    #[test]
    fn test_comment_advice_fingerprint_differs_for_different_text() {
        let src = "fn foo() {\n    let x = y.clone();\n}\n";
        let site =
            crate::site::compute_site("src/lib.rs", src, 2, &crate::pack::GrammarSpec::default())
                .unwrap();
        let fp1 = comment_advice_fingerprint("why does this need a clone?", &site);
        let fp2 = comment_advice_fingerprint("what about &str?", &site);
        assert_ne!(fp1, fp2);
    }

    // --- req 9: comment-ask prompt build ---

    #[test]
    fn test_build_comment_ask_prompt_contains_question_and_taxonomy() {
        let taxonomy = vec![crate::pack::TaxonomyConcept {
            slug: "borrow-vs-clone".to_string(),
            name: "Borrow vs. clone".to_string(),
            category: crate::pack::Category::Idiom,
        }];
        let prompt = build_comment_ask_prompt(
            "how do I avoid this clone?",
            "fn foo() { y.clone() }",
            &taxonomy,
        );
        assert!(prompt.contains("how do I avoid this clone?"));
        assert!(prompt.contains("borrow-vs-clone"));
        assert!(prompt.contains("fn foo()"));
    }
}

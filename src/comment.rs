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
    // Match the bare address WORD (the configured token conventionally ends
    // in ':', e.g. "murshid:", but that punctuation is inessential) followed
    // by ANY natural separator — ':', ',', '-', or whitespace — or end of
    // line. Dogfood 2026-07-05: a developer shouldn't have to remember exact
    // punctuation; "// murshid, how do I..." / "// murshid how..." now work
    // like "// murshid:". The word must still be at the START and be a whole
    // word (separator-or-end after it), so "murshiddocs" never false-matches.
    let word = address_token.trim_end_matches([':', ',', '-']).trim();
    let word_lower = word.to_lowercase();
    if word_lower.is_empty() || !lower.starts_with(&word_lower) {
        return None;
    }
    let rest = &trimmed[word.len()..];
    // A punctuation separator right after the address word (":", ",", "-")
    // marks a direct ask — the low-friction forms "murshid: ..." /
    // "murshid, ..." / "murshid - ...". A bare space does NOT on its own (so
    // a plain mention like "murshid helped me understand this" is not an
    // ask) — UNLESS the comment is phrased as a question (ends with "?"),
    // which is the other unmistakable "I'm asking you" signal.
    let after_punct = rest.trim_start().starts_with([':', ',', '-']);
    let space_then_question =
        rest.starts_with(|c: char| c.is_whitespace()) && trimmed.trim_end().ends_with('?');
    if !after_punct && !space_then_question {
        return None;
    }
    let question = rest.trim_start().trim_start_matches([':', ',', '-']).trim();
    if question.is_empty() {
        None
    } else {
        Some(question)
    }
}

/// req 9 (founder 2026-07-05 — broadened): scans the CURRENT file `content`
/// (every line, 1-based) for murshid-addressed comments, returning
/// `(line, question text)` for each — the line feeds the enclosing-item site
/// (req 10's `(comment_text_hash, site)` advice-fp). The direct-ask channel
/// (D17) answers a question WHEREVER it lives, not only on lines added this
/// session: a dev commonly EDITS or reuses an existing comment to ask a
/// question or a follow-up, and the added-lines-only gate silently swallowed
/// those. Answer-once and follow-up-on-edit are handled downstream by the
/// `(question, site)` advice-fingerprint dedup in `run_comment_asks` — an
/// unchanged comment matches an already-answered card and is skipped, while an
/// edited comment's changed text yields a new fingerprint and re-fires. (The
/// UNSOLICITED help-comment signal stays added-lines-only via
/// `struggle::find_fresh_help_comments`.) Reuses `strip_comment_token`'s
/// most-specific-first pattern so `// murshid: ...` is never confused with a
/// plain `//` comment merely mentioning the word "murshid".
pub fn find_murshid_comments(
    content: &str,
    comment_token: &str,
    address_token: &str,
) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with(comment_token) {
            let body = crate::struggle::strip_comment_token(trimmed, comment_token);
            if let Some(question) = strip_address_token(body, address_token) {
                if !question.is_empty() {
                    out.push((idx + 1, question.to_string())); // 1-based line
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
    // Dogfood 2026-07-06: this MUST demand the same strict JSON contract as
    // packs/*/prompts/stage2.md. The old prose phrasing ("answer it as a normal
    // card: name the concept, a verdict+why, …") made the model reply in
    // markdown (### Concept / ### Verdict / ```diff), which `parse_stage2_output`
    // + `validate_stage2_output` can't turn into a card → silent parse_error drop
    // (the "comment-ask never works" bug, caught via the comment-ask trace).
    s.push_str("A developer left a comment addressed directly to you (the mentor) in\n");
    s.push_str("their code, asking the question below. Answer it by judging the enclosing\n");
    s.push_str("item. Respond with a SINGLE JSON object and NOTHING else \u{2014} no markdown,\n");
    s.push_str("no code fences, no prose \u{2014} with exactly these fields: `concept` (must be\n");
    s.push_str("one of the taxonomy slugs below), `grounding_quote` (a string that appears\n");
    s.push_str("VERBATIM in the enclosing item), `why` (at most 3 sentences answering the\n");
    s.push_str("question), `rule` (one line), `worked_diff` (a commented rewrite),\n");
    s.push_str("`category` (bug|idiom|best-practice|architecture), and `likely_bug`\n");
    s.push_str("(true|false). If you cannot ground an answer, respond with `{}` and nothing\n");
    s.push_str("else.\n\n");
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
    fn test_strip_address_token_lenient_separators() {
        // Dogfood 2026-07-05: comma, space, dash, or a bare word all address
        // murshid — no exact-punctuation memory required.
        assert_eq!(
            strip_address_token("murshid, not sure how to fix this", "murshid:"),
            Some("not sure how to fix this")
        );
        // bare space + trailing "?" (a question) also addresses murshid
        assert_eq!(
            strip_address_token("Murshid how do I avoid the clone?", "murshid:"),
            Some("how do I avoid the clone?")
        );
        assert_eq!(
            strip_address_token("murshid - what's idiomatic here?", "murshid:"),
            Some("what's idiomatic here?")
        );
    }

    #[test]
    fn test_strip_address_token_bare_space_statement_is_not_an_ask() {
        // "murshid <words>" with no punctuation and no "?" is a plain mention,
        // not a direct ask (else "// murshid helped me..." would false-fire).
        assert_eq!(
            strip_address_token("murshid helped me understand this", "murshid:"),
            None
        );
    }

    #[test]
    fn test_strip_address_token_requires_whole_word() {
        // A comment merely starting with "murshid"-ish text but not addressing
        // it (no separator) must NOT match.
        assert_eq!(strip_address_token("murshiddocs are great", "murshid:"), None);
    }

    #[test]
    fn test_find_murshid_comments_default_rust_pack_token() {
        let content = "fn a() {\n    // murshid: how do I avoid this clone?\n}\n";
        let found = find_murshid_comments(content, "//", "murshid:");
        assert_eq!(found, vec![(2, "how do I avoid this clone?".to_string())]);
    }

    /// Acceptance: a synthetic non-`//` pack token still works (pack-
    /// agnosticism, D17's "comment syntax per language pack").
    #[test]
    fn test_find_murshid_comments_synthetic_non_slash_slash_pack() {
        let content = "func a() {\n    # mentor: why does this need a lock?\n}\n";
        let found = find_murshid_comments(content, "#", "mentor:");
        assert_eq!(found, vec![(2, "why does this need a lock?".to_string())]);
    }

    #[test]
    fn test_find_murshid_comments_finds_pre_existing_and_edited_lines() {
        // Founder 2026-07-05: the direct-ask channel scans the WHOLE file, so
        // a comment that was NOT added this session (a reused/edited comment or
        // a follow-up question) is still found — the inverse of the old
        // added-lines-only behavior. (Answer-once dedup lives in run_comment_asks.)
        let content = "fn a() {\n    // murshid: a follow-up question?\n    let x = 1;\n}\n";
        let found = find_murshid_comments(content, "//", "murshid:");
        assert_eq!(found, vec![(2, "a follow-up question?".to_string())]);
    }

    /// Ordering: a plain comment that merely mentions the address word
    /// without the comment-token-then-address-token shape must not match —
    /// the comment-token strip runs first, most-specific-first.
    #[test]
    fn test_plain_comment_mentioning_murshid_is_not_a_direct_ask() {
        let content = "fn a() {\n    // murshid helped me understand this\n}\n";
        let found = find_murshid_comments(content, "//", "murshid:");
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
        // Dogfood 2026-07-06: must demand the JSON contract validate_stage2_output
        // parses — else the model replies in markdown prose and the ask is dropped.
        assert!(prompt.contains("SINGLE JSON object"), "must request JSON, not prose");
        assert!(prompt.contains("grounding_quote") && prompt.contains("likely_bug"));
        assert!(prompt.contains("`{}`"), "must give the no-answer escape hatch");
    }
}

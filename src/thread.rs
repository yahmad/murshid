//! T4 reqs 5-8 / D20, C5, C6, C12 — card-anchored follow-up threads: an
//! anchor-scoped-by-construction prompt (card anchor, concept, canon entry,
//! and thread history, nothing else), the 5-user-turn cap, and rung-
//! respecting answer rendering (a follow-up is not an automatic bottom-out).

use crate::ladder::Rung;

/// C12: 5 user turns per card.
pub const THREAD_TURN_CAP: u32 = 5;

/// req 5: the cap-reached notice — the card suggests parking it.
pub const THREAD_CAP_NOTICE: &str =
    "thread cap \u{2014} anything more belongs in your own exploration";

/// req 5: whether the next user turn would exceed the cap.
pub fn thread_cap_reached(user_turns_so_far: u32) -> bool {
    user_turns_so_far >= THREAD_TURN_CAP
}

/// One prior turn in the thread transcript, for prompt assembly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadTurn {
    pub role: String, // "user" | "assistant"
    pub content: String,
}

/// req 5 / anchor-scoped-by-construction: everything the judge sees for a
/// thread turn — the card's own anchor (file:line + grounding quote), its
/// concept, the matching canon entry, and the thread history so far. There
/// is no parameter through which any other file or session-wide context
/// could enter this prompt — the acceptance test asserts exactly that by
/// construction (build a payload from data naming other files, and observe
/// the payload never mentions them).
#[allow(clippy::too_many_arguments)]
pub fn build_thread_prompt(
    file: &str,
    line: usize,
    grounding_quote: &str,
    concept_name: &str,
    canon_entry: Option<&crate::pack::CanonEntry>,
    history: &[ThreadTurn],
    question: &str,
) -> String {
    let mut s = String::new();
    s.push_str("Answer this follow-up question about one flagged card. Stay scoped to this\n");
    s.push_str("card's anchor and concept only.\n\n");
    s.push_str(&format!("Anchor: {}:{}\n", file, line));
    s.push_str(&format!("Quote: {}\n", grounding_quote));
    s.push_str(&format!("Concept: {}\n", concept_name));
    if let Some(entry) = canon_entry {
        s.push_str(&format!(
            "Canon: {} :: {}\n  why: {}\n  use instead: {}\n",
            entry.concept, entry.what_it_does, entry.why_is_this_bad, entry.use_instead
        ));
    }
    if !history.is_empty() {
        s.push_str("\nThread so far:\n");
        for turn in history {
            s.push_str(&format!("{}: {}\n", turn.role, turn.content));
        }
    }
    s.push_str(&format!("\nQuestion: {}\n", question));
    crate::sanitizer::sanitize_diagnostics(&s)
}

/// Redesign R5 (the "ask mode" finale): builds the free-form, conversational
/// follow-up prompt for a card's `k` thread — DELIBERATELY separate from
/// [`build_thread_prompt`]/[`ThreadAnswer`] above (that pair's strict
/// `{answer, reveals_fix}` JSON contract was never wired to a live call
/// site). Ask mode's answer is PROSE, persisted verbatim — no JSON, no
/// grounding contract, no rung-gated reveal — so this asks the model to just
/// answer plainly. Context is exactly what the card already carries: the
/// concept, its why/rule, the grounding quote, the enclosing item (when
/// known), the thread so far, and the new question — nothing else (anchor-
/// scoped by construction, same posture as `build_thread_prompt`).
pub fn build_ask_prompt(
    concept_name: &str,
    why: &str,
    rule: &str,
    grounding_quote: &str,
    enclosing_item: Option<&str>,
    history: &[ThreadTurn],
    question: &str,
) -> String {
    let mut s = String::new();
    s.push_str("A developer is looking at a mentoring card and has a follow-up question.\n");
    s.push_str("Answer conversationally, in plain prose (no JSON, no markdown headers,\n");
    s.push_str("no code fences unless showing a short snippet is genuinely clearer) \u{2014}\n");
    s.push_str("a few sentences, staying scoped to this card's concept and code.\n\n");
    s.push_str(&format!("Concept: {}\n", concept_name));
    s.push_str(&format!("Why: {}\n", why));
    s.push_str(&format!("Rule: {}\n", rule));
    s.push_str(&format!("Quote: {}\n", grounding_quote));
    if let Some(item) = enclosing_item {
        if !item.trim().is_empty() {
            s.push_str("\nEnclosing code:\n");
            s.push_str(item);
            s.push('\n');
        }
    }
    if !history.is_empty() {
        s.push_str("\nThread so far:\n");
        for turn in history {
            s.push_str(&format!("{}: {}\n", turn.role, turn.content));
        }
    }
    s.push_str(&format!("\nQuestion: {}\n", question));
    crate::sanitizer::sanitize_diagnostics(&s)
}

/// The judge's structured thread-answer contract: the answer text, plus
/// whether answering fully would reveal the fix (req 6).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct ThreadAnswer {
    pub answer: String,
    #[serde(default)]
    pub reveals_fix: bool,
}

pub fn parse_thread_answer(raw: &str) -> Result<ThreadAnswer, String> {
    serde_json::from_str(raw).map_err(|e| format!("thread answer parse error: {}", e))
}

/// req 6: thread turns respect the card's CURRENT rung — a follow-up is not
/// an automatic bottom-out. If the answer would reveal the fix while the
/// card is still at R1 (nudge), hint instead and point at `e` (escalate)
/// rather than bottoming out uninvited.
pub fn render_thread_answer(rung: Rung, answer: &ThreadAnswer) -> String {
    if rung == Rung::R1 && answer.reveals_fix {
        "that answer would reveal the fix \u{2014} press e to see it, or ask a narrower question"
            .to_string()
    } else {
        answer.answer.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::CanonEntry;

    fn canon_entry() -> CanonEntry {
        CanonEntry {
            id: "borrow-vs-clone".to_string(),
            concept: "borrow-vs-clone".to_string(),
            what_it_does: "does a thing".to_string(),
            why_is_this_bad: "it copies".to_string(),
            example: "example".to_string(),
            use_instead: "borrow".to_string(),
            refs: vec![],
            source_rule_ids: vec![],
        }
    }

    // --- req 5: turn cap ---

    #[test]
    fn test_thread_cap_reached_at_five_not_four() {
        assert!(!thread_cap_reached(4));
        assert!(thread_cap_reached(5));
        assert!(thread_cap_reached(6));
    }

    // --- req 5: anchor-scoped-by-construction prompt ---

    #[test]
    fn test_build_thread_prompt_contains_only_this_cards_anchor() {
        let history = vec![ThreadTurn {
            role: "user".to_string(),
            content: "why does this need a clone?".to_string(),
        }];
        let prompt = build_thread_prompt(
            "src/main.rs",
            42,
            "person.name.clone()",
            "Borrow vs. clone",
            Some(&canon_entry()),
            &history,
            "what about &str instead?",
        );
        assert!(prompt.contains("src/main.rs:42"));
        assert!(prompt.contains("person.name.clone()"));
        assert!(prompt.contains("Borrow vs. clone"));
        assert!(prompt.contains("what about &str instead?"));

        // No other project files ever appear — the function has no
        // parameter through which they could. Simulate the acceptance
        // check: unrelated filenames from elsewhere in a real project must
        // never leak in, since the payload is built purely from the
        // arguments passed.
        for other_file in ["src/db.rs", "src/watcher.rs", "src/config.rs"] {
            assert!(!prompt.contains(other_file));
        }
    }

    #[test]
    fn test_build_thread_prompt_includes_history() {
        let history = vec![
            ThreadTurn {
                role: "user".to_string(),
                content: "q1".to_string(),
            },
            ThreadTurn {
                role: "assistant".to_string(),
                content: "a1".to_string(),
            },
        ];
        let prompt = build_thread_prompt("f.rs", 1, "q", "concept", None, &history, "q2");
        assert!(prompt.contains("user: q1"));
        assert!(prompt.contains("assistant: a1"));
    }

    #[test]
    fn test_build_thread_prompt_no_canon_entry_is_fine() {
        let prompt = build_thread_prompt("f.rs", 1, "q", "concept", None, &[], "question");
        assert!(!prompt.contains("Canon:"));
    }

    // --- req 6: rung-respecting answers ---

    #[test]
    fn test_render_thread_answer_r1_hints_instead_of_revealing() {
        let answer = ThreadAnswer {
            answer: "the fix is to use &str".to_string(),
            reveals_fix: true,
        };
        let rendered = render_thread_answer(Rung::R1, &answer);
        assert!(rendered.contains("press e"));
        assert!(!rendered.contains("&str"));
    }

    #[test]
    fn test_render_thread_answer_r2_and_r3_show_full_answer_even_if_revealing() {
        let answer = ThreadAnswer {
            answer: "the fix is to use &str".to_string(),
            reveals_fix: true,
        };
        assert_eq!(render_thread_answer(Rung::R2, &answer), answer.answer);
        assert_eq!(render_thread_answer(Rung::R3, &answer), answer.answer);
    }

    #[test]
    fn test_render_thread_answer_non_revealing_shows_at_any_rung() {
        let answer = ThreadAnswer {
            answer: "it's just an idiom preference".to_string(),
            reveals_fix: false,
        };
        assert_eq!(render_thread_answer(Rung::R1, &answer), answer.answer);
    }

    // --- redesign R5: build_ask_prompt (free-form, non-JSON follow-up) ---

    #[test]
    fn test_build_ask_prompt_contains_concept_why_rule_grounding() {
        let prompt = build_ask_prompt(
            "Borrow vs. clone",
            "cloning here is unnecessary",
            "prefer borrowing over cloning",
            "person.name.clone()",
            None,
            &[],
            "why does &mut fix this but & doesn't?",
        );
        assert!(prompt.contains("Borrow vs. clone"));
        assert!(prompt.contains("cloning here is unnecessary"));
        assert!(prompt.contains("prefer borrowing over cloning"));
        assert!(prompt.contains("person.name.clone()"));
        assert!(prompt.contains("why does &mut fix this but & doesn't?"));
    }

    #[test]
    fn test_build_ask_prompt_includes_enclosing_item_when_present() {
        let prompt = build_ask_prompt(
            "concept",
            "why",
            "rule",
            "quote",
            Some("fn foo() { y.clone() }"),
            &[],
            "question",
        );
        assert!(prompt.contains("fn foo() { y.clone() }"));
    }

    #[test]
    fn test_build_ask_prompt_omits_enclosing_item_section_when_absent() {
        let prompt = build_ask_prompt("concept", "why", "rule", "quote", None, &[], "question");
        assert!(!prompt.contains("Enclosing code:"));
    }

    #[test]
    fn test_build_ask_prompt_includes_prior_turns() {
        let history = vec![
            ThreadTurn {
                role: "user".to_string(),
                content: "first question".to_string(),
            },
            ThreadTurn {
                role: "assistant".to_string(),
                content: "first answer".to_string(),
            },
        ];
        let prompt = build_ask_prompt(
            "concept",
            "why",
            "rule",
            "quote",
            None,
            &history,
            "second question",
        );
        assert!(prompt.contains("user: first question"));
        assert!(prompt.contains("assistant: first answer"));
        assert!(prompt.contains("second question"));
    }

    #[test]
    fn test_build_ask_prompt_demands_prose_not_json() {
        let prompt = build_ask_prompt("concept", "why", "rule", "quote", None, &[], "question");
        assert!(prompt.contains("no JSON"));
    }

    // --- judge output contract ---

    #[test]
    fn test_parse_thread_answer_valid() {
        let raw = r#"{"answer": "because clone allocates", "reveals_fix": false}"#;
        let parsed = parse_thread_answer(raw).unwrap();
        assert_eq!(parsed.answer, "because clone allocates");
        assert!(!parsed.reveals_fix);
    }

    #[test]
    fn test_parse_thread_answer_defaults_reveals_fix_false() {
        let raw = r#"{"answer": "because clone allocates"}"#;
        let parsed = parse_thread_answer(raw).unwrap();
        assert!(!parsed.reveals_fix);
    }

    #[test]
    fn test_parse_thread_answer_invalid_json_errors() {
        assert!(parse_thread_answer("not json").is_err());
    }
}

//! T4 req 8/12 / C6 BYOK consent — `consent.solicited_spend = ask|always`:
//! `ask` (default) prompts once per session for the first thread turn, and
//! once per `murshid review` invocation, each with a rough token estimate;
//! `always` skips the prompt. D17 comment-asks are consent-EXEMPT (C6,
//! amended 2026-07-03 after T4 review): the addressed comment IS the
//! consent gesture, and the answer card carries an informational token note
//! instead of a y/N gate — see [`token_note`].

/// A very rough token estimate — good enough for a consent line, not a
/// billing figure. ~4 chars/token is the commonly-cited English average.
pub fn estimate_tokens(text: &str) -> usize {
    (text.chars().count() / 4).max(1)
}

/// req 8: the first thread turn per session prompts under `ask`; every
/// later turn in the same session skips it (the session has already
/// consented once). `always` never prompts.
pub fn should_prompt_for_thread(setting: &str, already_confirmed_this_session: bool) -> bool {
    setting == "ask" && !already_confirmed_this_session
}

/// req 12: EVERY `murshid review` invocation prompts under `ask` (a full-
/// diff judge pass is the expensive call, and each invocation's diff/cost
/// differs) — unlike the thread's once-per-session gate.
pub fn should_prompt_for_review(setting: &str) -> bool {
    setting == "ask"
}

/// The consent line rendered before a solicited spend, with its rough token
/// estimate.
pub fn consent_prompt_line(purpose: &str, estimated_tokens: usize) -> String {
    format!(
        "{} will use your configured model key (~{} tokens). Continue? [y/N]",
        purpose, estimated_tokens
    )
}

/// req 9-10 / C6 (amended): D17 comment-asks are consent-EXEMPT — no y/N
/// gate — but the answer card still carries this one-line informational
/// note (which model answered it, and a rough token cost), so the spend is
/// visible even though it was never gated.
pub fn token_note(judge_model: &str, estimated_tokens: usize) -> String {
    format!(
        "answered via {} \u{2014} ~{} tokens",
        judge_model, estimated_tokens
    )
}

/// How a keypress resolves a plain y/N consent prompt. T17 R3: relocated
/// from `offer.rs` (as `classify_offer_key`/`OfferKeyAction`) when the
/// struggle-offer's own `[y/N]` consent dialogue was deleted — this generic
/// classifier survives because `murshid review`'s D18 consent prompt
/// (`cli::review::decide_review_gate`) still needs one and never had
/// anything to do with the offer feature itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YesNoAction {
    Yes,
    No,
    /// Not a yes/no answer at all.
    Ignore,
}

pub fn classify_yes_no_key(input: &str) -> YesNoAction {
    match input.trim().to_lowercase().as_str() {
        "y" => YesNoAction::Yes,
        "n" => YesNoAction::No,
        _ => YesNoAction::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_tokens_roughly_four_chars_per_token() {
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcdefgh"), 2);
    }

    #[test]
    fn test_estimate_tokens_never_zero_for_nonempty_text() {
        assert_eq!(estimate_tokens("a"), 1);
    }

    // --- req 8: thread consent, once per session ---

    #[test]
    fn test_thread_consent_prompts_first_turn_under_ask() {
        assert!(should_prompt_for_thread("ask", false));
    }

    #[test]
    fn test_thread_consent_skips_after_first_confirmation() {
        assert!(!should_prompt_for_thread("ask", true));
    }

    #[test]
    fn test_thread_consent_always_never_prompts() {
        assert!(!should_prompt_for_thread("always", false));
        assert!(!should_prompt_for_thread("always", true));
    }

    // --- req 12: review consent, every invocation ---

    #[test]
    fn test_review_consent_prompts_every_invocation_under_ask() {
        assert!(should_prompt_for_review("ask"));
    }

    #[test]
    fn test_review_consent_always_skips() {
        assert!(!should_prompt_for_review("always"));
    }

    #[test]
    fn test_consent_prompt_line_contains_estimate() {
        let line = consent_prompt_line("murshid review", 500);
        assert!(line.contains("~500 tokens"));
        assert!(line.contains("[y/N]"));
    }

    // --- reqs 9-10 / C6 (amended): comment-ask informational token note ---

    #[test]
    fn test_token_note_names_model_and_estimate() {
        let note = token_note("claude-3-5-sonnet-20241022", 42);
        assert!(note.contains("claude-3-5-sonnet-20241022"));
        assert!(note.contains("~42 tokens"));
        assert!(note.starts_with("answered via"));
    }

    #[test]
    fn test_token_note_never_a_yn_gate() {
        // No confirmation punctuation — this is informational only.
        let note = token_note("claude-3-5-sonnet-20241022", 42);
        assert!(!note.contains("[y/N]"));
        assert!(!note.contains("Continue?"));
    }

    // --- T17 R3: the generic y/N classifier (relocated from `offer.rs`) ---

    #[test]
    fn test_classify_yes_no_key_y_and_n() {
        assert_eq!(classify_yes_no_key("y"), YesNoAction::Yes);
        assert_eq!(classify_yes_no_key("Y"), YesNoAction::Yes);
        assert_eq!(classify_yes_no_key("  y\n"), YesNoAction::Yes);
        assert_eq!(classify_yes_no_key("n"), YesNoAction::No);
        assert_eq!(classify_yes_no_key("N"), YesNoAction::No);
    }

    #[test]
    fn test_classify_yes_no_key_everything_else_is_ignored() {
        for input in ["g", "m", "u", "1", "", "yes", "no"] {
            assert_eq!(
                classify_yes_no_key(input),
                YesNoAction::Ignore,
                "expected Ignore for {:?}",
                input
            );
        }
    }
}

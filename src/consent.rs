//! T4 req 8/12 / C6 BYOK consent — `consent.solicited_spend = ask|always`:
//! `ask` (default) prompts once per session for the first thread turn, and
//! once per `murshid review` invocation, each with a rough token estimate;
//! `always` skips the prompt.

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
}

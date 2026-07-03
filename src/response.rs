//! Card response capture (T1 req 10, C3): maps the in-pane keys `g`/`u`/`n`
//! to the C3 response-verb enum. Pure logic — the stdin-reading loop that
//! calls this lives in main.rs (untested, like the rest of the watch loop's
//! process glue).

/// Maps a trimmed, case-insensitive single-key input to its C3 response
/// verb. Unknown input (including empty) maps to `None` and is ignored by
/// the caller.
///
/// T4 req 1: `a` (manual `applied`) is checked first — most-specific-first
/// per repo convention, though none of these single-char keys actually
/// overlap as substrings; the ordering just mirrors the pattern elsewhere.
pub fn response_verb_for_key(input: &str) -> Option<&'static str> {
    match input.trim().to_lowercase().as_str() {
        "a" => Some("applied"),
        "g" => Some("got_it"),
        "u" => Some("not_useful"),
        "n" => Some("not_now"),
        _ => None,
    }
}

/// T4 reqs 2-5 / C4/C5: the focused-card key vocabulary beyond the plain
/// lifecycle verbs — escalate (`e`), jump-to-worked-example (`t`), and open
/// a thread (`k`). Extends the classifier-then-fall-through pattern used by
/// `offer::classify_offer_key`: every key that isn't one of these three
/// falls through to [`response_verb_for_key`], so `a`/`g`/`u`/`n` keep
/// working unchanged and an unrecognized key never swallows anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardKeyAction {
    /// A plain C3 lifecycle verb (`applied`/`got_it`/`not_useful`/`not_now`).
    Response(&'static str),
    /// `e`: step one rung up the C4 ladder.
    Escalate,
    /// `t`: "just tell me" — jump straight to R3.
    TellMe,
    /// `k`: open an inline thread prompt (D20).
    Ask,
    /// Not a card key at all (e.g. `m`/`r`/a queue number) — the caller's
    /// other bindings handle it; the pending card is left untouched.
    Ignore,
}

pub fn classify_card_key(input: &str) -> CardKeyAction {
    match input.trim().to_lowercase().as_str() {
        "e" => CardKeyAction::Escalate,
        "t" => CardKeyAction::TellMe,
        "k" => CardKeyAction::Ask,
        _ => match response_verb_for_key(input) {
            Some(verb) => CardKeyAction::Response(verb),
            None => CardKeyAction::Ignore,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_g_maps_to_got_it() {
        assert_eq!(response_verb_for_key("g"), Some("got_it"));
    }

    #[test]
    fn test_u_maps_to_not_useful() {
        assert_eq!(response_verb_for_key("u"), Some("not_useful"));
    }

    #[test]
    fn test_n_maps_to_not_now() {
        assert_eq!(response_verb_for_key("n"), Some("not_now"));
    }

    #[test]
    fn test_case_insensitive_and_trimmed() {
        assert_eq!(response_verb_for_key("  G\n"), Some("got_it"));
        assert_eq!(response_verb_for_key("U"), Some("not_useful"));
    }

    #[test]
    fn test_unknown_key_is_none() {
        assert_eq!(response_verb_for_key("x"), None);
        assert_eq!(response_verb_for_key(""), None);
        assert_eq!(response_verb_for_key("got_it"), None);
    }

    // --- T4 req 1: manual `a` = applied ---

    #[test]
    fn test_a_maps_to_applied() {
        assert_eq!(response_verb_for_key("a"), Some("applied"));
        assert_eq!(response_verb_for_key(" A \n"), Some("applied"));
    }

    // --- T4 reqs 2-5: card key classifier, non-swallowing fall-through ---

    #[test]
    fn test_classify_card_key_escalate_tell_me_ask() {
        assert_eq!(classify_card_key("e"), CardKeyAction::Escalate);
        assert_eq!(classify_card_key("E"), CardKeyAction::Escalate);
        assert_eq!(classify_card_key("t"), CardKeyAction::TellMe);
        assert_eq!(classify_card_key("k"), CardKeyAction::Ask);
    }

    #[test]
    fn test_classify_card_key_falls_through_to_lifecycle_verbs() {
        assert_eq!(
            classify_card_key("a"),
            CardKeyAction::Response("applied")
        );
        assert_eq!(
            classify_card_key("g"),
            CardKeyAction::Response("got_it")
        );
        assert_eq!(
            classify_card_key("u"),
            CardKeyAction::Response("not_useful")
        );
        assert_eq!(
            classify_card_key("n"),
            CardKeyAction::Response("not_now")
        );
    }

    /// New keys must never swallow other bindings (`m` queue browse, `r`
    /// review, `y`/`n` offer-scoped, a queue number) — everything else falls
    /// through to `Ignore` so the caller's other handlers still see it.
    #[test]
    fn test_classify_card_key_ignores_everything_else() {
        for input in ["m", "r", "y", "1", "", "?"] {
            assert_eq!(
                classify_card_key(input),
                CardKeyAction::Ignore,
                "expected Ignore for {:?}",
                input
            );
        }
    }
}

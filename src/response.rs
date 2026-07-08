//! Card response capture (T1 req 10, C3): maps the in-pane keys `g`/`u`/`n`
//! to the C3 response-verb enum. Pure logic — the stdin-reading loop that
//! calls this lives in main.rs (untested, like the rest of the watch loop's
//! process glue).

/// The closed set of C3 response verbs a card can be answered with. Mirrors
/// the `ladder::Rung` / `bkt::Grade` idiom: `as_str` for the exact on-disk
/// string, `parse` for the reverse (unknown input falls through to `None`,
/// never a panic).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseVerb {
    Applied,
    Escalated,
    GotIt,
    NotNow,
    NotUseful,
    Expired,
    /// T17 R3: the `👍 useful / more like this` positive-signal key — the
    /// offer's old "yes" replacement now that accept/decline is gone. Ledger-
    /// blocks the EXACT advice-fp (the same hint verbatim never re-fires) but
    /// is deliberately NOT concept-suppressing (unlike `NotUseful`) and NOT
    /// mastery evidence (see `memory::should_record_evidence_for_response`) —
    /// "more like this" means the concept stays live, only this one hint is
    /// satisfied.
    Useful,
}

impl ResponseVerb {
    pub fn as_str(&self) -> &'static str {
        match self {
            ResponseVerb::Applied => "applied",
            ResponseVerb::Escalated => "escalated",
            ResponseVerb::GotIt => "got_it",
            ResponseVerb::NotNow => "not_now",
            ResponseVerb::NotUseful => "not_useful",
            ResponseVerb::Expired => "expired",
            ResponseVerb::Useful => "useful",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "applied" => Some(ResponseVerb::Applied),
            "escalated" => Some(ResponseVerb::Escalated),
            "got_it" => Some(ResponseVerb::GotIt),
            "not_now" => Some(ResponseVerb::NotNow),
            "not_useful" => Some(ResponseVerb::NotUseful),
            "expired" => Some(ResponseVerb::Expired),
            "useful" => Some(ResponseVerb::Useful),
            _ => None,
        }
    }
}

/// Maps a trimmed, case-insensitive single-key input to its C3 response
/// verb. Unknown input (including empty) maps to `None` and is ignored by
/// the caller.
///
/// T4 req 1: `a` (manual `applied`) is checked first — most-specific-first
/// per repo convention, though none of these single-char keys actually
/// overlap as substrings; the ordering just mirrors the pattern elsewhere.
///
/// T17 R3: `y` maps to `Useful` — freed by the retired `[y/N]` offer accept/
/// decline dialogue, now the "👍 useful / more like this" positive signal.
pub fn response_verb_for_key(input: &str) -> Option<ResponseVerb> {
    match input.trim().to_lowercase().as_str() {
        "a" => Some(ResponseVerb::Applied),
        "g" => Some(ResponseVerb::GotIt),
        "u" => Some(ResponseVerb::NotUseful),
        "n" => Some(ResponseVerb::NotNow),
        "y" => Some(ResponseVerb::Useful),
        _ => None,
    }
}

/// T4 reqs 2-5 / C4/C5: the focused-card key vocabulary beyond the plain
/// lifecycle verbs — escalate (`e`), jump-to-worked-example (`t`), and open
/// a thread (`k`). Extends the classifier-then-fall-through pattern used by
/// `consent::classify_yes_no_key`: every key that isn't one of these three
/// falls through to [`response_verb_for_key`], so `a`/`g`/`y`/`u`/`n` keep
/// working unchanged and an unrecognized key never swallows anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardKeyAction {
    /// A plain C3 lifecycle verb (`applied`/`got_it`/`not_useful`/`not_now`).
    Response(ResponseVerb),
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
        assert_eq!(response_verb_for_key("g"), Some(ResponseVerb::GotIt));
    }

    #[test]
    fn test_u_maps_to_not_useful() {
        assert_eq!(response_verb_for_key("u"), Some(ResponseVerb::NotUseful));
    }

    #[test]
    fn test_n_maps_to_not_now() {
        assert_eq!(response_verb_for_key("n"), Some(ResponseVerb::NotNow));
    }

    #[test]
    fn test_case_insensitive_and_trimmed() {
        assert_eq!(response_verb_for_key("  G\n"), Some(ResponseVerb::GotIt));
        assert_eq!(response_verb_for_key("U"), Some(ResponseVerb::NotUseful));
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
        assert_eq!(response_verb_for_key("a"), Some(ResponseVerb::Applied));
        assert_eq!(response_verb_for_key(" A \n"), Some(ResponseVerb::Applied));
    }

    // --- ResponseVerb as_str/parse round-trip (de-stringify refactor) ---

    #[test]
    fn test_response_verb_as_str_exact_strings() {
        assert_eq!(ResponseVerb::Applied.as_str(), "applied");
        assert_eq!(ResponseVerb::Escalated.as_str(), "escalated");
        assert_eq!(ResponseVerb::GotIt.as_str(), "got_it");
        assert_eq!(ResponseVerb::NotNow.as_str(), "not_now");
        assert_eq!(ResponseVerb::NotUseful.as_str(), "not_useful");
        assert_eq!(ResponseVerb::Expired.as_str(), "expired");
        assert_eq!(ResponseVerb::Useful.as_str(), "useful");
    }

    #[test]
    fn test_response_verb_parse_round_trips_and_rejects_unknown() {
        for verb in [
            ResponseVerb::Applied,
            ResponseVerb::Escalated,
            ResponseVerb::GotIt,
            ResponseVerb::NotNow,
            ResponseVerb::NotUseful,
            ResponseVerb::Expired,
            ResponseVerb::Useful,
        ] {
            assert_eq!(ResponseVerb::parse(verb.as_str()), Some(verb));
        }
        assert_eq!(ResponseVerb::parse("nope"), None);
    }

    // --- T17 R3: `y` -> Useful (freed by the retired offer accept/decline) ---

    #[test]
    fn test_y_maps_to_useful() {
        assert_eq!(response_verb_for_key("y"), Some(ResponseVerb::Useful));
        assert_eq!(response_verb_for_key("Y"), Some(ResponseVerb::Useful));
    }

    #[test]
    fn test_classify_card_key_y_is_useful() {
        assert_eq!(
            classify_card_key("y"),
            CardKeyAction::Response(ResponseVerb::Useful)
        );
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
            CardKeyAction::Response(ResponseVerb::Applied)
        );
        assert_eq!(
            classify_card_key("g"),
            CardKeyAction::Response(ResponseVerb::GotIt)
        );
        assert_eq!(
            classify_card_key("u"),
            CardKeyAction::Response(ResponseVerb::NotUseful)
        );
        assert_eq!(
            classify_card_key("n"),
            CardKeyAction::Response(ResponseVerb::NotNow)
        );
    }

    /// New keys must never swallow other bindings (`m` queue browse, `r`
    /// review, a queue number) — everything else falls through to `Ignore`
    /// so the caller's other handlers still see it. (`y` is now bound to
    /// `Useful` — see `test_classify_card_key_y_is_useful` — since the offer
    /// accept/decline dialogue that used to reserve it is gone, T17 R3.)
    #[test]
    fn test_classify_card_key_ignores_everything_else() {
        for input in ["m", "r", "1", "", "?"] {
            assert_eq!(
                classify_card_key(input),
                CardKeyAction::Ignore,
                "expected Ignore for {:?}",
                input
            );
        }
    }
}

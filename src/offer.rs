//! T3 reqs 11-13 — the struggle offer: one line, idle-gated, never while
//! green, evidenced, budget-sharing, and decline-persistent (I9-I13, D16,
//! C3, C7).

use std::time::{Duration, SystemTime};

/// req 11 / I10: never prompt while the user is actively typing — the
/// offer's own idle gate (independent of the D8 quiescence gate, which only
/// governs judging).
pub const OFFER_IDLE_GATE: Duration = Duration::from_secs(20);

/// req 13: two declines across sessions for the same concept suppress it.
pub const DECLINES_BEFORE_SUPPRESSION: u32 = 2;
/// req 13: the suppression's lifetime.
pub const OFFER_SUPPRESSION_DAYS: u64 = 7;
/// req 13: the suppression row's scope tag.
pub const OFFER_SUPPRESSION_SCOPE: &str = "offer-concept";
/// req 12: the throttle-accounting category for offers.
pub const OFFER_CATEGORY: &str = "struggle-offer";

/// I11: every offer names the signal that fired it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Evidence {
    /// Converged signals 1+2: the E-code and how long it's been red.
    ErrorStreak { code: String, minutes: u64 },
    /// Signal 3: the fresh help-flavored comment itself, self-declared.
    HelpComment { snippet: String },
}

/// I11: the evidence clause is mandatory in the offer line. Example:
/// "stuck on E0308 for 14 min — hint? [y/N]".
pub fn offer_line(evidence: &Evidence) -> String {
    match evidence {
        Evidence::ErrorStreak { code, minutes } => {
            format!("stuck on {} for {} min \u{2014} hint? [y/N]", code, minutes)
        }
        Evidence::HelpComment { snippet } => {
            format!("saw \"{}\" \u{2014} hint? [y/N]", snippet)
        }
    }
}

/// The (signal, key) identity an offer fires under — req 13's same-session
/// no-refire and cross-session decline-count key, and the `cards`/events
/// `concept` field for a struggle-offer row.
pub fn offer_key(evidence: &Evidence) -> (&'static str, String) {
    match evidence {
        Evidence::ErrorStreak { code, .. } => ("error-streak", code.clone()),
        Evidence::HelpComment { snippet } => ("help-comment", snippet.clone()),
    }
}

/// req 11: idle gate — no file events for at least [`OFFER_IDLE_GATE`].
pub fn is_idle(last_event_at: SystemTime, now: SystemTime) -> bool {
    now.duration_since(last_event_at)
        .map(|d| d >= OFFER_IDLE_GATE)
        .unwrap_or(false)
}

/// req 11 (clarified 2026-07-03, T3 spec amendment after review): the
/// never-while-green gate applies to the INFERRED signals only — signals 1
/// and 2 describe stuck-ness, which by construction requires a red check.
/// Signal 3 (self-declared help comment) is a green-build question too
/// ("why does this need a clone?") and needs only the idle gate (throttle
/// and suppression are checked separately by the caller).
pub fn may_offer(evidence: &Evidence, last_check_success: Option<bool>, idle: bool) -> bool {
    if !idle {
        return false;
    }
    match evidence {
        Evidence::ErrorStreak { .. } => last_check_success == Some(false),
        Evidence::HelpComment { .. } => true,
    }
}

/// req 11 / I10: continuing to type (a file event after the offer fired)
/// dismisses it silently.
pub fn expired_by_continued_typing(offer_fired_at: SystemTime, last_event_at: SystemTime) -> bool {
    last_event_at > offer_fired_at
}

/// How a keypress resolves a pending offer. Only `y`/`n` (case-insensitive)
/// consume it — every other key (review fix) falls through to its normal
/// binding (e.g. `g`, `m`, a queue number, or a card response verb) and
/// leaves the offer live, mirroring `response::response_verb_for_key`'s
/// pure-classifier shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfferKeyAction {
    Accept,
    Decline,
    /// Not an offer response at all — the offer stays pending.
    Ignore,
}

pub fn classify_offer_key(input: &str) -> OfferKeyAction {
    match input.trim().to_lowercase().as_str() {
        "y" => OfferKeyAction::Accept,
        "n" => OfferKeyAction::Decline,
        _ => OfferKeyAction::Ignore,
    }
}

/// req 13: two declines across sessions for the same concept -> suppress.
pub fn should_suppress_after_declines(decline_count: u32) -> bool {
    decline_count >= DECLINES_BEFORE_SUPPRESSION
}

/// req 13: the epoch-seconds expiry for a freshly-suppressed offer concept.
pub fn suppression_expiry_epoch_secs(now: SystemTime) -> i64 {
    let now_secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    now_secs + (OFFER_SUPPRESSION_DAYS as i64 * 24 * 3600)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    #[test]
    fn test_offer_line_error_streak_format() {
        let evidence = Evidence::ErrorStreak {
            code: "E0308".to_string(),
            minutes: 14,
        };
        assert_eq!(offer_line(&evidence), "stuck on E0308 for 14 min \u{2014} hint? [y/N]");
    }

    #[test]
    fn test_offer_line_help_comment_format() {
        let evidence = Evidence::HelpComment {
            snippet: "why does this need a clone?".to_string(),
        };
        assert_eq!(
            offer_line(&evidence),
            "saw \"why does this need a clone?\" \u{2014} hint? [y/N]"
        );
    }

    #[test]
    fn test_offer_key_identity() {
        let e1 = Evidence::ErrorStreak {
            code: "E0308".to_string(),
            minutes: 5,
        };
        let e2 = Evidence::ErrorStreak {
            code: "E0308".to_string(),
            minutes: 20,
        };
        assert_eq!(offer_key(&e1), offer_key(&e2), "key ignores minutes");

        let e3 = Evidence::ErrorStreak {
            code: "E0502".to_string(),
            minutes: 5,
        };
        assert_ne!(offer_key(&e1), offer_key(&e3));
    }

    // --- idle gating (req 11) ---

    #[test]
    fn test_is_idle_boundary() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1000);
        assert!(!is_idle(t0, t0 + Duration::from_secs(19)));
        assert!(is_idle(t0, t0 + Duration::from_secs(20)));
    }

    fn error_streak_evidence() -> Evidence {
        Evidence::ErrorStreak {
            code: "E0308".to_string(),
            minutes: 14,
        }
    }

    fn help_comment_evidence() -> Evidence {
        Evidence::HelpComment {
            snippet: "why does this need a clone?".to_string(),
        }
    }

    #[test]
    fn test_may_offer_inferred_signal_requires_idle_and_red() {
        let e = error_streak_evidence();
        assert!(may_offer(&e, Some(false), true));
        assert!(!may_offer(&e, Some(false), false), "not idle");
        assert!(!may_offer(&e, Some(true), true), "never while green");
        assert!(!may_offer(&e, None, true), "no check yet");
    }

    /// req 11 (clarified): signal 3 (self-declared) needs only the idle
    /// gate — a fresh help-flavored comment may fire an offer on a green
    /// build.
    #[test]
    fn test_may_offer_help_comment_fires_on_green_build() {
        let e = help_comment_evidence();
        assert!(may_offer(&e, Some(true), true), "green build must not block signal 3");
        assert!(may_offer(&e, Some(false), true));
        assert!(may_offer(&e, None, true), "no check history yet is fine for signal 3");
        assert!(!may_offer(&e, Some(true), false), "still needs idle");
    }

    #[test]
    fn test_continuing_to_type_expires_offer_silently() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1000);
        assert!(expired_by_continued_typing(t0, t0 + Duration::from_secs(1)));
        assert!(!expired_by_continued_typing(t0, t0));
    }

    // --- offer key classification (review fix): only y/n consume ---

    #[test]
    fn test_classify_offer_key_y_accepts() {
        assert_eq!(classify_offer_key("y"), OfferKeyAction::Accept);
        assert_eq!(classify_offer_key("Y"), OfferKeyAction::Accept);
        assert_eq!(classify_offer_key("  y\n"), OfferKeyAction::Accept);
    }

    #[test]
    fn test_classify_offer_key_n_declines() {
        assert_eq!(classify_offer_key("n"), OfferKeyAction::Decline);
        assert_eq!(classify_offer_key("N"), OfferKeyAction::Decline);
    }

    #[test]
    fn test_classify_offer_key_everything_else_is_ignored() {
        // Review fix: `g` (goal edit / got_it), `m` (queue browse), a queue
        // number, and blank input must all fall through untouched rather
        // than being treated as an implicit decline.
        for input in ["g", "m", "u", "1", "", "yes", "no"] {
            assert_eq!(
                classify_offer_key(input),
                OfferKeyAction::Ignore,
                "expected Ignore for {:?}",
                input
            );
        }
    }

    // --- decline persistence (req 13) ---

    #[test]
    fn test_suppress_after_two_declines_not_one() {
        assert!(!should_suppress_after_declines(0));
        assert!(!should_suppress_after_declines(1));
        assert!(should_suppress_after_declines(2));
        assert!(should_suppress_after_declines(3));
    }

    #[test]
    fn test_suppression_expiry_is_seven_days_out() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let now_secs = now.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64;
        let expiry = suppression_expiry_epoch_secs(now);
        assert_eq!(expiry - now_secs, 7 * 24 * 3600);
    }
}

//! T2 req 8/9 — D11(c) tiered snooze and I3/A11 regression re-open. Pure
//! decision logic; the `suppressions` table CRUD lives in db.rs.

use crate::db::CardStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnoozeScope {
    Instance,
    Concept,
}

impl SnoozeScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            SnoozeScope::Instance => "instance",
            SnoozeScope::Concept => "concept",
        }
    }

    /// Inverse of [`as_str`](Self::as_str) — parses the stored `scope` TEXT back
    /// into the enum (`None` for an unrecognized value). The write path uses
    /// `as_str`; this completes the pair so a reader can decode the column
    /// type-safely rather than string-matching.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "instance" => Some(SnoozeScope::Instance),
            "concept" => Some(SnoozeScope::Concept),
            _ => None,
        }
    }
}

/// D11(c): first `not_now` on a concept this session snoozes just the
/// instance (site); a second `not_now` on the SAME concept auto-widens to
/// concept-for-session. `prior_instance_snoozes` is the count of
/// `instance`-scope suppression rows already recorded for this concept this
/// session (before this snooze).
pub fn tiered_snooze_scope(prior_instance_snoozes: u32) -> SnoozeScope {
    if prior_instance_snoozes == 0 {
        SnoozeScope::Instance
    } else {
        SnoozeScope::Concept
    }
}

/// The one-line notice shown only when a snooze widens to concept scope.
pub fn widening_notice(concept_name: &str) -> String {
    format!("okay \u{2014} parking {} for today", concept_name)
}

/// T2 req 5 / I3: statuses that permanently block a re-created card for the
/// same advice-fp.
pub fn is_ledger_blocking_status(status: CardStatus) -> bool {
    match status {
        CardStatus::Applied | CardStatus::GotIt | CardStatus::NotUseful | CardStatus::Resolved => {
            true
        }
        CardStatus::Shown
        | CardStatus::Queued
        | CardStatus::Escalated
        | CardStatus::NotNow
        | CardStatus::Expired
        | CardStatus::Collapsed => false,
    }
}

/// T2 req 9: the subset of ledger-blocking statuses a regression is allowed
/// to re-open — misuse re-opens *taught* advice (`applied`/`resolved`), not
/// something the user already dismissed as `got_it`/`not_useful`.
pub fn is_regression_eligible_status(status: CardStatus) -> bool {
    match status {
        CardStatus::Applied | CardStatus::Resolved => true,
        CardStatus::Shown
        | CardStatus::Queued
        | CardStatus::Escalated
        | CardStatus::GotIt
        | CardStatus::NotNow
        | CardStatus::NotUseful
        | CardStatus::Expired
        | CardStatus::Collapsed => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ROADMAP item 8: exhaustive parse(as_str(x)) == x over every variant.
    #[test]
    fn test_snooze_scope_round_trips_through_str() {
        for scope in [SnoozeScope::Instance, SnoozeScope::Concept] {
            assert_eq!(SnoozeScope::parse(scope.as_str()), Some(scope));
        }
        assert_eq!(SnoozeScope::parse("nonsense"), None);
    }

    #[test]
    fn test_first_not_now_is_instance_scoped() {
        assert_eq!(tiered_snooze_scope(0), SnoozeScope::Instance);
    }

    #[test]
    fn test_second_not_now_same_concept_widens_to_concept() {
        assert_eq!(tiered_snooze_scope(1), SnoozeScope::Concept);
    }

    #[test]
    fn test_third_and_beyond_stay_concept_scoped() {
        assert_eq!(tiered_snooze_scope(2), SnoozeScope::Concept);
    }

    #[test]
    fn test_widening_notice_names_the_concept() {
        let notice = widening_notice("Borrow vs. clone");
        assert!(notice.contains("Borrow vs. clone"));
        assert!(notice.starts_with("okay"));
    }

    #[test]
    fn test_ledger_blocking_statuses() {
        for s in [
            CardStatus::Applied,
            CardStatus::GotIt,
            CardStatus::NotUseful,
            CardStatus::Resolved,
        ] {
            assert!(is_ledger_blocking_status(s), "{:?} should block", s);
        }
        for s in [
            CardStatus::Shown,
            CardStatus::Queued,
            CardStatus::NotNow,
            CardStatus::Expired,
        ] {
            assert!(!is_ledger_blocking_status(s), "{:?} should not block", s);
        }
    }

    #[test]
    fn test_regression_eligible_statuses_are_a_subset_of_blocking() {
        for s in [CardStatus::Applied, CardStatus::Resolved] {
            assert!(is_regression_eligible_status(s));
        }
        for s in [CardStatus::GotIt, CardStatus::NotUseful] {
            assert!(
                !is_regression_eligible_status(s),
                "{:?} is ledger-blocking but not regression-eligible",
                s
            );
        }
    }
}

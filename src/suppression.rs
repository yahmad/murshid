//! T2 req 8/9 — D11(c) tiered snooze and I3/A11 regression re-open. Pure
//! decision logic; the `suppressions` table CRUD lives in db.rs.

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
pub fn is_ledger_blocking_status(status: &str) -> bool {
    matches!(status, "applied" | "got_it" | "not_useful" | "resolved")
}

/// T2 req 9: the subset of ledger-blocking statuses a regression is allowed
/// to re-open — misuse re-opens *taught* advice (`applied`/`resolved`), not
/// something the user already dismissed as `got_it`/`not_useful`.
pub fn is_regression_eligible_status(status: &str) -> bool {
    matches!(status, "applied" | "resolved")
}

#[cfg(test)]
mod tests {
    use super::*;

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
        for s in ["applied", "got_it", "not_useful", "resolved"] {
            assert!(is_ledger_blocking_status(s), "{} should block", s);
        }
        for s in ["shown", "queued", "not_now", "expired"] {
            assert!(!is_ledger_blocking_status(s), "{} should not block", s);
        }
    }

    #[test]
    fn test_regression_eligible_statuses_are_a_subset_of_blocking() {
        for s in ["applied", "resolved"] {
            assert!(is_regression_eligible_status(s));
        }
        for s in ["got_it", "not_useful"] {
            assert!(
                !is_regression_eligible_status(s),
                "{} is ledger-blocking but not regression-eligible",
                s
            );
        }
    }
}

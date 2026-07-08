//! T2 req 8/9 — D11(c) tiered snooze and I3/A11 regression re-open. T17 R2
//! adds the learning-memory anti-repeat machinery: the decaying
//! `hint-concept` cross-session window (pure sizing fn) and the composed
//! `hint_is_suppressed` gate, which DOES read the DB (cards/suppressions/
//! concept_memory) to fold every anti-repeat reason — same-session dedup,
//! the I3 ledger, the decaying concept suppression, the shown-and-ignored
//! tally, plain mastery, and D12 throttle — into one testable decision,
//! with the spaced-resurfacing exception applied last. The per-table CRUD
//! itself still lives in `db/*.rs`.

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

// --- T17 R2: the learning-memory anti-repeat gate ---

/// C1 config-grade knob: the `hint-concept` suppression's base window — a
/// first `not_useful` on a concept quiets it 14 days.
pub const HINT_CONCEPT_BASE_WINDOW_DAYS: i64 = 14;

/// C1 config-grade knob: how many silent (`expired`, no positive action)
/// shows of the exact same advice-fp, across sessions, before it auto-
/// suppresses — the common case for a passive panel, where nobody presses
/// `u`, they just don't act.
pub const IGNORED_SHOWS_THRESHOLD: u32 = 3;

/// T17 R2: the decaying `hint-concept` suppression window, in seconds —
/// pure, so the "repeat `u` -> longer window" rule is directly testable.
/// `prior_suppressions` is how many `hint-concept` rows already exist for
/// this concept (any session, expired or not — see
/// `db::count_hint_concept_suppressions_for_concept`); the window doubles
/// per prior suppression (capped at a 2^6 multiplier so a pathological
/// concept can't overflow the epoch-seconds arithmetic).
pub fn hint_concept_suppression_window_secs(prior_suppressions: u32) -> i64 {
    const DAY_SECS: i64 = 24 * 3600;
    let multiplier = 1i64 << prior_suppressions.min(6);
    HINT_CONCEPT_BASE_WINDOW_DAYS
        .saturating_mul(multiplier)
        .saturating_mul(DAY_SECS)
}

/// T17 R2: the epoch-seconds expiry for a freshly-suppressed `hint-concept`
/// row, from `now` and the prior-suppression count (same idiom as
/// `offer::suppression_expiry_epoch_secs`).
pub fn hint_concept_suppression_expiry_epoch_secs(
    now: std::time::SystemTime,
    prior_suppressions: u32,
) -> i64 {
    let now_secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    now_secs + hint_concept_suppression_window_secs(prior_suppressions)
}

/// T17 R2 — the composed learning-memory gate (the crux's load-bearing
/// rule): whether a hint for `concept_id`/`advice_fp` must NOT fire right
/// now. Built but intentionally NOT yet wired into the fire path — pre-flip
/// offers still carry a synthetic fp, so R3 (offer → direct hint) wires this
/// gate in at the same moment the hint gains its real advice_fp.
///
/// Precedence, most-specific/hardest-to-override first (each of these is
/// checked in order; the first match wins):
/// 1. same-session dedup (this exact fp already shown this session) —
///    absolute, cadence within a session is never "due" to repeat.
/// 2. the D12 channel throttle — absolute, a circuit-breaker on the whole
///    category, not a per-concept memory signal.
/// 3. the I3 ledger (`applied`/`got_it`/`not_useful`/`resolved` already
///    recorded for this EXACT fp) — absolute and UNCHANGED from T2: this
///    gate only ADDS suppression reasons, it never softens the pre-existing
///    "never re-raise" invariant.
/// 4. the shown-and-ignored tally (K silent `expired` shows, cross-session,
///    for this exact fp) — absolute, per the founder's explicit acceptance
///    wording: an ignored fp never comes back.
/// 5. the decaying `hint-concept` suppression (cross-session, concept-
///    scoped, seeded by `not_useful`) — the window itself IS the spacing:
///    while live, it blocks; once it expires, this reason simply stops
///    contributing (no separate resurfacing check needed here).
/// 6. plain BKT mastery — the ONLY reason subject to the spaced-resurfacing
///    exception (`memory::resurfacing_eligible`): a mastered concept stays
///    quiet only while it's neither regressed nor gone stale. This is the
///    founder's "got it/mastered -> quiet until stale/regressed, then
///    resurface" rule, driven purely by mastery + staleness, never by the
///    dismissal verb itself.
#[allow(clippy::too_many_arguments)]
pub fn hint_is_suppressed(
    conn: &rusqlite::Connection,
    session_id: &str,
    concept_id: &str,
    advice_fp: &str,
    category: &crate::pack::Category,
    category_throttled: bool,
    now_epoch_secs: i64,
) -> Result<bool, rusqlite::Error> {
    if crate::db::card_exists_with_advice_fp(conn, session_id, advice_fp)? {
        return Ok(true);
    }
    if category_throttled {
        return Ok(true);
    }

    let ledger_blocked = crate::db::find_ledger_card(conn, advice_fp)?
        .map(|(_, status)| {
            CardStatus::parse(&status)
                .map(is_ledger_blocking_status)
                .unwrap_or(false)
        })
        .unwrap_or(false);
    if ledger_blocked {
        return Ok(true);
    }

    if crate::db::count_expired_for_advice_fp(conn, advice_fp)? >= IGNORED_SHOWS_THRESHOLD {
        return Ok(true);
    }

    if crate::db::is_hint_concept_suppressed(conn, concept_id, now_epoch_secs)? {
        return Ok(true);
    }

    let row = crate::memory::read_or_default(conn, concept_id, category.as_str())?;
    if !crate::bkt::is_mastered(row.p_mastery) {
        return Ok(false);
    }
    let now_secs_u64 = u64::try_from(now_epoch_secs).unwrap_or(0);
    let eligible = crate::memory::resurfacing_eligible(&row, category, now_secs_u64);
    Ok(!eligible)
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

    // --- T17 R2: the decaying window (pure) + the composed learning gate ---

    #[test]
    fn test_hint_concept_window_decays_and_is_capped() {
        const DAY: i64 = 24 * 3600;
        // First `not_useful` (no prior rows) → the 14-day base window.
        assert_eq!(
            hint_concept_suppression_window_secs(0),
            HINT_CONCEPT_BASE_WINDOW_DAYS * DAY
        );
        // Each prior suppression DOUBLES the window (spaced/decaying — a
        // repeat dismissal quiets the concept longer).
        assert_eq!(
            hint_concept_suppression_window_secs(1),
            HINT_CONCEPT_BASE_WINDOW_DAYS * 2 * DAY
        );
        assert_eq!(
            hint_concept_suppression_window_secs(2),
            HINT_CONCEPT_BASE_WINDOW_DAYS * 4 * DAY
        );
        // Capped at 2^6 so a pathological concept can't overflow the epoch.
        assert_eq!(
            hint_concept_suppression_window_secs(20),
            hint_concept_suppression_window_secs(6)
        );
    }

    #[test]
    fn test_hint_is_suppressed_concept_scope_is_cross_site_and_temporary() {
        // Acceptance (crux gap #3 + spaced/decaying): a `not_useful` seeds a
        // CONCEPT-scoped suppression, so a DIFFERENT code site (a different
        // advice_fp) of the SAME concept is also suppressed — but only while
        // the window is live; once it elapses, muting stops (spaced, not
        // permanent).
        let conn = crate::db::initialize_db(":memory:").unwrap();
        let cat = crate::pack::Category::Idiom;

        // A clean, unseen, unmastered concept is NOT suppressed.
        assert!(
            !hint_is_suppressed(&conn, "s1", "borrow-vs-clone", "fp-site-A", &cat, false, 100)
                .unwrap(),
            "a clean concept must be eligible"
        );

        // Seed a hint-concept suppression that expires at t=1_000.
        crate::db::insert_hint_concept_suppression(&conn, "s1", "borrow-vs-clone", 1_000).unwrap();

        // A DIFFERENT site/fp of the SAME concept is now suppressed (gap #3).
        assert!(
            hint_is_suppressed(
                &conn,
                "s1",
                "borrow-vs-clone",
                "fp-site-B-different",
                &cat,
                false,
                500
            )
            .unwrap(),
            "concept-scoped suppression must cover OTHER sites of the concept"
        );

        // Past the window, muting is over — the concept can resurface.
        assert!(
            !hint_is_suppressed(
                &conn,
                "s1",
                "borrow-vs-clone",
                "fp-site-B-different",
                &cat,
                false,
                1_001
            )
            .unwrap(),
            "muting is TEMPORARY — past the window the concept is eligible again"
        );

        // A different concept is unaffected by borrow-vs-clone's suppression.
        assert!(
            !hint_is_suppressed(&conn, "s1", "string-vs-str", "fp-x", &cat, false, 500).unwrap()
        );
    }

    #[test]
    fn test_hint_is_suppressed_throttle_is_absolute() {
        let conn = crate::db::initialize_db(":memory:").unwrap();
        let cat = crate::pack::Category::Idiom;
        assert!(
            hint_is_suppressed(&conn, "s1", "borrow-vs-clone", "fp", &cat, true, 100).unwrap(),
            "a throttled category is suppressed regardless of per-concept memory"
        );
    }
}

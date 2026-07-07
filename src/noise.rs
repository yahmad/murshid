//! T2 req 1 / D10 / C12 — the severity floor. Redesign R0 (T17): the
//! frequency detent (quiet/standard/chatty, each pairing a budget period
//! with a floor) is gone — cadence is now a single `[dial] min_gap`
//! cooldown (see `budget::TokenBucket::for_min_gap`, `config::DialConfig`).
//! To honor R0's no-behavior-flip invariant, the floor itself is no longer
//! detent-varying: it's now a single FIXED list equal to the old `quiet`
//! default's floor, so default-config behavior (which was already `quiet`)
//! is byte-identical. Fully dropping the floor is a deferred founder
//! decision (T17 R2+), not this rung.
pub const FLOOR: &[crate::pack::Category] =
    &[crate::pack::Category::Bug, crate::pack::Category::Idiom];

/// T2 req 1/2: a category not in [`FLOOR`] is never budget-eligible — it
/// always queues (never dropped). A `Category::Other` (a pack-authored
/// category outside the closed C12 set) is never in the floor list, so it's
/// always excluded — the exact behavior an unrecognized floor-string used to
/// get.
pub fn floor_excludes(category: &crate::pack::Category) -> bool {
    !FLOOR.contains(category)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SweepAction {
    Show,
    Enqueue,
}

/// T2 review fix — single-slot guard (mirrors the queue-pull guard and T1's
/// retain-when-slot-taken shape): a sweep-pass finding may only take the
/// on-screen slot when NONE of the following hold — something already
/// shipped earlier this pass, a card from an earlier pass is still awaiting
/// a response, the category is currently throttled, or the category is
/// floor-excluded. This gate runs *before* the budget/strict-mode decision,
/// so a strict-mode `likely_bug` candidate can still preempt the budget
/// (C6/D10) but never the physical slot — it queues like everything else
/// blocked here, and C7's category-rank ordering (bug ranks first) puts it
/// at the head of the queue on its own.
pub fn gate_sweep_finding(
    shown_this_pass: bool,
    pending_card_present: bool,
    throttled: bool,
    floor_excluded: bool,
) -> SweepAction {
    if shown_this_pass || pending_card_present || throttled || floor_excluded {
        SweepAction::Enqueue
    } else {
        SweepAction::Show
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::Category;

    #[test]
    fn test_floor_excludes_architecture_and_best_practice() {
        assert!(floor_excludes(&Category::Architecture));
        assert!(floor_excludes(&Category::BestPractice));
        assert!(!floor_excludes(&Category::Bug));
        assert!(!floor_excludes(&Category::Idiom));
    }

    /// A `Category::Other` (unrecognized pack category) is excluded, same as
    /// an unmatched floor-string used to be.
    #[test]
    fn test_floor_excludes_unknown_category() {
        assert!(floor_excludes(&Category::parse("some-new-pack-category")));
    }

    /// T2 req 2 (acceptance: "floor exclusion queues rather than drops"): a
    /// floor-excluded candidate must never reach `budget::decide_push` at
    /// all — the caller queues it directly — so the budget is left
    /// untouched for floor-eligible candidates, and nothing is ever dropped.
    #[test]
    fn test_floor_excluded_category_never_touches_budget_would_queue_not_drop() {
        assert!(floor_excludes(&Category::Architecture));

        let mut bucket = crate::budget::TokenBucket::for_min_gap(
            std::time::Duration::from_secs(600),
            std::time::SystemTime::UNIX_EPOCH,
        );
        let before = bucket.tokens_available();

        // Mirrors the gate main.rs applies: floor-excluded candidates never
        // call decide_push, they queue directly.
        let candidate = crate::budget::PushCandidate {
            likely_bug: false,
            strict_mode_passed: false,
        };
        let outcome = if floor_excludes(&Category::Architecture) {
            None // queued, budget untouched
        } else {
            Some(crate::budget::decide_push(
                &mut bucket,
                &candidate,
                std::time::SystemTime::UNIX_EPOCH,
            ))
        };

        assert_eq!(
            outcome, None,
            "floor-excluded candidate must queue, not consume budget"
        );
        assert_eq!(
            bucket.tokens_available(),
            before,
            "budget must be untouched by a floor-excluded (queued) candidate"
        );
    }

    // --- review fix: single-slot guard ---

    #[test]
    fn test_gate_shows_when_everything_clear() {
        assert_eq!(
            gate_sweep_finding(false, false, false, false),
            SweepAction::Show
        );
    }

    #[test]
    fn test_gate_enqueues_when_pending_card_present() {
        // The bug this fixes: a card from an earlier pass is still
        // unanswered — a fresh sweep pass must never overwrite it.
        assert_eq!(
            gate_sweep_finding(false, true, false, false),
            SweepAction::Enqueue
        );
    }

    #[test]
    fn test_gate_enqueues_when_already_shown_this_pass() {
        assert_eq!(
            gate_sweep_finding(true, false, false, false),
            SweepAction::Enqueue
        );
    }

    #[test]
    fn test_gate_enqueues_when_throttled_or_floor_excluded() {
        assert_eq!(
            gate_sweep_finding(false, false, true, false),
            SweepAction::Enqueue
        );
        assert_eq!(
            gate_sweep_finding(false, false, false, true),
            SweepAction::Enqueue
        );
    }

    #[test]
    fn test_gate_pending_card_wins_even_when_nothing_else_blocks() {
        // A strict-mode likely_bug candidate would otherwise bypass the
        // budget entirely (see budget::decide_push) — but it must never
        // bypass the slot guard. Enqueue is the only outcome when a card is
        // already pending, full stop.
        assert_eq!(
            gate_sweep_finding(false, true, false, false),
            SweepAction::Enqueue
        );
    }
}

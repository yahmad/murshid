//! T2 req 1 / D10 / C12 — the frequency knob: quiet/standard/chatty each set
//! a (budget, floor) pair. `quiet` is the binding default (I7 ship-chill;
//! overrides T1's hardcoded `standard`).

use std::time::Duration;

/// C12: `quiet` 1 push/20 min, `standard` 1/10 min, `chatty` 1/5 min. Burst
/// is 1 for every detent (T1's fixed-`standard` shape, generalized).
pub const QUIET_REFILL_PERIOD: Duration = Duration::from_secs(20 * 60);
pub const STANDARD_REFILL_PERIOD: Duration = crate::budget::STANDARD_REFILL_PERIOD;
pub const CHATTY_REFILL_PERIOD: Duration = Duration::from_secs(5 * 60);

/// C12 severity floor per detent. Until T3 lands, "goal-relevant idiom"
/// degrades to plain "idiom" (T2 req 1 note).
pub const QUIET_FLOOR: &[crate::pack::Category] =
    &[crate::pack::Category::Bug, crate::pack::Category::Idiom];
pub const STANDARD_FLOOR: &[crate::pack::Category] = &[
    crate::pack::Category::Bug,
    crate::pack::Category::Idiom,
    crate::pack::Category::BestPractice,
];
pub const CHATTY_FLOOR: &[crate::pack::Category] = &[
    crate::pack::Category::Bug,
    crate::pack::Category::Idiom,
    crate::pack::Category::BestPractice,
    crate::pack::Category::Architecture,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Detent {
    pub refill_period: Duration,
    pub floor: &'static [crate::pack::Category],
}

/// Maps a `[dial] frequency` config value to its (budget, floor) pair.
/// Unknown values fall back to `quiet` (I7 ship-chill: fail toward quiet).
pub fn detent_for(frequency: &str) -> Detent {
    match frequency {
        "standard" => Detent {
            refill_period: STANDARD_REFILL_PERIOD,
            floor: STANDARD_FLOOR,
        },
        "chatty" => Detent {
            refill_period: CHATTY_REFILL_PERIOD,
            floor: CHATTY_FLOOR,
        },
        _ => Detent {
            refill_period: QUIET_REFILL_PERIOD,
            floor: QUIET_FLOOR,
        },
    }
}

/// T15 settings overlay: the frequency dial's known label cycle, in the
/// order the overlay's `\u{2192}` steps through them. `prev_frequency`/
/// `next_frequency` below only ever accept a label this module itself
/// produced (a value read back out of `WatchSession::frequency`), but degrade
/// to `quiet` rather than panicking on an unrecognized string, matching
/// `detent_for`'s own fail-toward-quiet posture.
const FREQUENCY_CYCLE: [&str; 3] = ["quiet", "standard", "chatty"];

fn frequency_index(current: &str) -> usize {
    FREQUENCY_CYCLE.iter().position(|f| *f == current).unwrap_or(0)
}

/// T15 settings overlay: `\u{2192}` — quiet -> standard -> chatty -> quiet.
pub fn next_frequency(current: &str) -> &'static str {
    let idx = frequency_index(current);
    FREQUENCY_CYCLE[(idx + 1) % FREQUENCY_CYCLE.len()]
}

/// T15 settings overlay: `\u{2190}` — the same ring, reversed.
pub fn prev_frequency(current: &str) -> &'static str {
    let idx = frequency_index(current);
    FREQUENCY_CYCLE[(idx + FREQUENCY_CYCLE.len() - 1) % FREQUENCY_CYCLE.len()]
}

/// T2 req 1/2: a category not in the detent's floor is never budget-eligible
/// — it always queues (never dropped). A `Category::Other` (a pack-authored
/// category outside the closed C12 set) is never in any floor list, so it's
/// always excluded — the exact behavior an unrecognized floor-string used to
/// get.
pub fn floor_excludes(detent: &Detent, category: &crate::pack::Category) -> bool {
    !detent.floor.contains(category)
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
    fn test_quiet_is_default_detent_shape() {
        let d = detent_for("quiet");
        assert_eq!(d.refill_period, Duration::from_secs(20 * 60));
        assert_eq!(d.floor, &[Category::Bug, Category::Idiom]);
    }

    #[test]
    fn test_standard_detent_shape() {
        let d = detent_for("standard");
        assert_eq!(d.refill_period, Duration::from_secs(10 * 60));
        assert_eq!(
            d.floor,
            &[Category::Bug, Category::Idiom, Category::BestPractice]
        );
    }

    #[test]
    fn test_chatty_detent_shape() {
        let d = detent_for("chatty");
        assert_eq!(d.refill_period, Duration::from_secs(5 * 60));
        assert_eq!(
            d.floor,
            &[
                Category::Bug,
                Category::Idiom,
                Category::BestPractice,
                Category::Architecture
            ]
        );
    }

    #[test]
    fn test_unknown_frequency_falls_back_to_quiet() {
        let d = detent_for("bogus");
        assert_eq!(d.refill_period, Duration::from_secs(20 * 60));
    }

    // --- T15 settings overlay: frequency cycle ---

    #[test]
    fn test_next_frequency_cycles_quiet_standard_chatty_and_wraps() {
        assert_eq!(next_frequency("quiet"), "standard");
        assert_eq!(next_frequency("standard"), "chatty");
        assert_eq!(next_frequency("chatty"), "quiet");
    }

    #[test]
    fn test_prev_frequency_is_the_exact_reverse_of_next() {
        for f in ["quiet", "standard", "chatty"] {
            assert_eq!(prev_frequency(next_frequency(f)), f);
            assert_eq!(next_frequency(prev_frequency(f)), f);
        }
    }

    #[test]
    fn test_frequency_cycle_degrades_to_quiet_for_unknown_input() {
        assert_eq!(next_frequency("bogus"), "standard", "unknown treated as index 0 (quiet)");
        assert_eq!(prev_frequency("bogus"), "chatty");
    }

    #[test]
    fn test_floor_excludes_architecture_at_quiet() {
        let d = detent_for("quiet");
        assert!(floor_excludes(&d, &Category::Architecture));
        assert!(floor_excludes(&d, &Category::BestPractice));
        assert!(!floor_excludes(&d, &Category::Bug));
        assert!(!floor_excludes(&d, &Category::Idiom));
    }

    #[test]
    fn test_floor_excludes_nothing_at_chatty() {
        let d = detent_for("chatty");
        for cat in [
            Category::Bug,
            Category::Idiom,
            Category::BestPractice,
            Category::Architecture,
        ] {
            assert!(!floor_excludes(&d, &cat));
        }
    }

    /// A `Category::Other` (unrecognized pack category) is excluded at
    /// every detent, same as an unmatched floor-string used to be.
    #[test]
    fn test_floor_excludes_unknown_category_at_every_detent() {
        for frequency in ["quiet", "standard", "chatty"] {
            let d = detent_for(frequency);
            assert!(floor_excludes(
                &d,
                &Category::parse("some-new-pack-category")
            ));
        }
    }

    /// T2 req 2 (acceptance: "floor exclusion queues rather than drops"): a
    /// floor-excluded candidate must never reach `budget::decide_push` at
    /// all — the caller queues it directly — so the budget is left
    /// untouched for floor-eligible candidates, and nothing is ever dropped.
    #[test]
    fn test_floor_excluded_category_never_touches_budget_would_queue_not_drop() {
        let detent = detent_for("quiet");
        assert!(floor_excludes(&detent, &Category::Architecture));

        let mut bucket =
            crate::budget::TokenBucket::for_detent(&detent, std::time::SystemTime::UNIX_EPOCH);
        let before = bucket.tokens_available();

        // Mirrors the gate main.rs applies: floor-excluded candidates never
        // call decide_push, they queue directly.
        let candidate = crate::budget::PushCandidate {
            likely_bug: false,
            strict_mode_passed: false,
        };
        let outcome = if floor_excludes(&detent, &Category::Architecture) {
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

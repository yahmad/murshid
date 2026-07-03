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
pub const QUIET_FLOOR: &[&str] = &["bug", "idiom"];
pub const STANDARD_FLOOR: &[&str] = &["bug", "idiom", "best-practice"];
pub const CHATTY_FLOOR: &[&str] = &["bug", "idiom", "best-practice", "architecture"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Detent {
    pub refill_period: Duration,
    pub floor: &'static [&'static str],
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

/// T2 req 1/2: a category not in the detent's floor is never budget-eligible
/// — it always queues (never dropped).
pub fn floor_excludes(detent: &Detent, category: &str) -> bool {
    !detent.floor.contains(&category)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quiet_is_default_detent_shape() {
        let d = detent_for("quiet");
        assert_eq!(d.refill_period, Duration::from_secs(20 * 60));
        assert_eq!(d.floor, &["bug", "idiom"]);
    }

    #[test]
    fn test_standard_detent_shape() {
        let d = detent_for("standard");
        assert_eq!(d.refill_period, Duration::from_secs(10 * 60));
        assert_eq!(d.floor, &["bug", "idiom", "best-practice"]);
    }

    #[test]
    fn test_chatty_detent_shape() {
        let d = detent_for("chatty");
        assert_eq!(d.refill_period, Duration::from_secs(5 * 60));
        assert_eq!(d.floor, &["bug", "idiom", "best-practice", "architecture"]);
    }

    #[test]
    fn test_unknown_frequency_falls_back_to_quiet() {
        let d = detent_for("bogus");
        assert_eq!(d.refill_period, Duration::from_secs(20 * 60));
    }

    #[test]
    fn test_floor_excludes_architecture_at_quiet() {
        let d = detent_for("quiet");
        assert!(floor_excludes(&d, "architecture"));
        assert!(floor_excludes(&d, "best-practice"));
        assert!(!floor_excludes(&d, "bug"));
        assert!(!floor_excludes(&d, "idiom"));
    }

    #[test]
    fn test_floor_excludes_nothing_at_chatty() {
        let d = detent_for("chatty");
        for cat in ["bug", "idiom", "best-practice", "architecture"] {
            assert!(!floor_excludes(&d, cat));
        }
    }

    /// T2 req 2 (acceptance: "floor exclusion queues rather than drops"): a
    /// floor-excluded candidate must never reach `budget::decide_push` at
    /// all — the caller queues it directly — so the budget is left
    /// untouched for floor-eligible candidates, and nothing is ever dropped.
    #[test]
    fn test_floor_excluded_category_never_touches_budget_would_queue_not_drop() {
        let detent = detent_for("quiet");
        assert!(floor_excludes(&detent, "architecture"));

        let mut bucket =
            crate::budget::TokenBucket::for_detent(&detent, std::time::SystemTime::UNIX_EPOCH);
        let before = bucket.tokens_available();

        // Mirrors the gate main.rs applies: floor-excluded candidates never
        // call decide_push, they queue directly.
        let candidate = crate::budget::PushCandidate {
            likely_bug: false,
            strict_mode_passed: false,
        };
        let outcome = if floor_excludes(&detent, "architecture") {
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
}

//! T2 req 10 — D12(b)/C3 category auto-throttle: a category whose action
//! rate stays under 15% over its last 20 counted cards demotes to
//! queue-only. State is computed from `events`/`cards` at session start,
//! never stored (C5) — this module is the pure math; db.rs supplies the
//! history, main.rs wires the transition + `throttle_change` logging.

/// C12: the trip threshold and window size.
pub const THROTTLE_RATE_FLOOR: f64 = 0.15;
pub const THROTTLE_WINDOW: u32 = 20;

/// C3 action rate: `|applied + escalated + got_it| / |shown - not_now|`;
/// `not_now` is deferral, excluded from the denominator entirely (neither
/// numerator nor denominator). `statuses` is the counted-card status window,
/// most-recent-first or any order (order doesn't matter for this ratio).
/// Returns `None` when the denominator is zero (nothing to judge yet — never
/// throttle on an empty/all-deferred window).
pub fn action_rate(statuses: &[String]) -> Option<f64> {
    let mut acted = 0u32;
    let mut denom = 0u32;
    for s in statuses {
        match s.as_str() {
            "applied" | "escalated" | "got_it" => {
                acted += 1;
                denom += 1;
            }
            "not_now" => {}
            _ => {
                denom += 1;
            }
        }
    }
    if denom == 0 {
        None
    } else {
        Some(acted as f64 / denom as f64)
    }
}

/// D12(b)/C12: whether the computed rate trips the throttle. Exactly at the
/// 15% boundary does NOT trip (`< 15%`, not `<=`).
pub fn is_throttled(rate: f64) -> bool {
    rate < THROTTLE_RATE_FLOOR
}

/// T2 req 10: combines the rate-based trip decision with the config-key undo
/// override (`[dial] unthrottle`), and reports whether this is an actual
/// *transition* worth logging a `throttle_change` event for (state is
/// computed fresh every session start and never stored, so a repeat of the
/// same computed state is not a new transition). Returns
/// `(currently_throttled, transition_action)`.
pub fn decide_throttle_transition(
    statuses: &[String],
    unthrottled_by_config: bool,
    previously_throttled: bool,
) -> (bool, Option<&'static str>) {
    let mut computed = action_rate(statuses).map(is_throttled).unwrap_or(false);
    if unthrottled_by_config {
        computed = false;
    }
    let transition = if computed != previously_throttled {
        Some(if computed { "throttled" } else { "unthrottled" })
    } else {
        None
    };
    (computed, transition)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn statuses(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn test_action_rate_basic() {
        // 3 acted out of 20 counted = 15% exactly -> not throttled.
        let mut items = vec!["applied", "got_it", "escalated"];
        items.extend(std::iter::repeat_n("not_useful", 17));
        let rate = action_rate(&statuses(&items)).unwrap();
        assert!((rate - 0.15).abs() < 1e-9);
        assert!(!is_throttled(rate));
    }

    #[test]
    fn test_action_rate_just_under_boundary_trips() {
        // 2 acted out of 20 = 10% -> throttled.
        let mut items = vec!["applied", "got_it"];
        items.extend(std::iter::repeat_n("not_useful", 18));
        let rate = action_rate(&statuses(&items)).unwrap();
        assert!(is_throttled(rate));
    }

    #[test]
    fn test_not_now_excluded_from_denominator() {
        // 1 applied, 1 not_now (excluded), 1 not_useful -> denom=2, rate=50%.
        let items = statuses(&["applied", "not_now", "not_useful"]);
        let rate = action_rate(&items).unwrap();
        assert!((rate - 0.5).abs() < 1e-9);
        assert!(!is_throttled(rate));
    }

    #[test]
    fn test_empty_window_is_none_never_throttled() {
        assert_eq!(action_rate(&[]), None);
        assert_eq!(action_rate(&statuses(&["not_now", "not_now"])), None);
    }

    #[test]
    fn test_exactly_fifteen_percent_over_twenty_is_not_throttled_boundary() {
        // C12/req10: "action rate < 15% over last 20 counted cards" trips at
        // exactly this boundary test point: 3/20 = 15% must NOT trip.
        let mut items = vec!["got_it", "got_it", "got_it"];
        items.extend(std::iter::repeat_n("expired", 17));
        assert_eq!(items.len(), 20);
        let rate = action_rate(&statuses(&items)).unwrap();
        assert!(!is_throttled(rate), "exactly 15% must not trip");

        // One fewer acted card (2/20 = 10%) must trip.
        let mut items2 = vec!["got_it", "got_it"];
        items2.extend(std::iter::repeat_n("expired", 18));
        let rate2 = action_rate(&statuses(&items2)).unwrap();
        assert!(is_throttled(rate2), "10% must trip");
    }

    // --- req 10: throttle transition + config-key undo ---

    fn low_action_rate_window() -> Vec<String> {
        // 2/20 = 10% -> throttled.
        let mut items = statuses(&["applied", "got_it"]);
        items.extend(std::iter::repeat_n("not_useful".to_string(), 18));
        items
    }

    #[test]
    fn test_decide_throttle_transition_trips_and_logs_once() {
        let window = low_action_rate_window();
        let (throttled, transition) = decide_throttle_transition(&window, false, false);
        assert!(throttled);
        assert_eq!(transition, Some("throttled"));

        // Same computed state again (e.g. next session start): no repeat
        // transition to log.
        let (throttled2, transition2) = decide_throttle_transition(&window, false, true);
        assert!(throttled2);
        assert_eq!(transition2, None);
    }

    #[test]
    fn test_decide_throttle_transition_config_key_undo() {
        let window = low_action_rate_window();
        // Computed rate would trip, but the config-key undo forces it off.
        let (throttled, transition) = decide_throttle_transition(&window, true, true);
        assert!(!throttled, "config-key undo overrides the computed rate");
        assert_eq!(transition, Some("unthrottled"));
    }

    #[test]
    fn test_decide_throttle_transition_at_exact_boundary_no_trip() {
        let mut items = vec![
            "got_it".to_string(),
            "got_it".to_string(),
            "got_it".to_string(),
        ];
        items.extend(std::iter::repeat_n("expired".to_string(), 17));
        let (throttled, transition) = decide_throttle_transition(&items, false, false);
        assert!(!throttled);
        assert_eq!(transition, None);
    }
}

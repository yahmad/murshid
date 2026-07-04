//! T5 req 6 / C8, C12 — staleness windows per taxonomy category. Staleness
//! never lowers `p_mastery` itself (no silent decay in v1, I26/req 6); it
//! only makes a concept eligible for req 7's retrieval questions.

use std::time::Duration;

/// C12: staleness windows per category. `bug` is n/a (`None`) — bugs aren't
/// "retained knowledge" that goes stale the way an idiom/practice does.
/// `Category::Other` (a pack-authored category the engine doesn't recognize)
/// gets the same `None` an unmatched string used to fall through to.
pub fn staleness_window(category: &crate::pack::Category) -> Option<Duration> {
    use crate::pack::Category;
    match category {
        Category::Idiom => Some(Duration::from_secs(21 * 24 * 3600)),
        Category::BestPractice => Some(Duration::from_secs(30 * 24 * 3600)),
        Category::Architecture => Some(Duration::from_secs(60 * 24 * 3600)),
        Category::Bug => None,
        Category::Other(_) => None,
    }
}

/// req 7: staleness backs off ×2 per retrieval skip — the re-eligibility
/// window widens each time the user skips the recall question.
pub fn backoff_multiplier(skip_count: u32) -> u32 {
    1u32.checked_shl(skip_count).unwrap_or(u32::MAX)
}

/// req 6: a below-1.0-retention concept is stale when the elapsed time
/// since its last encounter exceeds its category's window AND p is at
/// least 0.7 (things worth retaining). Categories with no staleness window
/// (`bug`) are never stale. `skip_count` applies req 7's ×2-per-skip
/// backoff to the window before comparing.
pub fn is_stale(
    category: &crate::pack::Category,
    p_mastery: f64,
    elapsed_since_last_encounter: Duration,
    skip_count: u32,
) -> bool {
    const RETENTION_WORTH_THRESHOLD: f64 = 0.7;
    if p_mastery < RETENTION_WORTH_THRESHOLD {
        return false;
    }
    let Some(window) = staleness_window(category) else {
        return false;
    };
    let backed_off = window.saturating_mul(backoff_multiplier(skip_count));
    elapsed_since_last_encounter > backed_off
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::Category;

    #[test]
    fn test_staleness_window_per_category() {
        assert_eq!(
            staleness_window(&Category::Idiom),
            Some(Duration::from_secs(21 * 86400))
        );
        assert_eq!(
            staleness_window(&Category::BestPractice),
            Some(Duration::from_secs(30 * 86400))
        );
        assert_eq!(
            staleness_window(&Category::Architecture),
            Some(Duration::from_secs(60 * 86400))
        );
        assert_eq!(staleness_window(&Category::Bug), None);
    }

    #[test]
    fn test_unknown_category_never_stale_same_as_bug() {
        assert_eq!(staleness_window(&Category::parse("nonsense")), None);
    }

    #[test]
    fn test_bug_category_never_stale() {
        assert!(!is_stale(
            &Category::Bug,
            0.99,
            Duration::from_secs(999 * 86400),
            0
        ));
    }

    #[test]
    fn test_below_retention_threshold_never_stale() {
        // p=0.69 < 0.7: not "worth retaining" yet, never stale regardless of elapsed time.
        assert!(!is_stale(
            &Category::Idiom,
            0.69,
            Duration::from_secs(999 * 86400),
            0
        ));
    }

    #[test]
    fn test_stale_boundary_exact_window_not_stale_strictly_greater_required() {
        let window = staleness_window(&Category::Idiom).unwrap();
        assert!(
            !is_stale(&Category::Idiom, 0.9, window, 0),
            "exactly at the window must not be stale"
        );
        assert!(is_stale(
            &Category::Idiom,
            0.9,
            window + Duration::from_secs(1),
            0
        ));
    }

    #[test]
    fn test_backoff_multiplier_doubles_per_skip() {
        assert_eq!(backoff_multiplier(0), 1);
        assert_eq!(backoff_multiplier(1), 2);
        assert_eq!(backoff_multiplier(2), 4);
        assert_eq!(backoff_multiplier(3), 8);
    }

    #[test]
    fn test_is_stale_respects_skip_backoff() {
        let window = staleness_window(&Category::Idiom).unwrap();
        // Just past the raw window: stale with no skips...
        let just_past = window + Duration::from_secs(1);
        assert!(is_stale(&Category::Idiom, 0.9, just_past, 0));
        // ...but not stale anymore after one skip doubles the window.
        assert!(!is_stale(&Category::Idiom, 0.9, just_past, 1));
    }
}

//! Quiescence gate (D8, C12 default): judge only after a save, a pause, and
//! a clean parse.

use std::time::{Duration, SystemTime};

/// C12 default: 2s typing/file-event pause after save.
pub const QUIESCENCE_PAUSE: Duration = Duration::from_secs(2);

/// True once at least `QUIESCENCE_PAUSE` has elapsed since the last file
/// event.
pub fn is_quiescent(last_event_at: SystemTime, now: SystemTime) -> bool {
    now.duration_since(last_event_at)
        .map(|gap| gap >= QUIESCENCE_PAUSE)
        .unwrap_or(false)
}

/// T1 req 4: judge only when a watched file was saved, the quiescence pause
/// elapsed, and the changed file parses (tree-sitter-rust; parse errors mean
/// "wait").
pub fn should_judge(last_event_at: SystemTime, now: SystemTime, current_content: &str) -> bool {
    is_quiescent(last_event_at, now) && crate::site::parses_without_errors(current_content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    #[test]
    fn test_not_quiescent_before_pause_elapses() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1000);
        assert!(!is_quiescent(t0, t0 + Duration::from_millis(500)));
        assert!(!is_quiescent(t0, t0 + Duration::from_millis(1999)));
    }

    #[test]
    fn test_quiescent_after_pause_elapses() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1000);
        assert!(is_quiescent(t0, t0 + Duration::from_secs(2)));
        assert!(is_quiescent(t0, t0 + Duration::from_secs(5)));
    }

    #[test]
    fn test_should_judge_waits_on_parse_error() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1000);
        let now = t0 + Duration::from_secs(3);
        let broken_source = "fn main() {\n    let x = 1;\n"; // missing closing brace
        assert!(!should_judge(t0, now, broken_source));
    }

    #[test]
    fn test_should_judge_true_when_quiescent_and_parses() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1000);
        let now = t0 + Duration::from_secs(3);
        let ok_source = "fn main() {\n    let x = 1;\n}\n";
        assert!(should_judge(t0, now, ok_source));
    }

    #[test]
    fn test_should_judge_false_when_not_quiescent_even_if_parses() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1000);
        let now = t0 + Duration::from_millis(500);
        let ok_source = "fn main() {}\n";
        assert!(!should_judge(t0, now, ok_source));
    }
}

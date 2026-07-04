//! T3 reqs 7-10 — struggle signals: repeated same-error, time-in-red vs the
//! user's own baseline, their convergence gate, and the self-declared
//! help-flavored-comment channel (D15, C12, I14).

// --- signal 1: repeated same-error (req 7) ---

/// C12: three consecutive same-primary-error check failures.
pub const SAME_ERROR_STREAK_TRIGGER: u32 = 3;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ErrorStreak {
    code: Option<String>,
    count: u32,
}

impl ErrorStreak {
    pub fn new() -> Self {
        Self::default()
    }

    /// Observes one check result. `primary_code` is the top-priority
    /// diagnostic's E-code when the check failed (`None` on an infra
    /// error/no diagnostics). Resets to zero on green OR on a different
    /// code; increments on a repeat of the same code.
    pub fn observe(&mut self, success: bool, primary_code: Option<&str>) {
        if success {
            self.code = None;
            self.count = 0;
            return;
        }
        match (self.code.as_deref(), primary_code) {
            (Some(c), Some(p)) if c == p => self.count += 1,
            (_, Some(p)) => {
                self.code = Some(p.to_string());
                self.count = 1;
            }
            (_, None) => {
                self.code = None;
                self.count = 0;
            }
        }
    }

    pub fn fired(&self) -> bool {
        self.count >= SAME_ERROR_STREAK_TRIGGER
    }

    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }

    pub fn count(&self) -> u32 {
        self.count
    }
}

// --- signal 2: time-in-red beyond self-baseline (req 8) ---

/// C12 cold start: 25 min.
pub const COLD_START_BASELINE_MS: u128 = 25 * 60 * 1000;

/// One `check_result` event's essentials for baseline computation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckResultPoint {
    pub success: bool,
    pub ts_ms: u128,
}

/// D15's baseline input: every red-streak-to-green duration found in
/// chronological `points` (across all sessions — "the user's own baseline").
pub fn time_to_green_durations_ms(points: &[CheckResultPoint]) -> Vec<u128> {
    let mut out = Vec::new();
    let mut red_since: Option<u128> = None;
    for p in points {
        if !p.success {
            if red_since.is_none() {
                red_since = Some(p.ts_ms);
            }
        } else if let Some(start) = red_since.take() {
            out.push(p.ts_ms.saturating_sub(start));
        }
    }
    out
}

/// C12: the 75th-percentile time-to-green; cold start (no history yet) is
/// the 25-minute constant.
pub fn percentile_75_ms(durations: &[u128]) -> u128 {
    if durations.is_empty() {
        return COLD_START_BASELINE_MS;
    }
    let mut sorted = durations.to_vec();
    sorted.sort_unstable();
    let idx = ((sorted.len() as f64) * 0.75).ceil() as usize;
    let idx = idx.saturating_sub(1).min(sorted.len() - 1);
    sorted[idx]
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RedStreak {
    red_since_ms: Option<u128>,
}

impl RedStreak {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn observe(&mut self, success: bool, now_ms: u128) {
        if success {
            self.red_since_ms = None;
        } else if self.red_since_ms.is_none() {
            self.red_since_ms = Some(now_ms);
        }
    }

    pub fn fired(&self, now_ms: u128, baseline_ms: u128) -> bool {
        self.red_since_ms
            .map(|since| now_ms.saturating_sub(since) >= baseline_ms)
            .unwrap_or(false)
    }

    pub fn minutes_in_red(&self, now_ms: u128) -> u64 {
        self.red_since_ms
            .map(|since| (now_ms.saturating_sub(since) / 60_000) as u64)
            .unwrap_or(0)
    }

    pub fn is_red(&self) -> bool {
        self.red_since_ms.is_some()
    }
}

// --- convergence gate (req 9 / I14) ---

/// I14: no proactive prompt fires on a single inferred signal; it takes
/// both signal 1 and signal 2 agreeing simultaneously.
pub fn inferred_pair_converged(same_error_fired: bool, time_in_red_fired: bool) -> bool {
    same_error_fired && time_in_red_fired
}

// --- signal 3: fresh help-flavored comment (req 10) ---

/// req 10: help-seeking phrasing, in the spec's own most-specific-first
/// order — the compound requirement-debt phrase before the shorter/more
/// generic phrases it could otherwise be mistaken to overlap with.
pub const DEFAULT_HELP_PATTERNS: &[&str] = &[
    "doesn't handle", // paired with a "yet" check below
    "doesn't work",
    "why does",
    "how does",
    "how do",
    "not sure",
    "stuck",
];

fn contains_issue_ref(text: &str) -> bool {
    let bytes = text.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'#' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
            return true;
        }
    }
    for word in text.split_whitespace() {
        let word = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '-');
        if let Some(dash) = word.rfind('-') {
            let (prefix, suffix) = (&word[..dash], &word[dash + 1..]);
            if prefix.len() >= 2
                && prefix.chars().all(|c| c.is_ascii_uppercase())
                && !suffix.is_empty()
                && suffix.chars().all(|c| c.is_ascii_digit())
            {
                return true;
            }
        }
    }
    false
}

fn contains_version_condition(text: &str) -> bool {
    for word in text.split_whitespace() {
        let word = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '.');
        let stripped = word.strip_prefix(['v', 'V']).unwrap_or(word);
        if stripped.contains('.') {
            let parts: Vec<&str> = stripped.split('.').collect();
            if parts.len() >= 2
                && parts
                    .iter()
                    .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
            {
                return true;
            }
        }
    }
    false
}

fn contains_when_lands_or_fixed(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("when ")
        && (lower.contains("lands")
            || lower.contains("landed")
            || lower.contains("fixed")
            || lower.contains("merges")
            || lower.contains("merged")
            || lower.contains("ships")
            || lower.contains("shipped"))
}

/// req 10: on-hold TODOs (blocked on external state) are excluded — this is
/// the most-specific gate, checked before any generic help-seeking pattern.
/// `on_hold_patterns` is pack-seeded literal phrasing layered on top of the
/// structural (language-agnostic) issue-ref/version/"when X lands" checks.
pub fn is_on_hold_todo(text: &str, on_hold_patterns: &[String]) -> bool {
    if contains_issue_ref(text)
        || contains_version_condition(text)
        || contains_when_lands_or_fixed(text)
    {
        return true;
    }
    let lower = text.to_lowercase();
    on_hold_patterns
        .iter()
        .any(|p| lower.contains(&p.to_lowercase()))
}

/// req 10: does `text` (a comment's body, comment-token already stripped)
/// match help-seeking phrasing? Exclusion is checked first (most-specific-
/// first per repo convention), then the ordered positive pattern list.
pub fn is_help_flavored_comment(
    text: &str,
    help_patterns: &[String],
    on_hold_patterns: &[String],
) -> bool {
    if is_on_hold_todo(text, on_hold_patterns) {
        return false;
    }
    if text.trim_end().ends_with('?') {
        return true;
    }
    let lower = text.to_lowercase();
    for pattern in help_patterns {
        let pattern_lower = pattern.to_lowercase();
        if lower.contains(&pattern_lower) {
            // Requirement-debt phrasing needs its "yet" too, else it's just
            // a plain code comment mentioning a gap, not help-seeking.
            if pattern_lower == "doesn't handle" && !lower.contains("yet") {
                continue;
            }
            return true;
        }
    }
    false
}

/// Strips a leading pack comment token (e.g. `"//"`, `"#"`) and surrounding
/// whitespace from a source line, for matching its natural-language content.
pub fn strip_comment_token<'a>(line: &'a str, comment_token: &str) -> &'a str {
    line.trim_start()
        .strip_prefix(comment_token)
        .unwrap_or(line)
        .trim()
}

/// req 10: scans a diff's newly-ADDED lines (I1: advice only ever attaches
/// to added lines) for fresh help-flavored comments.
pub fn find_fresh_help_comments(
    hunks: &[crate::diff::Hunk],
    comment_token: &str,
    help_patterns: &[String],
    on_hold_patterns: &[String],
) -> Vec<String> {
    let mut out = Vec::new();
    for hunk in hunks {
        for op in &hunk.ops {
            if let crate::diff::DiffOp::Added { text, .. } = op {
                let trimmed = text.trim_start();
                if trimmed.starts_with(comment_token) {
                    let body = strip_comment_token(trimmed, comment_token);
                    if !body.is_empty()
                        && is_help_flavored_comment(body, help_patterns, on_hold_patterns)
                    {
                        out.push(body.to_string());
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn help_patterns() -> Vec<String> {
        DEFAULT_HELP_PATTERNS
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    // --- signal 1 ---

    #[test]
    fn test_error_streak_fires_on_three_consecutive_same_code() {
        let mut s = ErrorStreak::new();
        s.observe(false, Some("E0308"));
        assert!(!s.fired());
        s.observe(false, Some("E0308"));
        assert!(!s.fired());
        s.observe(false, Some("E0308"));
        assert!(s.fired());
        assert_eq!(s.code(), Some("E0308"));
    }

    #[test]
    fn test_error_streak_resets_on_different_code() {
        let mut s = ErrorStreak::new();
        s.observe(false, Some("E0308"));
        s.observe(false, Some("E0308"));
        s.observe(false, Some("E0502")); // different code resets
        assert_eq!(s.count(), 1);
        assert!(!s.fired());
    }

    #[test]
    fn test_error_streak_resets_on_green() {
        let mut s = ErrorStreak::new();
        s.observe(false, Some("E0308"));
        s.observe(false, Some("E0308"));
        s.observe(true, None); // green resets
        assert_eq!(s.count(), 0);
        s.observe(false, Some("E0308"));
        assert!(!s.fired());
    }

    // --- signal 2: baseline ---

    #[test]
    fn test_time_to_green_durations_from_fixture_events() {
        let points = vec![
            CheckResultPoint {
                success: false,
                ts_ms: 0,
            },
            CheckResultPoint {
                success: true,
                ts_ms: 5_000,
            },
            CheckResultPoint {
                success: false,
                ts_ms: 10_000,
            },
            CheckResultPoint {
                success: false,
                ts_ms: 11_000,
            },
            CheckResultPoint {
                success: true,
                ts_ms: 20_000,
            },
        ];
        let durations = time_to_green_durations_ms(&points);
        assert_eq!(durations, vec![5_000, 10_000]);
    }

    #[test]
    fn test_percentile_75_cold_start_default() {
        assert_eq!(percentile_75_ms(&[]), COLD_START_BASELINE_MS);
    }

    #[test]
    fn test_percentile_75_from_fixture_durations() {
        // 10 evenly spaced durations 1..=10 minutes (in ms); 75th pct index
        // = ceil(0.75*10)-1 = 7 (0-indexed) -> the 8th value (8 min).
        let durations: Vec<u128> = (1..=10).map(|m| m * 60_000).collect();
        let p75 = percentile_75_ms(&durations);
        assert_eq!(p75, 8 * 60_000);
    }

    #[test]
    fn test_red_streak_fires_beyond_baseline() {
        let mut r = RedStreak::new();
        r.observe(false, 0);
        assert!(!r.fired(10_000, 25 * 60_000));
        assert!(r.fired(25 * 60_000, 25 * 60_000));
    }

    #[test]
    fn test_red_streak_resets_on_green() {
        let mut r = RedStreak::new();
        r.observe(false, 0);
        r.observe(true, 5_000);
        assert!(!r.is_red());
        assert!(!r.fired(1_000_000, 25 * 60_000));
    }

    #[test]
    fn test_red_streak_minutes_in_red() {
        let mut r = RedStreak::new();
        r.observe(false, 0);
        assert_eq!(r.minutes_in_red(14 * 60_000), 14);
    }

    // --- convergence (req 9 / I14) ---

    #[test]
    fn test_convergence_requires_both_signals() {
        assert!(!inferred_pair_converged(true, false));
        assert!(!inferred_pair_converged(false, true));
        assert!(!inferred_pair_converged(false, false));
        assert!(inferred_pair_converged(true, true));
    }

    // --- signal 3: help-flavored comments ---

    #[test]
    fn test_question_mark_ending_is_help_flavored() {
        assert!(is_help_flavored_comment(
            "why is this unwrap safe?",
            &help_patterns(),
            &[]
        ));
    }

    #[test]
    fn test_named_help_phrases_are_flavored() {
        for phrase in [
            "not sure this is the right approach",
            "why does this need a clone",
            "how do I avoid this allocation",
            "this doesn't work on windows",
            "stuck on this borrow",
        ] {
            assert!(
                is_help_flavored_comment(phrase, &help_patterns(), &[]),
                "expected help-flavored: {}",
                phrase
            );
        }
    }

    #[test]
    fn test_requirement_debt_phrasing_needs_yet() {
        assert!(is_help_flavored_comment(
            "doesn't handle unicode yet",
            &help_patterns(),
            &[]
        ));
        // "doesn't handle" without "yet" is not the requirement-debt signal.
        assert!(!is_help_flavored_comment(
            "doesn't handle the edge case at all",
            &help_patterns(),
            &[]
        ));
    }

    #[test]
    fn test_plain_comment_is_not_help_flavored() {
        assert!(!is_help_flavored_comment(
            "increment the counter here",
            &help_patterns(),
            &[]
        ));
    }

    // --- on-hold exclusion, most-specific-first ordering ---

    #[test]
    fn test_on_hold_issue_ref_excluded() {
        assert!(is_on_hold_todo("blocked on #234", &[]));
        assert!(is_on_hold_todo("see AUTH-42 before touching this", &[]));
        assert!(!is_on_hold_todo("just a normal comment", &[]));
    }

    #[test]
    fn test_on_hold_version_condition_excluded() {
        assert!(is_on_hold_todo("remove this once v1.2.0 ships", &[]));
        assert!(is_on_hold_todo("safe to delete after 2.0.1", &[]));
    }

    #[test]
    fn test_on_hold_when_lands_or_fixed_excluded() {
        assert!(is_on_hold_todo("remove when upstream lands the fix", &[]));
        assert!(is_on_hold_todo("todo: revisit when this is fixed", &[]));
    }

    #[test]
    fn test_on_hold_pack_seeded_patterns() {
        let on_hold = vec!["once merged".to_string()];
        assert!(is_on_hold_todo("cleanup once merged", &on_hold));
    }

    /// The order-sensitive case: a comment that matches BOTH a positive
    /// help-seeking phrase ("doesn't handle X yet") AND an on-hold
    /// exclusion pattern (an issue ref) must be excluded — the exclusion
    /// check must run first (most-specific-first per repo convention), or
    /// this would incorrectly fire as a struggle signal.
    #[test]
    fn test_on_hold_exclusion_checked_before_positive_pattern() {
        let text = "doesn't handle unicode yet, blocked on #234";
        assert!(is_on_hold_todo(text, &[]));
        assert!(
            !is_help_flavored_comment(text, &help_patterns(), &[]),
            "on-hold exclusion must win over the overlapping positive phrase"
        );
    }

    #[test]
    fn test_strip_comment_token() {
        assert_eq!(
            strip_comment_token("  // why does this work?", "//"),
            "why does this work?"
        );
        assert_eq!(strip_comment_token("not a comment", "//"), "not a comment");
    }

    #[test]
    fn test_find_fresh_help_comments_only_scans_added_lines() {
        let old = "fn a() {}\n";
        let new = "fn a() {\n    // why does this need a clone?\n}\n";
        let hunks = crate::diff::diff_lines(old, new);
        let found = find_fresh_help_comments(&hunks, "//", &help_patterns(), &[]);
        assert_eq!(found, vec!["why does this need a clone?".to_string()]);
    }

    #[test]
    fn test_find_fresh_help_comments_excludes_on_hold() {
        let old = "fn a() {}\n";
        let new = "fn a() {\n    // doesn't handle unicode yet, see #234\n}\n";
        let hunks = crate::diff::diff_lines(old, new);
        let found = find_fresh_help_comments(&hunks, "//", &help_patterns(), &[]);
        assert!(found.is_empty());
    }

    #[test]
    fn test_find_fresh_help_comments_ignores_non_comment_added_lines() {
        let old = "fn a() {}\n";
        let new = "fn a() {\n    let x = 1; // why does this need a clone?\n}\n";
        let hunks = crate::diff::diff_lines(old, new);
        // Not a comment-only line (code precedes the token) -> not scanned.
        let found = find_fresh_help_comments(&hunks, "//", &help_patterns(), &[]);
        assert!(found.is_empty());
    }
}

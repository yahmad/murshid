//! T16b — edit-log capture: a session-scoped, bounded, per-file ring buffer
//! of content-level edit events (timestamp, content hash, line-count
//! delta) — the substrate the two-point `session::compute_session_diff`
//! lacks. A written-then-reverted line is invisible to a before/after
//! snapshot; a content hash *reappearing* in a file's ring is exactly that
//! revert made visible. A high rate of distinct hashes is thrashing/
//! rewrite churn. Never stores full file contents — only their hash and a
//! line count, and only up to [`EDIT_LOG_CAP_PER_FILE`] entries per file.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Config-grade (per C1): the ring buffer's per-file retention bound —
/// oldest entries drop first once a file's ring exceeds this.
pub const EDIT_LOG_CAP_PER_FILE: usize = 20;

/// Config-grade: the recency window [`EditLog::churn_summary`]'s
/// `edits_in_window` counts against ("edits-in-last-N-min").
pub const CHURN_WINDOW: Duration = Duration::from_secs(10 * 60);

/// Config-grade: the T16b cost pre-gate's "churn above a base rate"
/// threshold — this many edits to the SAME file within [`CHURN_WINDOW`]
/// counts as warm enough to justify a perception-pass model call.
pub const CHURN_PRE_GATE_THRESHOLD: usize = 4;

/// One observed content-level edit event for a single file.
#[derive(Debug, Clone, PartialEq)]
pub struct EditEntry {
    pub ts: SystemTime,
    pub content_hash: String,
    /// Total line count of the file at this snapshot.
    pub line_count: usize,
    /// The absolute line-count delta vs the immediately-prior entry for
    /// this file (0 for a file's very first entry — nothing to compare
    /// against yet).
    pub changed_line_count: usize,
}

/// A compact churn summary of a file's recent edit history — what the
/// T16b perception pass reads, never the raw per-entry ring.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChurnSummary {
    /// Edits observed within the recency window ([`CHURN_WINDOW`]).
    pub edits_in_window: usize,
    /// Distinct content states across the file's whole retained ring.
    pub distinct_states: usize,
    /// Times a content hash reappeared (a revert) across the retained
    /// ring.
    pub revert_count: usize,
}

/// T16b: session-scoped, bounded edit-log — one ring buffer per touched
/// file, capped at [`EDIT_LOG_CAP_PER_FILE`] entries each. Lives on
/// `watch::WatchSession`, reset at every session split alongside the other
/// session-scoped tracking (`StruggleTracking`, `DriftTracking`, ...).
#[derive(Debug, Clone, Default)]
pub struct EditLog {
    per_file: HashMap<PathBuf, Vec<EditEntry>>,
}

impl EditLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one observed edit for `file` from its current `content` —
    /// hashes it (the content itself is never stored) and computes the
    /// line-count delta against the previous entry. Drops the oldest entry
    /// once the per-file ring exceeds [`EDIT_LOG_CAP_PER_FILE`].
    pub fn record(&mut self, file: &Path, ts: SystemTime, content: &str) {
        let content_hash = crate::sha256::sha256_hex(content.as_bytes());
        let line_count = content.lines().count();
        let entries = self.per_file.entry(file.to_path_buf()).or_default();
        let changed_line_count = entries
            .last()
            .map(|e| e.line_count.abs_diff(line_count))
            .unwrap_or(0);
        entries.push(EditEntry {
            ts,
            content_hash,
            line_count,
            changed_line_count,
        });
        if entries.len() > EDIT_LOG_CAP_PER_FILE {
            entries.remove(0);
        }
    }

    /// The compact churn summary the T16b perception pass feeds the
    /// model. An untouched (or never-recorded) file summarizes to all
    /// zeros.
    pub fn churn_summary(&self, file: &Path, now: SystemTime, window: Duration) -> ChurnSummary {
        let Some(entries) = self.per_file.get(file) else {
            return ChurnSummary::default();
        };
        let edits_in_window = entries
            .iter()
            .filter(|e| {
                now.duration_since(e.ts)
                    .map(|d| d <= window)
                    .unwrap_or(false)
            })
            .count();
        let distinct_states: HashSet<&str> =
            entries.iter().map(|e| e.content_hash.as_str()).collect();

        // A revert: a content hash that already appeared earlier in this
        // file's retained history reappearing later — most-specific-first
        // isn't relevant here (no compound-token ambiguity), but the scan
        // is still strictly chronological (the ring is append-ordered).
        let mut seen = HashSet::new();
        let mut revert_count = 0;
        for e in entries {
            if !seen.insert(e.content_hash.as_str()) {
                revert_count += 1;
            }
        }

        ChurnSummary {
            edits_in_window,
            distinct_states: distinct_states.len(),
            revert_count,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn t(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn test_edit_log_ring_buffer_is_bounded_per_file() {
        let mut log = EditLog::new();
        let file = Path::new("a.rs");
        for i in 0..(EDIT_LOG_CAP_PER_FILE + 5) {
            log.record(file, t(i as u64), &format!("fn a() {{ {} }}\n", i));
        }
        let summary = log.churn_summary(file, t(10_000), Duration::from_secs(10_000));
        // Every entry is distinct content here, so distinct_states IS the
        // retained ring length — proof the ring itself stayed capped.
        assert_eq!(summary.distinct_states, EDIT_LOG_CAP_PER_FILE);
    }

    #[test]
    fn test_edit_log_detects_revert_via_repeated_hash() {
        let mut log = EditLog::new();
        let file = Path::new("a.rs");
        log.record(file, t(0), "fn a() {}\n"); // state A
        log.record(file, t(1), "fn a() { 1 }\n"); // state B
        log.record(file, t(2), "fn a() {}\n"); // back to A: a revert

        let summary = log.churn_summary(file, t(10), Duration::from_secs(100));
        assert_eq!(summary.revert_count, 1);
        assert_eq!(summary.distinct_states, 2);
        assert_eq!(summary.edits_in_window, 3);
    }

    #[test]
    fn test_edit_log_no_revert_when_every_state_is_distinct() {
        let mut log = EditLog::new();
        let file = Path::new("a.rs");
        log.record(file, t(0), "fn a() {}\n");
        log.record(file, t(1), "fn a() { 1 }\n");
        log.record(file, t(2), "fn a() { 2 }\n");

        let summary = log.churn_summary(file, t(10), Duration::from_secs(100));
        assert_eq!(summary.revert_count, 0);
        assert_eq!(summary.distinct_states, 3);
    }

    #[test]
    fn test_edit_log_churn_summary_recency_window_excludes_stale_edits() {
        let mut log = EditLog::new();
        let file = Path::new("a.rs");
        log.record(file, t(0), "fn a() {}\n");
        log.record(file, t(1_000), "fn a() { 1 }\n"); // well outside the window below

        let summary = log.churn_summary(file, t(1_010), Duration::from_secs(30));
        assert_eq!(
            summary.edits_in_window, 1,
            "only the recent edit counts within a 30s window"
        );
        assert_eq!(
            summary.distinct_states, 2,
            "distinct_states still covers the whole retained ring, not just the window"
        );
    }

    #[test]
    fn test_edit_log_changed_line_count_tracks_line_delta_vs_prior() {
        let mut log = EditLog::new();
        let file = Path::new("a.rs");
        log.record(file, t(0), "line1\nline2\n"); // 2 lines, first entry -> 0
        log.record(file, t(1), "line1\nline2\nline3\nline4\n"); // 4 lines -> delta 2

        let entries = log.per_file.get(file).unwrap();
        assert_eq!(entries[0].changed_line_count, 0);
        assert_eq!(entries[1].changed_line_count, 2);
        assert_eq!(entries[1].line_count, 4);
    }

    #[test]
    fn test_edit_log_untouched_file_summarizes_to_zero() {
        let log = EditLog::new();
        let summary = log.churn_summary(Path::new("never-touched.rs"), t(0), CHURN_WINDOW);
        assert_eq!(summary, ChurnSummary::default());
    }
}

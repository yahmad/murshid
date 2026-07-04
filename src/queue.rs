//! T2 req 3/4 — the single pull queue: presence indicator, `m` browse list,
//! and C7 ordering (goal-relevance, then throttled-tail, then category rank,
//! then age). T3 req 5 fills in the goal-relevance term ahead of category
//! rank.

use crate::aggregate::AggregatedFinding;

/// One entry sitting in the pull queue, awaiting `m`/browse or session end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueEntry {
    pub finding: AggregatedFinding,
    /// Insertion order this session — the C7 tie-break "age" (older = shown
    /// first, i.e. smaller sequence number sorts first).
    pub seq: u64,
    /// C7: "throttled categories always tail", set at enqueue time from the
    /// category's currently-computed throttle state.
    pub throttled: bool,
    /// The `cards` row (`status='queued'`) already persisted for this
    /// finding — pulling this entry via `m` flips that row to `shown`
    /// in-place rather than inserting a second one.
    pub card_id: i64,
    pub session_id: String,
    /// T4 req 11 / C7 slot contention: set when a direct-ask answer
    /// displaced this entry from the single card slot on arrival — it must
    /// "return to the queue head", ahead of the normal goal/category/age
    /// ordering (this is a spec-mandated exception, not a generic priority
    /// rule: an architecture-category displaced card still jumps the queue).
    pub pinned_head: bool,
}

/// C7 category rank: bug > idiom > best-practice > architecture. Unknown
/// categories (`Category::Other`) rank last (never crash on unexpected pack
/// data) — exactly what an unmatched category string used to fall through
/// to. Shared with [`crate::review`] so the live queue and the solicited
/// review digest order findings identically.
pub(crate) fn category_rank(category: &crate::pack::Category) -> u8 {
    use crate::pack::Category;
    match category {
        Category::Bug => 0,
        Category::Idiom => 1,
        Category::BestPractice => 2,
        Category::Architecture => 3,
        Category::Other(_) => 4,
    }
}

/// T3 req 5 / C7: "goal-relevant first" — 0 when the finding's site file is
/// in the goal's file cluster or its concept is named in the goal text, 1
/// otherwise. An empty cluster + empty goal text ranks everything equal
/// (T2's original constant-zero shape, preserved when no goal is known).
fn goal_relevance_rank(
    cluster_dirs: &std::collections::HashSet<String>,
    goal_text: &str,
    finding: &AggregatedFinding,
) -> u8 {
    if crate::goal::is_goal_relevant(
        cluster_dirs,
        goal_text,
        &finding.card.file,
        &finding.card.concept_name,
    ) {
        0
    } else {
        1
    }
}

/// C7 ordering: T4 req 11 slot-contention pins first (ahead of everything,
/// including goal-relevance — a displaced direct-ask target unconditionally
/// returns to the queue head), then goal-relevance (T3 req 5), then
/// throttled-tail, then category rank, then age (insertion order).
pub fn sort_queue(
    entries: &mut [QueueEntry],
    goal_cluster_dirs: &std::collections::HashSet<String>,
    goal_text: &str,
) {
    entries.sort_by(|a, b| {
        b.pinned_head
            .cmp(&a.pinned_head)
            .then(
                goal_relevance_rank(goal_cluster_dirs, goal_text, &a.finding).cmp(
                    &goal_relevance_rank(goal_cluster_dirs, goal_text, &b.finding),
                ),
            )
            .then(a.throttled.cmp(&b.throttled))
            .then(
                category_rank(&crate::pack::Category::parse(&a.finding.category)).cmp(
                    &category_rank(&crate::pack::Category::parse(&b.finding.category)),
                ),
            )
            .then(a.seq.cmp(&b.seq))
    });
}

/// T2 req 3: "N more thoughts — m" — the one-line presence indicator; `None`
/// when the queue is empty (nothing to announce).
pub fn presence_indicator(queue_len: usize) -> Option<String> {
    if queue_len == 0 {
        None
    } else {
        Some(format!("{} more thoughts \u{2014} m", queue_len))
    }
}

/// T2 req 3: `m`'s plain numbered list — concept name + anchor, one line
/// each, in C7 order (the caller is expected to have already sorted
/// `entries` via [`sort_queue`]).
pub fn render_queue_list(entries: &[QueueEntry]) -> String {
    let mut out = String::new();
    for (i, entry) in entries.iter().enumerate() {
        out.push_str(&format!(
            "{}. {} \u{2014} {}:{}\n",
            i + 1,
            entry.finding.card.concept_name,
            entry.finding.card.file,
            entry.finding.card.line
        ));
    }
    out
}

/// T2 req 3: the session-end line when the queue bookend hasn't landed yet
/// (T3): "exiting prints a one-line count of unshown advice."
pub fn unshown_count_line(queue_len: usize) -> Option<String> {
    if queue_len == 0 {
        None
    } else {
        Some(format!(
            "{} thought{} went unshown this session.",
            queue_len,
            if queue_len == 1 { "" } else { "s" }
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card::Card;

    fn card(name: &str, file: &str, line: usize) -> Card {
        Card {
            concept_name: name.to_string(),
            file: file.to_string(),
            line,
            grounding_quote: "q".to_string(),
            why: "why".to_string(),
            rule: "rule".to_string(),
            doc_ref: "ref".to_string(),
            worked_diff: "diff".to_string(),
            additional_anchors: Vec::new(),
            overflow_site_count: 0,
        }
    }

    fn finding(category: &str, name: &str, file: &str, line: usize) -> AggregatedFinding {
        AggregatedFinding {
            concept_id: name.to_string(),
            category: category.to_string(),
            advice_fp: format!("{}-{}", name, file),
            card: card(name, file, line),
            likely_bug: false,
            strict_mode_passed: false,
            site_count: 1,
            remaining_sites: Vec::new(),
        }
    }

    fn entry(category: &str, name: &str, seq: u64, throttled: bool) -> QueueEntry {
        QueueEntry {
            finding: finding(category, name, "f.rs", 1),
            seq,
            throttled,
            card_id: seq as i64,
            session_id: "sess1".to_string(),
            pinned_head: false,
        }
    }

    #[test]
    fn test_presence_indicator_empty_is_none() {
        assert_eq!(presence_indicator(0), None);
    }

    #[test]
    fn test_presence_indicator_shows_count() {
        assert_eq!(
            presence_indicator(2),
            Some("2 more thoughts \u{2014} m".to_string())
        );
    }

    /// T2's original constant-zero shape: no goal known -> relevance ranks
    /// everything equal, falling through to category rank/age/throttle.
    fn no_goal() -> std::collections::HashSet<String> {
        std::collections::HashSet::new()
    }

    #[test]
    fn test_sort_by_category_rank() {
        let mut entries = vec![
            entry("architecture", "arch", 0, false),
            entry("bug", "bugc", 1, false),
            entry("idiom", "idiomc", 2, false),
            entry("best-practice", "bpc", 3, false),
        ];
        sort_queue(&mut entries, &no_goal(), "");
        let names: Vec<&str> = entries
            .iter()
            .map(|e| e.finding.concept_id.as_str())
            .collect();
        assert_eq!(names, vec!["bugc", "idiomc", "bpc", "arch"]);
    }

    #[test]
    fn test_sort_by_age_within_same_category() {
        let mut entries = vec![
            entry("idiom", "second", 5, false),
            entry("idiom", "first", 1, false),
        ];
        sort_queue(&mut entries, &no_goal(), "");
        assert_eq!(entries[0].finding.concept_id, "first");
        assert_eq!(entries[1].finding.concept_id, "second");
    }

    #[test]
    fn test_throttled_categories_always_tail() {
        let mut entries = vec![
            entry("bug", "throttled_bug", 0, true),
            entry("architecture", "plain_arch", 1, false),
        ];
        sort_queue(&mut entries, &no_goal(), "");
        // Even though bug outranks architecture, throttled always tails.
        assert_eq!(entries[0].finding.concept_id, "plain_arch");
        assert_eq!(entries[1].finding.concept_id, "throttled_bug");
    }

    // --- T4 req 11 / C7: slot contention pins the displaced card to the
    // queue head, ahead of even goal-relevance and category rank. ---

    #[test]
    fn test_pinned_head_wins_over_category_rank() {
        let mut entries = vec![
            entry("bug", "urgent_bug", 0, false),
            entry("architecture", "displaced_arch", 1, false),
        ];
        entries[1].pinned_head = true;
        sort_queue(&mut entries, &no_goal(), "");
        assert_eq!(
            entries[0].finding.concept_id, "displaced_arch",
            "a displaced card returns to the queue head, unconditionally"
        );
        assert_eq!(entries[1].finding.concept_id, "urgent_bug");
    }

    #[test]
    fn test_pinned_head_wins_over_goal_relevance() {
        let mut entries = vec![
            entry("bug", "goal_relevant_bug", 0, false),
            entry("idiom", "displaced_idiom", 1, false),
        ];
        entries[1].pinned_head = true;
        sort_queue(&mut entries, &no_goal(), "fix the goal_relevant_bug issue");
        assert_eq!(entries[0].finding.concept_id, "displaced_idiom");
    }

    #[test]
    fn test_multiple_pinned_entries_still_sort_among_themselves() {
        // Same category for both pinned entries so this isolates the
        // "pinned entries still order by age among themselves" behavior
        // from category rank.
        let mut entries = vec![
            entry("idiom", "not_pinned", 0, false),
            entry("architecture", "pinned_older", 1, false),
            entry("architecture", "pinned_newer", 2, false),
        ];
        entries[1].pinned_head = true;
        entries[2].pinned_head = true;
        sort_queue(&mut entries, &no_goal(), "");
        assert_eq!(entries[0].finding.concept_id, "pinned_older");
        assert_eq!(entries[1].finding.concept_id, "pinned_newer");
        assert_eq!(entries[2].finding.concept_id, "not_pinned");
    }

    // --- T3 req 5: goal-relevance precedes category rank ---

    #[test]
    fn test_goal_relevant_file_ranks_ahead_of_category() {
        let mut entries = vec![
            entry("bug", "unrelated_bug", 0, false),
            entry("architecture", "goal_relevant_arch", 1, false),
        ];
        entries[1].finding.card.file = "src/auth/login.rs".to_string();

        let mut cluster = std::collections::HashSet::new();
        cluster.insert("src/auth".to_string());

        sort_queue(&mut entries, &cluster, "");
        // architecture normally ranks last, but its site is in the goal
        // cluster, so it jumps ahead of the (goal-irrelevant) bug entry.
        assert_eq!(entries[0].finding.concept_id, "goal_relevant_arch");
        assert_eq!(entries[1].finding.concept_id, "unrelated_bug");
    }

    #[test]
    fn test_goal_relevant_concept_text_match_ranks_ahead() {
        let mut entries = vec![
            entry("bug", "unrelated_bug", 0, false),
            entry("architecture", "borrow-vs-clone", 1, false),
        ];

        sort_queue(&mut entries, &no_goal(), "fix the borrow-vs-clone overhead");
        assert_eq!(entries[0].finding.concept_id, "borrow-vs-clone");
        assert_eq!(entries[1].finding.concept_id, "unrelated_bug");
    }

    #[test]
    fn test_render_queue_list_numbered_with_anchor() {
        let entries = vec![
            entry("bug", "off-by-one", 0, false),
            entry("idiom", "borrow-vs-clone", 1, false),
        ];
        let rendered = render_queue_list(&entries);
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "1. off-by-one \u{2014} f.rs:1");
        assert_eq!(lines[1], "2. borrow-vs-clone \u{2014} f.rs:1");
    }

    #[test]
    fn test_unshown_count_line_pluralizes() {
        assert_eq!(unshown_count_line(0), None);
        assert_eq!(
            unshown_count_line(1),
            Some("1 thought went unshown this session.".to_string())
        );
        assert_eq!(
            unshown_count_line(3),
            Some("3 thoughts went unshown this session.".to_string())
        );
    }
}

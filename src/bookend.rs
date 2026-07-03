//! T3 req 6 — the session-end bookend (D14(b), C2's "queue's last call", I21
//! rendering rules): goal line, counts, concepts taught, throttled
//! categories, and the queue's top-3 one-liners. Auto, one screen, zero
//! required interaction.

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BookendCounts {
    pub shown: usize,
    pub applied: usize,
    pub queued_unshown: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookend {
    pub goal_line: String,
    pub counts: BookendCounts,
    /// Concept names only (I21/D14: no narrative, no scores here).
    pub concepts_taught: Vec<String>,
    pub throttled_categories: Vec<String>,
    /// Top 3 one-liners — the pull queue's last call before it dies at
    /// session end (C2).
    pub queue_last_call: Vec<String>,
    /// T4 req 7 / D20: card-anchored threads with no terminal C3 response
    /// yet, by concept name.
    pub unresolved_threads: Vec<String>,
    /// T4 req 10 / D17: murshid-comments with no terminal C3 response yet,
    /// by concept name.
    pub unresolved_comments: Vec<String>,
}

/// req 6: assembles the bookend from already-gathered pieces (the DB/queue
/// reads live in db.rs/main.rs; this is the pure assembly step).
#[allow(clippy::too_many_arguments)]
pub fn assemble_bookend(
    goal_text: Option<&str>,
    counts: BookendCounts,
    concepts_taught: Vec<String>,
    throttled_categories: Vec<String>,
    queue_last_call: Vec<String>,
    unresolved_threads: Vec<String>,
    unresolved_comments: Vec<String>,
) -> Bookend {
    let goal_line = match goal_text.map(str::trim) {
        Some(t) if !t.is_empty() => format!("goal: {}", t),
        _ => "goal: (none set)".to_string(),
    };
    Bookend {
        goal_line,
        counts,
        concepts_taught,
        throttled_categories,
        queue_last_call: queue_last_call.into_iter().take(3).collect(),
        unresolved_threads,
        unresolved_comments,
    }
}

/// I21: minimal, scannable, no color-only meaning, no interaction demanded.
/// T4 req 12 / D18: also carries the solicited-review offer, one line,
/// never auto-running.
pub fn render_bookend(b: &Bookend) -> String {
    let mut out = String::new();
    out.push_str("\u{2500}\u{2500}\u{2500} session bookend \u{2500}\u{2500}\u{2500}\n");
    out.push_str(&b.goal_line);
    out.push('\n');
    out.push_str(&format!(
        "cards: {} shown, {} applied, {} queued unshown\n",
        b.counts.shown, b.counts.applied, b.counts.queued_unshown
    ));
    if b.concepts_taught.is_empty() {
        out.push_str("concepts taught: none\n");
    } else {
        out.push_str(&format!(
            "concepts taught: {}\n",
            b.concepts_taught.join(", ")
        ));
    }
    if !b.throttled_categories.is_empty() {
        out.push_str(&format!(
            "throttled: {}\n",
            b.throttled_categories.join(", ")
        ));
    }
    if !b.unresolved_threads.is_empty() {
        out.push_str(&format!(
            "unresolved threads: {}\n",
            b.unresolved_threads.join(", ")
        ));
    }
    if !b.unresolved_comments.is_empty() {
        out.push_str(&format!(
            "unresolved murshid-comments: {}\n",
            b.unresolved_comments.join(", ")
        ));
    }
    if !b.queue_last_call.is_empty() {
        out.push_str("queue's last call:\n");
        for (i, line) in b.queue_last_call.iter().enumerate() {
            out.push_str(&format!("  {}. {}\n", i + 1, line));
        }
    }
    out.push_str(crate::review::REVIEW_OFFER_LINE);
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_assemble_bookend_goal_line_with_goal() {
        let b = assemble_bookend(Some("fix auth timeout"), BookendCounts::default(), vec![], vec![], vec![], vec![], vec![]);
        assert_eq!(b.goal_line, "goal: fix auth timeout");
    }

    #[test]
    fn test_assemble_bookend_goal_line_without_goal() {
        let b = assemble_bookend(None, BookendCounts::default(), vec![], vec![], vec![], vec![], vec![]);
        assert_eq!(b.goal_line, "goal: (none set)");
        let b2 = assemble_bookend(Some("   "), BookendCounts::default(), vec![], vec![], vec![], vec![], vec![]);
        assert_eq!(b2.goal_line, "goal: (none set)");
    }

    #[test]
    fn test_assemble_bookend_truncates_queue_last_call_to_three() {
        let queue = vec![
            "a — f.rs:1".to_string(),
            "b — f.rs:2".to_string(),
            "c — f.rs:3".to_string(),
            "d — f.rs:4".to_string(),
        ];
        let b = assemble_bookend(None, BookendCounts::default(), vec![], vec![], queue, vec![], vec![]);
        assert_eq!(b.queue_last_call.len(), 3);
        assert_eq!(b.queue_last_call[2], "c — f.rs:3");
    }

    #[test]
    fn test_render_bookend_counts_and_concepts() {
        let counts = BookendCounts {
            shown: 5,
            applied: 2,
            queued_unshown: 3,
        };
        let b = assemble_bookend(
            Some("fix auth timeout"),
            counts,
            vec!["Borrow vs. clone".to_string(), "Question mark propagation".to_string()],
            vec!["architecture".to_string()],
            vec!["idiom-chains \u{2014} a.rs:1".to_string()],
            vec![],
            vec![],
        );
        let rendered = render_bookend(&b);
        assert!(rendered.contains("goal: fix auth timeout"));
        assert!(rendered.contains("5 shown, 2 applied, 3 queued unshown"));
        assert!(rendered.contains("Borrow vs. clone, Question mark propagation"));
        assert!(rendered.contains("throttled: architecture"));
        assert!(rendered.contains("1. idiom-chains \u{2014} a.rs:1"));
    }

    #[test]
    fn test_render_bookend_empty_concepts_and_no_throttle_line() {
        let b = assemble_bookend(None, BookendCounts::default(), vec![], vec![], vec![], vec![], vec![]);
        let rendered = render_bookend(&b);
        assert!(rendered.contains("concepts taught: none"));
        assert!(!rendered.contains("throttled:"));
        assert!(!rendered.contains("queue's last call"));
    }

    // --- T4 reqs 7/10: unresolved threads/comments ---

    #[test]
    fn test_render_bookend_lists_unresolved_threads_and_comments() {
        let b = assemble_bookend(
            None,
            BookendCounts::default(),
            vec![],
            vec![],
            vec![],
            vec!["Borrow vs. clone".to_string()],
            vec!["Option combinators".to_string()],
        );
        let rendered = render_bookend(&b);
        assert!(rendered.contains("unresolved threads: Borrow vs. clone"));
        assert!(rendered.contains("unresolved murshid-comments: Option combinators"));
    }

    #[test]
    fn test_render_bookend_omits_unresolved_lines_when_empty() {
        let b = assemble_bookend(None, BookendCounts::default(), vec![], vec![], vec![], vec![], vec![]);
        let rendered = render_bookend(&b);
        assert!(!rendered.contains("unresolved threads:"));
        assert!(!rendered.contains("unresolved murshid-comments:"));
    }

    // --- T4 req 12 / D18: solicited review offered in the bookend ---

    #[test]
    fn test_render_bookend_always_offers_review() {
        let b = assemble_bookend(None, BookendCounts::default(), vec![], vec![], vec![], vec![], vec![]);
        let rendered = render_bookend(&b);
        assert!(rendered.contains("murshid review"));
    }
}

//! R2-form card rendering (I21 minimal, C3/C4 partial, T1 req 9-10).
//!
//! T1 always renders the R2 "teach" rung: `★ <concept name>` header, anchor
//! (file:line + quote), verdict+why, rule (+ doc ref), with the worked diff
//! folded behind a pointer to T4. ~80-col wrap; NO_COLOR honored; meaning is
//! never color-only (the ★ and text convey it regardless of color).

const WRAP_WIDTH: usize = 78;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Card {
    pub concept_name: String,
    pub file: String,
    pub line: usize,
    pub grounding_quote: String,
    pub why: String,
    pub rule: String,
    pub doc_ref: String,
    /// T1 req 9: exists in the card record even though it renders folded
    /// behind the "(fix available — full interaction in T4)" line — it must
    /// be a real, persisted value, not validated-then-discarded.
    pub worked_diff: String,
    /// T2 req 6: extra (file, line) anchors when the same concept was found
    /// at more than one site in the same judging sweep — up to 2 more (3
    /// total with the primary anchor above). Empty for a single-site card.
    pub additional_anchors: Vec<(String, usize)>,
    /// T2 req 6: count of sites beyond the 3 rendered anchors; recorded in
    /// the `card_shown` event payload, never rendered inline.
    pub overflow_site_count: usize,
}

/// Greedy word wrap to `width` columns; a single overlong word is placed on
/// its own line rather than split.
pub fn word_wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();

    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.len() + 1 + word.len() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(current);
            current = word.to_string();
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Whether ANSI color is permitted for this render: honors `NO_COLOR`
/// (https://no-color.org) per I21.
pub fn color_allowed() -> bool {
    std::env::var("NO_COLOR").is_err()
}

/// Renders the single on-screen R2 card. `queued_count` is T1's one-line
/// overflow counter ("N more queued — T2"); 0 means nothing queued.
pub fn render_card(card: &Card, queued_count: usize) -> String {
    render_card_with_color(card, queued_count, color_allowed())
}

pub fn render_card_with_color(card: &Card, queued_count: usize, use_color: bool) -> String {
    let mut out = String::new();

    let header = format!("\u{2605} {}", card.concept_name); // ★
    if use_color {
        out.push_str("\x1b[1;33m");
        out.push_str(&header);
        out.push_str("\x1b[0m");
    } else {
        out.push_str(&header);
    }
    out.push('\n');

    out.push_str(&format!("  {}:{}\n", card.file, card.line));
    out.push_str(&format!("  > {}\n", card.grounding_quote));
    out.push('\n');

    for line in word_wrap(&card.why, WRAP_WIDTH) {
        out.push_str("  ");
        out.push_str(&line);
        out.push('\n');
    }
    out.push('\n');

    let rule_line = format!("Rule: {} \u{2014} {}", card.rule, card.doc_ref); // —
    for line in word_wrap(&rule_line, WRAP_WIDTH) {
        out.push_str("  ");
        out.push_str(&line);
        out.push('\n');
    }

    out.push_str("  e to escalate, t for the fix, k to ask\n");

    // T2 req 6: same-sweep aggregation — up to 3 anchors rendered; sites
    // beyond that are recorded in the card_shown event payload, not here.
    let total_sites = 1 + card.additional_anchors.len() + card.overflow_site_count;
    if total_sites > 1 {
        out.push_str(&format!(
            "  this pattern appears in {} places\n",
            total_sites
        ));
        for (f, l) in &card.additional_anchors {
            out.push_str(&format!("    also: {}:{}\n", f, l));
        }
    }

    if queued_count > 0 {
        out.push_str(&format!("  {} more queued \u{2014} T2\n", queued_count));
    }

    out
}

/// T4 req 2-4 / C4: renders the card at its CURRENT rung. R2 is
/// [`render_card`] unchanged; R1 folds down to a one-line pointer (C4:
/// "one line naming the concept as a pointer or question"); R3 unfolds the
/// worked diff as a commented worked example in place of the folded pointer
/// line (I19).
pub fn render_card_at_rung(
    card: &Card,
    rung: crate::ladder::Rung,
    queued_count: usize,
    comment_token: &str,
) -> String {
    match rung {
        crate::ladder::Rung::R0 => render_r0_recall(card),
        crate::ladder::Rung::R1 => render_r1_nudge(card),
        crate::ladder::Rung::R2 => render_card(card, queued_count),
        crate::ladder::Rung::R3 => render_r3_worked_example(card, queued_count, comment_token),
    }
}

/// C4 R0 "recall": the generation moment — a question only, nothing
/// revealed until the user answers or escalates (D19: "a generation moment
/// selected only when the concept is partially known").
fn render_r0_recall(card: &Card) -> String {
    format!(
        "\u{2605} {}\n  {}:{}\n  > {}\n  how would you write this differently? (e for a hint, t for the fix)\n",
        card.concept_name, card.file, card.line, card.grounding_quote
    )
}

/// C4 R1 "nudge": a one-line pointer, nothing revealed until escalation.
fn render_r1_nudge(card: &Card) -> String {
    format!(
        "\u{2605} {}\n  {}:{}\n  (nudge \u{2014} e for more, t for the fix)\n",
        card.concept_name, card.file, card.line
    )
}

const FOLDED_FIX_LINE: &str = "  e to escalate, t for the fix, k to ask\n";

/// C4 R3 "worked example": the folded pointer line is replaced with the
/// unfolded, commented worked diff (I19).
fn render_r3_worked_example(card: &Card, queued_count: usize, comment_token: &str) -> String {
    let base = render_card(card, queued_count);
    let worked = crate::ladder::render_worked_example(&card.worked_diff, &card.why, comment_token);
    let mut worked_block = String::from("  worked example:\n");
    for line in worked.lines() {
        worked_block.push_str("  ");
        worked_block.push_str(line);
        worked_block.push('\n');
    }
    base.replacen(FOLDED_FIX_LINE, &worked_block, 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_card() -> Card {
        Card {
            concept_name: "Borrow vs. clone".to_string(),
            file: "src/main.rs".to_string(),
            line: 42,
            grounding_quote: "person.name.clone()".to_string(),
            why: "The call only reads the name, so cloning the String allocates and copies data a borrow would have served just as well.".to_string(),
            rule: "Take &str when the function only needs to read the value".to_string(),
            doc_ref: "https://example.com/pack-docs/redundant-clone".to_string(),
            worked_diff: "- fn print_name(name: String)\n+ fn print_name(name: &str)".to_string(),
            additional_anchors: Vec::new(),
            overflow_site_count: 0,
        }
    }

    #[test]
    fn test_word_wrap_respects_width() {
        let text = "The call only reads the name, so cloning the String allocates and copies data a borrow would have served just as well.";
        let lines = word_wrap(text, 78);
        for line in &lines {
            assert!(line.len() <= 78, "line too long ({}): {}", line.len(), line);
        }
        assert_eq!(lines.join(" "), text);
    }

    #[test]
    fn test_render_card_lines_stay_within_80_cols() {
        let card = sample_card();
        let rendered = render_card_with_color(&card, 0, false);
        for line in rendered.lines() {
            assert!(
                line.chars().count() <= 80,
                "line exceeds 80 cols ({}): {:?}",
                line.chars().count(),
                line
            );
        }
    }

    #[test]
    fn test_no_color_produces_no_ansi_escapes() {
        let card = sample_card();
        let rendered = render_card_with_color(&card, 0, false);
        assert!(
            !rendered.contains('\x1b'),
            "NO_COLOR render must not contain ANSI escapes"
        );
        // Meaning is conveyed via the ★ glyph + text regardless of color.
        assert!(rendered.starts_with("\u{2605} Borrow vs. clone"));
    }

    #[test]
    fn test_color_variant_wraps_header_but_meaning_survives_without_color() {
        let card = sample_card();
        let colored = render_card_with_color(&card, 0, true);
        assert!(colored.contains('\x1b'));
        assert!(colored.contains("Borrow vs. clone"));
    }

    #[test]
    fn test_render_card_snapshot_no_color() {
        let card = sample_card();
        let rendered = render_card_with_color(&card, 0, false);
        let expected = [
            "\u{2605} Borrow vs. clone",
            "  src/main.rs:42",
            "  > person.name.clone()",
            "",
            "  The call only reads the name, so cloning the String allocates and copies data",
            "  a borrow would have served just as well.",
            "",
            "  Rule: Take &str when the function only needs to read the value \u{2014}",
            "  https://example.com/pack-docs/redundant-clone",
            "  e to escalate, t for the fix, k to ask",
            "",
        ]
        .join("\n");
        assert_eq!(rendered, expected);
    }

    #[test]
    fn test_render_card_overflow_counter_one_line() {
        let card = sample_card();
        let rendered = render_card_with_color(&card, 3, false);
        assert!(rendered.contains("3 more queued \u{2014} T2"));
        assert_eq!(
            rendered
                .lines()
                .filter(|l| l.contains("more queued"))
                .count(),
            1,
            "overflow counter must be exactly one line"
        );
    }

    #[test]
    fn test_no_overflow_counter_when_zero_queued() {
        let card = sample_card();
        let rendered = render_card_with_color(&card, 0, false);
        assert!(!rendered.contains("more queued"));
    }

    #[test]
    fn test_render_card_single_site_has_no_aggregation_line() {
        let card = sample_card();
        let rendered = render_card_with_color(&card, 0, false);
        assert!(!rendered.contains("this pattern appears"));
    }

    #[test]
    fn test_render_card_aggregation_lists_anchors_and_count() {
        let mut card = sample_card();
        card.additional_anchors = vec![("b.rs".to_string(), 7), ("c.rs".to_string(), 9)];
        let rendered = render_card_with_color(&card, 0, false);
        assert!(rendered.contains("this pattern appears in 3 places"));
        assert!(rendered.contains("also: b.rs:7"));
        assert!(rendered.contains("also: c.rs:9"));
    }

    #[test]
    fn test_render_card_aggregation_overflow_not_listed_inline() {
        let mut card = sample_card();
        card.additional_anchors = vec![("b.rs".to_string(), 7), ("c.rs".to_string(), 9)];
        card.overflow_site_count = 2;
        let rendered = render_card_with_color(&card, 0, false);
        assert!(rendered.contains("this pattern appears in 5 places"));
        assert_eq!(rendered.matches("also:").count(), 2);
    }

    // --- T4 reqs 2-4 / C4: rung-aware rendering ---

    #[test]
    fn test_render_card_at_rung_r1_is_a_minimal_nudge() {
        let card = sample_card();
        let rendered = render_card_at_rung(&card, crate::ladder::Rung::R1, 0, "//");
        assert!(rendered.contains("Borrow vs. clone"));
        assert!(rendered.contains("nudge"));
        // R1 reveals nothing: no why/rule/worked-diff text present.
        assert!(!rendered.contains(&card.why));
        assert!(!rendered.contains(&card.rule));
        assert!(!rendered.contains(&card.worked_diff));
    }

    #[test]
    fn test_render_card_at_rung_r2_matches_render_card() {
        let card = sample_card();
        assert_eq!(
            render_card_at_rung(&card, crate::ladder::Rung::R2, 0, "//"),
            render_card(&card, 0)
        );
    }

    #[test]
    fn test_render_card_at_rung_r3_unfolds_worked_example_with_comments() {
        let card = sample_card();
        let rendered = render_card_at_rung(&card, crate::ladder::Rung::R3, 0, "//");
        assert!(rendered.contains("worked example:"));
        // The folded pointer line is gone, replaced by the real diff.
        assert!(!rendered.contains("e to escalate, t for the fix, k to ask"));
        assert!(rendered.contains("fn print_name(name: String)"));
        assert!(rendered.contains("fn print_name(name: &str)"));
        assert!(rendered.contains("// why:"));
    }
}

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

    out.push_str("  (fix available \u{2014} full interaction in T4)\n");

    if queued_count > 0 {
        out.push_str(&format!("  {} more queued \u{2014} T2\n", queued_count));
    }

    out
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
            doc_ref: "https://rust-lang.github.io/rust-clippy/master/#redundant_clone".to_string(),
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
            "  https://rust-lang.github.io/rust-clippy/master/#redundant_clone",
            "  (fix available \u{2014} full interaction in T4)",
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
}

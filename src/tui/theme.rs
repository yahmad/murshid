//! T15 UX redesign ("Focus", `specs/explorations/tui-ux-redesign.md`), Step
//! 0 — the shared foundation every other view in this module builds on:
//! the `(glyph, ratatui color, ascii fallback, word)` role table (design doc
//! §4.1), the NO_COLOR gate, and the small style helpers (`category_style`,
//! `state_style`, `chip`) the build plan calls for. Pure: no I/O beyond the
//! `NO_COLOR` env read `card::color_allowed` already performs.
//!
//! Design doc §4.1's whole point: color is always a REDUNDANT third
//! channel — a glyph and a word carry the same meaning with color turned
//! off entirely (NO_COLOR) or unavailable (16-color terminals still get the
//! named `Color` variants below, never `Rgb`/`Indexed`).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use crate::pack::Category;
use crate::progress::ConceptState;

/// One `(glyph, ascii fallback, color, word)` role — design doc §4.1's table
/// row shape. `color` is only ever consulted when the caller has already
/// decided color is allowed (see [`Role::span`]); glyph + word alone must
/// always carry full meaning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Role {
    pub glyph: &'static str,
    pub ascii: &'static str,
    pub color: Color,
    pub word: &'static str,
}

impl Role {
    /// `glyph + " " + word` (or just `word` when there's no glyph, e.g. the
    /// plain mastery states) as one styled `Span`, colored by `color` when
    /// `use_color` is true and `Color::Reset` — the terminal's own
    /// foreground, never a hardcoded white/black (design doc §4.1) —
    /// otherwise. This is the NO_COLOR degrade path (I21): the glyph and
    /// word alone still carry the meaning with no color at all.
    pub fn span(&self, use_color: bool) -> Span<'static> {
        let color = if use_color { self.color } else { Color::Reset };
        let text = if self.glyph.is_empty() {
            self.word.to_string()
        } else {
            format!("{} {}", self.glyph, self.word)
        };
        Span::styled(text, Style::default().fg(color))
    }
}

/// Whether ANSI color is permitted for this render (I21,
/// <https://no-color.org>) — reuses `card::color_allowed`, the pre-redesign
/// gate, so `NO_COLOR` behaves identically across the whole TUI, old and new.
pub fn color_allowed() -> bool {
    crate::card::color_allowed()
}

/// Whether glyphs render at all. Design doc §4.1: "there is no need to probe
/// terminal capability beyond [NO_COLOR] for v1" — so v1 has no live
/// glyph-capability probe and always renders the Unicode glyph. This gate
/// exists (build plan Step 0 names it explicitly) as the one place a future
/// real capability probe would change; every `Role::ascii` fallback is kept
/// on the table for that day, unused by any code path today.
pub fn glyphs_ok() -> bool {
    true
}

// --- §4.1's category row ---

pub const CATEGORY_BUG: Role = Role {
    glyph: "!",
    ascii: "!",
    color: Color::Red,
    word: "bug",
};
pub const CATEGORY_IDIOM: Role = Role {
    glyph: "\u{2605}", // ★
    ascii: "*",
    color: Color::Yellow,
    word: "idiom",
};
pub const CATEGORY_BEST_PRACTICE: Role = Role {
    glyph: "\u{25c6}", // ◆
    ascii: "+",
    color: Color::Cyan,
    word: "best-practice",
};
pub const CATEGORY_ARCHITECTURE: Role = Role {
    glyph: "\u{25b2}", // ▲
    ascii: "^",
    color: Color::Magenta,
    word: "architecture",
};
/// A pack-extended/unrecognized category (`Category::Other`) — never a
/// panic, same degrade posture `Category` itself documents; a neutral
/// marker rather than any of the four named accents.
pub const CATEGORY_OTHER: Role = Role {
    glyph: "?",
    ascii: "?",
    color: Color::Reset,
    word: "other",
};

/// §4.1's category row, keyed off the pack's `Category`.
pub fn category_style(category: &Category) -> Role {
    match category {
        Category::Bug => CATEGORY_BUG,
        Category::Idiom => CATEGORY_IDIOM,
        Category::BestPractice => CATEGORY_BEST_PRACTICE,
        Category::Architecture => CATEGORY_ARCHITECTURE,
        Category::Other(_) => CATEGORY_OTHER,
    }
}

// --- the mastery meter's per-row state (§3.6 prose: "green mastered,
// terminal-fg learning, amber stale, dim throttled") ---

pub const STATE_MASTERED: Role = Role {
    glyph: "\u{2713}", // ✓
    ascii: "v",
    color: Color::Green,
    word: "mastered",
};
pub const STATE_LEARNING: Role = Role {
    glyph: "",
    ascii: "",
    color: Color::Reset,
    word: "learning",
};
pub const STATE_STALE: Role = Role {
    glyph: "",
    ascii: "",
    color: Color::Yellow,
    word: "stale",
};
pub const STATE_THROTTLED: Role = Role {
    glyph: "",
    ascii: "",
    color: Color::DarkGray,
    word: "throttled",
};

pub fn state_style(state: &ConceptState) -> Role {
    match state {
        ConceptState::Mastered => STATE_MASTERED,
        ConceptState::Learning => STATE_LEARNING,
        ConceptState::Stale => STATE_STALE,
        ConceptState::Throttled => STATE_THROTTLED,
    }
}

// --- the rest of §4.1's table ---

pub const ATTENTION: Role = Role {
    glyph: "\u{2691}", // ⚑
    ascii: "!",
    color: Color::Yellow,
    word: "a moment",
};
pub const SUCCESS: Role = Role {
    glyph: "\u{2713}", // ✓
    ascii: "v",
    color: Color::Green,
    word: "applied",
};
pub const DECLINED: Role = Role {
    glyph: "~",
    ascii: "~",
    color: Color::DarkGray,
    word: "declined",
};
pub const DROPPED: Role = Role {
    glyph: "!",
    ascii: "!",
    color: Color::Red,
    word: "dropped",
};
pub const LIVE_PULSE: Role = Role {
    glyph: "\u{25cf}", // ●
    ascii: "*",
    color: Color::Green,
    word: "watching",
};
pub const WAITING_PULSE: Role = Role {
    glyph: "\u{23f8}", // ⏸
    ascii: "=",
    color: Color::DarkGray,
    word: "waiting",
};
pub const DEGRADED_JUDGE: Role = Role {
    glyph: "\u{25b2}", // ▲
    ascii: "!",
    color: Color::Red,
    word: "judge degraded",
};
/// T15 mentor-state indicator: the header pulse's "reviewing a file" face —
/// the NORMAL save→check→judge sweep's live face, same amber family as the
/// offer-accept struggle-judge's "thinking" (both are "the model/engine is
/// working" states). Header rendering animates the glyph via
/// [`working_pulse_frame`] rather than this constant's own static `glyph`
/// field (kept only so `ascii`/`color`/`word` sit in the same table shape
/// as every other role, per the design doc's convention).
pub const REVIEWING_PULSE: Role = Role {
    glyph: "\u{25d0}", // ◐ — same as the animation's first frame
    ascii: "~",
    color: Color::Yellow,
    word: "reviewing",
};

/// The "thinking" pulse's four animation frames (design doc §3.4/§4.1) —
/// `◐ ◓ ◑ ◒`, cycled by [`working_pulse_frame`].
pub const WORKING_PULSE_FRAMES: [&str; 4] =
    ["\u{25d0}", "\u{25d3}", "\u{25d1}", "\u{25d2}"];
/// The ASCII fallback cycle (`|/-\`) — table data for a future capability
/// probe; unused while [`glyphs_ok`] is unconditionally `true`.
pub const WORKING_PULSE_ASCII: [&str; 4] = ["|", "/", "-", "\\"];

/// The animated "thinking" pulse glyph for `tick` — cycles every frame the
/// event loop ticks (design doc §3.4: "index the frame by a tick counter",
/// "a cheap, honest signal that costs nothing").
pub fn working_pulse_frame(tick: u64) -> &'static str {
    WORKING_PULSE_FRAMES[(tick as usize) % WORKING_PULSE_FRAMES.len()]
}

/// Ambient/secondary text (design doc §4.1: "DarkGray + DIM") — the status
/// band, keybar labels, and every other line that must never compete with
/// the card for attention.
pub fn ambient_style() -> Style {
    Style::default().fg(Color::DarkGray).add_modifier(Modifier::DIM)
}

/// Focus/selection (design doc §4.1: `REVERSED`) — inherits the user's own
/// terminal colors, so it can never clash the way a hardcoded background
/// would.
pub fn focus_style() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

/// `[k] label` — the keybar's basic unit (design doc §5.3): the key
/// bracketed/bold, the label dim. A disabled action is simply omitted by
/// the caller, never grayed-in-place.
pub fn chip(key: &str, label: &str) -> Vec<Span<'static>> {
    vec![
        Span::styled(
            format!("[{}]", key),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(label.to_string(), ambient_style()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_category_style_covers_all_four_named_categories() {
        assert_eq!(category_style(&Category::Bug).word, "bug");
        assert_eq!(category_style(&Category::Idiom).word, "idiom");
        assert_eq!(category_style(&Category::BestPractice).word, "best-practice");
        assert_eq!(category_style(&Category::Architecture).word, "architecture");
    }

    #[test]
    fn test_category_style_other_degrades_without_panicking() {
        let role = category_style(&Category::Other("weird-pack-category".to_string()));
        assert_eq!(role, CATEGORY_OTHER);
    }

    #[test]
    fn test_state_style_covers_all_four_states() {
        assert_eq!(state_style(&ConceptState::Mastered).word, "mastered");
        assert_eq!(state_style(&ConceptState::Learning).word, "learning");
        assert_eq!(state_style(&ConceptState::Stale).word, "stale");
        assert_eq!(state_style(&ConceptState::Throttled).word, "throttled");
    }

    #[test]
    fn test_role_span_no_color_uses_reset_regardless_of_role_color() {
        let span = CATEGORY_BUG.span(false);
        assert_eq!(span.style.fg, Some(Color::Reset));
        // Meaning survives via glyph + word even with color stripped.
        assert!(span.content.contains('!'));
        assert!(span.content.contains("bug"));
    }

    #[test]
    fn test_role_span_with_color_uses_role_color() {
        let span = CATEGORY_BUG.span(true);
        assert_eq!(span.style.fg, Some(Color::Red));
    }

    #[test]
    fn test_role_span_with_empty_glyph_renders_word_only() {
        let span = STATE_LEARNING.span(true);
        assert_eq!(span.content, "learning");
    }

    #[test]
    fn test_chip_shape_brackets_the_key_and_keeps_label_separate() {
        let spans = chip("a", "applied");
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].content, "[a]");
        assert_eq!(spans[2].content, "applied");
    }

    #[test]
    fn test_working_pulse_frame_cycles_through_four_distinct_frames() {
        let frames: Vec<&str> = (0..4).map(working_pulse_frame).collect();
        assert_eq!(frames.len(), 4);
        assert_eq!(std::collections::HashSet::<&str>::from_iter(frames.clone()).len(), 4);
        // Wraps back to frame 0 at tick 4.
        assert_eq!(working_pulse_frame(4), frames[0]);
    }

    #[test]
    fn test_glyphs_ok_is_true_in_v1() {
        assert!(glyphs_ok());
    }
}

//! T4 reqs 2-4 / T5 reqs 4-5 / C4 — the rung ladder: entry-rung composition
//! (T5: BKT band -> Wood shift -> directness-knob offset, clamped, incl. R0
//! generation moment and silence), escalation stepping, and the R3
//! commented-worked-example render (I19).

/// C4's help levels. R0 is the "generation moment" (a recall question,
/// nothing revealed until answered) — T5 activates it via
/// [`compose_entry_rung`]; silence (concept mastered, no card) is
/// represented as `None` at the composition boundary, not a `Rung` variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rung {
    R0,
    R1,
    R2,
    R3,
}

impl Rung {
    pub fn as_str(&self) -> &'static str {
        match self {
            Rung::R0 => "R0",
            Rung::R1 => "R1",
            Rung::R2 => "R2",
            Rung::R3 => "R3",
        }
    }

    pub fn from_label(s: &str) -> Option<Self> {
        match s {
            "R0" => Some(Rung::R0),
            "R1" => Some(Rung::R1),
            "R2" => Some(Rung::R2),
            "R3" => Some(Rung::R3),
            _ => None,
        }
    }

    fn as_i32(self) -> i32 {
        match self {
            Rung::R0 => 0,
            Rung::R1 => 1,
            Rung::R2 => 2,
            Rung::R3 => 3,
        }
    }

    fn from_i32_clamped(v: i32) -> Self {
        match v.clamp(0, 3) {
            0 => Rung::R0,
            1 => Rung::R1,
            2 => Rung::R2,
            _ => Rung::R3,
        }
    }
}

/// C4's `[dial] directness` knob: `guide-me|balanced|tell-me`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Directness {
    GuideMe,
    Balanced,
    TellMe,
}

/// Unrecognized config values fall back to `balanced` (the C12 default).
pub fn directness_from_config(value: &str) -> Directness {
    match value {
        "guide-me" => Directness::GuideMe,
        "tell-me" => Directness::TellMe,
        _ => Directness::Balanced,
    }
}

/// C4: the directness knob's shift (guide-me -1, tell-me +1), shared by the
/// legacy static [`entry_rung`] and T5's [`compose_entry_rung`].
pub fn knob_offset(directness: Directness) -> i32 {
    match directness {
        Directness::GuideMe => -1,
        Directness::Balanced => 0,
        Directness::TellMe => 1,
    }
}

/// T4 req 2 / C4: static entry rung = R2 + knob offset, clamped to [R1, R3].
/// Superseded by [`compose_entry_rung`] (T5 req 4) for every concept that
/// has memory state; kept as the degraded-mode/no-memory fallback and for
/// the T4 test suite it still backs.
pub fn entry_rung(directness: Directness) -> Rung {
    Rung::from_i32_clamped(Rung::R2.as_i32() + knob_offset(directness)).max(Rung::R1)
}

/// T5 req 4 / C4: the BKT mastery band, numeric so it composes with the
/// Wood shift and knob offset before a single clamp — `p < 0.5 -> R3`,
/// `0.5-0.8 -> R2`, `0.8-0.95 -> R1`; the generation moment (R0) is
/// permitted for `0.6 <= p < 0.95` and, in this engine, is always taken
/// over the R1/R2 band it overlaps once eligible (a deterministic reading
/// of C4's "permitted" — the pure composition function has no other signal
/// to decide against it). `p >= 0.95` is silence (`None`) — no card, no
/// numeric band at all.
pub fn bkt_band(p_mastery: f64) -> Option<i32> {
    if p_mastery >= 0.95 {
        None
    } else if p_mastery >= 0.6 {
        Some(Rung::R0.as_i32())
    } else if p_mastery >= 0.5 {
        Some(Rung::R2.as_i32())
    } else {
        Some(Rung::R3.as_i32())
    }
}

/// T5 req 4 / C4: entry rung composition — BKT band -> Wood shift (from the
/// concept's most recent grade, C4's "next encounter" shift; `None` when
/// there is no prior encounter yet) -> directness-knob offset -> one final
/// clamp to [R0, R3]. `None` means silence (mastered, no card).
pub fn compose_entry_rung(
    p_mastery: f64,
    last_outcome: Option<crate::bkt::Grade>,
    directness: Directness,
) -> Option<Rung> {
    let band = bkt_band(p_mastery)?;
    let wood = last_outcome.map(crate::bkt::wood_delta).unwrap_or(0);
    let numeric = band + wood + knob_offset(directness);
    Some(Rung::from_i32_clamped(numeric))
}

/// T4 req 3: `e` steps one rung up the ladder (R1->R2->R3); already at R3
/// stays at R3 (per-moment escalation moves up only, C4).
pub fn escalate_one(current: Rung) -> Rung {
    Rung::from_i32_clamped(current.as_i32() + 1)
}

/// T4 req 3: `t` ("just tell me") jumps straight to R3 regardless of the
/// current rung.
pub fn tell_me(_current: Rung) -> Rung {
    Rung::R3
}

/// T4 req 4 / I19: renders `worked_diff` as a commented worked example — one
/// comment line (using `comment_token`) after each contiguous changed
/// region (a maximal run of `+`/`-` lines), carrying the card's `why` so
/// every changed region is paired with an explanation. Non-diff lines
/// (blank separators, context) pass through unchanged.
pub fn render_worked_example(worked_diff: &str, why: &str, comment_token: &str) -> String {
    let lines: Vec<&str> = worked_diff.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if line.starts_with('+') || line.starts_with('-') {
            while i < lines.len() && (lines[i].starts_with('+') || lines[i].starts_with('-')) {
                out.push(lines[i].to_string());
                i += 1;
            }
            out.push(format!("{} why: {}", comment_token, why));
        } else {
            out.push(line.to_string());
            i += 1;
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- req 2: directness -> entry rung, clamped ---

    #[test]
    fn test_directness_from_config_recognizes_all_three() {
        assert_eq!(directness_from_config("guide-me"), Directness::GuideMe);
        assert_eq!(directness_from_config("balanced"), Directness::Balanced);
        assert_eq!(directness_from_config("tell-me"), Directness::TellMe);
    }

    #[test]
    fn test_directness_from_config_unknown_falls_back_to_balanced() {
        assert_eq!(directness_from_config("bogus"), Directness::Balanced);
        assert_eq!(directness_from_config(""), Directness::Balanced);
    }

    #[test]
    fn test_entry_rung_balanced_is_r2() {
        assert_eq!(entry_rung(Directness::Balanced), Rung::R2);
    }

    #[test]
    fn test_entry_rung_guide_me_is_r1() {
        assert_eq!(entry_rung(Directness::GuideMe), Rung::R1);
    }

    #[test]
    fn test_entry_rung_tell_me_is_r3() {
        assert_eq!(entry_rung(Directness::TellMe), Rung::R3);
    }

    // --- req 3: escalation stepping ---

    #[test]
    fn test_escalate_one_steps_up_the_ladder() {
        assert_eq!(escalate_one(Rung::R1), Rung::R2);
        assert_eq!(escalate_one(Rung::R2), Rung::R3);
    }

    #[test]
    fn test_escalate_one_at_r3_stays_at_r3() {
        assert_eq!(escalate_one(Rung::R3), Rung::R3);
    }

    #[test]
    fn test_tell_me_jumps_straight_to_r3_from_any_rung() {
        assert_eq!(tell_me(Rung::R1), Rung::R3);
        assert_eq!(tell_me(Rung::R2), Rung::R3);
        assert_eq!(tell_me(Rung::R3), Rung::R3);
    }

    #[test]
    fn test_rung_round_trips_through_str() {
        for r in [Rung::R0, Rung::R1, Rung::R2, Rung::R3] {
            assert_eq!(Rung::from_label(r.as_str()), Some(r));
        }
        assert_eq!(Rung::from_label("R4"), None);
    }

    // --- req 4 / I19: R3 commented worked example ---

    #[test]
    fn test_render_worked_example_single_region_gets_one_comment() {
        let diff = "- fn print_name(name: String)\n+ fn print_name(name: &str)";
        let rendered = render_worked_example(diff, "borrowing avoids the copy", "//");
        assert!(rendered.contains("- fn print_name(name: String)"));
        assert!(rendered.contains("+ fn print_name(name: &str)"));
        assert_eq!(rendered.matches("// why:").count(), 1);
        assert!(rendered.contains("// why: borrowing avoids the copy"));
    }

    #[test]
    fn test_render_worked_example_per_region_comment_count() {
        // Two separate changed regions, separated by a blank context line.
        let diff = "- old1\n+ new1\n\n- old2\n+ new2";
        let rendered = render_worked_example(diff, "why text", "#");
        assert_eq!(
            rendered.matches("# why:").count(),
            2,
            "one comment line per changed region"
        );
    }

    #[test]
    fn test_render_worked_example_respects_pack_comment_token() {
        let diff = "- a\n+ b";
        let rendered = render_worked_example(diff, "explain", "#");
        assert!(rendered.contains("# why: explain"));
        assert!(!rendered.contains("//"));
    }
}

//! T4 reqs 2-4 / C4 — the rung ladder: entry-rung computation (T4 scope:
//! `R2 + knob offset` until T5's memory-driven entry lands), escalation
//! stepping, and the R3 commented-worked-example render (I19).

/// C4's help levels this task covers. R0 (generation moment) is explicitly
/// OUT of T4 scope (needs BKT mastery state, T5) — see the task spec's
/// scope note: "entry = R2 + knob offset, clamp [R1, R3]".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rung {
    R1,
    R2,
    R3,
}

impl Rung {
    pub fn as_str(&self) -> &'static str {
        match self {
            Rung::R1 => "R1",
            Rung::R2 => "R2",
            Rung::R3 => "R3",
        }
    }

    pub fn from_label(s: &str) -> Option<Self> {
        match s {
            "R1" => Some(Rung::R1),
            "R2" => Some(Rung::R2),
            "R3" => Some(Rung::R3),
            _ => None,
        }
    }

    fn as_i32(self) -> i32 {
        match self {
            Rung::R1 => 1,
            Rung::R2 => 2,
            Rung::R3 => 3,
        }
    }

    fn from_i32_clamped(v: i32) -> Self {
        match v.clamp(1, 3) {
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

/// T4 req 2 / C4: entry rung = R2 + knob offset (guide-me -1, tell-me +1),
/// clamped to [R1, R3]. Memory-driven entry (BKT mastery p) is T5 scope.
pub fn entry_rung(directness: Directness) -> Rung {
    let offset = match directness {
        Directness::GuideMe => -1,
        Directness::Balanced => 0,
        Directness::TellMe => 1,
    };
    Rung::from_i32_clamped(Rung::R2.as_i32() + offset)
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
        for r in [Rung::R1, Rung::R2, Rung::R3] {
            assert_eq!(Rung::from_label(r.as_str()), Some(r));
        }
        assert_eq!(Rung::from_label("R0"), None);
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

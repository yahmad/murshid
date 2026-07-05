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

/// De-stringify refactor: `cards.rung_shown` normally holds a [`Rung`] label
/// (`"R0"`.."R3"`), but the struggle-offer card creation site (`watch/offers.rs`)
/// overloads the same column with the sentinel `"offer"` — no schema change,
/// no migration, so this is a union over the existing on-disk TEXT rather
/// than a new `card_kind` column. `as_str`/`parse` round-trip the EXACT
/// strings already on disk (`Rung::as_str()`'s labels, plus `"offer"`), same
/// idiom as `Rung`/`bkt::Grade`/`CardStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RungShown {
    Rung(Rung),
    Offer,
}

impl RungShown {
    pub fn as_str(&self) -> &'static str {
        match self {
            RungShown::Rung(r) => r.as_str(),
            RungShown::Offer => "offer",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        if s == "offer" {
            Some(RungShown::Offer)
        } else {
            Rung::from_label(s).map(RungShown::Rung)
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

impl Directness {
    /// T15 settings overlay: the exact `[dial] directness` config string this
    /// variant round-trips through [`directness_from_config`] — the overlay's
    /// display label.
    pub fn as_str(&self) -> &'static str {
        match self {
            Directness::GuideMe => "guide-me",
            Directness::Balanced => "balanced",
            Directness::TellMe => "tell-me",
        }
    }

    /// T15 settings overlay: `\u{2192}` cycles guide-me -> balanced ->
    /// tell-me -> guide-me.
    pub fn next(&self) -> Directness {
        match self {
            Directness::GuideMe => Directness::Balanced,
            Directness::Balanced => Directness::TellMe,
            Directness::TellMe => Directness::GuideMe,
        }
    }

    /// T15 settings overlay: `\u{2190}` cycles the same ring in reverse.
    pub fn prev(&self) -> Directness {
        match self {
            Directness::GuideMe => Directness::TellMe,
            Directness::Balanced => Directness::GuideMe,
            Directness::TellMe => Directness::Balanced,
        }
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

/// T5 req 4 / C4 (amended 2026-07-03 after T5 review — SPEC.md commit
/// 743c5d8, decision log "CONTRACT AMENDED... C4 generation-moment"): the
/// BKT mastery band, numeric so it composes with the Wood shift and knob
/// offset before a single clamp — `p < 0.5 -> R3`, `0.5-0.6 -> R2`,
/// `0.6-0.8 -> R0` (the generation moment, TAKEN deterministically here —
/// squarely partially-known, where retrieval pays most), `0.8-0.95 -> R1`
/// (R1 keeps its normative nudge: it's already question-formed, and
/// demanding full recall this close to mastery is friction without
/// evidence). `p >= 0.95` is silence (`None`) — no card, no numeric band.
pub fn bkt_band(p_mastery: f64) -> Option<i32> {
    if p_mastery >= crate::bkt::MASTERY_THRESHOLD {
        None
    } else if p_mastery >= BAND_R1_LO {
        Some(Rung::R1.as_i32())
    } else if p_mastery >= BAND_R0_LO {
        Some(Rung::R0.as_i32())
    } else if p_mastery >= BAND_R2_LO {
        Some(Rung::R2.as_i32())
    } else {
        Some(Rung::R3.as_i32())
    }
}

/// Lower edges of the `bkt_band` mastery bands (the upper edge of each is the
/// next band's lower edge; the silence gate at the top is
/// [`crate::bkt::MASTERY_THRESHOLD`], kept as the single source of truth so
/// this band table and [`crate::bkt::is_mastered`] can never disagree). Named
/// rather than inlined so a retune touches one place — see the band rationale
/// on `bkt_band`.
const BAND_R1_LO: f64 = 0.8; // [0.8, 0.95) -> R1 (near mastery: normative nudge)
const BAND_R0_LO: f64 = 0.6; // [0.6, 0.8)  -> R0 (generation moment)
const BAND_R2_LO: f64 = 0.5; // [0.5, 0.6)  -> R2 (partially known)

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

    // --- T15 settings overlay: directness cycle + label round-trip ---

    #[test]
    fn test_directness_as_str_round_trips_through_directness_from_config() {
        for d in [Directness::GuideMe, Directness::Balanced, Directness::TellMe] {
            assert_eq!(directness_from_config(d.as_str()), d);
        }
    }

    #[test]
    fn test_directness_next_cycles_guide_balanced_tell_and_wraps() {
        assert_eq!(Directness::GuideMe.next(), Directness::Balanced);
        assert_eq!(Directness::Balanced.next(), Directness::TellMe);
        assert_eq!(Directness::TellMe.next(), Directness::GuideMe);
    }

    #[test]
    fn test_directness_prev_is_the_exact_reverse_of_next() {
        for d in [Directness::GuideMe, Directness::Balanced, Directness::TellMe] {
            assert_eq!(d.next().prev(), d);
            assert_eq!(d.prev().next(), d);
        }
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

    // --- de-stringify refactor: RungShown as_str/parse round-trip ---

    #[test]
    fn test_rung_shown_round_trips_through_str() {
        for rs in [
            RungShown::Rung(Rung::R0),
            RungShown::Rung(Rung::R1),
            RungShown::Rung(Rung::R2),
            RungShown::Rung(Rung::R3),
            RungShown::Offer,
        ] {
            assert_eq!(RungShown::parse(rs.as_str()), Some(rs));
        }
        assert_eq!(RungShown::parse("bogus"), None);
    }

    #[test]
    fn test_rung_shown_as_str_matches_existing_on_disk_strings() {
        assert_eq!(RungShown::Rung(Rung::R0).as_str(), "R0");
        assert_eq!(RungShown::Rung(Rung::R2).as_str(), "R2");
        assert_eq!(RungShown::Offer.as_str(), "offer");
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

    // --- T5 req 4 / C4: BKT band ---

    #[test]
    fn test_bkt_band_boundaries() {
        assert_eq!(bkt_band(0.95), None, "p >= 0.95 -> silence");
        assert_eq!(
            bkt_band(0.9),
            Some(Rung::R1.as_i32()),
            "0.8<=p<0.95 -> R1 (its normative nudge)"
        );
        assert_eq!(
            bkt_band(0.7),
            Some(Rung::R0.as_i32()),
            "0.6<=p<0.8 -> generation moment"
        );
        assert_eq!(
            bkt_band(0.55),
            Some(Rung::R2.as_i32()),
            "0.5<=p<0.6 -> R2 (below the generation window)"
        );
        assert_eq!(bkt_band(0.3), Some(Rung::R3.as_i32()), "p<0.5 -> R3");
    }

    /// Amended C4 (SPEC.md commit 743c5d8): the generation window is
    /// 0.6-0.8 ONLY — 0.8-0.95 keeps its normative R1, it is not absorbed
    /// into the generation moment.
    #[test]
    fn test_bkt_band_boundary_at_0_8_r0_below_r1_at_and_above() {
        assert_eq!(
            bkt_band(0.79999),
            Some(Rung::R0.as_i32()),
            "just below 0.8 -> still generation moment"
        );
        assert_eq!(
            bkt_band(0.8),
            Some(Rung::R1.as_i32()),
            "exactly 0.8 -> R1, the band is half-open [0.8, 0.95)"
        );
        assert_eq!(
            bkt_band(0.94999),
            Some(Rung::R1.as_i32()),
            "just below mastery -> still R1, not generation"
        );
    }

    // --- T5 req 4 / C4: entry rung composition ---

    #[test]
    fn test_compose_entry_rung_silence_ignores_wood_and_knob() {
        assert_eq!(
            compose_entry_rung(0.99, Some(crate::bkt::Grade::Fail), Directness::TellMe),
            None,
            "silence is authoritative regardless of shift/knob"
        );
    }

    #[test]
    fn test_compose_entry_rung_wood_shift_from_last_outcome() {
        // p=0.3 -> R3 band (3). A prior `pass` shifts -1 -> R2.
        let with_pass =
            compose_entry_rung(0.3, Some(crate::bkt::Grade::Pass), Directness::Balanced);
        assert_eq!(with_pass, Some(Rung::R2));
        // A prior `fail` shifts +1, clamped at R3 (already max).
        let with_fail =
            compose_entry_rung(0.3, Some(crate::bkt::Grade::Fail), Directness::Balanced);
        assert_eq!(with_fail, Some(Rung::R3));
    }

    #[test]
    fn test_compose_entry_rung_knob_offset_applies_after_wood() {
        // p=0.55 -> R2 band (2), no prior outcome, guide-me knob (-1) -> R1.
        let guide = compose_entry_rung(0.55, None, Directness::GuideMe);
        assert_eq!(guide, Some(Rung::R1));
        let tell = compose_entry_rung(0.55, None, Directness::TellMe);
        assert_eq!(tell, Some(Rung::R3));
    }

    /// Acceptance: "entry-rung band + Wood + knob composition (property:
    /// always in [R0,R3] or silence)" — an exhaustive sweep, not a sample.
    #[test]
    fn test_compose_entry_rung_property_always_valid_or_silence() {
        let mut p = 0.0;
        while p <= 1.0 {
            for last_outcome in [
                None,
                Some(crate::bkt::Grade::Pass),
                Some(crate::bkt::Grade::Hard),
                Some(crate::bkt::Grade::Fail),
            ] {
                for directness in [
                    Directness::GuideMe,
                    Directness::Balanced,
                    Directness::TellMe,
                ] {
                    let result = compose_entry_rung(p, last_outcome, directness);
                    match result {
                        None => {} // silence: always valid
                        Some(rung) => {
                            assert!(
                                (Rung::R0..=Rung::R3).contains(&rung),
                                "p={} outcome={:?} directness={:?} produced out-of-range {:?}",
                                p,
                                last_outcome,
                                directness,
                                rung
                            );
                        }
                    }
                }
            }
            p += 0.01;
        }
    }
}

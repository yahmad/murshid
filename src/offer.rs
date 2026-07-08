//! T3 reqs 11-13, amended T17 R3 — the struggle "offer" is now a direct
//! hint: still one line, idle-gated, never while green, evidenced (I9-I13,
//! D16, C3, C7) — but the `[y/N]` consent dialogue is gone (`watch::offers`
//! fires the judge inline at gate-pass instead of waiting on accept), so the
//! decline-persistence machinery this module used to carry
//! (`classify_offer_key`/`OfferKeyAction`, the 2-declines cross-session
//! suppression) is deleted with it — T17 R2's `hint-concept` decaying
//! suppression (`suppression.rs`/`db::suppressions`) is the load-bearing
//! anti-repeat mechanism now. The plain y/N classifier `murshid review`
//! still needs for its own, unrelated consent prompt moved to
//! `consent::classify_yes_no_key`.

use std::time::{Duration, SystemTime};

/// req 11 / I10: never prompt while the user is actively typing — the
/// offer's own idle gate (independent of the D8 quiescence gate, which only
/// governs judging).
pub const OFFER_IDLE_GATE: Duration = Duration::from_secs(20);

/// I11: every offer names the signal that fired it.
#[derive(Debug, Clone, PartialEq)]
pub enum Evidence {
    /// Converged signals 1+2: the E-code and how long it's been red.
    ErrorStreak { code: String, minutes: u64 },
    /// Signal 3: the fresh help-flavored comment itself, self-declared.
    HelpComment { snippet: String },
    /// T16b: the model-perception pass's candidate judgment — proposed by
    /// `perception::build_perception_prompt`/`parse_perception_output`,
    /// gated the SAME as every other evidence type (this module +
    /// `watch::offers::run_poll_loop`'s idle/throttle/suppression/
    /// one-at-a-time gate). `evidence_line` is the model's own
    /// concept-and-site-named clause (I11); `confidence` backs the
    /// [`perception_clears_floor`]/[`perception_fires_alone`] anti-nag
    /// gate below.
    Perceived {
        concept: String,
        evidence_line: String,
        confidence: f64,
    },
}

/// I11: the evidence clause is mandatory in the hint's activity-log notice.
/// T17 R3: no more `[y/N]` — a hint is no longer a question, it fires
/// directly. Example: "stuck on E0308 for 14 min — hint".
pub fn offer_line(evidence: &Evidence) -> String {
    match evidence {
        Evidence::ErrorStreak { code, minutes } => {
            format!("stuck on {} for {} min \u{2014} hint", code, minutes)
        }
        Evidence::HelpComment { snippet } => {
            format!("saw \"{}\" \u{2014} hint", snippet)
        }
        // T16b (I11 evidence-named): the model's own one-line evidence
        // clause, already phrased to read naturally before the fixed
        // "— hint" suffix — e.g. "looks like you're circling ownership in
        // parse_config — hint".
        Evidence::Perceived { evidence_line, .. } => {
            format!("{} \u{2014} hint", evidence_line)
        }
    }
}

/// The (signal, key) identity a hint fires under — the same-session
/// no-refire key (`already_offered`) and the `hint_shown` event's `signal`
/// field (T17 R3; the cross-session decline-count use is gone with the
/// consent dialogue).
pub fn offer_key(evidence: &Evidence) -> (&'static str, String) {
    match evidence {
        Evidence::ErrorStreak { code, .. } => ("error-streak", code.clone()),
        Evidence::HelpComment { snippet } => ("help-comment", snippet.clone()),
        // T16b: keyed by concept (mirrors the error-streak key's shape) —
        // the same-session no-refire / cross-session decline-count
        // identity for a perception-sourced offer.
        Evidence::Perceived { concept, .. } => ("perceived", concept.clone()),
    }
}

// --- T16b: perception anti-nag gate (I4/I14) ---

/// Config-grade (per C1), strawman per the T16 spec: the necessary-not-
/// sufficient minimum confidence (I4) a perception judgment must clear to
/// reach the offer gate as a candidate AT ALL. Below this, perception may
/// still be observed/logged by the caller, but never proposes an offer —
/// not even alongside a co-firing mechanical signal.
pub const PERCEPTION_CONFIDENCE_FLOOR: f64 = 0.7;

/// Config-grade: the higher bar a floor-clearing judgment must ALSO clear
/// to be "high confidence" — only a high-confidence, concept-named
/// judgment may fire the offer gate ALONE (I14, founder decision
/// 2026-07-06: "the model's judgment IS the convergence of the signals it
/// reasoned over"). A judgment at/above the floor but below this threshold
/// is "low/medium confidence": eligible only as a candidate that
/// CO-FIRES alongside an already-active mechanical signal
/// (ErrorStreak/RedStreak/help-comment), never alone.
pub const PERCEPTION_HIGH_CONFIDENCE_THRESHOLD: f64 = 0.85;

/// I4: the confidence floor is necessary but not sufficient — this only
/// answers "is this judgment even eligible to be proposed as a candidate",
/// never "should it fire" (the structural gate + fire-alone-vs-co-fire
/// rule below still decide that).
pub fn perception_clears_floor(confidence: f64) -> bool {
    confidence >= PERCEPTION_CONFIDENCE_FLOOR
}

/// I14 (founder decision 2026-07-06): whether an ALREADY-floor-cleared
/// perception judgment may fire the offer gate with no co-firing
/// mechanical signal required. Requires both a named concept and
/// confidence at/above [`PERCEPTION_HIGH_CONFIDENCE_THRESHOLD`]. Callers
/// must check [`perception_clears_floor`] first (or separately) — this
/// function alone doesn't re-check the floor since the threshold here is
/// always stricter than it anyway.
pub fn perception_fires_alone(confidence: f64, has_concept: bool) -> bool {
    has_concept && confidence >= PERCEPTION_HIGH_CONFIDENCE_THRESHOLD
}

/// T16b: selects perception's candidate for the offer gate, if any —
/// `watch::offers::run_poll_loop`'s single seam for the anti-nag rule
/// (I4/I14). `mechanical_signal_active` is whether ANY ONE of the three
/// named mechanical signals (error-streak fired, red-streak fired, a live
/// help comment) is currently active — the co-fire condition, distinct
/// from `struggle::inferred_pair_converged`'s stricter full-convergence
/// requirement.
///
/// Returns `None` (no candidate proposed to the gate at all) when: there
/// is no live perception candidate; it's below the confidence floor; or
/// it's below the high-confidence threshold with no mechanical signal
/// co-firing. This is proposal ONLY — every structural gate check that
/// runs on whatever this returns (idle / `already_offered` /
/// cross-session suppression / one-offer-at-a-time / D12 throttle) is
/// unchanged and still applies exactly as it does to every other evidence
/// type; this function has no way to skip them.
pub fn select_perceived_candidate(
    candidate: Option<&crate::perception::PerceptionCandidate>,
    mechanical_signal_active: bool,
) -> Option<(Evidence, std::path::PathBuf)> {
    let pc = candidate?;
    if !perception_clears_floor(pc.confidence) {
        return None;
    }
    if !perception_fires_alone(pc.confidence, true) && !mechanical_signal_active {
        return None;
    }
    Some((
        Evidence::Perceived {
            concept: pc.concept.clone(),
            evidence_line: pc.evidence_line.clone(),
            confidence: pc.confidence,
        },
        pc.site_file.clone(),
    ))
}

/// req 11: idle gate — no file events for at least [`OFFER_IDLE_GATE`].
pub fn is_idle(last_event_at: SystemTime, now: SystemTime) -> bool {
    now.duration_since(last_event_at)
        .map(|d| d >= OFFER_IDLE_GATE)
        .unwrap_or(false)
}

/// req 11 (clarified 2026-07-03, T3 spec amendment after review): the
/// never-while-green gate applies to the INFERRED signals only — signals 1
/// and 2 describe stuck-ness, which by construction requires a red check.
/// Signal 3 (self-declared help comment) is a green-build question too
/// ("why does this need a clone?") and needs only the idle gate (throttle
/// and suppression are checked separately by the caller).
pub fn may_offer(evidence: &Evidence, last_check_success: Option<bool>, idle: bool) -> bool {
    if !idle {
        return false;
    }
    match evidence {
        Evidence::ErrorStreak { .. } => last_check_success == Some(false),
        // T16b: same posture as signal 3 — the model's judgment already
        // reasons over red/green state as one of its inputs; the
        // floor/fire-alone-vs-co-fire gate (a separate check, applied at
        // candidate-selection time) is what actually decides eligibility,
        // not a green-build ban here.
        Evidence::HelpComment { .. } | Evidence::Perceived { .. } => true,
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    #[test]
    fn test_offer_line_error_streak_format() {
        let evidence = Evidence::ErrorStreak {
            code: "E0308".to_string(),
            minutes: 14,
        };
        assert_eq!(
            offer_line(&evidence),
            "stuck on E0308 for 14 min \u{2014} hint"
        );
    }

    #[test]
    fn test_offer_line_help_comment_format() {
        let evidence = Evidence::HelpComment {
            snippet: "why does this need a clone?".to_string(),
        };
        assert_eq!(
            offer_line(&evidence),
            "saw \"why does this need a clone?\" \u{2014} hint"
        );
    }

    #[test]
    fn test_offer_key_identity() {
        let e1 = Evidence::ErrorStreak {
            code: "E0308".to_string(),
            minutes: 5,
        };
        let e2 = Evidence::ErrorStreak {
            code: "E0308".to_string(),
            minutes: 20,
        };
        assert_eq!(offer_key(&e1), offer_key(&e2), "key ignores minutes");

        let e3 = Evidence::ErrorStreak {
            code: "E0502".to_string(),
            minutes: 5,
        };
        assert_ne!(offer_key(&e1), offer_key(&e3));
    }

    // --- idle gating (req 11) ---

    #[test]
    fn test_is_idle_boundary() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1000);
        assert!(!is_idle(t0, t0 + Duration::from_secs(19)));
        assert!(is_idle(t0, t0 + Duration::from_secs(20)));
    }

    fn error_streak_evidence() -> Evidence {
        Evidence::ErrorStreak {
            code: "E0308".to_string(),
            minutes: 14,
        }
    }

    fn help_comment_evidence() -> Evidence {
        Evidence::HelpComment {
            snippet: "why does this need a clone?".to_string(),
        }
    }

    #[test]
    fn test_may_offer_inferred_signal_requires_idle_and_red() {
        let e = error_streak_evidence();
        assert!(may_offer(&e, Some(false), true));
        assert!(!may_offer(&e, Some(false), false), "not idle");
        assert!(!may_offer(&e, Some(true), true), "never while green");
        assert!(!may_offer(&e, None, true), "no check yet");
    }

    /// req 11 (clarified): signal 3 (self-declared) needs only the idle
    /// gate — a fresh help-flavored comment may fire an offer on a green
    /// build.
    #[test]
    fn test_may_offer_help_comment_fires_on_green_build() {
        let e = help_comment_evidence();
        assert!(
            may_offer(&e, Some(true), true),
            "green build must not block signal 3"
        );
        assert!(may_offer(&e, Some(false), true));
        assert!(
            may_offer(&e, None, true),
            "no check history yet is fine for signal 3"
        );
        assert!(!may_offer(&e, Some(true), false), "still needs idle");
    }

    // --- T16b: perceived evidence ---

    #[test]
    fn test_offer_line_perceived_appends_fixed_suffix_to_model_evidence() {
        let evidence = Evidence::Perceived {
            concept: "ownership".to_string(),
            evidence_line: "looks like you're circling ownership in parse_config".to_string(),
            confidence: 0.9,
        };
        assert_eq!(
            offer_line(&evidence),
            "looks like you're circling ownership in parse_config \u{2014} hint"
        );
    }

    #[test]
    fn test_offer_key_perceived_is_keyed_by_concept() {
        let e1 = Evidence::Perceived {
            concept: "ownership".to_string(),
            evidence_line: "a".to_string(),
            confidence: 0.9,
        };
        let e2 = Evidence::Perceived {
            concept: "ownership".to_string(),
            evidence_line: "b".to_string(),
            confidence: 0.75,
        };
        assert_eq!(
            offer_key(&e1),
            offer_key(&e2),
            "key ignores evidence_line/confidence"
        );
        assert_eq!(offer_key(&e1).0, "perceived");

        let e3 = Evidence::Perceived {
            concept: "borrow-vs-clone".to_string(),
            evidence_line: "a".to_string(),
            confidence: 0.9,
        };
        assert_ne!(offer_key(&e1), offer_key(&e3));
    }

    #[test]
    fn test_may_offer_perceived_needs_only_idle() {
        let e = Evidence::Perceived {
            concept: "ownership".to_string(),
            evidence_line: "x".to_string(),
            confidence: 0.9,
        };
        assert!(may_offer(&e, Some(true), true), "fires on a green build too");
        assert!(may_offer(&e, None, true), "no check history yet is fine");
        assert!(!may_offer(&e, Some(true), false), "still needs idle");
    }

    // --- T16b anti-nag gate: confidence floor + fire-alone-vs-co-fire ---

    #[test]
    fn test_perception_clears_floor_boundary() {
        assert!(!perception_clears_floor(0.69));
        assert!(perception_clears_floor(0.7));
        assert!(perception_clears_floor(0.99));
    }

    #[test]
    fn test_perception_fires_alone_requires_high_confidence_and_concept() {
        assert!(perception_fires_alone(0.85, true));
        assert!(perception_fires_alone(0.95, true));
        assert!(
            !perception_fires_alone(0.85, false),
            "no concept named -> never fires alone"
        );
        assert!(
            !perception_fires_alone(0.8, true),
            "floor-clearing but not HIGH confidence -> must co-fire, not fire alone"
        );
    }

    // --- T16b: gate routing (offer::select_perceived_candidate) ---

    fn perception_candidate(confidence: f64) -> crate::perception::PerceptionCandidate {
        crate::perception::PerceptionCandidate {
            concept: "ownership".to_string(),
            evidence_line: "looks like you're circling ownership in parse_config".to_string(),
            confidence,
            site_file: std::path::PathBuf::from("src/parse_config.rs"),
        }
    }

    #[test]
    fn test_select_perceived_candidate_none_when_no_live_candidate() {
        assert!(select_perceived_candidate(None, true).is_none());
        assert!(select_perceived_candidate(None, false).is_none());
    }

    #[test]
    fn test_select_perceived_candidate_below_floor_never_produces_a_candidate() {
        let pc = perception_candidate(0.5);
        // Not even a co-firing mechanical signal rescues a below-floor
        // judgment (I4: the floor is necessary, full stop).
        assert!(select_perceived_candidate(Some(&pc), true).is_none());
        assert!(select_perceived_candidate(Some(&pc), false).is_none());
    }

    #[test]
    fn test_select_perceived_candidate_low_medium_confidence_needs_co_fire() {
        // Floor-clearing (>= 0.7) but below the high-confidence threshold
        // (0.85): must co-fire with an active mechanical signal.
        let pc = perception_candidate(0.75);
        assert!(
            select_perceived_candidate(Some(&pc), false).is_none(),
            "low/medium confidence must not fire alone"
        );
        let (evidence, site) = select_perceived_candidate(Some(&pc), true)
            .expect("co-firing with an active mechanical signal produces a candidate");
        assert_eq!(site, std::path::PathBuf::from("src/parse_config.rs"));
        match evidence {
            Evidence::Perceived { concept, .. } => assert_eq!(concept, "ownership"),
            other => panic!("expected Perceived evidence, got {:?}", other),
        }
    }

    #[test]
    fn test_select_perceived_candidate_high_confidence_fires_alone() {
        let pc = perception_candidate(0.9);
        let (evidence, _site) = select_perceived_candidate(Some(&pc), false)
            .expect("a high-confidence, concept-named judgment fires alone, no co-fire needed");
        assert_eq!(
            offer_line(&evidence),
            "looks like you're circling ownership in parse_config \u{2014} hint"
        );
        // Still produces a candidate when a mechanical signal ALSO happens
        // to be active — fire-alone is a floor, not an exclusive path.
        assert!(select_perceived_candidate(Some(&pc), true).is_some());
    }
}

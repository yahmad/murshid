//! T5 req 1-2 / C12 — Bayesian Knowledge Tracing: category priors and the
//! pure two-step update (posterior-given-observation, then learning
//! transition). Standard BKT, no forgetting term beyond the discrete
//! level-down T5 req 5/6 implement elsewhere (staleness never lowers p
//! itself — I26/req 6).
//!
//! `hard` is graded `Correct` for the BKT math (req 2: "correct for the BKT
//! update but applies no downward help-level shift") — see [`Grade::is_correct`].

/// C12 hand-set priors, per taxonomy category (C8's `bug`/`idiom`/
/// `best-practice`/`architecture`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BktPriors {
    pub p_l0: f64,
    pub p_t: f64,
    pub p_g: f64,
    pub p_s: f64,
}

/// C12 binding v1 defaults: `p_L0/p_T/p_G/p_S` per category.
pub const BUG_PRIORS: BktPriors = BktPriors {
    p_l0: 0.30,
    p_t: 0.20,
    p_g: 0.15,
    p_s: 0.10,
};
pub const IDIOM_PRIORS: BktPriors = BktPriors {
    p_l0: 0.20,
    p_t: 0.15,
    p_g: 0.20,
    p_s: 0.10,
};
pub const BEST_PRACTICE_PRIORS: BktPriors = BktPriors {
    p_l0: 0.25,
    p_t: 0.15,
    p_g: 0.20,
    p_s: 0.10,
};
pub const ARCHITECTURE_PRIORS: BktPriors = BktPriors {
    p_l0: 0.15,
    p_t: 0.10,
    p_g: 0.25,
    p_s: 0.10,
};

/// C8: BKT priors key on the taxonomy category. Unrecognized categories fall
/// back to `idiom`'s priors — the repo convention (see
/// `ladder::directness_from_config`) of "unknown falls back to the balanced/
/// middle default" rather than panicking on pack data the engine doesn't
/// recognize.
pub fn priors_for_category(category: &str) -> BktPriors {
    match category {
        "bug" => BUG_PRIORS,
        "idiom" => IDIOM_PRIORS,
        "best-practice" => BEST_PRACTICE_PRIORS,
        "architecture" => ARCHITECTURE_PRIORS,
        _ => IDIOM_PRIORS,
    }
}

/// C4/C12: the classical mastery gate — p >= 0.95 drives I18's silence rung
/// and I24's fade announcement.
pub const MASTERY_THRESHOLD: f64 = 0.95;

pub fn is_mastered(p_mastery: f64) -> bool {
    p_mastery >= MASTERY_THRESHOLD
}

/// req 2/C8: the three evidence grades. `Hard` is BKT-correct but carries no
/// downward Wood shift (req 4) — see [`Grade::is_correct`] /
/// [`crate::ladder`]'s Wood-shift consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grade {
    Pass,
    Hard,
    Fail,
}

impl Grade {
    pub fn as_str(&self) -> &'static str {
        match self {
            Grade::Pass => "pass",
            Grade::Hard => "hard",
            Grade::Fail => "fail",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pass" => Some(Grade::Pass),
            "hard" => Some(Grade::Hard),
            "fail" => Some(Grade::Fail),
            _ => None,
        }
    }

    /// req 2: pass/hard are both "correct" observations for the BKT update;
    /// only fail is "incorrect".
    fn is_correct(&self) -> bool {
        !matches!(self, Grade::Fail)
    }
}

/// req 2 — the pure two-step BKT update: posterior given the observation,
/// then the learning transition. Standard formulation (no dependency, a
/// dozen lines of arithmetic):
///
/// ```text
/// P(L | correct)   = P(L)(1-p_S) / [P(L)(1-p_S) + (1-P(L))p_G]
/// P(L | incorrect) = P(L)p_S     / [P(L)p_S     + (1-P(L))(1-p_G)]
/// P(L') = P(L | obs) + (1 - P(L | obs)) * p_T
/// ```
///
/// Pure: no side effects, no I/O — the caller persists the result.
pub fn bkt_update(p_prior: f64, grade: Grade, priors: &BktPriors) -> f64 {
    let p = p_prior.clamp(0.0, 1.0);
    let p_given_obs = if grade.is_correct() {
        let numerator = p * (1.0 - priors.p_s);
        let denominator = numerator + (1.0 - p) * priors.p_g;
        if denominator == 0.0 { p } else { numerator / denominator }
    } else {
        let numerator = p * priors.p_s;
        let denominator = numerator + (1.0 - p) * (1.0 - priors.p_g);
        if denominator == 0.0 { p } else { numerator / denominator }
    };
    p_given_obs + (1.0 - p_given_obs) * priors.p_t
}

/// req 4 / C4 — Wood's contingent ±1 shift: pass -> -1 (less help next
/// time), fail -> +1 (more help), hard -> 0 (correct, but no downward shift
/// — C8's explicit carve-out).
pub fn wood_delta(grade: Grade) -> i32 {
    match grade {
        Grade::Pass => -1,
        Grade::Fail => 1,
        Grade::Hard => 0,
    }
}

/// req 4: `help_level` per concept follows Wood's ±1, clamped [0,3] — the
/// persisted, displayable "how much help this concept currently needs"
/// counter (req 9's progress-meter column).
pub fn update_help_level(current: i32, grade: Grade) -> i32 {
    (current + wood_delta(grade)).clamp(0, 3)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-9;

    fn approx(a: f64, b: f64) {
        assert!((a - b).abs() < EPS, "{} != {}", a, b);
    }

    // --- req 1: category priors table (C12) ---

    #[test]
    fn test_priors_for_category_matches_c12_table() {
        approx(priors_for_category("bug").p_l0, 0.30);
        approx(priors_for_category("idiom").p_l0, 0.20);
        approx(priors_for_category("best-practice").p_l0, 0.25);
        approx(priors_for_category("architecture").p_l0, 0.15);
    }

    #[test]
    fn test_priors_for_unknown_category_falls_back_to_idiom() {
        assert_eq!(priors_for_category("nonsense"), IDIOM_PRIORS);
    }

    // --- req 2: hand-computed BKT update, all four categories ---
    //
    // Formula recap (p = prior, priors = p_L0/p_T/p_G/p_S):
    //   correct:   p_obs = p(1-p_S) / [p(1-p_S) + (1-p)p_G]
    //   incorrect: p_obs = p*p_S    / [p*p_S    + (1-p)(1-p_G)]
    //   p' = p_obs + (1-p_obs) * p_T

    #[test]
    fn test_bkt_update_idiom_pass_hand_computed() {
        // idiom priors: p_L0=.20 p_T=.15 p_G=.20 p_S=.10; prior p=0.20 (=p_L0).
        // correct: p_obs = .20*.9 / (.20*.9 + .80*.20) = .18/.34 = 9/17
        // p' = 9/17 + (8/17)*.15 = 9/17 + 1.2/17 = 10.2/17 = 0.6 exactly.
        let p = bkt_update(0.20, Grade::Pass, &IDIOM_PRIORS);
        approx(p, 0.6);
    }

    #[test]
    fn test_bkt_update_idiom_hard_matches_pass_math() {
        // req 2: hard is "correct" for the BKT update — identical arithmetic
        // to pass.
        let pass = bkt_update(0.20, Grade::Pass, &IDIOM_PRIORS);
        let hard = bkt_update(0.20, Grade::Hard, &IDIOM_PRIORS);
        approx(pass, hard);
    }

    #[test]
    fn test_bkt_update_idiom_fail_hand_computed() {
        // incorrect: p_obs = .20*.10 / (.20*.10 + .80*.80) = .02/.66 = 1/33
        // p' = 1/33 + (32/33)*.15 = 1/33 + 4.8/33 = 5.8/33 = 0.175757575...
        let p = bkt_update(0.20, Grade::Fail, &IDIOM_PRIORS);
        approx(p, 5.8 / 33.0);
    }

    #[test]
    fn test_bkt_update_bug_pass_and_fail_hand_computed() {
        // bug priors: p_L0=.30 p_T=.20 p_G=.15 p_S=.10; prior p=0.30.
        // pass: p_obs = .30*.9 / (.30*.9 + .70*.15) = .27/.375 = .72
        //       p' = .72 + .28*.20 = .72 + .056 = .776
        let pass = bkt_update(0.30, Grade::Pass, &BUG_PRIORS);
        approx(pass, 0.776);

        // fail: p_obs = .30*.10 / (.30*.10 + .70*.85) = .03/.625 = .048
        //       p' = .048 + .952*.20 = .048 + .1904 = .2384
        let fail = bkt_update(0.30, Grade::Fail, &BUG_PRIORS);
        approx(fail, 0.2384);
    }

    #[test]
    fn test_bkt_update_best_practice_pass_and_fail_hand_computed() {
        // best-practice priors: p_L0=.25 p_T=.15 p_G=.20 p_S=.10; prior p=0.25.
        // pass: p_obs = .25*.9 / (.25*.9 + .75*.20) = .225/.375 = .6
        //       p' = .6 + .4*.15 = .6 + .06 = .66
        let pass = bkt_update(0.25, Grade::Pass, &BEST_PRACTICE_PRIORS);
        approx(pass, 0.66);

        // fail: p_obs = .25*.10 / (.25*.10 + .75*.80) = .025/.625 = .04
        //       p' = .04 + .96*.15 = .04 + .144 = .184
        let fail = bkt_update(0.25, Grade::Fail, &BEST_PRACTICE_PRIORS);
        approx(fail, 0.184);
    }

    #[test]
    fn test_bkt_update_architecture_pass_and_fail_hand_computed() {
        // architecture priors: p_L0=.15 p_T=.10 p_G=.25 p_S=.10; prior p=0.15.
        // pass: p_obs = .15*.9 / (.15*.9 + .85*.25) = .135/.3475 = 27/69.5
        //       = 0.38848920863...
        //       p' = p_obs + (1-p_obs)*.10
        let p_obs = 0.135 / 0.3475;
        let expected_pass = p_obs + (1.0 - p_obs) * 0.10;
        let pass = bkt_update(0.15, Grade::Pass, &ARCHITECTURE_PRIORS);
        approx(pass, expected_pass);
        approx(pass, 0.4496402877697842);

        // fail: p_obs = .15*.10 / (.15*.10 + .85*.75) = .015/.6525
        let p_obs_fail = 0.015 / 0.6525;
        let expected_fail = p_obs_fail + (1.0 - p_obs_fail) * 0.10;
        let fail = bkt_update(0.15, Grade::Fail, &ARCHITECTURE_PRIORS);
        approx(fail, expected_fail);
        approx(fail, 0.12068965517241381);
    }

    #[test]
    fn test_bkt_update_repeated_pass_sequence_climbs_toward_mastery() {
        // A pass/pass/pass sequence on idiom priors must monotonically
        // increase p and eventually cross the 0.95 mastery gate.
        let mut p = IDIOM_PRIORS.p_l0;
        let mut prev = p;
        for _ in 0..20 {
            p = bkt_update(p, Grade::Pass, &IDIOM_PRIORS);
            assert!(p >= prev, "p must be monotonically non-decreasing on repeated passes");
            prev = p;
        }
        assert!(is_mastered(p), "20 straight passes must cross the mastery gate");
    }

    #[test]
    fn test_bkt_update_stays_in_unit_interval() {
        for grade in [Grade::Pass, Grade::Hard, Grade::Fail] {
            for category in ["bug", "idiom", "best-practice", "architecture"] {
                let priors = priors_for_category(category);
                let p = bkt_update(0.5, grade, &priors);
                assert!((0.0..=1.0).contains(&p), "p out of range: {}", p);
            }
        }
    }

    // --- mastery gate ---

    #[test]
    fn test_is_mastered_boundary() {
        assert!(!is_mastered(0.9499999));
        assert!(is_mastered(0.95));
        assert!(is_mastered(0.99));
    }

    // --- req 4: Wood shift / help_level ---

    #[test]
    fn test_wood_delta() {
        assert_eq!(wood_delta(Grade::Pass), -1);
        assert_eq!(wood_delta(Grade::Fail), 1);
        assert_eq!(wood_delta(Grade::Hard), 0);
    }

    #[test]
    fn test_update_help_level_clamps_to_0_3() {
        assert_eq!(update_help_level(0, Grade::Pass), 0, "floor clamp");
        assert_eq!(update_help_level(3, Grade::Fail), 3, "ceiling clamp");
        assert_eq!(update_help_level(1, Grade::Pass), 0);
        assert_eq!(update_help_level(1, Grade::Fail), 2);
        assert_eq!(update_help_level(1, Grade::Hard), 1, "hard: no change");
    }

    #[test]
    fn test_grade_str_round_trip() {
        for g in [Grade::Pass, Grade::Hard, Grade::Fail] {
            assert_eq!(Grade::from_str(g.as_str()), Some(g));
        }
        assert_eq!(Grade::from_str("bogus"), None);
    }
}

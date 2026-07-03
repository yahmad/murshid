//! Push budget (C12 fixed `standard` detent) — token bucket with a strict-
//! mode bug exemption (T1 req 11 / D10).

use std::time::{Duration, SystemTime};

/// C12 `standard` detent: 1 push / 10 min, burst 1.
pub const STANDARD_REFILL_PERIOD: Duration = Duration::from_secs(600);
pub const STANDARD_BURST: u32 = 1;

pub struct TokenBucket {
    capacity: u32,
    refill_period: Duration,
    tokens: f64,
    last_update: SystemTime,
}

impl TokenBucket {
    pub fn new(capacity: u32, refill_period: Duration, now: SystemTime) -> Self {
        Self {
            capacity,
            refill_period,
            tokens: capacity as f64,
            last_update: now,
        }
    }

    /// The fixed-v1 `standard` detent bucket (C12): 1 card / 10 min, burst 1.
    pub fn standard(now: SystemTime) -> Self {
        Self::new(STANDARD_BURST, STANDARD_REFILL_PERIOD, now)
    }

    /// T2 req 1: a bucket shaped by the frequency knob's chosen detent
    /// (quiet/standard/chatty), burst 1 for every detent.
    pub fn for_detent(detent: &crate::noise::Detent, now: SystemTime) -> Self {
        Self::new(STANDARD_BURST, detent.refill_period, now)
    }

    fn refill(&mut self, now: SystemTime) {
        if let Ok(elapsed) = now.duration_since(self.last_update) {
            let add = elapsed.as_secs_f64() / self.refill_period.as_secs_f64();
            if add > 0.0 {
                self.tokens = (self.tokens + add).min(self.capacity as f64);
                self.last_update = now;
            }
        }
    }

    /// Attempts to consume one token at `now`. Returns true if allowed.
    pub fn try_consume(&mut self, now: SystemTime) -> bool {
        self.refill(now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    pub fn tokens_available(&self) -> f64 {
        self.tokens
    }
}

/// The stage-2 signals needed to decide whether a `likely_bug` card may
/// bypass the budget (T1 req 11 / C6 strict mode): a second independent
/// stage-2 sample agreeing on `likely_bug`, plus a concrete failure scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushCandidate {
    pub likely_bug: bool,
    pub strict_mode_passed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushDecision {
    Shown,
    Queued,
}

/// Decides whether `candidate` is shown now or queued, per T1 req 11: a
/// `likely_bug=true` card bypasses the budget ONLY if strict mode passed;
/// otherwise it waits on the token bucket like everything else.
pub fn decide_push(
    bucket: &mut TokenBucket,
    candidate: &PushCandidate,
    now: SystemTime,
) -> PushDecision {
    if candidate.likely_bug && candidate.strict_mode_passed {
        return PushDecision::Shown;
    }
    if bucket.try_consume(now) {
        PushDecision::Shown
    } else {
        PushDecision::Queued
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    #[test]
    fn test_burst_then_empty() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1000);
        let mut bucket = TokenBucket::standard(t0);
        assert!(bucket.try_consume(t0));
        assert!(!bucket.try_consume(t0), "burst of 1 should be exhausted");
    }

    #[test]
    fn test_refill_after_period() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1000);
        let mut bucket = TokenBucket::standard(t0);
        assert!(bucket.try_consume(t0));
        assert!(!bucket.try_consume(t0));

        let t_before_refill = t0 + Duration::from_secs(599);
        assert!(!bucket.try_consume(t_before_refill));

        let t_after_refill = t0 + STANDARD_REFILL_PERIOD;
        assert!(bucket.try_consume(t_after_refill));
    }

    #[test]
    fn test_decide_push_uses_bucket_when_not_strict_bug() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1000);
        let mut bucket = TokenBucket::standard(t0);

        let normal = PushCandidate {
            likely_bug: false,
            strict_mode_passed: false,
        };
        assert_eq!(decide_push(&mut bucket, &normal, t0), PushDecision::Shown);
        assert_eq!(decide_push(&mut bucket, &normal, t0), PushDecision::Queued);
    }

    #[test]
    fn test_strict_likely_bug_bypasses_empty_budget() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1000);
        let mut bucket = TokenBucket::standard(t0);
        // Drain the bucket.
        assert!(bucket.try_consume(t0));

        let strict_bug = PushCandidate {
            likely_bug: true,
            strict_mode_passed: true,
        };
        assert_eq!(
            decide_push(&mut bucket, &strict_bug, t0),
            PushDecision::Shown
        );
    }

    #[test]
    fn test_likely_bug_without_strict_mode_still_waits() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1000);
        let mut bucket = TokenBucket::standard(t0);
        assert!(bucket.try_consume(t0)); // drain

        let unverified_bug = PushCandidate {
            likely_bug: true,
            strict_mode_passed: false,
        };
        assert_eq!(
            decide_push(&mut bucket, &unverified_bug, t0),
            PushDecision::Queued
        );
    }
}

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

    /// Tokens projected to `now` WITHOUT mutating (accounts for refill since
    /// the last update) — the honest live value for a read-only display,
    /// since the bucket only actually refills on `try_consume`/`consume`.
    fn tokens_projected(&self, now: SystemTime) -> f64 {
        let elapsed = now.duration_since(self.last_update).unwrap_or(Duration::ZERO);
        let add = elapsed.as_secs_f64() / self.refill_period.as_secs_f64();
        (self.tokens + add).min(self.capacity as f64)
    }

    /// T15 "next nudge" indicator: whether a proactive card may fire right now
    /// (a whole token is available), projected to `now`.
    pub fn is_ready_at(&self, now: SystemTime) -> bool {
        self.tokens_projected(now) >= 1.0
    }

    /// T15 "next nudge" indicator: time until the next proactive card may fire
    /// (`None` if already ready) — 1 token accrues per `refill_period`, so the
    /// wait is the fraction of a token still needed times the period.
    pub fn time_until_ready_at(&self, now: SystemTime) -> Option<Duration> {
        let projected = self.tokens_projected(now);
        if projected >= 1.0 {
            None
        } else {
            let secs = (1.0 - projected) * self.refill_period.as_secs_f64();
            Some(Duration::from_secs_f64(secs.max(0.0)))
        }
    }

    /// T15 UX redesign (flagged need #1, build plan §7 Step 2): the bucket's
    /// burst capacity — the denominator the ambient band's budget gauge
    /// needs to render a fraction (`tokens_available() / capacity()`)
    /// instead of just the raw token count.
    pub fn capacity(&self) -> u32 {
        self.capacity
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

/// T3 req 12 / D16 mitigation / C7: an accepted struggle offer always shows
/// now, preempting the queue. This is a no-op `try_consume`: it takes a
/// token if one is available, and does nothing (no debt is recorded) if
/// the bucket is empty — there is deliberately no borrow/debt path (founder
/// decision). The caller always shows regardless of the result; this only
/// keeps the bucket's own bookkeeping honest when a token happens to be
/// available.
pub fn consume_or_borrow(bucket: &mut TokenBucket, now: SystemTime) {
    let _ = bucket.try_consume(now);
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
    fn test_capacity_reports_the_configured_burst() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1000);
        assert_eq!(TokenBucket::standard(t0).capacity(), STANDARD_BURST);
        assert_eq!(TokenBucket::new(5, Duration::from_secs(60), t0).capacity(), 5);
    }

    #[test]
    fn test_is_ready_and_time_until_ready_project_over_the_refill_period() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1000);
        let period = Duration::from_secs(600); // 10 min
        let mut bucket = TokenBucket::new(1, period, t0);
        // Full → ready, no ETA.
        assert!(bucket.is_ready_at(t0));
        assert_eq!(bucket.time_until_ready_at(t0), None);
        // Spend the token → empty → not ready, ETA ~ full period.
        assert!(bucket.try_consume(t0));
        assert!(!bucket.is_ready_at(t0));
        assert_eq!(bucket.time_until_ready_at(t0), Some(period));
        // Halfway through the period → ETA ~ half (projected without mutating).
        let half = t0 + Duration::from_secs(300);
        assert!(!bucket.is_ready_at(half));
        let eta = bucket.time_until_ready_at(half).unwrap();
        assert!((eta.as_secs_f64() - 300.0).abs() < 1.0, "eta {:?}", eta);
        // After a full period → ready again.
        let later = t0 + period;
        assert!(bucket.is_ready_at(later));
        assert_eq!(bucket.time_until_ready_at(later), None);
    }

    #[test]
    fn test_burst_then_empty() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1000);
        let mut bucket = TokenBucket::standard(t0);
        assert!(bucket.try_consume(t0));
        assert!(!bucket.try_consume(t0), "burst of 1 should be exhausted");
    }

    #[test]
    fn test_consume_or_borrow_is_a_no_op_when_bucket_is_empty() {
        // Pins the documented no-op behavior: when the bucket is empty,
        // `consume_or_borrow` does NOT go into debt — tokens stay at 0 and
        // the very next `try_consume` still fails.
        let t0 = UNIX_EPOCH + Duration::from_secs(1000);
        let mut bucket = TokenBucket::standard(t0);
        assert!(bucket.try_consume(t0)); // drain the single burst token
        assert_eq!(bucket.tokens_available(), 0.0);

        consume_or_borrow(&mut bucket, t0); // bucket is empty: no-op
        assert_eq!(bucket.tokens_available(), 0.0, "no debt path: stays at 0");
        assert!(
            !bucket.try_consume(t0),
            "no borrowed token should be available"
        );
    }

    #[test]
    fn test_consume_or_borrow_takes_a_token_when_available() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1000);
        let mut bucket = TokenBucket::standard(t0);
        assert_eq!(bucket.tokens_available(), 1.0);

        consume_or_borrow(&mut bucket, t0);
        assert_eq!(bucket.tokens_available(), 0.0);
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

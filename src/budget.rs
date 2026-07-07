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

    /// T17 R0: a bucket shaped by the single `[dial] min_gap` cooldown —
    /// capacity 1 (burst 1, same as every prior detent), refill = `gap`.
    /// Replaces the per-detent `for_detent` (T2/D10's quiet/standard/chatty
    /// knob is gone — see `noise.rs`'s doc).
    pub fn for_min_gap(gap: Duration, now: SystemTime) -> Self {
        Self::new(1, gap, now)
    }

    /// T17 R0: the `min_gap = "off"` kill switch — capacity 0, so
    /// `try_consume`/`is_ready_at` never succeed regardless of elapsed time
    /// (a proactive card never fires while off). The refill period is a
    /// placeholder (irrelevant at capacity 0, but must be nonzero to keep
    /// `refill`'s division well-defined).
    pub fn off(now: SystemTime) -> Self {
        Self::new(0, Duration::from_secs(60), now)
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

    /// T15 settings overlay (T17 R0: now the `min_gap`-dial change) updates
    /// the bucket's refill RATE live without resetting accrued tokens or
    /// touching `last_update` — only `refill_period` changes; `capacity`
    /// (the burst size) is untouched by this call, so `tokens` is simply
    /// clamped down in case it was ever to exceed it. Because
    /// `is_ready_at`/`time_until_ready_at` project from `self.refill_period`
    /// fresh on every call, the "next hint" ETA reflects the new rate on the
    /// very next read — no separate signal needed.
    pub fn set_refill_period(&mut self, period: Duration) {
        self.refill_period = period;
        self.tokens = self.tokens.min(self.capacity as f64);
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

    // --- T17 R0: min_gap cooldown replaces the frequency detent ---

    #[test]
    fn test_for_min_gap_is_capacity_one_burst_shaped_by_the_gap() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1000);
        let gap = Duration::from_secs(8 * 60);
        let mut bucket = TokenBucket::for_min_gap(gap, t0);
        assert_eq!(bucket.capacity(), 1);
        assert!(bucket.try_consume(t0));
        assert!(!bucket.try_consume(t0), "burst of 1 should be exhausted");

        let t_before = t0 + gap - Duration::from_secs(1);
        assert!(!bucket.try_consume(t_before));
        let t_after = t0 + gap;
        assert!(bucket.try_consume(t_after));
    }

    #[test]
    fn test_off_bucket_never_becomes_ready() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1000);
        let mut bucket = TokenBucket::off(t0);
        assert_eq!(bucket.capacity(), 0);
        assert!(!bucket.is_ready_at(t0));
        assert!(!bucket.try_consume(t0));

        // Even a very long time later, an off (capacity 0) bucket is still
        // never ready — this is the kill switch, not just a slow refill.
        let much_later = t0 + Duration::from_secs(60 * 60 * 24 * 365);
        assert!(!bucket.is_ready_at(much_later));
        assert!(!bucket.try_consume(much_later));
    }

    // --- T15 settings overlay: live refill-period change ---

    #[test]
    fn test_set_refill_period_shortens_next_nudge_eta() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1000);
        let mut bucket = TokenBucket::new(1, Duration::from_secs(600), t0);
        assert!(bucket.try_consume(t0)); // drain to empty
        let long_eta = bucket.time_until_ready_at(t0).unwrap();

        bucket.set_refill_period(Duration::from_secs(60));
        let short_eta = bucket.time_until_ready_at(t0).unwrap();

        assert!(
            short_eta < long_eta,
            "shortening the period must shorten the ETA: {:?} vs {:?}",
            short_eta,
            long_eta
        );
        assert!((short_eta.as_secs_f64() - 60.0).abs() < 0.01);
    }

    #[test]
    fn test_set_refill_period_never_leaves_tokens_above_capacity() {
        let t0 = UNIX_EPOCH + Duration::from_secs(1000);
        let mut bucket = TokenBucket::new(1, Duration::from_secs(600), t0);
        assert_eq!(bucket.tokens_available(), 1.0);
        bucket.set_refill_period(Duration::from_secs(60));
        assert!(bucket.tokens_available() <= bucket.capacity() as f64);
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

//! 011-rate-limiting-qos T017 — hand-rolled lazy-refill token bucket.
//!
//! Each cap dimension (`bandwidth_in`, `bandwidth_out`,
//! `new_connections_per_sec`) gets one [`TokenBucket`]. `acquire(n)`
//! either succeeds (debits the pool) or returns the sleep deficit the
//! caller should `tokio::time::sleep` for before retrying. The pool
//! can never go negative; lazy refill mints up to `burst` tokens on
//! the next observe.
//!
//! No new workspace deps (R-001): only `tokio::time::Instant` for the
//! monotonic clock and `std::sync::atomic` for `tokens` /
//! `last_refill_micros`. Hot-reload of `rate` / `burst` is the
//! containing [`RuleRateLimiter`]'s concern (T018, R-008) — this
//! struct is intentionally state-stable per construction.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::time::Instant;

/// Lazy-refill token bucket.
///
/// `rate_per_sec` and `burst` are immutable for the bucket's lifetime.
/// Hot-reload swaps the entire owning limiter via Arc; the new bucket
/// inherits the old `tokens` / `last_refill` snapshot via
/// [`TokenBucket::with_carryover`] so a cap raise/lower never resets
/// the pool to a free-for-all burst nor stalls live flows.
#[derive(Debug)]
pub struct TokenBucket {
    /// Refill rate, tokens per second. Validated `> 0` by
    /// `portunus_core::rate_limit::validate` before construction.
    rate_per_sec: u64,
    /// Maximum pool size and starting fill. Validated
    /// `rate / 100 ≤ burst ≤ rate * 60` (R-011).
    burst: u64,
    /// Current pool, capped at `burst`.
    tokens: AtomicU64,
    /// Monotonic-clock reference micros (anchored to `epoch`).
    last_refill_micros: AtomicU64,
    /// Anchor for converting Instant → micros without wrapping. The
    /// process-lifetime monotonic anchor.
    epoch: Instant,
}

/// Outcome of [`TokenBucket::acquire`]. Sleep `deficit` then retry
/// (the loop may be a single retry — the deficit is always sufficient
/// to cover `n` at the configured rate).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Acquire {
    /// `n` tokens debited.
    Granted,
    /// Caller should sleep `deficit` before retrying. `deficit` is
    /// `(needed_tokens / rate)` rounded up to the nearest microsecond.
    Throttled { deficit: Duration },
}

impl TokenBucket {
    /// Construct a fresh bucket with the pool full at `burst`.
    #[must_use]
    pub fn new(rate_per_sec: u64, burst: u64) -> Self {
        debug_assert!(rate_per_sec > 0, "rate must be > 0 (validated upstream)");
        debug_assert!(burst > 0, "burst must be > 0 (validated upstream)");
        Self {
            rate_per_sec,
            burst,
            tokens: AtomicU64::new(burst),
            last_refill_micros: AtomicU64::new(0),
            epoch: Instant::now(),
        }
    }

    /// Construct a fresh bucket but inherit `tokens` and the relative
    /// `last_refill` micros from a pre-existing bucket. Used by the
    /// hot-reload path so a cap change preserves accumulated state
    /// (R-008): a raise doesn't suddenly mint a full new burst, and a
    /// lower doesn't strand the pool above the new ceiling.
    pub fn with_carryover(rate_per_sec: u64, burst: u64, prior: &TokenBucket) -> Self {
        debug_assert!(rate_per_sec > 0);
        debug_assert!(burst > 0);
        let inherited_tokens = prior.tokens.load(Ordering::Acquire).min(burst);
        // Anchor `last_refill_micros = 0` against a fresh `epoch = now`.
        // Refill from `prior.last_refill_abs` to `now` does not carry
        // forward — the new rate/burst defines a new accounting frame
        // and we already preserved the only piece of pool state that
        // matters (the token count).
        Self {
            rate_per_sec,
            burst,
            tokens: AtomicU64::new(inherited_tokens),
            last_refill_micros: AtomicU64::new(0),
            epoch: Instant::now(),
        }
    }

    #[allow(dead_code)] // wired up in T018 (RuleRateLimiter hot-reload introspection)
    pub fn rate_per_sec(&self) -> u64 {
        self.rate_per_sec
    }

    #[allow(dead_code)] // wired up in T018 (RuleRateLimiter hot-reload introspection)
    pub fn burst(&self) -> u64 {
        self.burst
    }

    /// Snapshot the current pool. For tests / observability only —
    /// the live count is racy under concurrent acquire.
    pub fn tokens_snapshot(&self) -> u64 {
        self.tokens.load(Ordering::Acquire)
    }

    /// Try to debit `n` tokens. On success the pool is decreased by
    /// `n`; on starvation the pool is left at zero and the deficit is
    /// reported. A second acquire attempt strictly after the deficit
    /// has elapsed will succeed.
    ///
    /// Sleeps are the caller's responsibility — this method is sync
    /// and never blocks.
    pub fn acquire(&self, n: u64) -> Acquire {
        if n == 0 {
            return Acquire::Granted;
        }
        let now_micros = self.now_micros();
        self.refill(now_micros);
        // Single CAS loop: load current pool, branch on whether `n`
        // fits. On success debit and return. On starvation report the
        // deficit; do not partially debit.
        loop {
            let current = self.tokens.load(Ordering::Acquire);
            if current >= n {
                let next = current - n;
                if self
                    .tokens
                    .compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
                {
                    return Acquire::Granted;
                }
                continue; // contended, retry
            }
            // Starved: caller must sleep until refill covers `n - current`.
            let needed = n - current;
            let deficit_micros = needed
                .saturating_mul(1_000_000)
                .div_ceil(self.rate_per_sec.max(1));
            return Acquire::Throttled {
                deficit: Duration::from_micros(deficit_micros),
            };
        }
    }

    fn now_micros(&self) -> u64 {
        let dur = Instant::now().saturating_duration_since(self.epoch);
        u64::try_from(dur.as_micros()).unwrap_or(u64::MAX)
    }

    /// Lazy refill: compute how many tokens have accrued since
    /// `last_refill` and add them, capped at `burst`. Idempotent under
    /// concurrent calls — the loser of a race observes the winner's
    /// updated pool/timestamp on its next acquire.
    fn refill(&self, now_micros: u64) {
        let last = self.last_refill_micros.load(Ordering::Acquire);
        if now_micros <= last {
            return;
        }
        let elapsed_micros = now_micros - last;
        // tokens_to_add = elapsed_micros * rate_per_sec / 1_000_000
        let added_u128 = u128::from(elapsed_micros) * u128::from(self.rate_per_sec) / 1_000_000;
        let added = u64::try_from(added_u128).unwrap_or(u64::MAX);
        if added == 0 {
            // Less than one token's worth of time has elapsed; do not
            // advance `last_refill_micros` so the fractional time
            // accumulates.
            return;
        }
        // Try to publish (last → now_micros). On a lost race the
        // winner has already minted, so skip.
        if self
            .last_refill_micros
            .compare_exchange(last, now_micros, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        // Add up to burst.
        let mut current = self.tokens.load(Ordering::Acquire);
        loop {
            let next = current.saturating_add(added).min(self.burst);
            if next == current {
                return;
            }
            match self.tokens.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(observed) => current = observed,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn acquire_succeeds_within_burst() {
        let b = TokenBucket::new(1_000_000, 10_000);
        assert_eq!(b.acquire(5_000), Acquire::Granted);
        assert_eq!(b.acquire(5_000), Acquire::Granted);
        // Pool exhausted; next acquire should be throttled.
        match b.acquire(1) {
            Acquire::Throttled { deficit } => assert!(deficit > Duration::ZERO),
            Acquire::Granted => panic!("should be throttled"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn refill_recovers_after_sleep() {
        let b = TokenBucket::new(1_000, 1_000);
        // Drain.
        assert_eq!(b.acquire(1_000), Acquire::Granted);
        assert!(matches!(b.acquire(1), Acquire::Throttled { .. }));
        // Advance the paused clock by 500 ms — should mint ~500 tokens.
        tokio::time::advance(Duration::from_millis(500)).await;
        assert_eq!(b.acquire(500), Acquire::Granted);
    }

    #[tokio::test(start_paused = true)]
    async fn deficit_matches_rate() {
        // 100 tokens/sec, burst 100 — drained pool needs 1s to refill 100.
        let b = TokenBucket::new(100, 100);
        assert_eq!(b.acquire(100), Acquire::Granted);
        match b.acquire(50) {
            Acquire::Throttled { deficit } => {
                // 50 tokens at 100/sec ≈ 500 ms; allow ±10% slack.
                let lo = Duration::from_millis(450);
                let hi = Duration::from_millis(550);
                assert!(
                    deficit >= lo && deficit <= hi,
                    "deficit {deficit:?} out of expected band [{lo:?}, {hi:?}]"
                );
            }
            Acquire::Granted => panic!("should be throttled"),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn pool_caps_at_burst() {
        let b = TokenBucket::new(1_000, 100);
        // No drain — wait long enough that refill would overshoot if
        // it weren't clamped, then drain in one go.
        tokio::time::advance(Duration::from_secs(10)).await;
        assert_eq!(b.acquire(100), Acquire::Granted);
        assert!(matches!(b.acquire(1), Acquire::Throttled { .. }));
    }

    #[tokio::test(start_paused = true)]
    async fn zero_acquire_is_a_noop_grant() {
        let b = TokenBucket::new(1, 1);
        // Drain so the bucket has nothing to give but `n=0` still
        // succeeds.
        let _ = b.acquire(1);
        assert_eq!(b.acquire(0), Acquire::Granted);
    }

    #[tokio::test(start_paused = true)]
    async fn carryover_preserves_tokens_on_rate_raise() {
        let prior = TokenBucket::new(1_000, 1_000);
        // Drain half.
        assert_eq!(prior.acquire(500), Acquire::Granted);
        let snapshot = prior.tokens_snapshot();
        assert!(snapshot <= 500, "expected ~500 left, got {snapshot}");

        // Raise rate. Carry forward existing tokens — must not jump
        // back up to the new burst.
        let next = TokenBucket::with_carryover(2_000, 2_000, &prior);
        let after = next.tokens_snapshot();
        assert!(
            after <= snapshot,
            "carryover minted free tokens: {after} > {snapshot}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn carryover_clamps_tokens_on_burst_lower() {
        let prior = TokenBucket::new(10_000, 10_000);
        // Pool starts at 10_000.
        assert_eq!(prior.tokens_snapshot(), 10_000);
        // New bucket with burst 1000 — tokens must be clamped down.
        let next = TokenBucket::with_carryover(10_000, 1_000, &prior);
        assert_eq!(next.tokens_snapshot(), 1_000);
        // Bucket is full at the new burst — second acquire of >burst
        // throttles.
        assert_eq!(next.acquire(1_000), Acquire::Granted);
        assert!(matches!(next.acquire(1), Acquire::Throttled { .. }));
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_acquire_does_not_double_spend() {
        // Two tasks race to drain a 1000-token bucket; together they
        // must take exactly 1000 (no double-spend, no leftover).
        let b = std::sync::Arc::new(TokenBucket::new(10, 1_000));
        let mut joins = Vec::new();
        for _ in 0..10 {
            let b = b.clone();
            joins.push(tokio::spawn(async move {
                let mut got = 0u64;
                for _ in 0..100 {
                    if b.acquire(1) == Acquire::Granted {
                        got += 1;
                    }
                }
                got
            }));
        }
        let mut total = 0u64;
        for j in joins {
            total += j.await.unwrap();
        }
        assert_eq!(total, 1_000, "race produced {total} grants, expected 1000");
    }
}

//! 013-traffic-quotas v1.4.0 client side: per-(user, client) byte
//! budget enforcement.
//!
//! `QuotaHandle` is the per-pair atomic budget consulted by the
//! data-plane copy loops (TCP userspace E1/E2, splice per-iteration
//! hook E3, UDP per-datagram E4). A saturating CAS consume preserves
//! the `remaining >= 0` invariant under concurrent IO, eliminating
//! underflow / wrap (spec §4.3 decision 6). Once the budget hits
//! zero, subsequent `consume` calls observe `is_exhausted = true` and
//! the caller can short-circuit without further atomics.

#![allow(
    dead_code,
    reason = "QuotaHandle / QuotaState / ConsumeOutcome are consumed by D2 QuotaScopeManager and the E-phase data-plane hooks; this module ships first to land the atomic primitive in isolation."
)]

pub mod scope;

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

#[derive(Debug)]
pub struct QuotaHandle {
    pub user_id: String,
    pub client_name: String,
    remaining: AtomicI64,
    exhausted: AtomicBool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsumeOutcome {
    Granted,
    Exhausted,
}

#[derive(Debug, Clone, Copy)]
pub struct QuotaState {
    pub monthly_bytes: i64,
    pub budget_remaining_bytes: i64,
    pub exhausted: bool,
}

impl QuotaHandle {
    #[must_use]
    pub fn new(user_id: String, client_name: String, state: QuotaState) -> Self {
        let init_remaining = state.budget_remaining_bytes.max(0);
        Self {
            user_id,
            client_name,
            remaining: AtomicI64::new(init_remaining),
            exhausted: AtomicBool::new(state.exhausted || init_remaining == 0),
        }
    }

    /// Replace the handle's state atomically (called on
    /// `TrafficQuotaUpdate{SET}` push or reconnect replay).
    pub fn replace(&self, state: QuotaState) {
        let new_remaining = state.budget_remaining_bytes.max(0);
        self.remaining.store(new_remaining, Ordering::Release);
        let was_exhausted = state.exhausted || new_remaining == 0;
        self.exhausted.store(was_exhausted, Ordering::Release);
    }

    #[must_use]
    pub fn is_exhausted(&self) -> bool {
        self.exhausted.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn remaining(&self) -> i64 {
        self.remaining.load(Ordering::Relaxed)
    }

    /// Try to consume `n` bytes. Returns `Granted` if the budget
    /// allowed the full draw, `Exhausted` if the call straddled the
    /// boundary (the saturating CAS still subtracts what is left, so
    /// `remaining` is exactly 0 afterward). Subsequent consumes
    /// short-circuit via the `exhausted` flag.
    pub fn consume(&self, n: i64) -> ConsumeOutcome {
        debug_assert!(n >= 0, "negative consume {n}");
        // Fast path: already exhausted.
        if self.exhausted.load(Ordering::Acquire) {
            return ConsumeOutcome::Exhausted;
        }
        let mut cur = self.remaining.load(Ordering::Relaxed);
        loop {
            if cur <= 0 {
                self.mark_exhausted();
                return ConsumeOutcome::Exhausted;
            }
            // Saturating subtract — `remaining` never goes below 0.
            let new = (cur - n).max(0);
            match self.remaining.compare_exchange_weak(
                cur,
                new,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    if new == 0 {
                        self.mark_exhausted();
                    }
                    // A `cur - n < 0` draw means we asked for more
                    // than was available; only the bytes that fit
                    // were credited. Caller treats that as a
                    // budget-straddling fail (Exhausted). An exact
                    // draw (`cur - n == 0`) succeeded in full but
                    // leaves the handle marked exhausted for the
                    // next call.
                    if cur - n < 0 {
                        return ConsumeOutcome::Exhausted;
                    }
                    return ConsumeOutcome::Granted;
                }
                Err(actual) => {
                    cur = actual;
                }
            }
        }
    }

    fn mark_exhausted(&self) {
        let _ = self
            .exhausted
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    fn state(monthly: i64, remaining: i64, exhausted: bool) -> QuotaState {
        QuotaState {
            monthly_bytes: monthly,
            budget_remaining_bytes: remaining,
            exhausted,
        }
    }

    #[test]
    fn consume_grants_under_budget() {
        let h = QuotaHandle::new("u".into(), "c".into(), state(1_000, 1_000, false));
        assert_eq!(h.consume(100), ConsumeOutcome::Granted);
        assert_eq!(h.remaining(), 900);
        assert!(!h.is_exhausted());
    }

    #[test]
    fn consume_exhausts_at_zero() {
        let h = QuotaHandle::new("u".into(), "c".into(), state(100, 100, false));
        assert_eq!(h.consume(100), ConsumeOutcome::Granted);
        assert_eq!(h.remaining(), 0);
        assert!(h.is_exhausted());
        assert_eq!(h.consume(1), ConsumeOutcome::Exhausted);
    }

    #[test]
    fn consume_saturates_does_not_underflow() {
        // After CAS hits 0, future consumes never bring remaining negative.
        let h = QuotaHandle::new("u".into(), "c".into(), state(100, 100, false));
        for _ in 0..1_000 {
            let _ = h.consume(10);
        }
        assert!(h.remaining() >= 0);
        assert!(h.is_exhausted());
    }

    #[test]
    fn replace_resets_exhausted_when_budget_returns() {
        let h = QuotaHandle::new("u".into(), "c".into(), state(100, 0, true));
        assert!(h.is_exhausted());
        h.replace(state(200, 200, false));
        assert!(!h.is_exhausted());
        assert_eq!(h.consume(50), ConsumeOutcome::Granted);
    }

    #[test]
    fn new_with_negative_remaining_starts_exhausted() {
        // Spec §4.2: budget_remaining_bytes "may be negative when
        // exhausted". Clamp the handle's atomic at 0; mark exhausted.
        let h = QuotaHandle::new("u".into(), "c".into(), state(100, -42, true));
        assert!(h.is_exhausted());
        assert_eq!(h.remaining(), 0);
    }

    #[test]
    fn concurrent_consumes_stay_at_or_above_zero() {
        let h = Arc::new(QuotaHandle::new(
            "u".into(),
            "c".into(),
            state(10_000, 10_000, false),
        ));
        let mut handles = Vec::new();
        for _ in 0..16 {
            let h2 = Arc::clone(&h);
            handles.push(thread::spawn(move || {
                for _ in 0..1_000 {
                    let _ = h2.consume(2);
                }
            }));
        }
        for jh in handles {
            jh.join().unwrap();
        }
        assert!(h.remaining() >= 0);
        assert!(h.is_exhausted());
    }
}

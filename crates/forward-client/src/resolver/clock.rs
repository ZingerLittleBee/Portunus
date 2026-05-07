//! Time abstraction for the resolver cache.
//!
//! T028 (003-domain-name-forward US2): the cache state machine has
//! several time-driven transitions (TTL expiry, stale-while-error
//! grace, negative-cache retry). Production wraps `Instant::now`;
//! tests use `MockClock` so they can advance time deterministically
//! without `tokio::time::sleep`.

use std::time::Instant;

/// Anything that can answer "what time is it now?". Production code
/// uses [`SystemClock`]; tests use the `MockClock` from
/// `super::test_support`.
pub(super) trait Clock: Send + Sync + std::fmt::Debug {
    fn now(&self) -> Instant;
}

#[derive(Debug, Default)]
pub(super) struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

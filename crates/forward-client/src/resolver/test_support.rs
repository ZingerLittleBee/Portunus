//! T022 (003-domain-name-forward US2): shared test fixtures for the
//! resolver layer — `MockResolver` (configurable answers/errors,
//! optional delay) and `MockClock` (advanceable time source).
//!
//! These live in a non-test module guarded by `#[cfg(test)]` so both
//! `resolver::cache::tests` and `resolver::tests` can pull them in
//! without re-implementing the same scaffolding.

#![cfg(test)]

use std::collections::VecDeque;
use std::net::IpAddr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use forward_core::Hostname;

use super::clock::Clock;
use super::{Resolve, ResolveAnswer, ResolverError};

/// Manually-advanced clock. `now()` returns `base + offset`; tests
/// call `advance(d)` between cache operations to simulate elapsed
/// time without sleeping.
#[derive(Debug)]
pub(crate) struct MockClock {
    base: Instant,
    offset: Mutex<Duration>,
}

impl MockClock {
    pub(crate) fn new() -> Self {
        Self {
            base: Instant::now(),
            offset: Mutex::new(Duration::ZERO),
        }
    }

    pub(crate) fn advance(&self, d: Duration) {
        let mut g = self.offset.lock().unwrap();
        *g += d;
    }
}

impl Clock for MockClock {
    fn now(&self) -> Instant {
        self.base + *self.offset.lock().unwrap()
    }
}

/// Resolver fixture with a queue of canned answers and an optional
/// per-call delay (for exercising the single-flight Pending window).
#[derive(Debug)]
pub(crate) struct MockResolver {
    calls: AtomicUsize,
    inner: Mutex<MockState>,
}

#[derive(Debug)]
struct MockState {
    /// Sequential responses. The Nth `resolve()` call pops the front;
    /// when empty, the resolver re-uses the last answer (so tests
    /// only need to enqueue what's interesting).
    queue: VecDeque<Result<ResolveAnswer, ResolverError>>,
    last: Option<Result<ResolveAnswer, ResolverError>>,
    /// Delay applied to every `resolve()` call. Used by single-flight
    /// tests to widen the Pending window.
    delay: Option<Duration>,
}

impl MockResolver {
    /// Always returns a single successful answer with the given addrs/TTL.
    pub(crate) fn ok(addrs: Vec<IpAddr>, ttl: Duration) -> Self {
        let answer = Ok(ResolveAnswer { addrs, ttl });
        Self {
            calls: AtomicUsize::new(0),
            inner: Mutex::new(MockState {
                queue: VecDeque::new(),
                last: Some(answer),
                delay: None,
            }),
        }
    }

    /// First call returns `Ok(addrs, ttl)`, every subsequent call
    /// returns `err`. Used for stale-while-error tests where the
    /// cache primes on success then refreshes against a broken
    /// resolver.
    pub(crate) fn ok_then_fail(addrs: Vec<IpAddr>, ttl: Duration, err: ResolverError) -> Self {
        let mut q: VecDeque<Result<ResolveAnswer, ResolverError>> = VecDeque::new();
        q.push_back(Ok(ResolveAnswer { addrs, ttl }));
        Self {
            calls: AtomicUsize::new(0),
            inner: Mutex::new(MockState {
                queue: q,
                last: Some(Err(err)),
                delay: None,
            }),
        }
    }

    /// Always fails with the same error.
    pub(crate) fn always_fail(err: ResolverError) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            inner: Mutex::new(MockState {
                queue: VecDeque::new(),
                last: Some(Err(err)),
                delay: None,
            }),
        }
    }

    /// Successful answer with a per-call delay. Used by the
    /// single-flight test to keep the cache in the Pending state long
    /// enough for concurrent waiters to pile up.
    pub(crate) fn delayed_ok(addrs: Vec<IpAddr>, ttl: Duration, delay: Duration) -> Self {
        let me = Self::ok(addrs, ttl);
        me.inner.lock().unwrap().delay = Some(delay);
        me
    }

    pub(crate) fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }

    fn next(&self) -> Result<ResolveAnswer, ResolverError> {
        let mut g = self.inner.lock().unwrap();
        if let Some(front) = g.queue.pop_front() {
            return front;
        }
        g.last
            .clone()
            .expect("MockResolver has no canned response — call ok()/always_fail() first")
    }
}

#[async_trait]
impl Resolve for MockResolver {
    async fn resolve(&self, _name: &Hostname) -> Result<ResolveAnswer, ResolverError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let delay = self.inner.lock().unwrap().delay;
        if let Some(d) = delay {
            tokio::time::sleep(d).await;
        }
        self.next()
    }
}

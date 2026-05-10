//! T053 + T054 (003-domain-name-forward, US Polish): DNS-resolver
//! cache-hit hot-path + single-flight coalescing benchmarks.
//!
//! Like `data_plane.rs` and `range_install.rs`, these reproduce the
//! production shape inline: `portunus-client` ships as a binary with
//! no `lib` target, so benches cannot import `LiveResolver` /
//! `Cache` directly. Any divergence between the inlined model here
//! and the production cache is itself a regression worth catching —
//! the cache state machine in
//! `crates/portunus-client/src/resolver/cache.rs` is the source of
//! truth.
//!
//! ## What we measure
//!
//! - **`dns_resolver_cache_hit`** (T053 / SC-004): warm-cache lookup
//!   cost. The cache stores `Resolved { addrs, expiry }` under a
//!   `tokio::sync::Mutex<HashMap>`; a hit pays one async-mutex
//!   acquisition + one HashMap get + one `Vec<IpAddr>` clone. The
//!   median MUST be ≪ a kernel `connect()` syscall (microseconds vs
//!   sub-millisecond at minimum on loopback) so adding a DNS rule
//!   doesn't regress the per-connection budget.
//!
//! - **`dns_resolver_singleflight`** (T054 / FR-012): under burst,
//!   N concurrent first-connects to the SAME unresolved hostname
//!   collapse to ONE upstream resolver call. The benchmark spins up
//!   100 concurrent Tokio tasks, all waiting on a single Pending
//!   placeholder backed by `tokio::sync::Notify`; we assert the
//!   resolver call counter ends at exactly 1 and report the median
//!   per-task wakeup latency.
//!
//! SC-005 ("≤ 1 query per rule per cache window across 100 mixed
//! rules") follows by composition: per-rule single-flight (this
//! bench) × per-rule cache lifetime (the unit-tested cache state
//! machine — see `cache::tests::*`). No fleet-scale bench is run
//! because the bound is structural, not statistical.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use criterion::{Criterion, criterion_group, criterion_main};
use tokio::runtime::Runtime;
use tokio::sync::{Mutex, Notify};

#[derive(Debug, Clone)]
enum Entry {
    Resolved { addrs: Vec<IpAddr>, expiry: Instant },
    Pending { notify: Arc<Notify> },
}

#[derive(Default)]
struct Cache {
    inner: Mutex<HashMap<String, Entry>>,
}

impl Cache {
    /// Cache-hit fast path equivalent to
    /// `Cache::get_or_resolve` returning `AnswerSource::Cached`.
    /// Holds the mutex for one HashMap get + one Vec clone, no awaits
    /// past the lock acquisition.
    async fn cached_get(&self, name: &str, now: Instant) -> Option<Vec<IpAddr>> {
        let g = self.inner.lock().await;
        match g.get(name) {
            Some(Entry::Resolved { addrs, expiry }) if now < *expiry => Some(addrs.clone()),
            _ => None,
        }
    }

    /// Single-flight pattern: first caller installs Pending and
    /// drives the resolver; concurrent callers wait on the Notify
    /// and re-read on wakeup. Mirrors the `Action::{Wait, Refresh}`
    /// loop in production.
    async fn get_or_resolve_single_flight(
        &self,
        name: &str,
        resolver_call_count: &AtomicUsize,
    ) -> Vec<IpAddr> {
        loop {
            let action = {
                let mut g = self.inner.lock().await;
                match g.get(name) {
                    Some(Entry::Resolved { addrs, .. }) => return addrs.clone(),
                    Some(Entry::Pending { notify }) => Action::Wait(Arc::clone(notify)),
                    None => {
                        let notify = Arc::new(Notify::new());
                        g.insert(
                            name.to_string(),
                            Entry::Pending {
                                notify: Arc::clone(&notify),
                            },
                        );
                        Action::Refresh(notify)
                    }
                }
            };
            match action {
                Action::Wait(n) => {
                    n.notified().await;
                }
                Action::Refresh(notify) => {
                    // Simulate an upstream resolver call. The
                    // production path awaits `R::resolve` here.
                    resolver_call_count.fetch_add(1, Ordering::Relaxed);
                    tokio::time::sleep(Duration::from_micros(50)).await;
                    let addrs = vec![IpAddr::V4(Ipv4Addr::LOCALHOST)];
                    let mut g = self.inner.lock().await;
                    g.insert(
                        name.to_string(),
                        Entry::Resolved {
                            addrs: addrs.clone(),
                            expiry: Instant::now() + Duration::from_secs(60),
                        },
                    );
                    notify.notify_waiters();
                    return addrs;
                }
            }
        }
    }
}

enum Action {
    Wait(Arc<Notify>),
    Refresh(Arc<Notify>),
}

fn bench_cache_hit(c: &mut Criterion) {
    let rt = Runtime::new().expect("tokio runtime");
    let cache = rt.block_on(async {
        let c = Cache::default();
        c.inner.lock().await.insert(
            "api.example.com".into(),
            Entry::Resolved {
                addrs: vec![IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1))],
                expiry: Instant::now() + Duration::from_secs(300),
            },
        );
        c
    });
    let cache = Arc::new(cache);

    c.bench_function("dns_resolver_cache_hit", |b| {
        b.iter(|| {
            rt.block_on(async {
                let now = Instant::now();
                let v = cache.cached_get("api.example.com", now).await;
                assert!(v.is_some());
            });
        });
    });
}

fn bench_singleflight(c: &mut Criterion) {
    const N: usize = 100;
    let rt = Runtime::new().expect("tokio runtime");

    c.bench_function("dns_resolver_singleflight_100x", |b| {
        b.iter(|| {
            rt.block_on(async {
                let cache = Arc::new(Cache::default());
                let counter = Arc::new(AtomicUsize::new(0));
                let mut handles = Vec::with_capacity(N);
                for _ in 0..N {
                    let cache = Arc::clone(&cache);
                    let counter = Arc::clone(&counter);
                    handles.push(tokio::spawn(async move {
                        cache
                            .get_or_resolve_single_flight("burst.example", &counter)
                            .await
                    }));
                }
                for h in handles {
                    let _ = h.await;
                }
                // FR-012 invariant: every burst collapses to ONE
                // upstream resolver call. A regression here means the
                // single-flight machinery has broken — the bench
                // panics rather than silently reporting numbers.
                assert_eq!(
                    counter.load(Ordering::Relaxed),
                    1,
                    "single-flight FR-012 violated: expected 1 resolver call, got {}",
                    counter.load(Ordering::Relaxed)
                );
            });
        });
    });
}

criterion_group!(benches, bench_cache_hit, bench_singleflight);
criterion_main!(benches);

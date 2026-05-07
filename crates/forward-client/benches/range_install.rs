//! T055 — port-range bind fan-out benchmark.
//!
//! `forwarder::range::bind_all` calls `TcpListener::bind` once per port
//! in the rule's listen range. SC-001 ("operator can push and tear down
//! a 1024-port range in under five seconds") boils down to: how long do
//! 1024 sequential `bind` syscalls take on the target host? This bench
//! reproduces that shape against the loopback `0.0.0.0` address (same
//! `BindOpts` as the production code) and reports a wall-clock per range
//! size, so we can spot regressions before they bite operators.
//!
//! The bench does NOT exercise the public `bind_all` directly — the
//! `forward-client` crate is a binary with no lib target, so we
//! reproduce its bind shape inline. Any divergence between this bench
//! and `forwarder::range::bind_all` is itself a regression worth
//! catching.

use std::net::Ipv4Addr;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use tokio::net::TcpListener;
use tokio::runtime::Runtime;

/// Bind `count` ephemeral loopback ports sequentially. Mirrors the
/// `bind_all` shape: any failure rolls back every previously-bound
/// listener. Returns the bound listeners; the caller drops them to
/// release the ports.
async fn bind_n(count: usize) -> Vec<TcpListener> {
    let mut bound = Vec::with_capacity(count);
    for _ in 0..count {
        match TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await {
            Ok(l) => bound.push(l),
            Err(e) => panic!("bind failed at {}/{count}: {e}", bound.len()),
        }
    }
    bound
}

fn bench_bind_fanout(c: &mut Criterion) {
    let rt = Runtime::new().expect("tokio runtime");
    let mut group = c.benchmark_group("range_install");
    group.sample_size(20);
    group.measurement_time(Duration::from_secs(8));
    for size in [1usize, 10, 100, 1024] {
        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.iter(|| {
                rt.block_on(async {
                    let bound = bind_n(size).await;
                    drop(bound);
                });
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_bind_fanout);
criterion_main!(benches);

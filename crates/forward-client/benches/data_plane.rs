//! T054a — single-rule loopback data-plane benchmark harness.
//!
//! Measures throughput and added latency of the proxy primitive. The
//! `proxy()` function in `crates/forward-client/src/forwarder/proxy.rs`
//! delegates to `tokio::io::copy_bidirectional` after a single connect, so
//! this bench reproduces that shape end-to-end (loopback echo + an in-bench
//! proxy task) and exercises the same kernel/tokio paths.
//!
//! Constitution Principle II requires every hot-path-touching change to ship
//! with a benchmark. This file establishes the harness; T065 captures a
//! baseline once US3 lands.

use std::net::{Ipv4Addr, SocketAddr};
use std::time::{Duration, Instant};

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use tokio::io::{AsyncReadExt, AsyncWriteExt, copy_bidirectional};
use tokio::net::{TcpListener, TcpStream};
use tokio::runtime::Runtime;

/// Spawn an echo server. Returns the listening address and a handle that the
/// caller drops to stop accepting.
async fn spawn_echo() -> SocketAddr {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 64 * 1024];
                while let Ok(n) = sock.read(&mut buf).await {
                    if n == 0 {
                        break;
                    }
                    if sock.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    addr
}

/// Spawn the proxy primitive on its own listener, identical in shape to
/// `forwarder::proxy::proxy`. Returns the listen address.
async fn spawn_proxy(target: SocketAddr) -> SocketAddr {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((mut inbound, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                if let Ok(mut outbound) = TcpStream::connect(target).await {
                    let _ = inbound.set_nodelay(true);
                    let _ = outbound.set_nodelay(true);
                    let _ = copy_bidirectional(&mut inbound, &mut outbound).await;
                }
            });
        }
    });
    addr
}

fn rt() -> Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn bench_throughput(c: &mut Criterion) {
    let runtime = rt();
    let (proxy_addr, _echo_addr) = runtime.block_on(async {
        let echo = spawn_echo().await;
        let proxy = spawn_proxy(echo).await;
        (proxy, echo)
    });

    let mut group = c.benchmark_group("data_plane.throughput");
    for &size in &[64 * 1024usize, 1024 * 1024] {
        let payload = vec![0xa5u8; size];
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_function(format!("{}KiB_echo", size / 1024), |b| {
            b.iter(|| {
                runtime.block_on(async {
                    let mut sock = TcpStream::connect(proxy_addr).await.unwrap();
                    sock.set_nodelay(true).unwrap();
                    let (mut rd, mut wr) = sock.split();
                    let payload_clone = payload.clone();
                    let writer = async {
                        wr.write_all(&payload_clone).await.unwrap();
                        wr.shutdown().await.unwrap();
                    };
                    let mut got = Vec::with_capacity(size);
                    let reader = async {
                        rd.read_to_end(&mut got).await.unwrap();
                    };
                    tokio::join!(writer, reader);
                    assert_eq!(got.len(), size);
                });
            });
        });
    }
    group.finish();
}

fn bench_added_latency(c: &mut Criterion) {
    let runtime = rt();
    let (proxy_addr, _echo_addr) = runtime.block_on(async {
        let echo = spawn_echo().await;
        let proxy = spawn_proxy(echo).await;
        (proxy, echo)
    });

    c.bench_function("data_plane.rtt_1byte_through_proxy", |b| {
        b.iter_custom(|iters| {
            runtime.block_on(async {
                let mut sock = TcpStream::connect(proxy_addr).await.unwrap();
                sock.set_nodelay(true).unwrap();
                let (mut rd, mut wr) = sock.split();
                let mut buf = [0u8; 1];
                let start = Instant::now();
                for _ in 0..iters {
                    wr.write_all(b"x").await.unwrap();
                    rd.read_exact(&mut buf).await.unwrap();
                }
                start.elapsed()
            })
        });
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(20)
        .measurement_time(Duration::from_secs(5));
    targets = bench_throughput, bench_added_latency
}
criterion_main!(benches);

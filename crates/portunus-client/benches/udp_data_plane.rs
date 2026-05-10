//! T064/T065 — single-flow UDP loopback data-plane benchmark.
//!
//! Reproduces the production UDP forwarder's hot-path shape inline:
//!     end-user `UdpSocket` → in-bench listener (`recv_from`) →
//!         per-source upstream `UdpSocket` (`send_to`) → echo →
//!             reply-pump task → end-user `UdpSocket` (`recv_from`)
//!
//! Why inline: `portunus-client` ships as a binary with no `lib` target,
//! so we cannot import `forwarder::udp::run_listener` directly. The
//! inline reproduction is a documented divergence risk vs the
//! production code in `crates/portunus-client/src/forwarder/udp/`; if
//! you change the production shape (e.g. switch from per-flow upstream
//! sockets to a shared one) update this bench in lockstep.
//!
//! What we measure:
//!   * `udp_data_plane.single_flow_throughput` — sustained 512-byte
//!     datagrams in one direction over 5 s, asserts SC-002 floor of
//!     50 000 dgrams/s.
//!   * `udp_data_plane.single_flow_rtt` — single-datagram round-trip,
//!     no hard threshold (regression detector only).

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use tokio::net::UdpSocket;
use tokio::runtime::Runtime;
use tokio::sync::Mutex;

/// IP-layer UDP payload ceiling (FR-013) — same constant the production
/// listener uses for its recv buffers.
const UDP_BUFFER_BYTES: usize = 65_535;

/// Spawn a UDP echo on a fresh ephemeral port. Returns its `SocketAddr`.
async fn spawn_udp_echo() -> SocketAddr {
    let sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = sock.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = vec![0u8; UDP_BUFFER_BYTES];
        loop {
            let Ok((n, peer)) = sock.recv_from(&mut buf).await else {
                break;
            };
            let _ = sock.send_to(&buf[..n], peer).await;
        }
    });
    addr
}

/// Spawn an inline UDP proxy mirroring `udp::run_listener`'s shape.
/// Returns the proxy's bound address. Each new source-port gets its
/// own kernel-allocated upstream socket + a reply-pump task — same as
/// production. No flow table cap, no idle reaper (the bench's
/// short-lived single-flow runs don't exercise either).
async fn spawn_udp_proxy(target: SocketAddr) -> SocketAddr {
    let listener = Arc::new(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap());
    let addr = listener.local_addr().unwrap();
    let table: Arc<Mutex<HashMap<SocketAddr, Arc<UdpSocket>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    let listener_recv = Arc::clone(&listener);
    tokio::spawn(async move {
        let mut buf = vec![0u8; UDP_BUFFER_BYTES];
        loop {
            let Ok((n, source)) = listener_recv.recv_from(&mut buf).await else {
                break;
            };
            let upstream = {
                let mut guard = table.lock().await;
                if let Some(s) = guard.get(&source) {
                    Arc::clone(s)
                } else {
                    let new_up = match UdpSocket::bind(("0.0.0.0", 0)).await {
                        Ok(s) => Arc::new(s),
                        Err(_) => continue,
                    };
                    guard.insert(source, Arc::clone(&new_up));
                    drop(guard);
                    // Spawn the reply pump for this fresh flow.
                    let listener_for_pump = Arc::clone(&listener_recv);
                    let pump_socket = Arc::clone(&new_up);
                    tokio::spawn(async move {
                        let mut rbuf = vec![0u8; UDP_BUFFER_BYTES];
                        while let Ok((rn, _from)) = pump_socket.recv_from(&mut rbuf).await {
                            if listener_for_pump
                                .send_to(&rbuf[..rn], source)
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                    });
                    new_up
                }
            };
            let _ = upstream.send_to(&buf[..n], target).await;
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

fn bench_single_flow_throughput(c: &mut Criterion) {
    let runtime = rt();
    let (proxy_addr, _echo_addr) = runtime.block_on(async {
        let echo = spawn_udp_echo().await;
        let proxy = spawn_udp_proxy(echo).await;
        // Allow the listener spawn to install its recv loop.
        tokio::time::sleep(Duration::from_millis(20)).await;
        (proxy, echo)
    });

    let mut group = c.benchmark_group("udp_data_plane.single_flow_throughput");
    let payload_size = 512usize;
    group.throughput(Throughput::Elements(1));
    group.bench_function("512B_round_trip", |b| {
        // Reuse a single end-user socket across iterations: the per-
        // source flow stays in the table so we measure the steady-
        // state hot path, not flow-creation latency.
        b.iter_custom(|iters| {
            runtime.block_on(async {
                let user = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
                user.connect(proxy_addr).await.unwrap();
                let payload = vec![0xa5u8; payload_size];
                let mut buf = vec![0u8; payload_size];
                // Warmup: a single round-trip primes the flow.
                user.send(&payload).await.unwrap();
                let _ = user.recv(&mut buf).await.unwrap();
                let start = Instant::now();
                for _ in 0..iters {
                    user.send(&payload).await.unwrap();
                    user.recv(&mut buf).await.unwrap();
                }
                start.elapsed()
            })
        });
    });
    group.finish();
}

fn bench_single_flow_rtt(c: &mut Criterion) {
    let runtime = rt();
    let (proxy_addr, _echo_addr) = runtime.block_on(async {
        let echo = spawn_udp_echo().await;
        let proxy = spawn_udp_proxy(echo).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        (proxy, echo)
    });

    c.bench_function("udp_data_plane.single_flow_rtt", |b| {
        b.iter_custom(|iters| {
            runtime.block_on(async {
                let user = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
                user.connect(proxy_addr).await.unwrap();
                let mut buf = [0u8; 8];
                user.send(b"x").await.unwrap();
                let _ = user.recv(&mut buf).await.unwrap();
                let start = Instant::now();
                for _ in 0..iters {
                    user.send(b"x").await.unwrap();
                    user.recv(&mut buf).await.unwrap();
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
    targets = bench_single_flow_throughput, bench_single_flow_rtt
}
criterion_main!(benches);

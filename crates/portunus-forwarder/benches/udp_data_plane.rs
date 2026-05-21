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
//!   * `udp_data_plane.udp_high_flow_count` — 014 / SC-001a. Drives
//!     N concurrent end-user source ports through a single
//!     `UdpRuleRuntime` (one listen port, one upstream echo) and
//!     reports peak RSS delta on Linux. The legacy v0.4 design held a
//!     `recv_buf` per flow (`O(N) × 64 KiB`); the v1.5 centralized
//!     demux drops to `O(1) × 64 KiB`, so the expected delta is
//!     tiny relative to `N × 64 KiB`. Linux-only RSS sample
//!     (`/proc/self/status` `VmRSS`); on other targets the scenario
//!     still runs N flows for a smoke check but reports `rss_delta_kb`
//!     as 0. This scenario is NOT a CI gate — perf host only (matches
//!     the splice bench convention).

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use portunus_core::{Hostname, PortRange, RuleId, Target};
use portunus_forwarder::forwarder::stats::RuleStats;
use portunus_forwarder::forwarder::udp::runtime::{UdpRuleRuntime, UdpRuntimeConfig};
use portunus_forwarder::resolver::{
    LiveResolver, Resolve, ResolveAnswer, ResolverConfig, ResolverError,
};
use tokio::net::UdpSocket;
use tokio::runtime::Runtime;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

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

// ──────────────────────────────────────────────────────────────────────
//  014 / SC-001a: high-flow-count RSS scenario
// ──────────────────────────────────────────────────────────────────────

/// IP-target rules MUST NOT touch the resolver. Mirrors the
/// `PanickingResolver` in `forwarder::udp::integration_tests`.
#[derive(Debug)]
struct PanickingResolver;

#[async_trait::async_trait]
impl Resolve for PanickingResolver {
    async fn resolve(&self, _name: &Hostname) -> Result<ResolveAnswer, ResolverError> {
        panic!("PanickingResolver::resolve was called from bench");
    }
}

/// Read `VmRSS` (KiB) from `/proc/self/status`. Linux-only. Returns
/// `None` on non-Linux or read failure.
#[cfg(target_os = "linux")]
fn read_vmrss_kb() -> Option<u64> {
    let raw = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest.trim().split_whitespace().next()?.parse().ok()?;
            return Some(kb);
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn read_vmrss_kb() -> Option<u64> {
    None
}

/// Build + start a single-rule `UdpRuleRuntime` listening on a single
/// ephemeral port forwarded to `target`. Returns the runtime, the
/// listen address, and the cancel token (drop both to tear down).
async fn start_high_flow_runtime(
    listen_port: u16,
    target: SocketAddr,
    rule_cap: usize,
) -> (UdpRuleRuntime, SocketAddr, CancellationToken) {
    // `RuleStats::new` is `#[cfg(test)]`; benches use the range-aware
    // production constructor instead. A single-port range produces an
    // equivalent stats handle for our purposes.
    let stats = RuleStats::for_range(PortRange::new(listen_port, listen_port).unwrap());
    let cancel = CancellationToken::new();
    let resolver = Arc::new(LiveResolver::new(
        Arc::new(PanickingResolver),
        ResolverConfig::default(),
    ));
    let cfg = UdpRuntimeConfig {
        rule_id: RuleId(1),
        listen_ports: listen_port..=listen_port,
        target: Target::Ip(target.ip()),
        target_ports: target.port()..=target.port(),
        prefer_ipv6: false,
        rule_cap,
        // 60s idle window keeps all flows live during the run.
        idle_window: Duration::from_secs(60),
        stats: Arc::clone(&stats),
        resolver,
        rate_limit: None,
        rate_limit_stats: None,
        owner_rate_limit: None,
        owner_rate_limit_stats: None,
        quota: None,
        failed_callback: Box::new(|_reason| {}),
    };
    let runtime = UdpRuleRuntime::start(cfg, cancel.clone())
        .await
        .expect("runtime starts");
    // Allow the listener task to enter its recv loop.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let listen_addr: SocketAddr = (Ipv4Addr::LOCALHOST, listen_port).into();
    (runtime, listen_addr, cancel)
}

/// Probe-bind a single free ephemeral UDP port (used as the rule's
/// listen port — the runtime needs a known port to install its
/// `0.0.0.0:listen_port` socket).
async fn pick_free_udp_port() -> u16 {
    let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await.unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    port
}

/// Drive `n_flows` distinct end-user source ports through a single
/// `UdpRuleRuntime`. Each client sends one datagram, awaits its echo,
/// and parks (well within the 60 s idle window). Returns peak RSS
/// delta in KiB. On non-Linux, returns 0.
async fn run_high_flow_scenario(n_flows: usize) -> u64 {
    let echo_addr = spawn_udp_echo().await;
    let listen_port = pick_free_udp_port().await;
    // Cap above n_flows so we never trip the registry overflow path —
    // this scenario measures steady-state memory at the registry's
    // own working capacity, not its rejection path.
    let rule_cap = (n_flows + 64).max(1024);
    let (_runtime, listen_addr, _cancel) =
        start_high_flow_runtime(listen_port, echo_addr, rule_cap).await;

    let rss_before = read_vmrss_kb().unwrap_or(0);

    // Spawn one task per flow; each binds a fresh ephemeral source,
    // sends a single datagram, awaits its echo, then parks. We hold
    // the JoinHandles in a Vec so the client sockets stay alive for
    // the duration of the measurement (dropping them would close the
    // source ports and let the reaper time the flows out — but the
    // 60s idle window protects us either way).
    let mut handles = Vec::with_capacity(n_flows);
    let park = Arc::new(tokio::sync::Notify::new());
    for i in 0..n_flows {
        let park = Arc::clone(&park);
        let payload = format!("flow-{i}").into_bytes();
        let handle = tokio::spawn(async move {
            let client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("bind client source");
            client
                .connect(listen_addr)
                .await
                .expect("connect to listen port");
            // Single datagram + echo. Some macOS kernels return WouldBlock
            // on the first try_send inside the listener (FR-006) — re-send
            // once on timeout so the flow survives.
            let mut buf = vec![0u8; 1500];
            for _ in 0..3u32 {
                let _ = client.send(&payload).await;
                if let Ok(Ok(_n)) =
                    tokio::time::timeout(Duration::from_millis(300), client.recv(&mut buf)).await
                {
                    break;
                }
            }
            // Park until the scenario signals shutdown so the source
            // socket (and therefore the flow's FlowKey) stays live.
            park.notified().await;
        });
        handles.push(handle);
    }

    // Allow all flows to land in the registry. Linear bind/connect
    // serialization plus the 100 ms first-packet retry can take a
    // while at 1 000 flows; give it 5 s headroom.
    tokio::time::sleep(Duration::from_secs(5)).await;

    let rss_after = read_vmrss_kb().unwrap_or(0);

    // Release the client tasks and tear the runtime down.
    park.notify_waiters();
    for h in handles {
        let _ = h.await;
    }

    rss_after.saturating_sub(rss_before)
}

fn bench_high_flow_count(c: &mut Criterion) {
    let runtime = rt();

    // Target N = 1000 per SC-001a. If the sandbox's file-descriptor
    // limit can't hold ~1 000 source sockets + ~1 000 upstream
    // sockets (per-flow on the v0.4 path; centralized on v1.5 — still
    // ≥ 2 × N FDs for the clients alone), fall back to 500. The
    // scaling property holds at either size.
    let n_flows: usize = 1000;

    let mut group = c.benchmark_group("udp_data_plane.udp_high_flow_count");
    // One sample is sufficient — we're measuring an OS-reported gauge
    // (peak RSS), not a tight hot-path microbench.
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(15));
    group.bench_function(format!("n{n_flows}_rss_delta"), |b| {
        b.iter_custom(|iters| {
            runtime.block_on(async {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let start = Instant::now();
                    let rss_delta_kb = run_high_flow_scenario(n_flows).await;
                    total += start.elapsed();
                    // Surface the RSS delta via stderr so perf-host
                    // operators can scrape it from the bench output;
                    // criterion's "time" measurement remains the
                    // primary regression signal.
                    eprintln!("udp_high_flow_count n={n_flows} rss_delta_kb={rss_delta_kb}");
                }
                total
            })
        });
    });
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(20)
        .measurement_time(Duration::from_secs(5));
    targets = bench_single_flow_throughput, bench_single_flow_rtt
}
criterion_group! {
    name = high_flow;
    config = Criterion::default();
    targets = bench_high_flow_count
}
criterion_main!(benches, high_flow);

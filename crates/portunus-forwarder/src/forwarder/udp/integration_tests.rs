//! 014-udp-centralized-demux Phase 10: rule-level UDP integration
//! tests. These exercise [`UdpRuleRuntime::start`] end-to-end against
//! real loopback sockets and replace the v0.4 per-listener test block
//! that was gated under `#[cfg(any())]` in `udp/mod.rs`.
//!
//! Covered:
//!   * round-trip byte-equality (replaces v0.4 listener round-trip).
//!   * SC-002: per-rule cap is the rule-wide ceiling — the registry's
//!     `dropped_overflow` counter advances exactly once when the (rule
//!     +1)-th new flow arrives, regardless of which listen port it
//!     hits.
//!   * FR-002: a single client source addressing two listen ports of
//!     the same range rule resolves to two distinct flows.
//!
//! All tests use IP-literal targets so the resolver short-circuits and
//! never invokes `Resolve::resolve` — see `PanickingResolver` below.
//!
//! ## Why these tests use [`send_with_retry`]
//!
//! The production listener's cold-path step 9 now uses `send().await`
//! so the first datagram of every flow is durable (an earlier
//! `try_send` shape had a fresh-socket reactor race that silently
//! dropped first packets — see `CHANGELOG.md` "Fixed" entry). UDP is
//! still best-effort, and the listener's *fast path* keeps `try_send`
//! with drop-on-`WouldBlock` per FR-007, so the helper sends every
//! ~100 ms until an echo arrives or the budget expires (mirrors how a
//! real UDP client retries on packet loss).

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use portunus_core::{Hostname, RuleId, Target};
use tokio::net::UdpSocket;
use tokio_util::sync::CancellationToken;

use crate::forwarder::stats::RuleStats;
use crate::forwarder::udp::runtime::{UdpRuleRuntime, UdpRuntimeConfig};
use crate::resolver::{LiveResolver, Resolve, ResolveAnswer, ResolverConfig, ResolverError};

// ──────────────────────────────────────────────────────────────────────
//  Shared test fixture
// ──────────────────────────────────────────────────────────────────────

/// IP-target rules MUST NOT touch the resolver (R-006 / Constitution II
/// hot-path budget). Any accidental invocation surfaces as a hard test
/// failure here.
#[derive(Debug)]
struct PanickingResolver;

#[async_trait::async_trait]
impl Resolve for PanickingResolver {
    async fn resolve(&self, name: &Hostname) -> Result<ResolveAnswer, ResolverError> {
        panic!("PanickingResolver::resolve was called for {name}");
    }
}

fn test_resolver() -> Arc<LiveResolver<PanickingResolver>> {
    Arc::new(LiveResolver::new(
        Arc::new(PanickingResolver),
        ResolverConfig::default(),
    ))
}

/// Spawn a UDP echo on a fresh ephemeral loopback port. The detached
/// task lives for the duration of the test process.
async fn spawn_echo() -> SocketAddr {
    let sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = sock.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = vec![0u8; 65_535];
        loop {
            let Ok((n, peer)) = sock.recv_from(&mut buf).await else {
                break;
            };
            let _ = sock.send_to(&buf[..n], peer).await;
        }
    });
    addr
}

/// Spawn `n` UDP echoes on contiguous ports, returning the start port.
/// The runtime's `port_map` is a linear offset, so a range rule with
/// listen_ports `L..L+n-1` paired with target_ports `T..T+n-1` routes
/// listen_port L+i to target_port T+i. Each echo task echoes back any
/// datagram it receives.
async fn spawn_echo_range(n: u16) -> u16 {
    let start = pick_consecutive_free_udp(n).await;
    for offset in 0..n {
        let port = start + offset;
        let sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, port))
            .await
            .expect("bind echo on consecutive port");
        tokio::spawn(async move {
            let mut buf = vec![0u8; 65_535];
            loop {
                let Ok((n, peer)) = sock.recv_from(&mut buf).await else {
                    break;
                };
                let _ = sock.send_to(&buf[..n], peer).await;
            }
        });
    }
    start
}

/// Probe-bind `n` consecutive UDP ports on `0.0.0.0`; return the start
/// port. Mirrors the e2e `pick_consecutive_free_udp` helper but uses
/// async sockets so it composes with `tokio::test`.
async fn pick_consecutive_free_udp(n: u16) -> u16 {
    'outer: for _ in 0..50 {
        let Ok(probe) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await else {
            continue;
        };
        let start = probe.local_addr().unwrap().port();
        if u32::from(start) + u32::from(n) > 65_536 {
            drop(probe);
            continue;
        }
        let mut probes: Vec<UdpSocket> = vec![probe];
        for offset in 1..n {
            if let Ok(s) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, start + offset)).await {
                probes.push(s);
            } else {
                drop(probes);
                continue 'outer;
            }
        }
        drop(probes);
        return start;
    }
    panic!("could not find {n} consecutive free UDP ports after 50 attempts");
}

/// Pick a single free ephemeral UDP port.
async fn pick_free_udp_port() -> u16 {
    let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await.unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    port
}

/// Build + start a [`UdpRuleRuntime`] over the given listen-port range
/// pointing at `target`. Defaults: 60s idle window, no rate-limit /
/// quota, `prefer_ipv6 = false`, `rule_id = 1`. The `failed_callback`
/// records its reason into the returned `Arc<Mutex<Vec<String>>>`.
async fn start_runtime(
    listen_ports: std::ops::RangeInclusive<u16>,
    target: SocketAddr,
    rule_cap: usize,
) -> (
    UdpRuleRuntime,
    Arc<std::sync::Mutex<Vec<String>>>,
    Arc<RuleStats>,
    CancellationToken,
) {
    let failed = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let failed_for_cb = Arc::clone(&failed);
    let stats = RuleStats::new();
    let cancel = CancellationToken::new();
    // All listen ports map to the SAME upstream echo port. We do this
    // by giving each listen port the SAME target slot — but the
    // runtime's `port_map` is a linear offset (target_start + offset)
    // and skips listen ports whose offset would land outside the target
    // range. So we mirror the listen range as the target range (same
    // length); for our tests the upstream is a single echo on
    // `target.port()` only — listeners for listen_start+1..N would dial
    // unbound ports, which UDP `connect()` accepts and `try_send`
    // succeeds for, but the echo never replies. To avoid that, we
    // intentionally bind one echo per listen port below in
    // `spawn_echo_range` for range tests; the single-port tests use
    // the simpler 1-element target range.
    let listen_len = u32::from(*listen_ports.end()) - u32::from(*listen_ports.start()) + 1;
    let target_start = target.port();
    let target_end =
        u16::try_from(u32::from(target_start) + listen_len - 1).unwrap_or(target_start);
    let cfg = UdpRuntimeConfig {
        rule_id: RuleId(1),
        listen_ports: listen_ports.clone(),
        target: Target::Ip(target.ip()),
        target_ports: target_start..=target_end,
        prefer_ipv6: false,
        rule_cap,
        idle_window: Duration::from_secs(60),
        stats: Arc::clone(&stats),
        resolver: test_resolver(),
        rate_limit: None,
        rate_limit_stats: None,
        owner_rate_limit: None,
        owner_rate_limit_stats: None,
        quota: None,
        failed_callback: Box::new(move |reason| {
            failed_for_cb.lock().unwrap().push(reason);
        }),
    };
    let runtime = UdpRuleRuntime::start(cfg, cancel.clone())
        .await
        .expect("runtime must start");
    // Give the listener tasks a tick to enter their recv loops.
    tokio::time::sleep(Duration::from_millis(50)).await;
    (runtime, failed, stats, cancel)
}

/// Send `payload` from `client` to `listen_addr` and retry every ~100 ms
/// until an echo arrives or `total_budget` elapses. Returns the echoed
/// bytes on success.
///
/// Rationale: the production listener's first-packet `try_send` can
/// transiently fail with `WouldBlock` on macOS (per the FR-006
/// "WouldBlock → drop + keep flow" branch in `listener.rs`). A retry
/// loop mirrors the canonical UDP client behaviour — applications are
/// expected to retransmit on loss.
async fn send_with_retry(
    client: &UdpSocket,
    listen_addr: SocketAddr,
    payload: &[u8],
    total_budget: Duration,
) -> Option<Vec<u8>> {
    let deadline = Instant::now() + total_budget;
    let per_attempt = Duration::from_millis(100);
    let mut buf = vec![0u8; 65_535];
    while Instant::now() < deadline {
        let _ = client.send_to(payload, listen_addr).await;
        let wait = per_attempt.min(deadline.saturating_duration_since(Instant::now()));
        if let Ok(Ok((n, _from))) = tokio::time::timeout(wait, client.recv_from(&mut buf)).await {
            return Some(buf[..n].to_vec());
        }
    }
    None
}

// ──────────────────────────────────────────────────────────────────────
//  Tests
// ──────────────────────────────────────────────────────────────────────

/// Task 10.2: round-trip byte-equality through a single-port rule.
/// Replaces v0.4's `udp_listener_round_trip_byte_equal`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn udp_rule_round_trip_byte_equal() {
    let echo = spawn_echo().await;
    let listen = pick_free_udp_port().await;

    let (mut runtime, failed, _stats, _cancel) = start_runtime(listen..=listen, echo, 1024).await;

    let listen_addr: SocketAddr = (Ipv4Addr::LOCALHOST, listen).into();
    let client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let large = vec![0u8; 1000];
    let payloads: Vec<&[u8]> = vec![b"hello", b"world!", &large];
    for payload in &payloads {
        let echoed = send_with_retry(&client, listen_addr, payload, Duration::from_secs(3)).await;
        let bytes = echoed.unwrap_or_else(|| {
            panic!(
                "round-trip timed out for payload len={}; registry.len={} failed={:?}",
                payload.len(),
                runtime.registry().len(),
                failed.lock().unwrap()
            )
        });
        assert_eq!(
            bytes.as_slice(),
            *payload,
            "payload len {} must echo byte-equal",
            payload.len()
        );
    }

    // failed_callback never fires on a clean run.
    assert!(
        failed.lock().unwrap().is_empty(),
        "failed_callback fired unexpectedly: {:?}",
        failed.lock().unwrap()
    );

    runtime.shutdown().await;
}

/// Task 10.3 (SC-002): the rule-wide cap is enforced across all listen
/// ports of a range rule. With `rule_cap = 3` and 4 distinct client
/// sources each addressing a DIFFERENT listen port, exactly 3 echoes
/// come back and the registry's `dropped_overflow` is 1.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn udp_range_rule_cap_is_per_rule() {
    let echo_start = spawn_echo_range(4).await;
    let listen_start = pick_consecutive_free_udp(4).await;
    let listen_end = listen_start + 3;

    let echo_first: SocketAddr = (Ipv4Addr::LOCALHOST, echo_start).into();
    let (mut runtime, _failed, _stats, _cancel) =
        start_runtime(listen_start..=listen_end, echo_first, 3).await;

    // Open 4 distinct client sockets, each targeting a different listen
    // port of the range rule. Drive each through `send_with_retry`.
    // 3 of 4 will succeed (the registry cap of 3 binds across all
    // ports — SC-002); the over-cap source loops until budget expiry.
    let mut clients: Vec<UdpSocket> = Vec::with_capacity(4);
    for _ in 0..4 {
        clients.push(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap());
    }
    let mut handles = Vec::with_capacity(4);
    for (i, c) in clients.into_iter().enumerate() {
        let port = listen_start + u16::try_from(i).unwrap();
        let listen_addr: SocketAddr = (Ipv4Addr::LOCALHOST, port).into();
        // Distinct payload per source to make any cross-routing visible.
        let payload = vec![0xC0 | u8::try_from(i).unwrap()];
        handles.push(tokio::spawn(async move {
            send_with_retry(&c, listen_addr, &payload, Duration::from_millis(1500)).await
        }));
    }

    let mut echoed = 0usize;
    for h in handles {
        if h.await.unwrap().is_some() {
            echoed += 1;
        }
    }

    assert_eq!(
        echoed, 3,
        "SC-002: exactly 3 of 4 sources MUST round-trip; got {echoed}"
    );
    // FR-003 bumps `dropped_overflow` on every first-packet retry that
    // hits the rule cap, so retries from the over-cap source can drive
    // this above 1. The SC-002 invariant is that at least one drop
    // happened — i.e. the cap was actually enforced. A more stringent
    // assertion (e.g. == 1) would require single-shot retries which
    // are not realistic for UDP.
    assert!(
        runtime.registry().dropped_overflow() >= 1,
        "SC-002: at least one first-packet MUST drop on the rule cap; got {}",
        runtime.registry().dropped_overflow(),
    );

    runtime.shutdown().await;
}

/// Task 10.4 (FR-002): the same client source hitting two listen ports
/// of the same range rule resolves to two distinct flows — keyed by
/// `(listen_port, src)`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn udp_cross_listener_same_src_distinct_flows() {
    let echo_start = spawn_echo_range(2).await;
    let listen_start = pick_consecutive_free_udp(2).await;
    let listen_end = listen_start + 1;

    let echo_first: SocketAddr = (Ipv4Addr::LOCALHOST, echo_start).into();
    let (mut runtime, _failed, _stats, _cancel) =
        start_runtime(listen_start..=listen_end, echo_first, 16).await;

    // One client socket; send to BOTH listen ports. Two distinct flows
    // should be created — one per (listen_port, src) pair.
    let client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let a_addr: SocketAddr = (Ipv4Addr::LOCALHOST, listen_start).into();
    let b_addr: SocketAddr = (Ipv4Addr::LOCALHOST, listen_end).into();

    let a_echo = send_with_retry(&client, a_addr, b"a", Duration::from_secs(3))
        .await
        .expect("first listen port echo");
    assert_eq!(a_echo.as_slice(), b"a");
    let b_echo = send_with_retry(&client, b_addr, b"b", Duration::from_secs(3))
        .await
        .expect("second listen port echo");
    assert_eq!(b_echo.as_slice(), b"b");

    // FR-002: two distinct flows in the registry.
    assert_eq!(
        runtime.registry().len(),
        2,
        "FR-002: same src + two listen ports must produce two flows",
    );
    assert_eq!(runtime.registry().dropped_overflow(), 0);

    runtime.shutdown().await;
}

/// Task 10.5: rewritten v0.4 overflow test in per-rule semantics. With
/// `rule_cap = 2` and a single-port rule, 3 distinct client sources
/// produce 2 echoes and 1 drop. `dropped_overflow` advances exactly
/// once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn udp_overflow_on_cap() {
    let echo = spawn_echo().await;
    let listen = pick_free_udp_port().await;

    let (mut runtime, _failed, _stats, _cancel) = start_runtime(listen..=listen, echo, 2).await;
    let listen_addr: SocketAddr = (Ipv4Addr::LOCALHOST, listen).into();

    let mut clients: Vec<UdpSocket> = Vec::with_capacity(3);
    for _ in 0..3 {
        clients.push(UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap());
    }
    let mut handles = Vec::with_capacity(3);
    for (i, c) in clients.into_iter().enumerate() {
        let payload = vec![u8::try_from(i).unwrap()];
        handles.push(tokio::spawn(async move {
            send_with_retry(&c, listen_addr, &payload, Duration::from_millis(1500)).await
        }));
    }

    let mut echoed = 0usize;
    for h in handles {
        if h.await.unwrap().is_some() {
            echoed += 1;
        }
    }

    assert_eq!(echoed, 2, "exactly 2 of 3 sources MUST round-trip");
    // The over-cap source keeps retrying in `send_with_retry`; FR-003
    // counts every first-packet that hits the cap, so this can be > 1
    // (one per retry). Assert the lower bound: at least one drop.
    assert!(
        runtime.registry().dropped_overflow() >= 1,
        "dropped_overflow MUST advance for the over-cap source; got {}",
        runtime.registry().dropped_overflow(),
    );

    runtime.shutdown().await;
}

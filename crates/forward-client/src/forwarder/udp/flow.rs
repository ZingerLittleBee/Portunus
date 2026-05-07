//! Per-flow UDP state.
//!
//! Spec: 004-udp-forward, `data-model.md` § UdpFlow. One `UdpFlow`
//! exists per `(rule, source_addr)` pair while the source is active
//! within the configured idle window. The flow owns:
//!   * a kernel-allocated upstream `UdpSocket` (one per flow — provides
//!     NAT-style return-path isolation; the kernel's source-port
//!     selection guarantees per-source isolation cheaper than tracking
//!     it ourselves);
//!   * the ordered list of upstream `SocketAddr` candidates (one entry
//!     for IP-target rules in US1; the full multi-A list for DNS-target
//!     rules in US2);
//!   * a `last_seen` timestamp the per-rule reaper consults (idle
//!     eviction lands in US4 — for US1 the reaper is dormant);
//!   * a `CancellationToken` the reply-pump task watches so the listener
//!     can fire-and-forget the per-flow cleanup.
//!
//! Hot path: the flow is `Arc`-wrapped so the listener (recv side) and
//! the reply-pump (send side) can hold cheap clones without coordinating
//! through a mutex. The only mutex on the struct is around `last_seen`
//! — touched once per datagram in either direction; idle eviction reads
//! it under the same lock during the reaper sweep.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use forward_core::{RuleId, Target};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use crate::forwarder::stats::RuleStats;
use crate::resolver::{ConnectError, LiveResolver, Resolve, ResolverError};

#[derive(Debug)]
pub struct UdpFlow {
    /// Source `(addr, port)` of the end-user. Used as the flow-table
    /// key by the listener.
    pub source_addr: SocketAddr,

    /// Kernel-allocated upstream socket bound to `0.0.0.0:0`. The
    /// reply pump receives on this socket; the listener `send_to`s
    /// upstream through it. Wrapping in `Arc` lets the reply-pump task
    /// hold a clone without coordinating with the recv-side cleanup.
    pub upstream_socket: Arc<UdpSocket>,

    /// Ordered candidate upstream addresses. `current_addr_idx` indexes
    /// into this. For IP-target rules in US1 this is a 1-element vec;
    /// for DNS-target rules in US2 it carries the full multi-A list so
    /// `send_to` errors can fall back to the next address (FR-006).
    pub upstream_addrs: Vec<SocketAddr>,

    /// Index into `upstream_addrs` of the address currently being used.
    /// `AtomicUsize` so the listener can advance it on send errors
    /// without holding a lock. Always < `upstream_addrs.len()` (we
    /// guard at construction).
    pub current_addr_idx: AtomicUsize,

    /// Last time a datagram flowed in either direction. The idle
    /// reaper (US4) checks this against the configured `idle_window`.
    /// `Mutex` because both the listener and the reply-pump bump it,
    /// and `Instant` is `!Copy` for `AtomicCell` reuse.
    pub last_seen: Mutex<Instant>,

    /// Per-flow cumulative byte counters for diagnostic logs. The
    /// rule-level aggregates live on `RuleStats` and update on every
    /// datagram; these are populated only when a flow closes (US1
    /// `rule.udp_flow_closed` log event would consume them — deferred
    /// to US4 polish).
    pub bytes_in: AtomicU64,
    pub bytes_out: AtomicU64,
    pub datagrams_in: AtomicU64,
    pub datagrams_out: AtomicU64,

    /// Fired by the listener (or the reaper in US4) to tear down the
    /// reply-pump task associated with this flow. The reply-pump
    /// awaits `cancel.cancelled()` in its select arm.
    pub cancel: CancellationToken,
}

impl UdpFlow {
    /// Construct an Arc<UdpFlow> with the given upstream candidates.
    /// Panics if `upstream_addrs` is empty (caller is expected to bump
    /// `dns_failures` and drop the datagram instead of building an
    /// empty flow).
    #[must_use]
    pub fn new(
        source_addr: SocketAddr,
        upstream_socket: Arc<UdpSocket>,
        upstream_addrs: Vec<SocketAddr>,
    ) -> Arc<Self> {
        assert!(
            !upstream_addrs.is_empty(),
            "UdpFlow must be constructed with at least one upstream address"
        );
        Arc::new(Self {
            source_addr,
            upstream_socket,
            upstream_addrs,
            current_addr_idx: AtomicUsize::new(0),
            last_seen: Mutex::new(Instant::now()),
            bytes_in: AtomicU64::new(0),
            bytes_out: AtomicU64::new(0),
            datagrams_in: AtomicU64::new(0),
            datagrams_out: AtomicU64::new(0),
            cancel: CancellationToken::new(),
        })
    }

    /// Currently-active upstream address. Reads `current_addr_idx`
    /// once and looks up the slot — safe to call from either the
    /// recv or send side without holding a lock.
    #[must_use]
    pub fn current_upstream(&self) -> SocketAddr {
        let idx = self.current_addr_idx.load(Ordering::Relaxed);
        // Bounded at construction; the only mutator is `advance_upstream`
        // which clamps to `upstream_addrs.len() - 1`.
        self.upstream_addrs[idx.min(self.upstream_addrs.len() - 1)]
    }

    /// Advance `current_addr_idx` to the next candidate. Returns the
    /// new active address, or `None` when the list is exhausted (US2
    /// callers map this to `dns_failures` + datagram drop).
    pub fn advance_upstream(&self) -> Option<SocketAddr> {
        let next = self.current_addr_idx.fetch_add(1, Ordering::Relaxed) + 1;
        if next >= self.upstream_addrs.len() {
            // Restore the index so a subsequent `current_upstream()`
            // doesn't panic — though by contract callers should have
            // dropped the flow by now.
            self.current_addr_idx
                .store(self.upstream_addrs.len() - 1, Ordering::Relaxed);
            None
        } else {
            Some(self.upstream_addrs[next])
        }
    }

    /// Record that a datagram of `n` bytes flowed inbound (end-user →
    /// upstream). Updates the per-flow counters and bumps `last_seen`.
    pub async fn bump_inbound(&self, n: u64) {
        self.bytes_in.fetch_add(n, Ordering::Relaxed);
        self.datagrams_in.fetch_add(1, Ordering::Relaxed);
        *self.last_seen.lock().await = Instant::now();
    }

    /// Record that a datagram of `n` bytes flowed outbound (upstream →
    /// end-user). Updates the per-flow counters and bumps `last_seen`.
    pub async fn bump_outbound(&self, n: u64) {
        self.bytes_out.fetch_add(n, Ordering::Relaxed);
        self.datagrams_out.fetch_add(1, Ordering::Relaxed);
        *self.last_seen.lock().await = Instant::now();
    }

    /// Synchronous read of the last activity timestamp — held under
    /// the mutex briefly. The reaper (US4) calls this during its sweep.
    pub async fn last_seen_at(&self) -> Instant {
        *self.last_seen.lock().await
    }
}

/// 004-udp-forward T044: resolver-aware flow constructor.
///
/// Resolves `target` through the shared `LiveResolver` (cache, single-
/// flight, family ordering all carry over from the TCP path), binds an
/// upstream socket, and returns a fresh `Arc<UdpFlow>` ready for the
/// caller to insert into the per-rule flow table.
///
/// On `ConnectError::Resolution`, bumps the per-rule
/// `RuleStats.dns_failures` counter and returns the error WITHOUT
/// allocating a flow slot (an unresolvable target shouldn't reserve a
/// slot; FR-008 + the cardinality budget rationale in
/// `data-model.md` § UdpFlowTable).
///
/// `Target::Ip` short-circuits to a single `SocketAddr` without
/// touching the resolver (R-006 / Constitution II).
#[allow(dead_code)]
pub async fn build_flow_dns<R: Resolve>(
    resolver: &LiveResolver<R>,
    rule_id: RuleId,
    target: &Target,
    port: u16,
    prefer_ipv6: bool,
    source_addr: SocketAddr,
    stats: &RuleStats,
) -> Result<Arc<UdpFlow>, ConnectError> {
    let (addrs, _source) = match resolver
        .resolve_target(rule_id, target, port, prefer_ipv6)
        .await
    {
        Ok(v) => v,
        Err(e) => {
            if matches!(&e, ConnectError::Resolution(_)) {
                stats.inc_dns_failure();
            }
            return Err(e);
        }
    };

    if addrs.is_empty() {
        stats.inc_dns_failure();
        return Err(ConnectError::Resolution(ResolverError::EmptyAnswer));
    }

    let upstream_socket = match UdpSocket::bind(("0.0.0.0", 0)).await {
        Ok(s) => Arc::new(s),
        Err(e) => return Err(ConnectError::Dial(e)),
    };

    Ok(UdpFlow::new(source_addr, upstream_socket, addrs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    async fn make_socket() -> Arc<UdpSocket> {
        Arc::new(
            UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("bind upstream"),
        )
    }

    #[tokio::test]
    async fn current_upstream_returns_head_initially() {
        let sock = make_socket().await;
        let flow = UdpFlow::new(
            "127.0.0.1:50000".parse().unwrap(),
            sock,
            vec![
                "127.0.0.1:9999".parse().unwrap(),
                "127.0.0.1:9998".parse().unwrap(),
            ],
        );
        assert_eq!(
            flow.current_upstream(),
            "127.0.0.1:9999".parse::<SocketAddr>().unwrap()
        );
    }

    #[tokio::test]
    async fn advance_upstream_walks_then_returns_none() {
        let sock = make_socket().await;
        let flow = UdpFlow::new(
            "127.0.0.1:50000".parse().unwrap(),
            sock,
            vec![
                "127.0.0.1:9999".parse().unwrap(),
                "127.0.0.1:9998".parse().unwrap(),
            ],
        );
        assert_eq!(
            flow.advance_upstream(),
            Some("127.0.0.1:9998".parse().unwrap())
        );
        assert_eq!(flow.advance_upstream(), None);
        // After exhaustion, `current_upstream` MUST still be safe.
        assert_eq!(
            flow.current_upstream(),
            "127.0.0.1:9998".parse::<SocketAddr>().unwrap()
        );
    }

    // ---- T041 / T042 (US2): build_flow_dns wraps resolver + bind ----

    use crate::resolver::ResolverConfig;
    use crate::resolver::test_support::MockResolver;
    use forward_core::Hostname;
    use std::net::IpAddr;
    use std::time::Duration as StdDuration;

    /// T041: a successful resolution produces a flow with the resolved
    /// candidates in the order the resolver returned them. The first
    /// `current_upstream()` matches the head address and a `send_to` to
    /// it succeeds against an echo bound on that port.
    #[tokio::test]
    async fn build_flow_dns_resolves_and_binds_for_single_a() {
        let echo = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let echo_addr = echo.local_addr().unwrap();

        let resolver = LiveResolver::new(
            Arc::new(MockResolver::ok(
                vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
                StdDuration::from_secs(60),
            )),
            ResolverConfig::default(),
        );
        let stats = RuleStats::new();
        let target = Target::Dns(Hostname::new("test.example").unwrap());
        let source: SocketAddr = "127.0.0.1:50100".parse().unwrap();

        let flow = build_flow_dns(
            &resolver,
            RuleId(1),
            &target,
            echo_addr.port(),
            false,
            source,
            &stats,
        )
        .await
        .expect("resolve must succeed");

        assert_eq!(flow.upstream_addrs.len(), 1);
        assert_eq!(
            flow.current_upstream(),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), echo_addr.port())
        );
        // Ensure the bound socket actually reaches the echo target.
        flow.upstream_socket
            .send_to(b"ping", flow.current_upstream())
            .await
            .expect("send_to bound socket");
        // dns_failures stays at 0 on a successful resolve.
        assert_eq!(stats.snapshot_dns_failures(), 0);
    }

    /// T041: multi-A resolve produces a flow whose `upstream_addrs`
    /// preserves resolver order. `advance_upstream()` walks past a
    /// dead first entry to the live second one — emulating the
    /// "first send_to fails, second succeeds" path.
    #[tokio::test]
    async fn build_flow_dns_preserves_multi_a_order_and_falls_back() {
        // Bind a real echo as the LIVE address.
        let echo = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let echo_addr = echo.local_addr().unwrap();
        // Spawn an echo loop so the live address actually responds.
        tokio::spawn(async move {
            let mut buf = [0u8; 1024];
            loop {
                let Ok((n, peer)) = echo.recv_from(&mut buf).await else {
                    break;
                };
                let _ = echo.send_to(&buf[..n], peer).await;
            }
        });

        // Resolver returns [LOCALHOST, LOCALHOST]. We can't easily
        // craft an "unreachable" loopback IP that's portable, so we
        // verify the multi-A *list* is preserved + advance_upstream
        // walks it. (The mod.rs send-fallback test covers the actual
        // EHOSTUNREACH retry against `127.255.255.254` on macOS.)
        let resolver = LiveResolver::new(
            Arc::new(MockResolver::ok(
                vec![
                    IpAddr::V4(Ipv4Addr::LOCALHOST),
                    IpAddr::V4(Ipv4Addr::LOCALHOST),
                ],
                StdDuration::from_secs(60),
            )),
            ResolverConfig::default(),
        );
        let stats = RuleStats::new();
        let target = Target::Dns(Hostname::new("multi.example").unwrap());
        let source: SocketAddr = "127.0.0.1:50101".parse().unwrap();

        let flow = build_flow_dns(
            &resolver,
            RuleId(2),
            &target,
            echo_addr.port(),
            false,
            source,
            &stats,
        )
        .await
        .expect("multi-A resolve must succeed");

        assert_eq!(flow.upstream_addrs.len(), 2, "multi-A list preserved");
        let head = flow.current_upstream();
        assert!(flow.advance_upstream().is_some(), "fallback to second");
        assert_ne!(
            flow.current_addr_idx.load(Ordering::Relaxed),
            0,
            "advance_upstream must move the cursor"
        );
        // The second slot is also LOCALHOST:echo_port — same address
        // by construction, but the cursor advanced.
        assert_eq!(flow.current_upstream(), head);
        // No further candidates after walking the second.
        assert!(flow.advance_upstream().is_none());
    }

    /// T042: a resolver error (NXDOMAIN-class) bumps `dns_failures`
    /// exactly once, returns `ConnectError::Resolution`, and does NOT
    /// allocate an upstream socket / flow.
    #[tokio::test]
    async fn build_flow_dns_bumps_dns_failures_on_resolver_error() {
        let resolver = LiveResolver::new(
            Arc::new(MockResolver::always_fail(ResolverError::Lookup(
                "nope".into(),
            ))),
            ResolverConfig::default(),
        );
        let stats = RuleStats::new();
        let target = Target::Dns(Hostname::new("bad.example").unwrap());
        let source: SocketAddr = "127.0.0.1:50102".parse().unwrap();

        let err = build_flow_dns(&resolver, RuleId(3), &target, 9999, false, source, &stats)
            .await
            .expect_err("resolver failure must surface");
        assert!(
            matches!(err, ConnectError::Resolution(_)),
            "expected ConnectError::Resolution, got {err:?}",
        );
        assert_eq!(
            stats.snapshot_dns_failures(),
            1,
            "exactly one dns_failures bump per failed resolve",
        );
    }

    #[tokio::test]
    async fn bump_inbound_updates_counters_and_last_seen() {
        let sock = make_socket().await;
        let flow = UdpFlow::new(
            "127.0.0.1:50000".parse().unwrap(),
            sock,
            vec!["127.0.0.1:9999".parse().unwrap()],
        );
        let before = flow.last_seen_at().await;
        // Crank time forward enough that monotonic Instant detects motion
        // even on coarse-resolution clocks.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        flow.bump_inbound(128).await;
        assert_eq!(flow.bytes_in.load(Ordering::Relaxed), 128);
        assert_eq!(flow.datagrams_in.load(Ordering::Relaxed), 1);
        assert!(flow.last_seen_at().await > before);
    }
}

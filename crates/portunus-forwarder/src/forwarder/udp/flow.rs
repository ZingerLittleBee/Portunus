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
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use portunus_core::{RuleId, Target};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Process-wide baseline used to encode `Instant` values as `u64`
/// nanoseconds inside an atomic. Set lazily on first use; identity does
/// not matter because callers always go through `now_nanos` /
/// `instant_from_nanos` which both reference the same baseline.
fn baseline() -> Instant {
    static BASELINE: OnceLock<Instant> = OnceLock::new();
    *BASELINE.get_or_init(Instant::now)
}

/// Current monotonic time encoded as nanoseconds since `baseline()`.
/// Saturates at `u64::MAX` (~584 years) to keep the encoding total.
fn now_nanos() -> u64 {
    let raw = Instant::now().duration_since(baseline()).as_nanos();
    u64::try_from(raw).unwrap_or(u64::MAX)
}

/// Decode a stored nanosecond value back to `Instant`.
fn instant_from_nanos(nanos: u64) -> Instant {
    baseline() + Duration::from_nanos(nanos)
}

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

    /// Last time a datagram flowed in either direction, encoded as
    /// nanoseconds since the process-wide `baseline()` so reads and
    /// writes are wait-free atomic operations (no `tokio::sync::Mutex`,
    /// no scheduler dispatch). The idle reaper (US4) compares this
    /// against `idle_window` on every sweep tick.
    pub last_seen_nanos: AtomicU64,

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

    /// 007-multi-target-failover (T024/T034): index of the target this
    /// flow stuck to on its first packet (FR-012). `None` for legacy
    /// single-target rules so `bump_*` skips per-target crediting and
    /// the byte-identical v0.6.0 hot path is preserved (Constitution
    /// Principle II). `Some(idx)` for multi-target rules; the flow
    /// holds a clone of the per-rule `health_states` slice so the
    /// reply-pump can credit per-target bytes without re-plumbing
    /// through the listener task.
    pub target_idx: Option<u32>,
    pub health_states: Option<Arc<Vec<Mutex<crate::forwarder::failover::HealthState>>>>,

    /// 013-traffic-quotas E4: per-(user, client) byte budget handle.
    /// `None` for legacy unmetered rules — `quota_allows()` short-
    /// circuits without an atomic load on those paths so the v0.6
    /// hot path stays byte-identical. `Some` flows consume `n` bytes
    /// after every successful `send_to` and short-circuit further
    /// datagrams on the exhausted side.
    pub quota: Option<Arc<crate::forwarder::quota::QuotaHandle>>,

    /// 014-udp-centralized-demux: v0.11 active-connection guards
    /// (owner + rule scope). Held for the flow's lifetime so the
    /// rate limiter's `concurrent_connections` cap correctly
    /// reflects live UDP flows. When the registry releases the
    /// `Arc<UdpFlow>` (idle eviction / explicit remove), this Vec
    /// drops and `ActiveGuard::Drop` decrements
    /// `active_connections`. The Mutex is touched once at flow
    /// construction (via `attach_admit_guards`) — admission lookups
    /// never read it on the hot path. `Vec` rather than two
    /// `Option`s so the helper can return 0, 1, or 2 guards without
    /// shape-matching on which layer was capped.
    pub admit_guards: Mutex<Vec<crate::forwarder::rate_limit::scope::ActiveGuard>>,
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
            last_seen_nanos: AtomicU64::new(now_nanos()),
            bytes_in: AtomicU64::new(0),
            bytes_out: AtomicU64::new(0),
            datagrams_in: AtomicU64::new(0),
            datagrams_out: AtomicU64::new(0),
            cancel: CancellationToken::new(),
            target_idx: None,
            health_states: None,
            quota: None,
            admit_guards: Mutex::new(Vec::new()),
        })
    }

    /// 007-multi-target-failover T024/T034 — multi-target constructor.
    /// `target_idx` is the slot the flow stuck to on its first packet
    /// (FR-012). `health_states` is a clone of the per-rule slice so
    /// `bump_inbound`/`bump_outbound` can credit per-target byte
    /// counters without holding any listener-side lock.
    #[must_use]
    pub fn new_multi_target(
        source_addr: SocketAddr,
        upstream_socket: Arc<UdpSocket>,
        upstream_addrs: Vec<SocketAddr>,
        target_idx: u32,
        health_states: Arc<Vec<Mutex<crate::forwarder::failover::HealthState>>>,
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
            last_seen_nanos: AtomicU64::new(now_nanos()),
            bytes_in: AtomicU64::new(0),
            bytes_out: AtomicU64::new(0),
            datagrams_in: AtomicU64::new(0),
            datagrams_out: AtomicU64::new(0),
            cancel: CancellationToken::new(),
            target_idx: Some(target_idx),
            health_states: Some(health_states),
            quota: None,
            admit_guards: Mutex::new(Vec::new()),
        })
    }

    /// 013-traffic-quotas E4: attach a quota handle at construction
    /// time. Used by the listener to install the per-(user, client)
    /// budget into a freshly-built flow before it's inserted into the
    /// flow table. The flow keeps an `Arc<QuotaHandle>` clone for the
    /// life of the flow; replay-time `QuotaScopeManager.install`
    /// updates reach in-flight UDP via the shared atomic, mirroring TCP.
    pub fn attach_quota(
        mut self: Arc<Self>,
        quota: Arc<crate::forwarder::quota::QuotaHandle>,
    ) -> Arc<Self> {
        // The Arc is unique at this point (the listener has not yet
        // inserted it into the flow table or cloned it for the reply
        // pump). `Arc::get_mut` returns Some because no other clones
        // exist.
        if let Some(slot) = Arc::get_mut(&mut self) {
            slot.quota = Some(quota);
        }
        self
    }

    /// 014-udp-centralized-demux: install the v0.11 layered admission
    /// guards into a freshly-built flow. Stores 0, 1, or 2
    /// `ActiveGuard`s depending on which scopes (owner / rule) were
    /// capped. The guards ride the flow's `Arc` lifetime — when the
    /// registry releases the last strong ref (idle reaper, explicit
    /// `remove`, AddFlow rollback), the Vec drops and each guard's
    /// `Drop` decrements `active_connections`. This is the v0.11
    /// `concurrent_connections` enforcement seam for UDP under the
    /// centralized-demux design (v0.4 used a per-flow guard task
    /// via `spawn_admit_guard` — replaced here by attaching to the
    /// flow itself, eliminating the extra task per flow).
    pub async fn attach_admit_guards(
        &self,
        guards: Vec<crate::forwarder::rate_limit::scope::ActiveGuard>,
    ) {
        if guards.is_empty() {
            return;
        }
        *self.admit_guards.lock().await = guards;
    }

    /// 013-traffic-quotas E4: true iff the budget is not exhausted.
    /// `None` quota → always true (legacy fast path; one branch, no
    /// atomic load).
    #[must_use]
    pub fn quota_allows(&self) -> bool {
        match self.quota.as_ref() {
            None => true,
            Some(q) => !q.is_exhausted(),
        }
    }

    /// 013-traffic-quotas E4: consume `n` bytes after a successful
    /// `send_to`. No-op on unmetered flows. Returns false iff the
    /// consume straddled the budget boundary or it was already
    /// exhausted — the datagram still landed (consume is post-send),
    /// but the caller treats `false` as a signal to drop subsequent
    /// datagrams via `quota_allows`.
    pub fn quota_consume_after_send(&self, n: u64) -> bool {
        let Some(q) = self.quota.as_ref() else {
            return true;
        };
        matches!(
            q.consume(i64::try_from(n).unwrap_or(i64::MAX)),
            crate::forwarder::quota::ConsumeOutcome::Granted,
        )
    }

    /// v1.5.1 batched-listener seam: try to debit `n` bytes BEFORE
    /// the send (so the batched path can pre-budget a whole run and
    /// drop tail packets immediately when the budget runs out
    /// instead of overshooting by `(batch_size - 1) × MTU`).
    /// Returns `true` if the debit was granted in full. The caller
    /// MUST call [`quota_restore`] for each packet that ends up not
    /// being sent (sendmmsg partial / WouldBlock fallback) so the
    /// budget stays exact.
    ///
    /// Single-packet callers should keep using `quota_allows` +
    /// `quota_consume_after_send`; the pre-debit pattern is strictly
    /// for the eager-build / late-flush batched path.
    #[must_use]
    pub fn quota_try_consume(&self, n: u64) -> bool {
        let Some(q) = self.quota.as_ref() else {
            return true;
        };
        matches!(
            q.consume(i64::try_from(n).unwrap_or(i64::MAX)),
            crate::forwarder::quota::ConsumeOutcome::Granted,
        )
    }

    /// v1.5.1 batched-listener seam: refund `n` bytes previously
    /// pre-debited by [`quota_try_consume`] when the corresponding
    /// datagram turned out not to reach the upstream. No-op on
    /// unmetered flows.
    pub fn quota_restore(&self, n: u64) {
        if let Some(q) = self.quota.as_ref() {
            q.restore(i64::try_from(n).unwrap_or(i64::MAX));
        }
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
    /// 007-multi-target-failover T024/T034 — also credits the per-target
    /// byte counter when `target_idx`/`health_states` are set (multi-
    /// target rules only). The legacy hot path (`target_idx: None`)
    /// skips the per-target work entirely and stays byte-identical to
    /// v0.6.0.
    pub async fn bump_inbound(&self, n: u64) {
        self.bytes_in.fetch_add(n, Ordering::Relaxed);
        self.datagrams_in.fetch_add(1, Ordering::Relaxed);
        self.last_seen_nanos.store(now_nanos(), Ordering::Relaxed);
        self.credit_target_in(n).await;
    }

    /// Record that a datagram of `n` bytes flowed outbound (upstream →
    /// end-user). Updates the per-flow counters and bumps `last_seen`.
    /// 007-multi-target-failover T024/T034 — also credits the per-target
    /// byte counter when `target_idx`/`health_states` are set.
    pub async fn bump_outbound(&self, n: u64) {
        self.bytes_out.fetch_add(n, Ordering::Relaxed);
        self.datagrams_out.fetch_add(1, Ordering::Relaxed);
        self.last_seen_nanos.store(now_nanos(), Ordering::Relaxed);
        self.credit_target_out(n).await;
    }

    async fn credit_target_in(&self, n: u64) {
        if let (Some(idx), Some(states)) = (self.target_idx, self.health_states.as_ref())
            && let Some(slot) = states.get(idx as usize)
        {
            slot.lock().await.add_bytes_in(n);
        }
    }

    async fn credit_target_out(&self, n: u64) {
        if let (Some(idx), Some(states)) = (self.target_idx, self.health_states.as_ref())
            && let Some(slot) = states.get(idx as usize)
        {
            slot.lock().await.add_bytes_out(n);
        }
    }

    /// Read the last activity timestamp. Wait-free atomic load — the
    /// reaper (US4) calls this on every sweep tick across every live
    /// flow, so we want zero scheduler involvement.
    #[must_use]
    pub fn last_seen_at(&self) -> Instant {
        instant_from_nanos(self.last_seen_nanos.load(Ordering::Relaxed))
    }

    /// 014-udp-centralized-demux: lightweight constructor for unit
    /// tests that only need a valid `UdpFlow` shape (e.g. registry
    /// tests). Binds `0.0.0.0:0` and seeds an empty upstream list
    /// placeholder of `src` so invariants hold without actually
    /// dialing anything.
    #[cfg(test)]
    #[must_use]
    pub async fn for_test(src: SocketAddr) -> Arc<Self> {
        let sock = Arc::new(
            UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, 0))
                .await
                .expect("for_test bind"),
        );
        Self::new(src, sock, vec![src])
    }

    /// 014-udp-centralized-demux test helper: forcibly rewind
    /// `last_seen` to a specific instant so reaper-style unit tests
    /// can drive idle eviction deterministically.
    #[cfg(test)]
    pub fn force_last_seen(&self, t: Instant) {
        let baseline = baseline();
        let nanos = if t >= baseline {
            let raw = t.duration_since(baseline).as_nanos();
            u64::try_from(raw).unwrap_or(u64::MAX)
        } else {
            // Caller asked for a moment before our baseline — store 0 so
            // any `idle_window` check fires immediately.
            0
        };
        self.last_seen_nanos.store(nanos, Ordering::Relaxed);
    }

    /// 014-udp-centralized-demux: lightweight constructor for demux
    /// unit tests that need to provide a pre-built, already-connected
    /// upstream `UdpSocket`. Used by the demux fairness / round-trip
    /// tests so they can wire the upstream to a known peer before
    /// handing the flow to `run_demux`.
    ///
    /// Kept `async` to mirror the sibling `for_test` constructor so
    /// call sites can swap helpers without rearranging `.await`s.
    #[cfg(test)]
    #[must_use]
    #[allow(clippy::unused_async)]
    pub async fn for_test_with_socket(src: SocketAddr, sock: Arc<UdpSocket>) -> Arc<Self> {
        Self::new(src, sock, vec![src])
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
    use portunus_core::Hostname;
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
        let before = flow.last_seen_at();
        // Crank time forward enough that monotonic Instant detects motion
        // even on coarse-resolution clocks.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        flow.bump_inbound(128).await;
        assert_eq!(flow.bytes_in.load(Ordering::Relaxed), 128);
        assert_eq!(flow.datagrams_in.load(Ordering::Relaxed), 1);
        assert!(flow.last_seen_at() > before);
    }
}

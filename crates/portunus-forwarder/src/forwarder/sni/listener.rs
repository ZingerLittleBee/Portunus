//! SNI-mode TCP listener. Spec 009-tls-sni-routing data-model.md §2.3.
//!
//! Owns the bound `TcpListener`, the `watch::Receiver<Arc<SniDispatchState>>`
//! (routing table + resolver slots as one atomic payload — #55(6)),
//! the cancellation token, and an `Arc<SniListenerCounters>`. On each
//! accept it peeks the ClientHello, looks up the SNI in a snapshot of
//! the routing table, and dispatches into `proxy::proxy_with_preread`
//! so the captured handshake bytes reach the upstream verbatim.
//!
//! Lookup result mapping (data-model.md §2.3 + R-009):
//!
//! | outcome                        | action                                  | tracing event             |
//! |--------------------------------|-----------------------------------------|---------------------------|
//! | Hit { Exact / Wildcard }       | dispatch + bump per-rule counter        | `tls.sni_routed` INFO     |
//! | Hit { Fallback } w/ SNI        | dispatch fallback + bump fallback ctr   | `tls.sni_routed` INFO     |
//! | Hit { Fallback } w/o SNI       | dispatch fallback + bump fallback ctr   | `tls.no_sni` INFO         |
//! | Miss (host present, no rule)   | drop, bump listener-miss counter        | `tls.sni_no_match` WARN   |
//! | Miss (no SNI, no fallback)     | drop, bump listener-miss counter        | `tls.no_sni` INFO         |
//! | PeekError::Timeout             | drop                                    | `tls.client_hello_timeout` WARN |
//! | PeekError::NotTls / Malformed  | drop, bump parse_failures counter       | `tls.parse_failed` WARN   |
//! | PeekError::Io / SizeCap        | drop, bump parse_failures counter       | `tls.parse_failed` WARN   |

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use portunus_core::{RuleId, Target, peek_histogram};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::forwarder::proxy::proxy_with_preread_and_prelude;
use crate::forwarder::proxy_protocol::ProxyProtocolPrelude;
use crate::forwarder::rate_limit::scope::{LayeredAcquire, try_acquire_layered};
use crate::forwarder::stats::{RuleStats, SniListenerStatsSnapshot};
use crate::resolver::{LiveResolver, Resolve};

use super::peek::{self, PeekError};
use super::route_table::{SniMatch, SniMatchKind, SniRoutingTable};

/// Upper bound on ClientHello peeks in flight per SNI listener (#50).
///
/// Every accepted connection may sit in the ClientHello peek for up to
/// `peek::PEEK_TIMEOUT` (3 s) while accumulating up to
/// `peek::PEEK_BYTE_CAP` (64 KiB), and no rule-level rate limit can
/// apply until *after* the SNI is parsed — the rule is unknown before
/// then, so the limiter cannot gate the peek (structural). Without a
/// cap, a flood of silent/slow ("slowloris") connections would pin
/// unbounded task + peek-buffer memory. This semaphore bounds the number
/// of concurrent peeks; excess connections are dropped (RST) at accept
/// and counted. The permit is released the instant the peek finishes, so
/// the cap composes with the per-connection 3 s / 64 KiB bound to give a
/// worst case of ~`MAX_INFLIGHT_PEEKS` × 64 KiB ≈ 64 MiB of transient
/// peek buffers — generous for legitimate connection bursts while still
/// bounded. It does NOT limit established proxy connections (those run
/// after the permit is dropped and are governed by the rule/owner rate
/// limiter).
const MAX_INFLIGHT_PEEKS: usize = 1024;

/// Per-listener counters surfaced via `proto::SniListenerStats`
/// (T078). Bumped from the accept loop's miss / parse-failure paths.
#[derive(Default, Debug)]
pub struct SniListenerCounters {
    pub miss: AtomicU64,
    pub parse_failures: AtomicU64,
    /// #50: connections dropped at accept because the in-flight
    /// ClientHello peek cap (`MAX_INFLIGHT_PEEKS`) was saturated. A
    /// non-zero value flags either a legitimate connection burst beyond
    /// the cap or a slowloris-style peek-exhaustion attempt. Kept out of
    /// the `proto::SniListenerStats` wire snapshot for now to avoid a
    /// schema change; observable via this in-process counter.
    pub peek_capacity_rejections: AtomicU64,
    pub peek_histogram: PeekDurationHistogram,
}

#[derive(Debug)]
pub struct PeekDurationHistogram {
    buckets: Box<[AtomicU64]>,
    sum_micros: AtomicU64,
    count: AtomicU64,
}

impl Default for PeekDurationHistogram {
    fn default() -> Self {
        Self {
            buckets: std::iter::repeat_with(|| AtomicU64::new(0))
                .take(peek_histogram::bucket_count())
                .collect(),
            sum_micros: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }
}

impl PeekDurationHistogram {
    pub fn observe(&self, elapsed: std::time::Duration) {
        if let Some(idx) = peek_histogram::bucket_index(elapsed) {
            for bucket in self.buckets.iter().skip(idx) {
                bucket.fetch_add(1, Ordering::Relaxed);
            }
        }
        let micros = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
        self.sum_micros.fetch_add(micros, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    #[must_use]
    pub fn snapshot(&self) -> (Vec<u64>, u64, u64) {
        let buckets = self
            .buckets
            .iter()
            .map(|bucket| bucket.load(Ordering::Relaxed))
            .collect();
        let sum_micros = self.sum_micros.load(Ordering::Relaxed);
        let count = self.count.load(Ordering::Relaxed);
        (buckets, sum_micros, count)
    }
}

impl SniListenerCounters {
    /// Return a wire-neutral snapshot of all listener counters for the
    /// given `listen_port`. Atomic loads use `Relaxed` ordering,
    /// matching the pattern of every other counter in this module.
    #[must_use]
    pub fn snapshot(&self, listen_port: u16) -> SniListenerStatsSnapshot {
        let (
            client_hello_peek_bucket_counts,
            client_hello_peek_sum_micros,
            client_hello_peek_count,
        ) = self.peek_histogram.snapshot();
        SniListenerStatsSnapshot {
            listen_port,
            sni_route_miss_total: self.miss.load(Ordering::Relaxed),
            client_hello_parse_failures_total: self.parse_failures.load(Ordering::Relaxed),
            client_hello_peek_bucket_counts,
            client_hello_peek_sum_micros,
            client_hello_peek_count,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn snapshot_returns_zeroed_for_fresh_counters() {
        let c = SniListenerCounters::default();
        let snap = c.snapshot(8443);
        assert_eq!(snap.listen_port, 8443);
        assert_eq!(snap.sni_route_miss_total, 0);
        assert_eq!(snap.client_hello_parse_failures_total, 0);
        assert_eq!(snap.client_hello_peek_count, 0);
        assert_eq!(snap.client_hello_peek_sum_micros, 0);
        assert_eq!(
            snap.client_hello_peek_bucket_counts.len(),
            portunus_core::PEEK_HISTOGRAM_BUCKETS_SECS.len()
        );
    }

    #[test]
    fn peek_histogram_snapshots_cumulative_buckets() {
        let histogram = PeekDurationHistogram::default();

        histogram.observe(Duration::from_millis(1));
        histogram.observe(Duration::from_secs(4));

        let (buckets, sum_micros, count) = histogram.snapshot();
        let one_ms_idx = peek_histogram::PEEK_HISTOGRAM_BUCKETS_SECS
            .iter()
            .position(|upper| upper.to_bits() == 0.001f64.to_bits())
            .expect("1ms bucket exists");

        assert_eq!(buckets[one_ms_idx], 1);
        assert_eq!(buckets[one_ms_idx + 1], 1);
        assert_eq!(buckets.last().copied(), Some(1));
        assert_eq!(sum_micros, 4_001_000);
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn peek_cap_refuses_connections_beyond_the_in_flight_limit() {
        // #50 regression: with N peek permits and N silent connections
        // parked in the ClientHello peek, the (N+1)th connection must be
        // refused at accept (dropped/RST) and counted, NOT queued into
        // another 3 s peek. We use a small injected cap (like
        // `read_client_hello_with` injects a short timeout) so the test is
        // deterministic and finishes well inside PEEK_TIMEOUT (3 s).
        use crate::resolver::{ResolveAnswer, ResolverConfig, ResolverError};
        use portunus_core::Hostname;
        use std::net::{IpAddr, Ipv4Addr};

        #[derive(Debug)]
        struct StubResolver;

        #[async_trait::async_trait]
        impl Resolve for StubResolver {
            async fn resolve(&self, _name: &Hostname) -> Result<ResolveAnswer, ResolverError> {
                Ok(ResolveAnswer {
                    addrs: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
                    ttl: Duration::from_secs(60),
                })
            }
        }

        // One rule slot so `handle_accept`'s non-empty-slots debug_assert
        // holds for the parked connections.
        let mut slots = std::collections::HashMap::new();
        slots.insert(
            RuleId(1),
            SniRuleSlot {
                rule_id: RuleId(1),
                target: Target::Ip(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                target_port: 443,
                proxy_protocol: None,
                prefer_ipv6: false,
                listen_port: 0,
                stats: RuleStats::new(),
                sni_route_exact_total: Arc::new(AtomicU64::new(0)),
                sni_route_wildcard_total: Arc::new(AtomicU64::new(0)),
                sni_route_fallback_total: Arc::new(AtomicU64::new(0)),
                rate_limit: None,
                rate_limit_stats: None,
                owner_rate_limit: None,
                owner_rate_limit_stats: None,
                quota: None,
            },
        );

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();

        let counters = Arc::new(SniListenerCounters::default());
        let (_state_tx, state_rx) = watch::channel(Arc::new(SniDispatchState {
            table: Arc::new(SniRoutingTable::default()),
            resolver: Arc::new(SniRouteResolver { slots }),
        }));
        let cancel = CancellationToken::new();

        let sni = SniListener {
            listen_port: addr.port(),
            counters: Arc::clone(&counters),
            state_rx,
            cancel: cancel.clone(),
        };
        let live_resolver = Arc::new(LiveResolver::new(
            Arc::new(StubResolver),
            ResolverConfig::default(),
        ));

        // Cap of 2 in-flight peeks.
        let server = tokio::spawn(sni.run_with_peek_cap(listener, live_resolver, 2));

        // Two silent connections (never send a ClientHello) each park in
        // the peek, holding a permit for up to PEEK_TIMEOUT.
        let mut parked = Vec::new();
        for _ in 0..2 {
            parked.push(tokio::net::TcpStream::connect(addr).await.unwrap());
        }
        // The third silent connection must be refused at accept.
        let _third = tokio::net::TcpStream::connect(addr).await.unwrap();

        // Poll the rejection counter within the window the two peeks stay
        // parked. Robust: permits are held for ~3 s, far beyond this poll.
        let deadline = tokio::time::Instant::now() + Duration::from_millis(1500);
        while counters.peek_capacity_rejections.load(Ordering::Relaxed) == 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the third connection was not refused within the peek window",
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(counters.peek_capacity_rejections.load(Ordering::Relaxed), 1);

        cancel.cancel();
        drop(parked);
        let _ = server.await;
    }
}

/// Per-rule resolution + dispatch context. The listener resolves a
/// `RuleId` from the SNI lookup into one of these slots so the
/// per-rule data plane (Target classification, port range, stats) is
/// ready to feed into `proxy_with_preread`.
#[derive(Clone)]
pub struct SniRuleSlot {
    pub rule_id: RuleId,
    pub target: Target,
    pub target_port: u16,
    pub proxy_protocol: Option<portunus_core::ProxyProtocolVersion>,
    pub prefer_ipv6: bool,
    pub listen_port: u16,
    pub stats: Arc<RuleStats>,
    /// Per-rule SNI hit counter. The right slot
    /// (exact/wildcard/fallback) is chosen by the listener based on
    /// `SniMatchKind` and bumped before dispatch.
    pub sni_route_exact_total: Arc<AtomicU64>,
    pub sni_route_wildcard_total: Arc<AtomicU64>,
    pub sni_route_fallback_total: Arc<AtomicU64>,
    /// 011-rate-limiting-qos: per-rule data-plane limiter handle.
    /// Cloned from `GroupMember.rate_limit` so the SNI dispatcher
    /// hands the same `try_acquire_layered` cascade the legacy
    /// accept path uses.
    pub rate_limit: Option<Arc<crate::forwarder::rate_limit::scope::RuleRateLimitHandle>>,
    /// 011-rate-limiting-qos: per-rule reject/active stats accumulator.
    pub rate_limit_stats:
        Option<Arc<crate::forwarder::rate_limit::stats::RateLimitStatsAccumulator>>,
    /// 011-rate-limiting-qos: per-owner data-plane limiter handle.
    pub owner_rate_limit: Option<Arc<crate::forwarder::rate_limit::scope::OwnerRateLimitHandle>>,
    /// 011-rate-limiting-qos: per-owner reject/active stats accumulator.
    pub owner_rate_limit_stats:
        Option<Arc<crate::forwarder::rate_limit::stats::RateLimitStatsAccumulator>>,
    /// 013-traffic-quotas E2: per-(user, client) byte budget handle.
    /// Cloned from `ClientRule.quota`; routes copy_uncapped through
    /// the quota-aware userspace path when present.
    pub quota: Option<Arc<crate::forwarder::quota::QuotaHandle>>,
}

/// Snapshot of the rule slots a SNI listener can dispatch to.
/// Wrapped in `Arc` and swapped in via `watch` whenever the route
/// group composition changes (PUSH/REMOVE).
#[derive(Clone, Default)]
pub struct SniRouteResolver {
    pub slots: std::collections::HashMap<RuleId, SniRuleSlot>,
}

/// #55(6): the routing table and the per-rule resolver slots published
/// as ONE atomic `watch` payload. A connection landing mid-reconfig
/// takes a single `borrow()` snapshot, so it can never observe a table
/// carrying a `rule_id` whose resolver slot has not yet been installed
/// (or vice versa) — the pair is always mutually consistent. The
/// publisher (`PortGroupManager::rebuild_watches`) builds both halves
/// and sends them together on every update.
#[derive(Clone, Default)]
pub struct SniDispatchState {
    pub table: Arc<SniRoutingTable>,
    pub resolver: Arc<SniRouteResolver>,
}

/// Configuration for one SNI-mode listener. Owned by the
/// `PortGroupManager` (T042); the listener task reads through the
/// shared `Arc`s.
pub struct SniListener {
    pub listen_port: u16,
    pub counters: Arc<SniListenerCounters>,
    /// #55(6): one channel carrying the table + resolver pair so a
    /// mid-reconfig accept snapshots a consistent state in a single
    /// `borrow()`.
    pub state_rx: watch::Receiver<Arc<SniDispatchState>>,
    pub cancel: CancellationToken,
}

impl SniListener {
    /// Spawn the accept loop. Returns when `cancel` fires.
    pub async fn run<R: Resolve + 'static>(
        self,
        listener: TcpListener,
        live_resolver: Arc<LiveResolver<R>>,
    ) {
        self.run_with_peek_cap(listener, live_resolver, MAX_INFLIGHT_PEEKS)
            .await;
    }

    /// Accept loop with an injectable in-flight peek cap. The public
    /// [`run`](Self::run) wires in `MAX_INFLIGHT_PEEKS`; tests pass a
    /// small cap to exercise the slowloris guard (#50) deterministically,
    /// mirroring how `peek::read_client_hello_with` injects a short
    /// timeout.
    async fn run_with_peek_cap<R: Resolve + 'static>(
        self,
        listener: TcpListener,
        live_resolver: Arc<LiveResolver<R>>,
        max_inflight_peeks: usize,
    ) {
        let SniListener {
            listen_port,
            counters,
            state_rx,
            cancel,
        } = self;
        // #50: bound concurrent ClientHello peeks. A permit is acquired
        // at accept and released the moment the peek finishes (see
        // `handle_accept`), so this caps peeks — not live proxy sessions.
        let peek_semaphore = Arc::new(tokio::sync::Semaphore::new(max_inflight_peeks));
        info!(
            target = "tls_sni",
            event = "tls.sni_listener.started",
            listen_port,
        );
        loop {
            tokio::select! {
                () = cancel.cancelled() => {
                    debug!(
                        target = "tls_sni",
                        event = "tls.sni_listener.stopped",
                        listen_port,
                    );
                    return;
                }
                accept = listener.accept() => {
                    match accept {
                        Ok((stream, peer)) => {
                            // #50: grab a peek permit before spawning. On
                            // saturation, drop the socket immediately (the
                            // OS sends RST) rather than queueing another
                            // 3 s / 64 KiB peek — this is the slowloris
                            // backpressure.
                            let Ok(peek_permit) =
                                Arc::clone(&peek_semaphore).try_acquire_owned()
                            else {
                                counters
                                    .peek_capacity_rejections
                                    .fetch_add(1, Ordering::Relaxed);
                                warn!(
                                    target = "tls_sni",
                                    event = "tls.sni_peek_capacity",
                                    listen_port,
                                    peer = %peer,
                                    max_inflight_peeks,
                                );
                                drop(stream);
                                continue;
                            };
                            let counters = Arc::clone(&counters);
                            // #55(6): the table + resolver pair is one
                            // `watch` payload, so a single `borrow()`
                            // snapshot is always mutually consistent — a
                            // connection landing mid-reconfig sees either
                            // the fully-old or fully-new state, never a
                            // table entry whose resolver slot is missing.
                            let state = state_rx.borrow().clone();
                            let table = Arc::clone(&state.table);
                            let routes = Arc::clone(&state.resolver);
                            let resolver = Arc::clone(&live_resolver);
                            let cancel = cancel.clone();
                            tokio::spawn(async move {
                                handle_accept(
                                    stream,
                                    peer,
                                    listen_port,
                                    table,
                                    routes,
                                    counters,
                                    resolver,
                                    cancel,
                                    peek_permit,
                                )
                                .await;
                            });
                        }
                        Err(e) => {
                            warn!(
                                target = "tls_sni",
                                event = "tls.sni_listener.accept_error",
                                listen_port,
                                error = %e,
                            );
                            // Brief backoff; the legacy listener uses the
                            // same pattern.
                            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                        }
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_accept<R: Resolve + 'static>(
    mut stream: TcpStream,
    peer: std::net::SocketAddr,
    listen_port: u16,
    table: Arc<SniRoutingTable>,
    routes: Arc<SniRouteResolver>,
    counters: Arc<SniListenerCounters>,
    resolver: Arc<LiveResolver<R>>,
    cancel: CancellationToken,
    // #50: held only for the ClientHello peek phase. Dropped (explicitly
    // on success, implicitly on every early `return`) before the
    // long-lived proxy phase so the in-flight cap bounds peeks, not
    // established connections.
    peek_permit: tokio::sync::OwnedSemaphorePermit,
) {
    let peek_started = Instant::now();
    // 009-tls-sni-routing T068: structural mode-lock guard. The
    // PortGroupManager only constructs an `SniListener` for ports
    // running in SNI dispatch mode (R-004). A non-empty routing
    // table (or a populated resolver) is the live invariant; if a
    // future "Legacy mode" gets bolted onto the same listener type,
    // this assertion trips before any byte is peeked. Cheap in
    // release builds (debug_assert!).
    debug_assert!(
        !routes.slots.is_empty(),
        "SniListener::handle_accept invoked with no rule slots — legacy listeners must run a different task type (R-004)"
    );
    let (preread, sni) = match peek::read_client_hello(&mut stream).await {
        Ok((buf, sni)) => {
            counters.peek_histogram.observe(peek_started.elapsed());
            (buf, sni)
        }
        Err(PeekError::Timeout { bytes_read }) => {
            counters.peek_histogram.observe(peek_started.elapsed());
            counters.parse_failures.fetch_add(1, Ordering::Relaxed);
            warn!(
                target = "tls_sni",
                event = "tls.client_hello_timeout",
                listen_port,
                peer = %peer,
                bytes_read,
            );
            return;
        }
        Err(other) => {
            counters.peek_histogram.observe(peek_started.elapsed());
            counters.parse_failures.fetch_add(1, Ordering::Relaxed);
            warn!(
                target = "tls_sni",
                event = "tls.parse_failed",
                listen_port,
                peer = %peer,
                error = ?other,
            );
            return;
        }
    };
    // #50: the ClientHello peek is complete — release the permit now,
    // BEFORE the (potentially long-lived) proxy phase, so the cap bounds
    // concurrent peeks rather than total connections.
    drop(peek_permit);

    let sni_str = sni.as_deref();
    let m = table.lookup(sni_str);
    let (rule_id, kind) = match m {
        SniMatch::Hit { rule_id, kind } => (rule_id, kind),
        SniMatch::Miss => {
            counters.miss.fetch_add(1, Ordering::Relaxed);
            match sni_str {
                Some(host) => {
                    warn!(
                        target = "tls_sni",
                        event = "tls.sni_no_match",
                        listen_port,
                        peer = %peer,
                        server_name = %host,
                    );
                }
                None => {
                    info!(
                        target = "tls_sni",
                        event = "tls.no_sni",
                        listen_port,
                        peer = %peer,
                        fallback_used = false,
                    );
                }
            }
            return;
        }
    };

    let Some(slot) = routes.slots.get(&rule_id) else {
        // Race: the rule was REMOVE'd between the table snapshot and
        // this lookup. Drop quietly — no per-rule counter to bump.
        counters.miss.fetch_add(1, Ordering::Relaxed);
        warn!(
            target = "tls_sni",
            event = "tls.sni_no_match",
            listen_port,
            peer = %peer,
            reason = "rule_id_unknown",
            rule_id = %rule_id,
        );
        return;
    };

    let match_kind = match kind {
        SniMatchKind::Exact => {
            slot.sni_route_exact_total.fetch_add(1, Ordering::Relaxed);
            "exact"
        }
        SniMatchKind::Wildcard => {
            slot.sni_route_wildcard_total
                .fetch_add(1, Ordering::Relaxed);
            "wildcard"
        }
        SniMatchKind::Fallback => {
            slot.sni_route_fallback_total
                .fetch_add(1, Ordering::Relaxed);
            "fallback"
        }
    };
    if matches!(kind, SniMatchKind::Fallback) && sni_str.is_none() {
        info!(
            target = "tls_sni",
            event = "tls.no_sni",
            listen_port,
            peer = %peer,
            fallback_used = true,
        );
    } else {
        info!(
            target = "tls_sni",
            event = "tls.sni_routed",
            listen_port,
            peer = %peer,
            server_name = sni_str.unwrap_or(""),
            match_kind = match_kind,
            rule_id = %rule_id,
        );
    }

    let proxy_prelude = match slot.proxy_protocol {
        Some(version) => match stream.local_addr() {
            Ok(destination) => Some(ProxyProtocolPrelude {
                version,
                source: peer,
                destination,
            }),
            Err(error) => {
                warn!(
                    target = "tls_sni",
                    event = "tls.proxy_protocol_local_addr_failed",
                    listen_port,
                    peer = %peer,
                    rule_id = %rule_id,
                    error = %error,
                );
                return;
            }
        },
        None => None,
    };

    // 011-rate-limiting-qos T020/T030: the owner-then-rule admission
    // cascade. SNI dispatch has to peek the ClientHello before it can
    // resolve the rule, so the gate runs after route lookup rather
    // than at accept (FR-013 ordering is preserved within the layered
    // call). A reject drops the post-peek socket; the OS sends RST
    // (Q3 / FR-009). Guards held for the lifetime of the proxy task
    // so the active-connections gauge decrements on close (mirrors
    // the legacy accept_loop and failover_path patterns).
    let (_owner_admit, _rule_admit) = match try_acquire_layered(
        slot.owner_rate_limit.as_ref(),
        slot.rate_limit.as_ref(),
        false,
    ) {
        LayeredAcquire::Granted {
            owner_guard,
            rule_guard,
        } => (owner_guard, rule_guard),
        LayeredAcquire::OwnerRejected(reason) => {
            if let Some(s) = slot.owner_rate_limit_stats.as_ref() {
                s.record_reject(reason);
            }
            debug!(
                target = "tls_sni",
                event = "tls.rate_limit_reject",
                rule_id = %rule_id,
                listen_port,
                peer = %peer,
                scope = "owner",
                reason = reason.as_metric_label(),
            );
            return;
        }
        LayeredAcquire::RuleRejected(reason) => {
            if let Some(s) = slot.rate_limit_stats.as_ref() {
                s.record_reject(reason);
            }
            debug!(
                target = "tls_sni",
                event = "tls.rate_limit_reject",
                rule_id = %rule_id,
                listen_port,
                peer = %peer,
                scope = "rule",
                reason = reason.as_metric_label(),
            );
            return;
        }
    };

    let res = proxy_with_preread_and_prelude(
        stream,
        Some(preread),
        &resolver,
        slot.rule_id,
        &slot.target,
        slot.target_port,
        slot.prefer_ipv6,
        proxy_prelude,
        cancel,
        Some(Arc::clone(&slot.stats)),
        slot.listen_port,
        // 011-rate-limiting-qos limiters plumbed end-to-end through
        // PortGroupManager (GroupMember) and the per-port watch into
        // SniRuleSlot. The layered admission cascade above gates the
        // accept; here the same handles flow into the proxy so the
        // bandwidth-cap-aware bidi copy applies any per-chunk caps,
        // identical to the legacy accept path. Capped SNI rules now
        // enforce both per-rule and per-owner caps without any
        // diversion through the legacy accept_loop.
        slot.rate_limit.clone(),
        slot.rate_limit_stats.clone(),
        slot.owner_rate_limit.clone(),
        slot.owner_rate_limit_stats.clone(),
        slot.quota.clone(),
    )
    .await;
    if let Err(e) = res
        && e.kind() != std::io::ErrorKind::Other
    {
        // ErrorKind::Other carries the deliberate "proxy_cancelled"
        // signal — not worth a warning.
        debug!(
            target = "tls_sni",
            event = "tls.proxy_finished_error",
            listen_port,
            rule_id = %rule_id,
            error = %e,
        );
    }
}

//! SNI-mode TCP listener. Spec 009-tls-sni-routing data-model.md §2.3.
//!
//! Owns the bound `TcpListener`, the `watch::Receiver<Arc<SniRoutingTable>>`,
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

/// Per-listener counters surfaced via `proto::SniListenerStats`
/// (T078). Bumped from the accept loop's miss / parse-failure paths.
#[derive(Default, Debug)]
pub struct SniListenerCounters {
    pub miss: AtomicU64,
    pub parse_failures: AtomicU64,
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

/// Configuration for one SNI-mode listener. Owned by the
/// `PortGroupManager` (T042); the listener task reads through the
/// shared `Arc`s.
pub struct SniListener {
    pub listen_port: u16,
    pub counters: Arc<SniListenerCounters>,
    pub table_rx: watch::Receiver<Arc<SniRoutingTable>>,
    pub resolver_rx: watch::Receiver<Arc<SniRouteResolver>>,
    pub cancel: CancellationToken,
}

impl SniListener {
    /// Spawn the accept loop. Returns when `cancel` fires.
    pub async fn run<R: Resolve + 'static>(
        self,
        listener: TcpListener,
        live_resolver: Arc<LiveResolver<R>>,
    ) {
        let SniListener {
            listen_port,
            counters,
            table_rx,
            resolver_rx,
            cancel,
        } = self;
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
                            let counters = Arc::clone(&counters);
                            let table = table_rx.borrow().clone();
                            let routes = resolver_rx.borrow().clone();
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

//! Per-listener UDP recv loop for the 014-udp-centralized-demux
//! runtime. Owns the listen socket's `recv_from` loop and, on each
//! datagram, applies the FR-004 cold-path admission order:
//!
//!   1. existing-flow fast path (registry `get` → quota check →
//!      `try_send` → classify);
//!   2. quota exhaustion short-circuit;
//!   3. per-rule cap reservation (`registry.try_get_or_reserve`);
//!   4. layered owner+rule rate-limit gate (`try_acquire_layered`);
//!   5. resolver lookup;
//!   6. multi-A walk: bind+connect; first success wins;
//!   7. build `UdpFlow`, attach quota, commit reservation;
//!   8. hand the flow to the demux via `DemuxCommand::AddFlow`;
//!   9. first-packet `try_send`, classified per FR-006/FR-007.
//!
//! Listener does NOT bind the socket — the runtime (Phase 7) probes
//! all ports up-front and shares the resulting `Arc<UdpSocket>` with
//! both this loop and the demux task. Reaper and demux run as
//! sibling tasks owned by the same runtime supervisor.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use portunus_core::{RuleId, Target};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, trace, warn};

use crate::forwarder::quota::QuotaHandle;
use crate::forwarder::rate_limit::scope::{
    ActiveGuard, LayeredAcquire, OwnerRateLimitHandle, RuleRateLimitHandle, try_acquire_layered,
};
use crate::forwarder::rate_limit::stats::RateLimitStatsAccumulator;
use crate::forwarder::stats::RuleStats;
use crate::forwarder::udp::demux::DemuxCommand;
use crate::forwarder::udp::error::{UdpAction, classify_udp_error};
use crate::forwarder::udp::flow::UdpFlow;
use crate::forwarder::udp::registry::{FlowKey, TryGetOrReserve, UdpFlowRegistry};
use crate::resolver::{LiveResolver, Resolve};

/// IP-layer UDP payload ceiling (FR-013). One static heap buffer per
/// recv loop sized to this value means `recv_from` cannot truncate any
/// well-formed datagram at the proxy layer.
const UDP_BUFFER_BYTES: usize = 65_535;

/// Per-listener configuration handed to [`run_listener`].
///
/// All shared state (registry, demux channel, stats, resolver, rate
/// limits, quota) is owned by the rule runtime in Phase 7 and cloned
/// into each per-port listener at spawn time. `cancel` is the
/// per-listener token: the runtime cancels it to tear an individual
/// listen socket down (e.g. on a range-rule resize) without disturbing
/// the shared demux/reaper.
pub struct ListenerConfig<R: Resolve + 'static> {
    pub rule_id: RuleId,
    pub listen_port: u16,
    pub target: Target,
    pub target_port: u16,
    pub prefer_ipv6: bool,
    pub idle_window: Duration,
    pub registry: Arc<UdpFlowRegistry>,
    pub demux_tx: mpsc::Sender<DemuxCommand>,
    pub stats: Arc<RuleStats>,
    pub resolver: Arc<LiveResolver<R>>,
    pub rate_limit: Option<Arc<RuleRateLimitHandle>>,
    pub rate_limit_stats: Option<Arc<RateLimitStatsAccumulator>>,
    pub owner_rate_limit: Option<Arc<OwnerRateLimitHandle>>,
    pub owner_rate_limit_stats: Option<Arc<RateLimitStatsAccumulator>>,
    pub quota: Option<Arc<QuotaHandle>>,
    pub cancel: CancellationToken,
}

/// Run a per-port UDP listener loop until `cfg.cancel` fires. The
/// runtime supervises the task; this function never spawns its own.
/// `listener_socket` is pre-bound by the runtime and shared with the
/// demux via the runtime's `listener_sockets` map.
pub async fn run_listener<R: Resolve + 'static>(
    cfg: ListenerConfig<R>,
    listener_socket: Arc<UdpSocket>,
) {
    let mut buf = vec![0u8; UDP_BUFFER_BYTES];
    loop {
        tokio::select! {
            () = cfg.cancel.cancelled() => {
                break;
            }
            recv = listener_socket.recv_from(&mut buf) => match recv {
                Ok((n, src)) => {
                    handle_datagram(&cfg, &listener_socket, &buf[..n], src).await;
                }
                Err(e) => {
                    warn!(
                        event = "rule.udp_listener_recv_failed",
                        rule_id = %cfg.rule_id,
                        listen_port = cfg.listen_port,
                        error = %e,
                    );
                }
            }
        }
    }
    info!(
        event = "rule.udp_listener_drained",
        rule_id = %cfg.rule_id,
        listen_port = cfg.listen_port,
    );
}

/// Apply FR-004 admission order to a single datagram. Pure async
/// function so unit / integration tests can drive it without spinning
/// up the full select loop (the supervisor tests in Phase 7 use
/// `run_listener`; the per-datagram tests in Phase 10 can target this
/// helper directly).
async fn handle_datagram<R: Resolve + 'static>(
    cfg: &ListenerConfig<R>,
    listener_socket: &Arc<UdpSocket>,
    payload: &[u8],
    src: SocketAddr,
) {
    let key = FlowKey::new(cfg.listen_port, src);
    let n = payload.len();
    let n_u64 = u64::try_from(n).unwrap_or(u64::MAX);

    // ---- Fast path (FR-004 step 1): existing Live flow ----
    if let Some(flow) = cfg.registry.get(key).await {
        if flow.cancel.is_cancelled() {
            // Race vs reaper / demux Evict — fall through to cold path
            // so the next datagram from this source rebuilds the flow.
        } else {
            // Quota check (FR-013): silent drop when exhausted. We must
            // re-check on every existing-flow datagram because the
            // budget may have drained mid-flow.
            if !flow.quota_allows() {
                return;
            }
            match flow.upstream_socket.try_send(payload) {
                Ok(_) => {
                    flow.bump_inbound(n_u64).await;
                    cfg.stats.inc_datagram_in(cfg.listen_port, n_u64);
                    let _ = flow.quota_consume_after_send(n_u64);
                }
                Err(e) => match classify_udp_error(&e) {
                    UdpAction::Evict => {
                        info!(
                            event = "rule.udp_flow_evicted_icmp",
                            rule_id = %cfg.rule_id,
                            listen_port = cfg.listen_port,
                            source = %src,
                            error = %e,
                        );
                        let _ = cfg.registry.remove(key).await;
                        flow.cancel.cancel();
                    }
                    UdpAction::MessageTooLarge => {
                        debug!(
                            event = "rule.udp_emsgsize",
                            rule_id = %cfg.rule_id,
                            listen_port = cfg.listen_port,
                            source = %src,
                        );
                    }
                    UdpAction::WouldBlock => {
                        trace!(
                            event = "rule.udp_upstream_wouldblock",
                            rule_id = %cfg.rule_id,
                            listen_port = cfg.listen_port,
                            source = %src,
                        );
                    }
                    UdpAction::Transient => {
                        // Drop datagram, keep flow. Next packet retries.
                    }
                },
            }
            return;
        }
    }

    // ---- Cold path (FR-004 strict order) ----

    // (2) Quota exhaustion short-circuit — don't burn resolver / bind /
    //     rate-limit work on a budget that can't deliver bytes.
    if let Some(q) = cfg.quota.as_ref()
        && q.is_exhausted()
    {
        return;
    }

    // (3) Reserve a slot in the per-rule registry. Cap exhaustion is
    //     accounted for by `try_get_or_reserve` itself (it bumps
    //     `dropped_overflow`). Reservation is RAII — early returns
    //     below release it via Drop.
    let reservation = match cfg.registry.try_get_or_reserve(key).await {
        TryGetOrReserve::Existing(flow) => {
            // Rare: another listener committed for the same (port, src)
            // between our `get` above and `try_get_or_reserve` here.
            // Treat as if we'd seen the Live flow on the fast path —
            // forward the current datagram through it.
            if !flow.quota_allows() {
                return;
            }
            if flow.upstream_socket.try_send(payload).is_ok() {
                flow.bump_inbound(n_u64).await;
                cfg.stats.inc_datagram_in(cfg.listen_port, n_u64);
                let _ = flow.quota_consume_after_send(n_u64);
            }
            // On error: drop this datagram; the next one hits the
            // fast path and classifies the error there.
            return;
        }
        TryGetOrReserve::Reserved(r) => r,
        TryGetOrReserve::CapExhausted => {
            cfg.stats.inc_flow_dropped_overflow();
            warn!(
                event = "rule.udp_flow_dropped_overflow",
                rule_id = %cfg.rule_id,
                listen_port = cfg.listen_port,
                source = %src,
            );
            return;
        }
    };

    // (4) Layered owner+rule rate-limit gate. Owner first per FR-013.
    //     Reject → silent drop, RAII releases the reservation. On
    //     admit, the returned guards ride the `UdpFlow` Arc for the
    //     flow's lifetime (v0.11 `concurrent_connections` cap).
    let admit_guards = match acquire_first_packet(cfg, src) {
        AdmitOutcome::Allowed { guards } => guards,
        AdmitOutcome::Rejected => return,
    };

    // (5) Resolve target. Single SocketAddr for IP targets;
    //     multi-A ordered list for DNS targets.
    let resolved = match cfg
        .resolver
        .resolve_target(cfg.rule_id, &cfg.target, cfg.target_port, cfg.prefer_ipv6)
        .await
    {
        Ok((addrs, _src)) if !addrs.is_empty() => addrs,
        Ok((_, _)) => {
            cfg.stats.inc_dns_failure();
            warn!(
                event = "rule.udp_dns_failed",
                rule_id = %cfg.rule_id,
                listen_port = cfg.listen_port,
                source = %src,
                reason = "empty",
            );
            return;
        }
        Err(err) => {
            cfg.stats.inc_dns_failure();
            warn!(
                event = "rule.udp_dns_failed",
                rule_id = %cfg.rule_id,
                listen_port = cfg.listen_port,
                source = %src,
                error = %err,
            );
            return;
        }
    };

    // (6) Walk the resolver list at the bind+connect seam. First
    //     successful (bind, connect) pair wins; the flow sticks to
    //     that target for its lifetime (FR-012 parity with v0.7).
    let mut selected: Option<(Arc<UdpSocket>, SocketAddr)> = None;
    for &addr in &resolved {
        let bind_addr: SocketAddr = match addr {
            SocketAddr::V4(_) => SocketAddr::from(([0u8, 0, 0, 0], 0)),
            SocketAddr::V6(_) => SocketAddr::from(([0u16; 8], 0)),
        };
        let sock = match UdpSocket::bind(bind_addr).await {
            Ok(s) => s,
            Err(e) => {
                warn!(
                    event = "rule.udp_upstream_bind_failed",
                    rule_id = %cfg.rule_id,
                    listen_port = cfg.listen_port,
                    source = %src,
                    target = %addr,
                    error = %e,
                );
                continue;
            }
        };
        match sock.connect(addr).await {
            Ok(()) => {
                selected = Some((Arc::new(sock), addr));
                break;
            }
            Err(e) => {
                warn!(
                    event = "rule.udp_upstream_connect_failed",
                    rule_id = %cfg.rule_id,
                    listen_port = cfg.listen_port,
                    source = %src,
                    target = %addr,
                    error = %e,
                );
            }
        }
    }
    let Some((upstream_socket, chosen_target)) = selected else {
        cfg.stats.inc_dns_failure();
        warn!(
            event = "rule.udp_dns_failed",
            rule_id = %cfg.rule_id,
            listen_port = cfg.listen_port,
            source = %src,
            reason = "all_targets_unreachable",
        );
        return;
    };

    // (7) Build the flow, attach quota, attach v0.11 admit guards,
    //     commit reservation. The Arc is unique through `attach_quota`
    //     (which uses `Arc::get_mut`). `attach_admit_guards` locks the
    //     internal Mutex, so it tolerates additional clones — but we
    //     install it pre-commit so the guards are bound to the flow
    //     before any other task can observe it.
    let mut flow = UdpFlow::new(src, Arc::clone(&upstream_socket), vec![chosen_target]);
    if let Some(q) = cfg.quota.as_ref() {
        flow = flow.attach_quota(Arc::clone(q));
    }
    flow.attach_admit_guards(admit_guards).await;
    cfg.registry.commit(reservation, Arc::clone(&flow)).await;

    // (8) Hand the flow to the demux. Channel full = back-pressured
    //     demux — rollback so we don't leave a Live slot the demux
    //     never sees (FR-005 invariant).
    if let Err(send_err) = cfg.demux_tx.try_send(DemuxCommand::AddFlow {
        key,
        flow: Arc::clone(&flow),
    }) {
        warn!(
            event = "rule.udp_addflow_dropped",
            rule_id = %cfg.rule_id,
            listen_port = cfg.listen_port,
            source = %src,
            reason = ?send_err,
        );
        let _ = cfg.registry.remove(key).await;
        flow.cancel.cancel();
        return;
    }

    // (9) First-packet send. Classify per FR-006/FR-007. On Evict the
    //     flow is torn down without counting `datagram_in` — the
    //     bytes never landed upstream. On WouldBlock / EMSGSIZE /
    //     Transient we drop *this* datagram but keep the flow:
    //     the demux's ReadWait is already armed, and the next packet
    //     from the same source hits the fast path.
    match upstream_socket.try_send(payload) {
        Ok(_) => {
            flow.bump_inbound(n_u64).await;
            cfg.stats.inc_datagram_in(cfg.listen_port, n_u64);
            let _ = flow.quota_consume_after_send(n_u64);
            info!(
                event = "rule.udp_flow_opened",
                rule_id = %cfg.rule_id,
                listen_port = cfg.listen_port,
                source = %src,
                target = %chosen_target,
            );
        }
        Err(e) => match classify_udp_error(&e) {
            UdpAction::Evict => {
                info!(
                    event = "rule.udp_flow_evicted_icmp",
                    rule_id = %cfg.rule_id,
                    listen_port = cfg.listen_port,
                    source = %src,
                    target = %chosen_target,
                    error = %e,
                    phase = "first_packet",
                );
                let _ = cfg.registry.remove(key).await;
                flow.cancel.cancel();
            }
            action
            @ (UdpAction::WouldBlock | UdpAction::MessageTooLarge | UdpAction::Transient) => {
                debug!(
                    event = "rule.udp_first_packet_send_dropped",
                    rule_id = %cfg.rule_id,
                    listen_port = cfg.listen_port,
                    source = %src,
                    target = %chosen_target,
                    action = ?action,
                    error = %e,
                );
                info!(
                    event = "rule.udp_flow_opened",
                    rule_id = %cfg.rule_id,
                    listen_port = cfg.listen_port,
                    source = %src,
                    target = %chosen_target,
                );
            }
        },
    }

    // `listener_socket` is held by the runtime, but we reference it via
    // the parameter to keep the signature stable for unit tests that
    // pass a hand-rolled socket; suppress the unused-binding warning
    // here without leaking the binding to callers.
    let _ = listener_socket;
}

/// Outcome of [`acquire_first_packet`]. On `Allowed`, the caller MUST
/// attach the returned guards to the freshly-built `UdpFlow` (via
/// `UdpFlow::attach_admit_guards`) so the v0.11
/// `concurrent_connections` cap remains enforced over the flow's
/// lifetime. Dropping the guards here (the original v014 Batch 3
/// regression) would let `udp_max_flows_per_rule` silently override
/// a tighter `concurrent_connections` cap.
enum AdmitOutcome {
    Allowed { guards: Vec<ActiveGuard> },
    Rejected,
}

/// FR-013 layered owner+rule rate-limit gate for the first packet of a
/// NEW UDP flow. Returns `Allowed { guards }` on admission, where
/// `guards` contains 0, 1, or 2 `ActiveGuard`s (one per capped scope).
/// The caller is responsible for moving the guards onto the new
/// `UdpFlow` so they ride the registry's Arc lifetime — this is the
/// v0.11 `concurrent_connections` enforcement seam under the
/// centralized-demux design (v0.4 used a per-flow task via
/// `spawn_admit_guard`; folding the guards into the flow itself avoids
/// that per-flow task without losing the lifetime tie).
///
/// On reject, bumps the scope's reject counter and emits
/// `rule.udp_first_packet_rejected`. No guard is leaked: the layered
/// gate releases the owner slot internally when the rule layer
/// rejects.
fn acquire_first_packet<R: Resolve + 'static>(
    cfg: &ListenerConfig<R>,
    src: SocketAddr,
) -> AdmitOutcome {
    match try_acquire_layered(cfg.owner_rate_limit.as_ref(), cfg.rate_limit.as_ref(), true) {
        LayeredAcquire::Granted {
            owner_guard,
            rule_guard,
        } => {
            let mut guards = Vec::with_capacity(2);
            if let Some(g) = owner_guard {
                guards.push(g);
            }
            if let Some(g) = rule_guard {
                guards.push(g);
            }
            AdmitOutcome::Allowed { guards }
        }
        LayeredAcquire::OwnerRejected(reason) => {
            if let Some(s) = cfg.owner_rate_limit_stats.as_deref() {
                s.record_reject(reason);
            }
            warn!(
                event = "rule.udp_first_packet_rejected",
                rule_id = %cfg.rule_id,
                listen_port = cfg.listen_port,
                source = %src,
                scope = "owner",
                reason = ?reason,
            );
            AdmitOutcome::Rejected
        }
        LayeredAcquire::RuleRejected(reason) => {
            if let Some(s) = cfg.rate_limit_stats.as_deref() {
                s.record_reject(reason);
            }
            warn!(
                event = "rule.udp_first_packet_rejected",
                rule_id = %cfg.rule_id,
                listen_port = cfg.listen_port,
                source = %src,
                scope = "rule",
                reason = ?reason,
            );
            AdmitOutcome::Rejected
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forwarder::rate_limit::scope::{RateLimitScopeManager, RuleRateLimitHandle};
    use crate::forwarder::stats::RuleStats;
    use crate::resolver::{LiveResolver, ResolveAnswer, ResolverConfig, ResolverError};
    use async_trait::async_trait;
    use portunus_core::{Hostname, PortRange, RateLimit};
    use std::net::Ipv4Addr;
    use std::time::Duration;

    /// Minimal `Resolve` impl: `Target::Ip(...)` short-circuits in
    /// `resolve_target` before touching the inner resolver, so this
    /// stub never gets called. It only exists to satisfy the generic
    /// bound on `LiveResolver<R>`.
    struct NoopResolver;

    #[async_trait]
    impl crate::resolver::Resolve for NoopResolver {
        async fn resolve(&self, _name: &Hostname) -> Result<ResolveAnswer, ResolverError> {
            Err(ResolverError::EmptyAnswer)
        }
    }

    fn rule_stats_for(port: u16) -> Arc<RuleStats> {
        RuleStats::for_range(PortRange::single(port))
    }

    async fn bind_loopback() -> (Arc<UdpSocket>, SocketAddr) {
        let s = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = s.local_addr().unwrap();
        (Arc::new(s), addr)
    }

    fn make_resolver() -> Arc<LiveResolver<NoopResolver>> {
        Arc::new(LiveResolver::new(
            Arc::new(NoopResolver),
            ResolverConfig::default(),
        ))
    }

    /// Phase 6 minimum: cancelling the listener token must let the
    /// loop exit promptly. The 100ms budget is generous —
    /// `recv_from` is selected against `cancel.cancelled()`, so the
    /// branch fires on the next runtime poll.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn listener_returns_on_cancel() {
        let (listener_sock, listen_addr) = bind_loopback().await;
        let registry = UdpFlowRegistry::new(4);
        let (demux_tx, _demux_rx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let cfg = ListenerConfig {
            rule_id: RuleId(1),
            listen_port: listen_addr.port(),
            target: Target::Ip(Ipv4Addr::LOCALHOST.into()),
            target_port: 1, // unused — we cancel before any datagram
            prefer_ipv6: false,
            idle_window: Duration::from_secs(30),
            registry,
            demux_tx,
            stats: rule_stats_for(listen_addr.port()),
            resolver: make_resolver(),
            rate_limit: None,
            rate_limit_stats: None,
            owner_rate_limit: None,
            owner_rate_limit_stats: None,
            quota: None,
            cancel: cancel.clone(),
        };
        let h = tokio::spawn(run_listener(cfg, listener_sock));
        cancel.cancel();
        tokio::time::timeout(Duration::from_millis(100), h)
            .await
            .expect("listener should exit within 100ms of cancel")
            .expect("join");
    }

    /// Phase 6 minimum: when the demux channel is closed (its receiver
    /// has been dropped), the cold path's `try_send(AddFlow{...})`
    /// must fail and the listener must roll the registry slot back —
    /// otherwise the demux can never observe the Live flow and
    /// replies would be lost forever (FR-005).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cold_path_addflow_channel_full_rolls_back() {
        // Listener socket — needed for the signature; we drive
        // `handle_datagram` directly so we don't actually `recv_from`.
        let (listener_sock, listen_addr) = bind_loopback().await;
        // Real target socket so connect() succeeds. We never read from
        // it; the first-packet send may or may not land — we only
        // care about the AddFlow path.
        let (_target_sock, target_addr) = bind_loopback().await;

        let registry = UdpFlowRegistry::new(4);
        // Channel with rx dropped immediately → every `try_send`
        // returns `TrySendError::Closed`.
        let (demux_tx, demux_rx) = mpsc::channel::<DemuxCommand>(1);
        drop(demux_rx);

        let cfg = ListenerConfig {
            rule_id: RuleId(42),
            listen_port: listen_addr.port(),
            target: Target::Ip(target_addr.ip()),
            target_port: target_addr.port(),
            prefer_ipv6: false,
            idle_window: Duration::from_secs(30),
            registry: Arc::clone(&registry),
            demux_tx,
            stats: rule_stats_for(listen_addr.port()),
            resolver: make_resolver(),
            rate_limit: None,
            rate_limit_stats: None,
            owner_rate_limit: None,
            owner_rate_limit_stats: None,
            quota: None,
            cancel: CancellationToken::new(),
        };

        let src: SocketAddr = "127.0.0.1:50001".parse().unwrap();
        handle_datagram(&cfg, &listener_sock, b"first", src).await;

        // The Pending reservation should have been committed (Live)
        // and then `remove`d on the AddFlow failure. Either way, no
        // Pending or Live slot must remain.
        assert!(
            registry.is_empty(),
            "AddFlow failure must roll back the registry slot; len = {}",
            registry.len()
        );
        // And `get` returns None for the key — defensive double-check.
        let key = FlowKey::new(listen_addr.port(), src);
        assert!(registry.get(key).await.is_none());
    }

    /// Regression: in v014 Batch 3 the new `acquire_first_packet`
    /// helper dropped the layered `ActiveGuard`s at the helper
    /// boundary, silently defeating the v0.11
    /// `concurrent_connections` cap for UDP (the registry's
    /// `udp_max_flows_per_rule` would be the only ceiling). The fix
    /// folds the guards into the `UdpFlow` Arc so they live for the
    /// flow's lifetime.
    ///
    /// This test drives `handle_datagram` directly with a per-rule
    /// limiter capped at 2 concurrent connections. The 3rd distinct
    /// source must be rejected at the rate-limit gate (no flow
    /// committed). After explicitly removing one flow from the
    /// registry, a 4th source must be admitted (the guard's `Drop`
    /// decremented `active_connections`).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_connections_cap_bounds_udp_flows() {
        let (listener_sock, listen_addr) = bind_loopback().await;
        // Real bound target so `connect()` in the cold path succeeds.
        let (_target_sock, target_addr) = bind_loopback().await;

        // Per-rule limiter with concurrent_connections = 2.
        let rule_id = RuleId(7);
        let scope = Arc::new(RateLimitScopeManager::new());
        scope.install(
            rule_id,
            Some(&RateLimit {
                concurrent_connections: Some(2),
                ..Default::default()
            }),
        );
        let rate_limit = Arc::new(RuleRateLimitHandle::new(rule_id, Arc::clone(&scope)));

        // Large registry cap so the *only* binding constraint is the
        // concurrent_connections gate — that's what we're validating.
        let registry = UdpFlowRegistry::new(64);
        // Channel deep enough that AddFlow always succeeds; keep the
        // receiver alive so `try_send` never returns Closed. We drain
        // it manually later so AddFlow-borne `Arc<UdpFlow>` clones drop
        // alongside the registry entry.
        let (demux_tx, mut demux_rx) = mpsc::channel::<DemuxCommand>(64);

        let cfg = ListenerConfig {
            rule_id,
            listen_port: listen_addr.port(),
            target: Target::Ip(target_addr.ip()),
            target_port: target_addr.port(),
            prefer_ipv6: false,
            idle_window: Duration::from_secs(30),
            registry: Arc::clone(&registry),
            demux_tx,
            stats: rule_stats_for(listen_addr.port()),
            resolver: make_resolver(),
            rate_limit: Some(Arc::clone(&rate_limit)),
            rate_limit_stats: None,
            owner_rate_limit: None,
            owner_rate_limit_stats: None,
            quota: None,
            cancel: CancellationToken::new(),
        };

        let src1: SocketAddr = "127.0.0.1:60001".parse().unwrap();
        let src2: SocketAddr = "127.0.0.1:60002".parse().unwrap();
        let src3: SocketAddr = "127.0.0.1:60003".parse().unwrap();
        let src4: SocketAddr = "127.0.0.1:60004".parse().unwrap();

        handle_datagram(&cfg, &listener_sock, b"d1", src1).await;
        handle_datagram(&cfg, &listener_sock, b"d2", src2).await;
        // Third should reject at the rate-limit gate; no flow commits.
        handle_datagram(&cfg, &listener_sock, b"d3", src3).await;
        // `Reservation::Drop` defers slot removal to a spawned task
        // (it can't await inline). Give that task time to run before
        // we inspect the registry. Two short sleeps + yields
        // suffice — the cleanup is a single mutex-lock + remove.
        tokio::time::sleep(Duration::from_millis(20)).await;
        tokio::task::yield_now().await;

        assert_eq!(
            registry.len(),
            2,
            "concurrent_connections=2 must bound live flows; got {}",
            registry.len()
        );
        assert_eq!(
            rate_limit.active_connections(),
            2,
            "active_connections must reflect 2 live guards",
        );
        let k3 = FlowKey::new(listen_addr.port(), src3);
        assert!(
            registry.get(k3).await.is_none(),
            "rejected flow must NOT appear in registry",
        );

        // Drain pending AddFlow envelopes BEFORE removing the flow.
        // In production the demux task consumes these instantly; the
        // test must do the same so the only Arc<UdpFlow> ref count
        // for src1 left in play is the registry's.
        while let Ok(cmd) = demux_rx.try_recv() {
            drop(cmd);
        }

        // Drop one flow → the guard inside `UdpFlow.admit_guards`
        // drops with the Arc, decrementing `active_connections`.
        let k1 = FlowKey::new(listen_addr.port(), src1);
        let removed = registry
            .remove(k1)
            .await
            .expect("flow should exist before remove");
        drop(removed);

        // Small yield to let any deferred Drop side effects settle.
        tokio::task::yield_now().await;

        assert_eq!(
            rate_limit.active_connections(),
            1,
            "dropping a flow must release one ActiveGuard",
        );

        // 4th source must now be admitted.
        handle_datagram(&cfg, &listener_sock, b"d4", src4).await;

        let k4 = FlowKey::new(listen_addr.port(), src4);
        assert!(
            registry.get(k4).await.is_some(),
            "4th source must be admitted after a slot frees up",
        );
        assert_eq!(
            rate_limit.active_connections(),
            2,
            "active_connections must be back at the 2-cap ceiling",
        );
    }
}

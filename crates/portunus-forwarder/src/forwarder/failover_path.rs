//! Multi-target TCP forwarder run path
//! (007-multi-target-failover, US1 + T022 + T023 + T025).
//!
//! Entered from `forwarder::run` ONLY when `rule.targets` is non-
//! empty. Single-target rules never reach this module — they stay on
//! the byte-identical v0.6.0 hot path in `mod.rs` + `proxy.rs`
//! (Constitution Principle II).
//!
//! The data path: bind a listener per port in `rule.listen_range`
//! (typically one for multi-target rules), accept connections, and
//! per accept walk `rule.targets` in priority order until one
//! connects. Each connect attempt feeds the per-target `HealthState`;
//! the resulting health transitions increment `target_failovers_total`
//! (FR-010) and emit structured `event = "rule.target.health_changed"`
//! log lines.
//!
//! Per-target byte / connection counters are wired in Phase 5 (T034).
//! UDP multi-target failover (T024) lives in a sibling module under
//! `forwarder::udp`.

#![allow(clippy::similar_names)]

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant, SystemTime};

use portunus_core::RuleId;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::failover::{self, HealthState};
use super::probe;
use super::proxy_protocol::{self, ProxyProtocolPrelude};
use super::range::{self, BindFailure};
use super::stats::RuleStats;
use super::udp;
use super::{ClientRule, MultiTarget, RuleStatusEvent, drain};
use crate::resolver::{LiveResolver, Resolve};

/// Multi-target TCP entry. Mirrors `forwarder::run`'s lifecycle:
/// emits exactly one `Activated|Failed` on startup and exactly one
/// `Removed` on teardown (only after a successful Activated).
pub async fn run_tcp<R: Resolve + 'static>(
    rule: ClientRule,
    resolver: Arc<LiveResolver<R>>,
    status_tx: mpsc::Sender<RuleStatusEvent>,
    cancel: CancellationToken,
    drain_timeout: Duration,
    stats: Arc<RuleStats>,
) {
    debug_assert!(
        !rule.targets.is_empty(),
        "failover_path entered for single-target rule — would violate \
         Constitution Principle II"
    );

    let listeners = match range::bind_all(&rule.listen_range) {
        Ok(v) => v,
        Err(BindFailure {
            offending_port,
            reason,
        }) => {
            stats.errors.inc_port_in_use();
            warn!(
                event = "rule.failed",
                rule_id = %rule.rule_id,
                listen_port = rule.listen_range.start(),
                listen_port_end = rule.listen_range.end(),
                offending_port = offending_port,
                reason = reason,
                multi_target = true,
            );
            let reason_str = if rule.listen_range.len() == 1 {
                reason.to_string()
            } else {
                format!("{reason}:{offending_port}")
            };
            let _ = status_tx
                .send(RuleStatusEvent::Failed {
                    rule_id: rule.rule_id,
                    reason: reason_str,
                })
                .await;
            return;
        }
    };

    info!(
        event = "rule.activated",
        rule_id = %rule.rule_id,
        listen_port = rule.listen_range.start(),
        listen_port_end = rule.listen_range.end(),
        target_count = rule.targets.len(),
        targets = ?rule.targets.iter().map(|t| format!("{}:{}", t.spec.host, t.spec.port)).collect::<Vec<_>>(),
    );
    if status_tx
        .send(RuleStatusEvent::Activated {
            rule_id: rule.rule_id,
        })
        .await
        .is_err()
    {
        return;
    }

    // T033: per-target health + failover counter come from the
    // control loop's pre-built observability handle so the periodic
    // StatsReport tick can read the same state we mutate.
    let obs = rule
        .multi_target_obs
        .as_ref()
        .expect("failover_path::run_tcp requires multi_target_obs to be set")
        .clone();
    let states = Arc::clone(&obs.states);
    let target_failovers_total = Arc::clone(&obs.target_failovers_total);
    debug_assert_eq!(states.len(), rule.targets.len());

    let proxy_cancel = CancellationToken::new();
    let mut in_flight: JoinSet<()> = JoinSet::new();

    // T029: opt-in active TCP-connect prober. Spawned per-rule, drained
    // by the same `cancel` that drives the accept loops. The prober
    // task and the data path share the per-target HealthState mutexes
    // so passive + active signals merge into a single health view.
    let probe_handle = if let Some(interval) = rule.health_check_interval_secs {
        let targets_arc = Arc::new(rule.targets.clone());
        Some(probe::spawn(
            rule.rule_id,
            targets_arc,
            Arc::clone(&states),
            Arc::clone(&target_failovers_total),
            rule.prefer_ipv6,
            interval,
            Arc::clone(&resolver),
            cancel.clone(),
        ))
    } else {
        None
    };

    for (listen_port, listener) in listeners {
        let accept_cancel = cancel.clone();
        let conn_proxy_cancel = proxy_cancel.clone();
        let accept_resolver = Arc::clone(&resolver);
        let accept_targets = rule.targets.clone();
        let accept_states = Arc::clone(&states);
        let accept_counter = Arc::clone(&target_failovers_total);
        let accept_stats = Arc::clone(&stats);
        // 011-rate-limiting-qos T019/T030: thread per-rule and per-
        // owner limiters + accumulators into each multi-target accept
        // loop. Owner layer (FR-013) is consulted before per-rule.
        // Same shape as the legacy single-target path so a v0.7+
        // multi-target rule and a v0.6 single-target rule observe
        // identical gate semantics.
        let accept_rate_limiter = rule.rate_limit.clone();
        let accept_rate_stats = rule.rate_limit_stats.clone();
        let accept_owner_limiter = rule.owner_rate_limit.clone();
        let accept_owner_stats = rule.owner_rate_limit_stats.clone();
        // 013-traffic-quotas: thread the per-(user, client) byte budget
        // into each multi-target accept loop so failover TCP rules debit
        // the quota the same way the single-target path does.
        let accept_quota = rule.quota.clone();
        let rule_id = rule.rule_id;
        let prefer_ipv6 = rule.prefer_ipv6;
        in_flight.spawn(async move {
            accept_loop(
                listener,
                listen_port,
                rule_id,
                accept_resolver,
                accept_targets,
                accept_states,
                accept_counter,
                prefer_ipv6,
                accept_cancel,
                conn_proxy_cancel,
                accept_stats,
                accept_rate_limiter,
                accept_rate_stats,
                accept_owner_limiter,
                accept_owner_stats,
                accept_quota,
            )
            .await;
        });
    }

    cancel.cancelled().await;
    drain(in_flight, proxy_cancel, drain_timeout).await;
    if let Some(h) = probe_handle {
        h.abort();
    }

    info!(
        event = "rule.removed",
        rule_id = %rule.rule_id,
        multi_target = true,
        target_failovers_total = target_failovers_total.load(std::sync::atomic::Ordering::Relaxed),
    );
    let _ = status_tx
        .send(RuleStatusEvent::Removed {
            rule_id: rule.rule_id,
        })
        .await;
}

#[allow(clippy::too_many_arguments)]
async fn accept_loop<R: Resolve + 'static>(
    listener: TcpListener,
    listen_port: u16,
    rule_id: RuleId,
    resolver: Arc<LiveResolver<R>>,
    targets: Vec<MultiTarget>,
    states: Arc<Vec<tokio::sync::Mutex<HealthState>>>,
    target_failovers_total: Arc<AtomicU64>,
    prefer_ipv6: bool,
    cancel: CancellationToken,
    proxy_cancel: CancellationToken,
    stats: Arc<RuleStats>,
    // 011-rate-limiting-qos T019: per-rule cap envelope. None keeps
    // the byte-identical v0.7 path.
    rate_limiter: Option<Arc<crate::forwarder::rate_limit::scope::RuleRateLimitHandle>>,
    rate_limit_stats: Option<Arc<crate::forwarder::rate_limit::stats::RateLimitStatsAccumulator>>,
    // 011-rate-limiting-qos T030: per-owner cap envelope. Consulted
    // BEFORE the per-rule layer (FR-013) and emits owner-prefixed
    // reject reasons (FR-014).
    owner_rate_limiter: Option<Arc<crate::forwarder::rate_limit::scope::OwnerRateLimitHandle>>,
    owner_rate_limit_stats: Option<
        Arc<crate::forwarder::rate_limit::stats::RateLimitStatsAccumulator>,
    >,
    // 013-traffic-quotas: per-(user, client) byte budget, threaded
    // through to `handle_connection` so both copy branches debit it.
    quota: Option<Arc<crate::forwarder::quota::QuotaHandle>>,
) {
    use crate::forwarder::rate_limit::scope::{LayeredAcquire, try_acquire_layered};
    let mut local: JoinSet<()> = JoinSet::new();
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            joined = local.join_next(), if !local.is_empty() => {
                let _ = joined;
            }
            accept = listener.accept() => match accept {
                Ok((sock, peer)) => {
                    // 011-rate-limiting-qos T019/T030: layered gate runs
                    // BEFORE multi-target selection (FR-010 + FR-013).
                    // Surplus accepts at either layer get accept-then-
                    // RST: the socket drops here and the OS sends RST.
                    let (owner_admit, rule_admit) = match try_acquire_layered(
                        owner_rate_limiter.as_ref(),
                        rate_limiter.as_ref(),
                        false,
                    ) {
                        LayeredAcquire::Granted { owner_guard, rule_guard } => {
                            (owner_guard, rule_guard)
                        }
                        LayeredAcquire::OwnerRejected(reason) => {
                            if let Some(s) = owner_rate_limit_stats.as_ref() {
                                s.record_reject(reason);
                            }
                            tracing::debug!(
                                event = "rule.rate_limit_reject",
                                rule_id = %rule_id,
                                listen_port = listen_port,
                                peer = %peer,
                                scope = "owner",
                                reason = reason.as_metric_label(),
                                multi_target = true,
                            );
                            drop(sock);
                            continue;
                        }
                        LayeredAcquire::RuleRejected(reason) => {
                            if let Some(s) = rate_limit_stats.as_ref() {
                                s.record_reject(reason);
                            }
                            tracing::debug!(
                                event = "rule.rate_limit_reject",
                                rule_id = %rule_id,
                                listen_port = listen_port,
                                peer = %peer,
                                scope = "rule",
                                reason = reason.as_metric_label(),
                                multi_target = true,
                            );
                            drop(sock);
                            continue;
                        }
                    };

                    stats.inc_connection();
                    let conn_cancel = proxy_cancel.clone();
                    let conn_resolver = Arc::clone(&resolver);
                    let conn_targets = targets.clone();
                    let conn_states = Arc::clone(&states);
                    let conn_counter = Arc::clone(&target_failovers_total);
                    let conn_stats = Arc::clone(&stats);
                    let conn_rate_limiter = rate_limiter.clone();
                    let conn_rate_stats = rate_limit_stats.clone();
                    let conn_owner_limiter = owner_rate_limiter.clone();
                    let conn_owner_stats = owner_rate_limit_stats.clone();
                    let conn_quota = quota.clone();
                    local.spawn(async move {
                        let admit_guards = (owner_admit, rule_admit);
                        handle_connection(
                            sock,
                            peer,
                            listen_port,
                            rule_id,
                            conn_resolver.as_ref(),
                            &conn_targets,
                            &conn_states,
                            &conn_counter,
                            prefer_ipv6,
                            conn_cancel,
                            conn_stats,
                            conn_rate_limiter,
                            conn_rate_stats,
                            conn_owner_limiter,
                            conn_owner_stats,
                            conn_quota,
                        )
                        .await;
                        drop(admit_guards);
                    });
                }
                Err(e) => {
                    warn!(
                        event = "rule.accept_error",
                        rule_id = %rule_id,
                        listen_port = listen_port,
                        multi_target = true,
                        error = %e,
                    );
                    // Brief backoff so a persistent error (e.g. EMFILE
                    // when the fd table is exhausted) doesn't spin the
                    // accept loop at 100% CPU with a log flood. Mirrors
                    // the SNI listener's accept-error pattern.
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            }
        }
    }

    drop(listener);
    while local.join_next().await.is_some() {}
}

#[allow(clippy::too_many_arguments)]
async fn handle_connection<R: Resolve>(
    mut inbound: TcpStream,
    peer: std::net::SocketAddr,
    listen_port: u16,
    rule_id: RuleId,
    resolver: &LiveResolver<R>,
    targets: &[MultiTarget],
    states: &[tokio::sync::Mutex<HealthState>],
    target_failovers_total: &AtomicU64,
    prefer_ipv6: bool,
    shutdown: CancellationToken,
    stats: Arc<RuleStats>,
    // 011-rate-limiting-qos T020: optional bandwidth limiter +
    // accumulator. None for uncapped or
    // connection-only-capped rules — the multi-target path keeps the
    // byte-stable v0.7 `tokio::io::copy_bidirectional` behaviour.
    rate_limit: Option<Arc<crate::forwarder::rate_limit::scope::RuleRateLimitHandle>>,
    rate_limit_stats: Option<Arc<crate::forwarder::rate_limit::stats::RateLimitStatsAccumulator>>,
    // 011-rate-limiting-qos T030: per-owner bandwidth limiter +
    // accumulator. None when the owner has no bandwidth caps.
    owner_rate_limit: Option<Arc<crate::forwarder::rate_limit::scope::OwnerRateLimitHandle>>,
    owner_rate_limit_stats: Option<
        Arc<crate::forwarder::rate_limit::stats::RateLimitStatsAccumulator>,
    >,
    // 013-traffic-quotas: per-(user, client) byte budget. Wired into
    // BOTH the rate-limited and uncapped copy branches below so a
    // multi-target TCP rule debits its quota like the single-target path.
    quota: Option<Arc<crate::forwarder::quota::QuotaHandle>>,
) {
    let Ok(local_addr) = inbound.local_addr() else {
        warn!(
            event = "rule.conn_error",
            rule_id = %rule_id,
            listen_port = listen_port,
            peer = %peer,
            error = "local_addr_unavailable",
            multi_target = true,
        );
        let _ = inbound.shutdown().await;
        return;
    };
    let outbound_result = dial_with_failover(
        rule_id,
        resolver,
        targets,
        states,
        target_failovers_total,
        prefer_ipv6,
        peer,
        local_addr,
    )
    .await;
    let Some((mut outbound, idx)) = outbound_result else {
        // FR-007: even with all-Failed targets we still attempted
        // index 0 — the connect failed too. Drop the inbound socket
        // cleanly and let the operator's per-rule connect-failure
        // metric surface the issue.
        warn!(
            event = "rule.conn_error_all_targets_failed",
            rule_id = %rule_id,
            listen_port = listen_port,
            peer = %peer,
            target_count = targets.len(),
        );
        let _ = inbound.shutdown().await;
        return;
    };
    // Per-target connection counter (Phase 5 wires this into
    // PerTargetStats — the inc happens here so the order-of-magnitude
    // reflects the chosen-target stickiness).
    states[idx].lock().await.increment_connections_accepted();

    // Disable Nagle on both halves — same trade-off as proxy.rs.
    let _ = inbound.set_nodelay(true);
    let _ = outbound.set_nodelay(true);

    let _guard = ActiveGuard::new(Arc::clone(&stats), listen_port);

    // 011-rate-limiting-qos T020/T030: throttling fork fires when
    // EITHER per-rule or per-owner has a bandwidth cap. Uncapped
    // rules with no owner caps keep the byte-stable v0.7 path.
    let rule_has_bw = rate_limit.as_ref().is_some_and(|l| l.has_bandwidth_cap());
    let owner_has_bw = owner_rate_limit
        .as_ref()
        .is_some_and(|l| l.has_bandwidth_cap());
    // 016-failover-splice-live-bytes: the uncapped branch reuses the
    // single-target `copy_uncapped` (splice fast path + live sink) so
    // multi-target rules get zero-copy AND real-time `bytes_in/out`
    // updates instead of a frozen gauge until connection close. The
    // sink stays at zero on the capped branch / userspace fallback, so
    // the post-copy batch record below remains byte-identical.
    let live_sink = crate::forwarder::stats::LiveBytesSink::new(Arc::clone(&stats), listen_port);
    let result = tokio::select! {
        () = shutdown.cancelled() => {
            Err(io::Error::other("proxy_cancelled"))
        }
        result = async {
            if rule_has_bw || owner_has_bw {
                let rule_for_copy = rate_limit.clone().unwrap_or_else(|| {
                    let scope =
                        Arc::new(crate::forwarder::rate_limit::scope::RateLimitScopeManager::new());
                    scope.install(
                        rule_id,
                        Some(&portunus_core::RateLimit::default()),
                    );
                    Arc::new(
                        crate::forwarder::rate_limit::scope::RuleRateLimitHandle::new(
                            rule_id, scope,
                        ),
                    )
                });
                crate::forwarder::rate_limit::copy::copy_bidirectional_with_rate_limit(
                    &mut inbound,
                    &mut outbound,
                    rule_for_copy,
                    rate_limit_stats.clone(),
                    owner_rate_limit.clone(),
                    owner_rate_limit_stats.clone(),
                    // 013-traffic-quotas: the rate-limited failover branch
                    // must debit the budget too — otherwise a multi-target
                    // rule with a bandwidth cap bypasses its quota.
                    quota.clone(),
                )
                .await
            } else {
                // 013-traffic-quotas: thread the rule's quota into the
                // uncapped failover copy path so a multi-target TCP rule
                // debits its budget exactly like the single-target path —
                // copy_uncapped routes it through splice or the userspace
                // quota-aware copy.
                // preread / had_proxy_prelude: no SNI preread here, and the
                // per-target PROXY prelude was already written in
                // `dial_with_failover`; the flag is a tracing-only CopyCtx field.
                crate::forwarder::proxy::copy_uncapped(
                    &mut inbound,
                    &mut outbound,
                    rule_id,
                    rate_limit.as_deref(),
                    owner_rate_limit.as_deref(),
                    quota.as_ref(),
                    false,
                    false,
                    Some(&live_sink),
                )
                .await
            }
        } => {
            result
        }
    };
    match result {
        Ok((bin, bout)) => {
            // Book whatever the live sink did not already flush per-chunk;
            // the capped branch / userspace fallback leave it at zero so the
            // full amount is recorded, byte-identical to the pre-live path.
            live_sink.record_remaining(bin, bout);
            // T034: per-target byte accumulation. Same atomicity as the
            // global counters — `add_bytes_in/out` use Relaxed adds.
            let state = states[idx].lock().await;
            state.add_bytes_in(bin);
            state.add_bytes_out(bout);
            info!(
                event = "rule.conn_closed",
                rule_id = %rule_id,
                listen_port = listen_port,
                peer = %peer,
                bytes_in = bin,
                bytes_out = bout,
                multi_target = true,
            );
        }
        Err(e) => {
            // On a mid-stream error the live sink already pushed the
            // transferred bytes into the global RuleStats per chunk (zero on
            // the capped / userspace-fallback paths). Mirror that same amount
            // into the per-target counters so the global rule total and the
            // sum of per-target totals stay consistent for errored
            // connections.
            let (live_in, live_out) = live_sink.snapshot_recorded();
            if live_in > 0 || live_out > 0 {
                let state = states[idx].lock().await;
                state.add_bytes_in(live_in);
                state.add_bytes_out(live_out);
            }
            warn!(
                event = "rule.conn_error",
                rule_id = %rule_id,
                listen_port = listen_port,
                peer = %peer,
                error = %e,
                multi_target = true,
            );
        }
    }
}

/// Walk `targets` in priority order (caller pre-sorts) — for each
/// candidate, attempt `resolver.connect_target`. On failure, mark
/// that target's HealthState as failed and try the next. On success,
/// mark that target's HealthState as success and return the
/// connected stream + the chosen index.
///
/// Returns `None` only when EVERY target failed to connect. The
/// caller surfaces this as a connection-failure to the inbound peer
/// (FR-007 — never silently drop; the failure IS the signal).
///
/// The selection function (`failover::select`) determines the FIRST
/// candidate. After that we walk the remaining targets in priority
/// order skipping the one we already tried — this preserves the
/// "highest-priority gets attempted first" guarantee and naturally
/// handles the "all-Failed → still attempt index 0" fallback (the
/// selector returns 0 in that case, and we walk 1..n if 0 fails).
#[allow(clippy::too_many_arguments)]
async fn dial_with_failover<R: Resolve>(
    rule_id: RuleId,
    resolver: &LiveResolver<R>,
    targets: &[MultiTarget],
    states: &[tokio::sync::Mutex<HealthState>],
    target_failovers_total: &AtomicU64,
    prefer_ipv6: bool,
    downstream_peer: SocketAddr,
    downstream_local: SocketAddr,
) -> Option<(TcpStream, usize)> {
    debug_assert_eq!(targets.len(), states.len());

    // Build the dial order: failover::select picks the first
    // candidate by health, then we fall through the remaining
    // targets in priority/row order.
    let order: Vec<usize> = {
        // Snapshot health for selection (don't hold locks across
        // awaits during the actual dials).
        let mut snap: Vec<failover::Health> = Vec::with_capacity(states.len());
        for s in states {
            snap.push(s.lock().await.health());
        }
        // Inline priority+health select: walk by Health first, then
        // index. Identical semantics to `failover::select` but we
        // also need the fallback list, so we sort.
        let first = snap
            .iter()
            .position(|h| matches!(h, failover::Health::Healthy))
            .unwrap_or(0);
        let mut rest: Vec<usize> = (0..targets.len()).filter(|&i| i != first).collect();
        // Stable: row index preserves priority sort the caller
        // already established.
        rest.sort_unstable();
        let mut out = Vec::with_capacity(targets.len());
        out.push(first);
        out.extend(rest);
        out
    };

    for &idx in &order {
        let candidate = &targets[idx];
        let dial = resolver
            .connect_target(rule_id, &candidate.target, candidate.spec.port, prefer_ipv6)
            .await;
        let now = Instant::now();
        let wall = SystemTime::now();
        match dial {
            Ok((mut sock, _source)) => {
                if let Some(mode) = candidate.spec.proxy_protocol
                    && let Err(error) = write_proxy_protocol_prelude(
                        &mut sock,
                        mode,
                        downstream_peer,
                        downstream_local,
                    )
                    .await
                {
                    states[idx]
                        .lock()
                        .await
                        .record_failure(now, wall, target_failovers_total);
                    warn!(
                        event = "rule.target.proxy_protocol_write_failed",
                        rule_id = %rule_id,
                        target_index = idx,
                        target_host = %candidate.spec.host,
                        target_port = candidate.spec.port,
                        proxy_protocol = ?mode,
                        error = %error,
                    );
                    continue;
                }
                states[idx]
                    .lock()
                    .await
                    .record_success(now, wall, target_failovers_total);
                return Some((sock, idx));
            }
            Err(e) => {
                // Treat any dial failure (resolver, all-addrs-
                // unreachable, plain dial error) as a connect
                // failure for this target's health. T025 — DNS
                // resolution failure counts as a connect failure
                // for health attribution.
                states[idx]
                    .lock()
                    .await
                    .record_failure(now, wall, target_failovers_total);
                warn!(
                    event = "rule.target.dial_failed",
                    rule_id = %rule_id,
                    target_index = idx,
                    target_host = %candidate.spec.host,
                    target_port = candidate.spec.port,
                    error = %e,
                );
            }
        }
    }
    None
}

async fn write_proxy_protocol_prelude(
    outbound: &mut TcpStream,
    version: portunus_core::ProxyProtocolVersion,
    source: SocketAddr,
    destination: SocketAddr,
) -> io::Result<()> {
    proxy_protocol::write_prelude(
        outbound,
        ProxyProtocolPrelude {
            version,
            source,
            destination,
        },
    )
    .await
}

/// 007-multi-target-failover (T024): multi-target UDP entry point.
///
/// Mirrors `forwarder::run_udp` lifecycle (probe-bind every port, emit
/// Activated, spawn one listener per port, then drain on cancel) but
/// dispatches to `udp::run_listener_multi_target` so each new flow's
/// first packet drives a per-target select. UDP failover applies to
/// NEW flows only — once a flow is bound to a target, subsequent
/// packets stick (FR-012).
pub async fn run_udp<R: Resolve + 'static>(
    rule: ClientRule,
    resolver: Arc<LiveResolver<R>>,
    status_tx: mpsc::Sender<RuleStatusEvent>,
    cancel: CancellationToken,
    stats: Arc<RuleStats>,
) {
    debug_assert!(
        !rule.targets.is_empty(),
        "failover_path::run_udp entered for single-target rule"
    );

    let listen_start = rule.listen_range.start();
    let listen_end = rule.listen_range.end();
    let range_size = rule.listen_range.len();

    // Probe-bind every port in the range so a partial-success surfaces
    // atomically (mirrors run_udp).
    let mut probes = Vec::with_capacity(range_size as usize);
    for port in listen_start..=listen_end {
        match tokio::net::UdpSocket::bind(("0.0.0.0", port)).await {
            Ok(p) => probes.push(p),
            Err(e) => {
                let reason = if range_size == 1 {
                    "port_in_use".to_string()
                } else {
                    format!("port_in_use:{port}")
                };
                stats.errors.inc_port_in_use();
                warn!(
                    event = "rule.failed",
                    rule_id = %rule.rule_id,
                    listen_port = port,
                    multi_target = true,
                    reason = %reason,
                    error = %e,
                );
                let _ = status_tx
                    .send(RuleStatusEvent::Failed {
                        rule_id: rule.rule_id,
                        reason,
                    })
                    .await;
                return;
            }
        }
    }
    drop(probes);

    info!(
        event = "rule.activated",
        rule_id = %rule.rule_id,
        listen_port = listen_start,
        listen_port_end = listen_end,
        range_size = range_size,
        protocol = "udp",
        target_count = rule.targets.len(),
        multi_target = true,
    );
    if status_tx
        .send(RuleStatusEvent::Activated {
            rule_id: rule.rule_id,
        })
        .await
        .is_err()
    {
        return;
    }

    let cap = super::resolve_udp_cap(rule.udp_max_flows);
    let idle_window = super::resolve_udp_idle_window(rule.udp_flow_idle_secs);

    let obs = rule
        .multi_target_obs
        .as_ref()
        .expect("failover_path::run_udp requires multi_target_obs to be set")
        .clone();
    let states = Arc::clone(&obs.states);
    let target_failovers_total = Arc::clone(&obs.target_failovers_total);
    let targets = Arc::new(rule.targets.clone());

    let probe_handle = if let Some(interval) = rule.health_check_interval_secs {
        Some(probe::spawn(
            rule.rule_id,
            Arc::clone(&targets),
            Arc::clone(&states),
            Arc::clone(&target_failovers_total),
            rule.prefer_ipv6,
            interval,
            Arc::clone(&resolver),
            cancel.clone(),
        ))
    } else {
        None
    };

    let mut tasks: JoinSet<()> = JoinSet::new();
    for listen_port in listen_start..=listen_end {
        let rule_id = rule.rule_id;
        let prefer_ipv6 = rule.prefer_ipv6;
        let task_stats = Arc::clone(&stats);
        let task_resolver = Arc::clone(&resolver);
        let task_cancel = cancel.clone();
        let task_targets = Arc::clone(&targets);
        let task_states = Arc::clone(&states);
        let task_counter = Arc::clone(&target_failovers_total);
        let task_rate_limit = rule.rate_limit.clone();
        let task_rate_limit_stats = rule.rate_limit_stats.clone();
        let task_owner_rate_limit = rule.owner_rate_limit.clone();
        let task_owner_rate_limit_stats = rule.owner_rate_limit_stats.clone();
        let task_quota = rule.quota.clone();
        tasks.spawn(async move {
            udp::run_listener_multi_target(
                rule_id,
                listen_port,
                task_targets,
                task_states,
                task_counter,
                prefer_ipv6,
                cap,
                idle_window,
                task_stats,
                task_resolver,
                task_cancel,
                task_rate_limit,
                task_rate_limit_stats,
                task_owner_rate_limit,
                task_owner_rate_limit_stats,
                task_quota,
            )
            .await;
        });
    }

    while tasks.join_next().await.is_some() {}
    if let Some(h) = probe_handle {
        h.abort();
    }

    info!(
        event = "rule.removed",
        rule_id = %rule.rule_id,
        multi_target = true,
        target_failovers_total = target_failovers_total.load(std::sync::atomic::Ordering::Relaxed),
    );
    let _ = status_tx
        .send(RuleStatusEvent::Removed {
            rule_id: rule.rule_id,
        })
        .await;
}

/// RAII guard for `stats.active_connections` parallel to proxy::proxy's.
struct ActiveGuard {
    stats: Arc<RuleStats>,
    listen_port: u16,
}

impl ActiveGuard {
    fn new(stats: Arc<RuleStats>, listen_port: u16) -> Self {
        stats.inc_active(listen_port);
        Self { stats, listen_port }
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.stats.dec_active(self.listen_port);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forwarder::quota::{QuotaHandle, QuotaState};
    use crate::resolver::{ResolveAnswer, ResolverConfig, ResolverError};
    use portunus_core::{Hostname, Target};
    use std::net::Ipv4Addr;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::Mutex;

    /// IP targets never resolve — any call is a wiring bug.
    #[derive(Debug, Default)]
    struct PanickingResolver;

    #[async_trait::async_trait]
    impl Resolve for PanickingResolver {
        async fn resolve(&self, name: &Hostname) -> Result<ResolveAnswer, ResolverError> {
            panic!("PanickingResolver::resolve was called for {name}");
        }
    }

    fn ip_resolver() -> LiveResolver<PanickingResolver> {
        LiveResolver::new(Arc::new(PanickingResolver), ResolverConfig::default())
    }

    async fn spawn_echo() -> SocketAddr {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    while let Ok(n) = sock.read(&mut buf).await {
                        if n == 0 || sock.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        addr
    }

    /// Drive `push` bytes through a single-rule multi-target
    /// `handle_connection` whose owner holds a `budget`-byte quota.
    /// Returns `(quota_exhausted, bytes_echoed_back)`.
    async fn drive_failover_quota(
        rate_limit: Option<Arc<crate::forwarder::rate_limit::scope::RuleRateLimitHandle>>,
        budget: i64,
        push: usize,
    ) -> (bool, usize) {
        let echo = spawn_echo().await;
        let front = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let front_addr = front.local_addr().unwrap();

        let quota = Arc::new(QuotaHandle::new(
            "alice".into(),
            "edge-01".into(),
            QuotaState {
                monthly_bytes: 1_000_000,
                budget_remaining_bytes: budget,
                exhausted: false,
            },
        ));
        let cancel = CancellationToken::new();
        let q = Arc::clone(&quota);

        let hc = tokio::spawn(async move {
            let resolver = ip_resolver();
            let targets = vec![MultiTarget {
                spec: portunus_core::RuleTarget {
                    host: echo.ip().to_string(),
                    port: echo.port(),
                    priority: 0,
                    proxy_protocol: None,
                },
                target: Target::Ip(echo.ip()),
            }];
            let states = vec![Mutex::new(HealthState::new())];
            let counter = AtomicU64::new(0);
            let stats = RuleStats::new();
            let (inbound, peer) = front.accept().await.unwrap();
            handle_connection(
                inbound,
                peer,
                0,
                RuleId(0),
                &resolver,
                &targets,
                &states,
                &counter,
                false,
                cancel,
                stats,
                rate_limit,
                None,
                None,
                None,
                Some(q),
            )
            .await;
        });

        let client = TcpStream::connect(front_addr).await.unwrap();
        let (mut crd, mut cwr) = client.into_split();
        let writer = tokio::spawn(async move {
            let payload = vec![0xAB_u8; push];
            // write_all may fail once the proxy half-closes on quota — fine.
            let _ = cwr.write_all(&payload).await;
            let _ = cwr.shutdown().await;
        });
        let reader = tokio::spawn(async move {
            let mut sink = Vec::new();
            let _ = crd.read_to_end(&mut sink).await;
            sink.len()
        });

        let _ = writer.await;
        let echoed = reader.await.unwrap();
        let _ = hc.await;
        (quota.is_exhausted(), echoed)
    }

    /// 013-traffic-quotas: a multi-target TCP rule must debit its quota
    /// on the UNCAPPED copy branch. Before the fix the failover path
    /// passed `None` for quota (the self-admitting "not threaded into the
    /// failover copy path" comment), so converting a single-target rule
    /// to multi-target silently disabled TCP quota enforcement.
    #[tokio::test]
    async fn multi_target_uncapped_debits_quota() {
        let (exhausted, echoed) = drive_failover_quota(None, 64 * 1024, 256 * 1024).await;
        assert!(
            exhausted,
            "multi-target uncapped path must debit the quota to exhaustion"
        );
        assert!(
            echoed < 256 * 1024,
            "quota cutoff must truncate the echoed stream (got {echoed} bytes)"
        );
    }

    /// 013-traffic-quotas: the OTHER failover branch — a multi-target
    /// rule that ALSO carries a bandwidth cap routes through
    /// `copy_bidirectional_with_rate_limit`, which must debit the quota
    /// too. Otherwise a multi-target rule with a bandwidth cap bypasses
    /// its budget on both counts.
    #[tokio::test]
    async fn multi_target_rate_limited_debits_quota() {
        // High bandwidth cap forces the rate-limited branch without
        // actually throttling this small transfer.
        let scope = Arc::new(crate::forwarder::rate_limit::scope::RateLimitScopeManager::new());
        scope.install(
            RuleId(0),
            Some(&portunus_core::RateLimit {
                bandwidth_in_bps: Some(1024 * 1024 * 1024),
                bandwidth_out_bps: Some(1024 * 1024 * 1024),
                ..Default::default()
            }),
        );
        let limiter = Arc::new(
            crate::forwarder::rate_limit::scope::RuleRateLimitHandle::new(RuleId(0), scope),
        );

        let (exhausted, echoed) = drive_failover_quota(Some(limiter), 64 * 1024, 256 * 1024).await;
        assert!(
            exhausted,
            "multi-target rate-limited path must debit the quota to exhaustion"
        );
        assert!(
            echoed < 256 * 1024,
            "quota cutoff must truncate the echoed stream (got {echoed} bytes)"
        );
    }
}

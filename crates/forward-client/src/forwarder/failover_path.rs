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
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant, SystemTime};

use forward_core::RuleId;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use super::failover::{self, HealthState};
use super::probe;
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

    let listeners = match range::bind_all(&rule.listen_range).await {
        Ok(v) => v,
        Err(BindFailure {
            offending_port,
            reason,
        }) => {
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

    // Per-target health state, parallel to `rule.targets`. Wrapped in
    // `Arc<Mutex<>>` so both the accept loop and the (eventual) active
    // probe task can mutate them. `tokio::sync::Mutex` not `std`'s —
    // we hold across awaits inside the dial loop.
    let states: Arc<Vec<tokio::sync::Mutex<HealthState>>> = Arc::new(
        (0..rule.targets.len())
            .map(|_| tokio::sync::Mutex::new(HealthState::new()))
            .collect(),
    );
    let target_failovers_total = Arc::new(AtomicU64::new(0));

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
            )
            .await;
        });
    }

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
) {
    let mut local: JoinSet<()> = JoinSet::new();
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            joined = local.join_next(), if !local.is_empty() => {
                let _ = joined;
            }
            accept = listener.accept() => match accept {
                Ok((sock, peer)) => {
                    let conn_cancel = proxy_cancel.clone();
                    let conn_resolver = Arc::clone(&resolver);
                    let conn_targets = targets.clone();
                    let conn_states = Arc::clone(&states);
                    let conn_counter = Arc::clone(&target_failovers_total);
                    let conn_stats = Arc::clone(&stats);
                    local.spawn(async move {
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
                        )
                        .await;
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
                }
            }
        }
    }
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
) {
    let outbound_result = dial_with_failover(
        rule_id,
        resolver,
        targets,
        states,
        target_failovers_total,
        prefer_ipv6,
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
    states[idx]
        .lock()
        .await
        .increment_connections_accepted();

    // Disable Nagle on both halves — same trade-off as proxy.rs.
    let _ = inbound.set_nodelay(true);
    let _ = outbound.set_nodelay(true);

    let _guard = ActiveGuard::new(Arc::clone(&stats), listen_port);

    let result = tokio::select! {
        () = shutdown.cancelled() => {
            Err(io::Error::other("proxy_cancelled"))
        }
        result = tokio::io::copy_bidirectional(&mut inbound, &mut outbound) => {
            result
        }
    };
    if let Ok((bin, bout)) = result.as_ref() {
        stats.record_in(listen_port, *bin);
        stats.record_out(listen_port, *bout);
    }
    match result {
        Ok((bin, bout)) => {
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
async fn dial_with_failover<R: Resolve>(
    rule_id: RuleId,
    resolver: &LiveResolver<R>,
    targets: &[MultiTarget],
    states: &[tokio::sync::Mutex<HealthState>],
    target_failovers_total: &AtomicU64,
    prefer_ipv6: bool,
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
            Ok((sock, _source)) => {
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

    let states: Arc<Vec<tokio::sync::Mutex<HealthState>>> = Arc::new(
        (0..rule.targets.len())
            .map(|_| tokio::sync::Mutex::new(HealthState::new()))
            .collect(),
    );
    let target_failovers_total = Arc::new(AtomicU64::new(0));
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

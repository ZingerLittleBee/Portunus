//! Per-rule UDP forwarder.
//!
//! Spec: 004-udp-forward, plan.md § Data plane. The listener binds
//! `0.0.0.0:<listen_port>`, receives end-user datagrams, looks up or
//! creates a per-source flow, and forwards through the flow's
//! kernel-allocated upstream socket. Each flow runs an independent
//! reply-pump task that receives upstream datagrams and sends them
//! back to the original `(source_addr, source_port)`.
//!
//! Threading model — one tokio task per:
//!   * listener (recv loop bound to `listen_port`)
//!   * flow's reply pump (recv loop bound to the flow's kernel-allocated
//!     upstream socket; spawned on flow creation, torn down via the
//!     flow's `cancel` token).
//!
//! Buffer sizing: a single 65535-byte heap buffer per loop. UDP at the
//! IP layer caps at 65535 bytes total payload; sizing the buffer to
//! that ceiling means `recv_from` can never truncate a datagram (FR-013
//! / R-004). Tokio's `UdpSocket::recv_from` does not expose `MSG_TRUNC`
//! either way, so we size for safety.
//!
//! Concurrency: per-flow upstream sockets means the kernel's UDP
//! source-port selection gives us NAT-style return-path isolation for
//! free — no shared upstream socket means no need to demux by `(addr,
//! port)` ourselves. Cost is O(flows) sockets and tasks; the per-rule
//! `UdpFlowTable` cap keeps that bounded.

pub mod flow;
pub mod registry;
pub mod table;

use std::net::SocketAddr;
use std::sync::Arc;

use portunus_core::{RuleId, Target};
use tokio::net::UdpSocket;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::forwarder::rate_limit::scope::{
    ActiveGuard, LayeredAcquire, OwnerRateLimitHandle, RuleRateLimitHandle, try_acquire_layered,
};
use crate::forwarder::rate_limit::stats::RateLimitStatsAccumulator;
use crate::forwarder::stats::RuleStats;
use crate::forwarder::udp::flow::UdpFlow;
use crate::forwarder::udp::table::{OverflowDropped, UdpFlowTable};
use crate::resolver::{ConnectError, LiveResolver, Resolve};

/// IP-layer UDP payload ceiling (FR-013). One static heap buffer per
/// recv loop sized to this value means `recv_from` cannot truncate any
/// well-formed datagram at the proxy layer.
const UDP_BUFFER_BYTES: usize = 65_535;

/// 007-multi-target-failover (T024): per-flow target selection happens
/// once on the first inbound packet of a NEW flow; the chosen upstream
/// sticks for the lifetime of the flow (FR-012).
///
/// This entry point mirrors `run_listener` but consults a parallel
/// `targets` + `health_states` slice for each new flow. Existing flows
/// take the byte-identical fast path. On dial failure for the chosen
/// target the per-target health is incremented and the next-priority
/// target is attempted; this attribution mirrors the TCP failover loop
/// (FR-010 / quickstart §3).
#[allow(clippy::too_many_arguments)]
pub async fn run_listener_multi_target<R: Resolve + 'static>(
    rule_id: RuleId,
    listen_port: u16,
    targets: Arc<Vec<crate::forwarder::MultiTarget>>,
    health_states: Arc<Vec<tokio::sync::Mutex<crate::forwarder::failover::HealthState>>>,
    target_failovers_total: Arc<std::sync::atomic::AtomicU64>,
    prefer_ipv6: bool,
    flow_cap: usize,
    idle_window: std::time::Duration,
    stats: Arc<RuleStats>,
    resolver: Arc<LiveResolver<R>>,
    cancel: CancellationToken,
    rate_limit: Option<Arc<RuleRateLimitHandle>>,
    rate_limit_stats: Option<Arc<RateLimitStatsAccumulator>>,
    owner_rate_limit: Option<Arc<OwnerRateLimitHandle>>,
    owner_rate_limit_stats: Option<Arc<RateLimitStatsAccumulator>>,
    quota: Option<Arc<crate::forwarder::quota::QuotaHandle>>,
) {
    let listen_addr: SocketAddr = ([0, 0, 0, 0], listen_port).into();
    let listener = match UdpSocket::bind(listen_addr).await {
        Ok(s) => Arc::new(s),
        Err(e) => {
            warn!(
                event = "rule.udp_bind_failed",
                rule_id = %rule_id,
                listen_port = listen_port,
                multi_target = true,
                error = %e,
            );
            return;
        }
    };
    let flow_table = Arc::new(UdpFlowTable::new(flow_cap));
    flow_table.spawn_reaper(idle_window, rule_id, cancel.clone());

    let mut buf = vec![0u8; UDP_BUFFER_BYTES];
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            recv = listener.recv_from(&mut buf) => match recv {
                Ok((n, source)) => {
                    handle_inbound_multi_target(
                        rule_id,
                        listen_port,
                        &buf[..n],
                        source,
                        Arc::clone(&targets),
                        Arc::clone(&health_states),
                        Arc::clone(&target_failovers_total),
                        prefer_ipv6,
                        Arc::clone(&listener),
                        Arc::clone(&flow_table),
                        Arc::clone(&stats),
                        Arc::clone(&resolver),
                        rate_limit.clone(),
                        rate_limit_stats.clone(),
                        owner_rate_limit.clone(),
                        owner_rate_limit_stats.clone(),
                        quota.clone(),
                    )
                    .await;
                }
                Err(e) => warn!(
                    event = "rule.udp_recv_error",
                    rule_id = %rule_id,
                    listen_port = listen_port,
                    multi_target = true,
                    error = %e,
                ),
            }
        }
    }

    let final_len = flow_table.len().await;
    if let Ok(n) = u32::try_from(final_len) {
        stats.set_active_flows(n);
    }
    flow_table.drain().await;
    info!(
        event = "rule.udp_listener_drained",
        rule_id = %rule_id,
        listen_port = listen_port,
        multi_target = true,
    );
}

/// Multi-target counterpart to `handle_inbound`. Existing-flow path is
/// byte-identical (sticky target per FR-012). New-flow path: snapshot
/// per-target health → walk in priority order until one resolves +
/// dials → bind upstream + insert flow.
#[allow(clippy::too_many_arguments)]
async fn handle_inbound_multi_target<R: Resolve>(
    rule_id: RuleId,
    listen_port: u16,
    payload: &[u8],
    source: SocketAddr,
    targets: Arc<Vec<crate::forwarder::MultiTarget>>,
    health_states: Arc<Vec<tokio::sync::Mutex<crate::forwarder::failover::HealthState>>>,
    target_failovers_total: Arc<std::sync::atomic::AtomicU64>,
    prefer_ipv6: bool,
    listener: Arc<UdpSocket>,
    flow_table: Arc<UdpFlowTable>,
    stats: Arc<RuleStats>,
    resolver: Arc<LiveResolver<R>>,
    rate_limit: Option<Arc<RuleRateLimitHandle>>,
    rate_limit_stats: Option<Arc<RateLimitStatsAccumulator>>,
    owner_rate_limit: Option<Arc<OwnerRateLimitHandle>>,
    owner_rate_limit_stats: Option<Arc<RateLimitStatsAccumulator>>,
    quota: Option<Arc<crate::forwarder::quota::QuotaHandle>>,
) {
    use crate::forwarder::failover;
    use std::time::{Instant, SystemTime};

    if let Some(existing) = flow_table.get(source).await {
        relay_existing_flow(
            rule_id,
            listen_port,
            payload,
            source,
            existing,
            &flow_table,
            &stats,
        )
        .await;
        return;
    }

    // 013-traffic-quotas E4: drop datagrams on a NEW flow when the
    // budget is already exhausted. Mirrors TCP `is_exhausted` short-
    // circuit — we don't burn a resolve / bind / first-packet rate
    // grant on a flow that can't actually deliver bytes.
    if let Some(q) = quota.as_ref()
        && q.is_exhausted()
    {
        return;
    }

    // T021/T030: layered owner+rule rate-limit gate before NAT bind.
    // Run on the first packet of every NEW flow so a flooding source
    // can't burn upstream sockets / DNS lookups (FR-009 + FR-013).
    let Some((owner_admit_guard, admit_guard)) = acquire_first_packet(
        rule_id,
        listen_port,
        source,
        rate_limit.as_ref(),
        rate_limit_stats.as_deref(),
        owner_rate_limit.as_ref(),
        owner_rate_limit_stats.as_deref(),
    ) else {
        return;
    };

    // New flow: pick a healthy target (or fall back to row 0 if all
    // failed — FR-007). Walk through remaining targets on dial
    // failure, attributing each failure to the right HealthState.
    let order: Vec<usize> = {
        let mut snap: Vec<failover::Health> = Vec::with_capacity(health_states.len());
        for s in health_states.iter() {
            snap.push(s.lock().await.health());
        }
        let first = snap
            .iter()
            .position(|h| matches!(h, failover::Health::Healthy))
            .unwrap_or(0);
        let mut rest: Vec<usize> = (0..targets.len()).filter(|&i| i != first).collect();
        rest.sort_unstable();
        let mut out = Vec::with_capacity(targets.len());
        out.push(first);
        out.extend(rest);
        out
    };

    for &idx in &order {
        let candidate = &targets[idx];
        let resolve = resolver
            .resolve_target(rule_id, &candidate.target, candidate.spec.port, prefer_ipv6)
            .await;
        let now = Instant::now();
        let wall = SystemTime::now();
        let upstream_addrs = match resolve {
            Ok((addrs, _src)) if !addrs.is_empty() => addrs,
            _ => {
                health_states[idx]
                    .lock()
                    .await
                    .record_failure(now, wall, &target_failovers_total);
                stats.inc_dns_failure();
                warn!(
                    event = "rule.udp_target.dial_failed",
                    rule_id = %rule_id,
                    listen_port = listen_port,
                    source = %source,
                    target_index = idx,
                    target_host = %candidate.spec.host,
                    target_port = candidate.spec.port,
                    reason = "resolve_or_empty",
                );
                continue;
            }
        };
        // Resolution succeeded — count as success on this target's
        // health and build the flow. Per-flow stickiness means
        // subsequent send_to errors don't roll back the per-target
        // health; they route through the existing `advance_upstream`
        // multi-A walk (FR-012 — failover only on NEW flows).
        health_states[idx]
            .lock()
            .await
            .record_success(now, wall, &target_failovers_total);
        let target_idx = u32::try_from(idx).unwrap_or(u32::MAX);
        match build_or_lookup_flow(
            Arc::clone(&flow_table),
            source,
            upstream_addrs,
            rule_id,
            listen_port,
            Arc::clone(&listener),
            Arc::clone(&stats),
            Some((target_idx, Arc::clone(&health_states))),
            admit_guard,
            owner_admit_guard,
            quota.clone(),
        )
        .await
        {
            Some(f) => {
                relay_existing_flow(
                    rule_id,
                    listen_port,
                    payload,
                    source,
                    f,
                    &flow_table,
                    &stats,
                )
                .await;
                return;
            }
            None => return,
        }
    }
    // All targets exhausted.
    stats.inc_dns_failure();
    warn!(
        event = "rule.udp_all_targets_failed",
        rule_id = %rule_id,
        listen_port = listen_port,
        source = %source,
        target_count = targets.len(),
    );
}

/// Drain a payload through an already-resolved flow. Mirrors the loop
/// in `handle_inbound` post-flow-lookup.
async fn relay_existing_flow(
    rule_id: RuleId,
    listen_port: u16,
    payload: &[u8],
    source: SocketAddr,
    phase_flow: Arc<UdpFlow>,
    flow_table: &Arc<UdpFlowTable>,
    stats: &Arc<RuleStats>,
) {
    // 013-traffic-quotas E4: silently drop further inbound datagrams
    // once the budget is exhausted. Mirrors the TCP `is_exhausted`
    // short-circuit.
    if !phase_flow.quota_allows() {
        return;
    }
    let n = u64::try_from(payload.len()).unwrap_or(u64::MAX);
    loop {
        let upstream = phase_flow.current_upstream();
        match phase_flow.upstream_socket.send_to(payload, upstream).await {
            Ok(_) => {
                stats.inc_datagram_in(listen_port, n);
                phase_flow.bump_inbound(n).await;
                // 013-traffic-quotas E4: consume AFTER send_to landed —
                // counters and budget agree byte-for-byte.
                let _ = phase_flow.quota_consume_after_send(n);
                if let Ok(live) = u32::try_from(flow_table.len().await) {
                    stats.set_active_flows(live);
                }
                return;
            }
            Err(e) => {
                if let Some(next) = phase_flow.advance_upstream() {
                    warn!(
                        event = "rule.udp_send_to_fallback",
                        rule_id = %rule_id,
                        listen_port = listen_port,
                        source = %source,
                        failed_upstream = %upstream,
                        next_upstream = %next,
                        error = %e,
                    );
                } else {
                    stats.inc_dns_failure();
                    warn!(
                        event = "rule.udp_send_to_exhausted",
                        rule_id = %rule_id,
                        listen_port = listen_port,
                        source = %source,
                        failed_upstream = %upstream,
                        error = %e,
                    );
                    return;
                }
            }
        }
    }
}

/// Pair of admission guards from the layered owner+rule cascade —
/// either side may be `None` when its layer is uncapped.
type AdmitPair = (Option<ActiveGuard>, Option<ActiveGuard>);

/// T021/T030: gate the first packet of a NEW UDP flow against the
/// per-owner ceiling AND the per-rule cap. Owner gate runs first
/// (FR-013); rejects on either layer return `None` (silent drop per
/// FR-009), and reject reasons land in the corresponding scope's
/// stats accumulator (FR-014).
///
/// On admission the caller receives `Some((owner_guard, rule_guard))`
/// — both guards must be attached to the resulting `UdpFlow` so the
/// owner and rule active-connection gauges decrement when the flow
/// tears down.
fn acquire_first_packet(
    rule_id: RuleId,
    listen_port: u16,
    source: SocketAddr,
    rate_limit: Option<&Arc<RuleRateLimitHandle>>,
    rate_limit_stats: Option<&RateLimitStatsAccumulator>,
    owner_rate_limit: Option<&Arc<OwnerRateLimitHandle>>,
    owner_rate_limit_stats: Option<&RateLimitStatsAccumulator>,
) -> Option<AdmitPair> {
    match try_acquire_layered(owner_rate_limit, rate_limit, true) {
        LayeredAcquire::Granted {
            owner_guard,
            rule_guard,
        } => Some((owner_guard, rule_guard)),
        LayeredAcquire::OwnerRejected(reason) => {
            if let Some(s) = owner_rate_limit_stats {
                s.record_reject(reason);
            }
            tracing::warn!(
                event = "rule.udp_first_packet_rejected",
                rule_id = %rule_id,
                listen_port = listen_port,
                source = %source,
                scope = "owner",
                reason = ?reason,
            );
            None
        }
        LayeredAcquire::RuleRejected(reason) => {
            if let Some(s) = rate_limit_stats {
                s.record_reject(reason);
            }
            tracing::warn!(
                event = "rule.udp_first_packet_rejected",
                rule_id = %rule_id,
                listen_port = listen_port,
                source = %source,
                scope = "rule",
                reason = ?reason,
            );
            None
        }
    }
}

/// Spawn a detached task that drops `guard` when `cancel` fires —
/// ties the per-rule `active_connections` slot to the lifetime of the
/// `UdpFlow.cancel` token. Used by `build_or_lookup_flow` after it
/// confirms (via `Arc::ptr_eq`) that the closure-built flow won the
/// race; lost-race callers drop the guard locally.
fn spawn_admit_guard(cancel: CancellationToken, guard: ActiveGuard) {
    tokio::spawn(async move {
        cancel.cancelled().await;
        drop(guard);
    });
}

/// Run a per-port UDP listener on `listen_port`. The listener binds,
/// loops `recv_from` on the listen socket, dispatches each datagram
/// through a per-source flow, and tears down on `cancel`.
///
/// US1: IP-target rules. US2 (T044/T046): DNS-target rules go through
/// `resolver.resolve_target` on first datagram of a new flow; the
/// resolver is shared with the TCP path so cache + single-flight +
/// `dns_failures` semantics are unified.
///
/// `flow_cap` is the per-rule cap on simultaneous live flows
/// (`udp_max_flows_per_rule` from `Welcome`, default 1024).
#[allow(clippy::too_many_arguments)]
pub async fn run_listener<R: Resolve + 'static>(
    rule_id: RuleId,
    listen_port: u16,
    target: Target,
    target_port: u16,
    prefer_ipv6: bool,
    flow_cap: usize,
    idle_window: std::time::Duration,
    stats: Arc<RuleStats>,
    resolver: Arc<LiveResolver<R>>,
    cancel: CancellationToken,
    rate_limit: Option<Arc<RuleRateLimitHandle>>,
    rate_limit_stats: Option<Arc<RateLimitStatsAccumulator>>,
    owner_rate_limit: Option<Arc<OwnerRateLimitHandle>>,
    owner_rate_limit_stats: Option<Arc<RateLimitStatsAccumulator>>,
    quota: Option<Arc<crate::forwarder::quota::QuotaHandle>>,
) {
    let listen_addr: SocketAddr = ([0, 0, 0, 0], listen_port).into();
    let listener = match UdpSocket::bind(listen_addr).await {
        Ok(s) => Arc::new(s),
        Err(e) => {
            warn!(
                event = "rule.udp_bind_failed",
                rule_id = %rule_id,
                listen_port = listen_port,
                error = %e,
            );
            return;
        }
    };
    let flow_table = Arc::new(UdpFlowTable::new(flow_cap));
    // 004-udp-forward T060/T062: the reaper task tears stale flows down
    // every `idle_window / 4`. `idle_window == 0` (test escape hatch)
    // disables the reaper; production callers always pass a positive
    // window from the Welcome-derived runtime config.
    flow_table.spawn_reaper(idle_window, rule_id, cancel.clone());

    let mut buf = vec![0u8; UDP_BUFFER_BYTES];
    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                break;
            }
            recv = listener.recv_from(&mut buf) => match recv {
                Ok((n, source)) => {
                    handle_inbound(
                        rule_id,
                        listen_port,
                        &buf[..n],
                        source,
                        &target,
                        target_port,
                        prefer_ipv6,
                        Arc::clone(&listener),
                        Arc::clone(&flow_table),
                        Arc::clone(&stats),
                        Arc::clone(&resolver),
                        rate_limit.clone(),
                        rate_limit_stats.clone(),
                        owner_rate_limit.clone(),
                        owner_rate_limit_stats.clone(),
                        quota.clone(),
                    )
                    .await;
                }
                Err(e) => {
                    warn!(
                        event = "rule.udp_recv_error",
                        rule_id = %rule_id,
                        listen_port = listen_port,
                        error = %e,
                    );
                }
            }
        }
    }

    // Snapshot live flow count one more time before draining so the
    // last `active_flows` gauge value isn't a stale high-water mark.
    let final_len = flow_table.len().await;
    if let Ok(n) = u32::try_from(final_len) {
        stats.set_active_flows(n);
    }
    flow_table.drain().await;
    info!(
        event = "rule.udp_listener_drained",
        rule_id = %rule_id,
        listen_port = listen_port,
    );
}

#[allow(clippy::too_many_arguments)]
async fn handle_inbound<R: Resolve>(
    rule_id: RuleId,
    listen_port: u16,
    payload: &[u8],
    source: SocketAddr,
    target: &Target,
    target_port: u16,
    prefer_ipv6: bool,
    listener: Arc<UdpSocket>,
    flow_table: Arc<UdpFlowTable>,
    stats: Arc<RuleStats>,
    resolver: Arc<LiveResolver<R>>,
    rate_limit: Option<Arc<RuleRateLimitHandle>>,
    rate_limit_stats: Option<Arc<RateLimitStatsAccumulator>>,
    owner_rate_limit: Option<Arc<OwnerRateLimitHandle>>,
    owner_rate_limit_stats: Option<Arc<RateLimitStatsAccumulator>>,
    quota: Option<Arc<crate::forwarder::quota::QuotaHandle>>,
) {
    // Fast path: existing flow. Skips both resolver and upstream-bind
    // in the common case of a long-lived sender. The resolver's cache
    // already coalesces repeated lookups for the same hostname, but a
    // table hit avoids even that cheap path.
    let phase_flow = if let Some(f) = flow_table.get(source).await {
        f
    } else {
        // 013-traffic-quotas E4: drop datagrams that would open a NEW
        // flow under an exhausted budget. Existing-flow datagrams hit
        // the `quota_allows` check inside `relay_existing_flow`.
        if let Some(q) = quota.as_ref()
            && q.is_exhausted()
        {
            return;
        }
        // T021/T030: layered owner+rule rate-limit gate before any
        // resolver / NAT bind work. Reject = silent drop (FR-009 UDP
        // path); FR-013 ordering — owner first.
        let Some((owner_admit_guard, admit_guard)) = acquire_first_packet(
            rule_id,
            listen_port,
            source,
            rate_limit.as_ref(),
            rate_limit_stats.as_deref(),
            owner_rate_limit.as_ref(),
            owner_rate_limit_stats.as_deref(),
        ) else {
            return;
        };
        // US2 (T044): resolve through the shared `LiveResolver`. For
        // `Target::Ip` this short-circuits to a single SocketAddr
        // without touching DNS; for `Target::Dns` it consults the
        // cache (single-flight on miss) and applies family
        // ordering per `prefer_ipv6`.
        let upstream_addrs = match resolver
            .resolve_target(rule_id, target, target_port, prefer_ipv6)
            .await
        {
            Ok((addrs, _src)) => addrs,
            Err(ConnectError::Resolution(err)) => {
                // FR-008: resolver-side failure → bump per-rule
                // counter and drop the datagram. The rule stays
                // Active (FR-012) — a future datagram may succeed
                // when the resolver recovers.
                stats.inc_dns_failure();
                warn!(
                    event = "rule.udp_dns_failed",
                    rule_id = %rule_id,
                    listen_port = listen_port,
                    source = %source,
                    error = %err,
                );
                return;
            }
            Err(other) => {
                // resolve_target only emits Resolution errors; this
                // branch is defensive against future variants.
                warn!(
                    event = "rule.udp_resolve_unexpected_error",
                    rule_id = %rule_id,
                    listen_port = listen_port,
                    source = %source,
                    error = %other,
                );
                return;
            }
        };
        if upstream_addrs.is_empty() {
            stats.inc_dns_failure();
            return;
        }
        match build_or_lookup_flow(
            Arc::clone(&flow_table),
            source,
            upstream_addrs,
            rule_id,
            listen_port,
            Arc::clone(&listener),
            Arc::clone(&stats),
            None, // legacy single-target rule — preserve v0.6.0 hot path
            admit_guard,
            owner_admit_guard,
            quota.clone(),
        )
        .await
        {
            Some(f) => f,
            None => return, // overflow already logged + counted
        }
    };

    // 013-traffic-quotas E4: an existing flow could have been built
    // before exhaustion but the budget might have drained mid-flow;
    // short-circuit before the send_to. Mirrors `relay_existing_flow`.
    if !phase_flow.quota_allows() {
        return;
    }
    // Forward inbound datagram through the flow's upstream socket. On
    // send_to error, US2 (T045) walks remaining multi-A candidates
    // before giving up — only the LAST candidate's failure bumps
    // `dns_failures` and drops the datagram (FR-006/FR-012).
    let n = u64::try_from(payload.len()).unwrap_or(u64::MAX);
    loop {
        let upstream = phase_flow.current_upstream();
        match phase_flow.upstream_socket.send_to(payload, upstream).await {
            Ok(_) => {
                stats.inc_datagram_in(listen_port, n);
                phase_flow.bump_inbound(n).await;
                // 013-traffic-quotas E4: consume after the bytes
                // landed upstream.
                let _ = phase_flow.quota_consume_after_send(n);
                // Opportunistic gauge update — the exact value is
                // re-read on the StatsReport tick anyway.
                if let Ok(live) = u32::try_from(flow_table.len().await) {
                    stats.set_active_flows(live);
                }
                return;
            }
            Err(e) => {
                if let Some(next) = phase_flow.advance_upstream() {
                    warn!(
                        event = "rule.udp_send_to_fallback",
                        rule_id = %rule_id,
                        listen_port = listen_port,
                        source = %source,
                        failed_upstream = %upstream,
                        next_upstream = %next,
                        error = %e,
                    );
                    // Loop: retry on next address.
                } else {
                    // Multi-A list exhausted (or single-address rule).
                    // Drop the datagram and surface the failure via
                    // `dns_failures` so operators see "nothing reaches
                    // the upstream" without grepping logs.
                    stats.inc_dns_failure();
                    warn!(
                        event = "rule.udp_send_to_exhausted",
                        rule_id = %rule_id,
                        listen_port = listen_port,
                        source = %source,
                        failed_upstream = %upstream,
                        error = %e,
                    );
                    return;
                }
            }
        }
    }
}

/// Bind an upstream socket and insert a fresh flow atomically. On
/// overflow the freshly-bound socket is dropped (kernel cleans up).
/// On a concurrent insertion race, the closure-built flow is silently
/// discarded — `lookup_or_insert` returns the winner, our pre-bound
/// socket falls out of scope.
///
/// `upstream_addrs` is the resolver-ordered candidate list (single
/// element for IP-target rules; full multi-A list for DNS-target
/// rules). Caller is responsible for ensuring it's non-empty.
#[allow(clippy::too_many_arguments)]
async fn build_or_lookup_flow(
    flow_table: Arc<UdpFlowTable>,
    source: SocketAddr,
    upstream_addrs: Vec<SocketAddr>,
    rule_id: RuleId,
    listen_port: u16,
    listener: Arc<UdpSocket>,
    stats: Arc<RuleStats>,
    // 007-multi-target-failover T024/T034: when `Some(idx, states)`,
    // the constructed flow stores them so `bump_inbound`/`bump_outbound`
    // can credit the per-target byte counter. `None` for legacy
    // single-target rules (byte-identical hot path preserved).
    multi_target: Option<(
        u32,
        Arc<Vec<tokio::sync::Mutex<crate::forwarder::failover::HealthState>>>,
    )>,
    // T021: per-rule concurrent-cap guard for THIS new flow's source.
    // Attached to `flow.cancel` after we confirm the build closure
    // ran (i.e. we won the lookup_or_insert race). Lost races drop
    // the guard locally and the cap auto-decrements.
    admit_guard: Option<ActiveGuard>,
    // T030: per-owner concurrent-cap guard. Same lifetime semantics
    // as `admit_guard` but attached to a separate AtomicU64 so the
    // owner gauge tracks across all flows owned by the same RBAC
    // identity.
    owner_admit_guard: Option<ActiveGuard>,
    // 013-traffic-quotas E4: per-(user, client) byte budget handle.
    // Attached to the freshly-built flow via `attach_quota` so the
    // relay loop and reply pump see the same `Arc<QuotaHandle>` for
    // the life of the flow.
    quota: Option<Arc<crate::forwarder::quota::QuotaHandle>>,
) -> Option<Arc<UdpFlow>> {
    let upstream_socket = match UdpSocket::bind(("0.0.0.0", 0)).await {
        Ok(s) => Arc::new(s),
        Err(e) => {
            warn!(
                event = "rule.udp_upstream_bind_failed",
                rule_id = %rule_id,
                listen_port = listen_port,
                source = %source,
                error = %e,
            );
            return None;
        }
    };

    let bound_local = upstream_socket
        .local_addr()
        .map_or_else(|_| "unknown".to_string(), |a| a.to_string());

    let head_upstream = upstream_addrs[0];
    let addr_count = upstream_addrs.len();
    let flow_for_build = Arc::clone(&upstream_socket);
    let addrs_for_build = upstream_addrs.clone();
    let multi_for_build = multi_target.clone();
    let quota_for_build = quota.clone();
    let result = flow_table
        .lookup_or_insert(source, move || {
            let flow = match multi_for_build {
                Some((target_idx, hstates)) => UdpFlow::new_multi_target(
                    source,
                    flow_for_build,
                    addrs_for_build,
                    target_idx,
                    hstates,
                ),
                None => UdpFlow::new(source, flow_for_build, addrs_for_build),
            };
            match quota_for_build {
                Some(q) => flow.attach_quota(q),
                None => flow,
            }
        })
        .await;

    match result {
        Ok(flow) => {
            // Spawn the reply pump for this flow. If the flow is the one
            // we just built, the spawn binds it to the lifetime of the
            // cancel token; if a concurrent build won the race the new
            // socket we bound above is dropped harmlessly.
            // We can detect "we built this flow" by ptr-equality with
            // the upstream_socket we bound — the closure ran iff the
            // table entry is fresh. This is the cleanest signal without
            // adding a flag to UdpFlow.
            if Arc::ptr_eq(&flow.upstream_socket, &upstream_socket) {
                info!(
                    event = "rule.udp_flow_opened",
                    rule_id = %rule_id,
                    listen_port = listen_port,
                    source = %source,
                    upstream = %head_upstream,
                    upstream_addr_count = addr_count,
                    upstream_local = %bound_local,
                );
                if let Some(g) = admit_guard {
                    spawn_admit_guard(flow.cancel.clone(), g);
                }
                if let Some(g) = owner_admit_guard {
                    spawn_admit_guard(flow.cancel.clone(), g);
                }
                spawn_reply_pump(
                    rule_id,
                    listen_port,
                    Arc::clone(&flow),
                    Arc::clone(&listener),
                    Arc::clone(&stats),
                );
            }
            // Lost-race path: both `admit_guard` and `owner_admit_guard`
            // fall out of scope here (the `if let`s consumed them only
            // on the won-race branch), so any concurrent slots we
            // briefly held are released.
            Some(flow)
        }
        Err(OverflowDropped { source: src }) => {
            stats.inc_flow_dropped_overflow();
            warn!(
                event = "rule.udp_flow_dropped_overflow",
                rule_id = %rule_id,
                listen_port = listen_port,
                source = %src,
            );
            None
        }
    }
}

/// Spawn the per-flow reply-pump task. Receives on the flow's
/// kernel-allocated upstream socket and forwards each datagram back
/// to `flow.source_addr` via the listener socket. Exits on
/// `flow.cancel`.
fn spawn_reply_pump(
    rule_id: RuleId,
    listen_port: u16,
    flow: Arc<UdpFlow>,
    listener: Arc<UdpSocket>,
    stats: Arc<RuleStats>,
) {
    tokio::spawn(async move {
        let mut buf = vec![0u8; UDP_BUFFER_BYTES];
        loop {
            tokio::select! {
                () = flow.cancel.cancelled() => break,
                recv = flow.upstream_socket.recv_from(&mut buf) => match recv {
                    Ok((n, _from)) => {
                        // 013-traffic-quotas E4: drop the reply when
                        // the budget is exhausted. The flow stays open
                        // — the idle reaper or next admin update will
                        // tear it down.
                        if !flow.quota_allows() {
                            continue;
                        }
                        let bytes = u64::try_from(n).unwrap_or(u64::MAX);
                        match listener.send_to(&buf[..n], flow.source_addr).await {
                            Ok(_) => {
                                stats.inc_datagram_out(listen_port, bytes);
                                flow.bump_outbound(bytes).await;
                                // Consume AFTER the reply landed.
                                let _ = flow.quota_consume_after_send(bytes);
                            }
                            Err(e) => {
                                warn!(
                                    event = "rule.udp_reply_send_failed",
                                    rule_id = %rule_id,
                                    listen_port = listen_port,
                                    source = %flow.source_addr,
                                    error = %e,
                                );
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            event = "rule.udp_reply_recv_error",
                            rule_id = %rule_id,
                            listen_port = listen_port,
                            source = %flow.source_addr,
                            error = %e,
                        );
                        break;
                    }
                }
            }
        }
        info!(
            event = "rule.udp_flow_closed",
            rule_id = %rule_id,
            listen_port = listen_port,
            source = %flow.source_addr,
            bytes_in = flow.bytes_in.load(std::sync::atomic::Ordering::Relaxed),
            bytes_out = flow.bytes_out.load(std::sync::atomic::Ordering::Relaxed),
            datagrams_in = flow.datagrams_in.load(std::sync::atomic::Ordering::Relaxed),
            datagrams_out = flow.datagrams_out.load(std::sync::atomic::Ordering::Relaxed),
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::{ResolveAnswer, ResolverConfig, ResolverError};
    use portunus_core::{Hostname, Target};
    use std::net::Ipv4Addr;
    use std::time::Duration;
    use tokio::net::UdpSocket;
    use tokio_util::sync::CancellationToken;

    /// IP-target tests use a `PanickingResolver` so any accidental
    /// resolver invocation surfaces as a hard test failure (R-006).
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

    /// Spawn a UDP echo on a fresh ephemeral port. Returns the bound
    /// SocketAddr.
    async fn spawn_udp_echo() -> SocketAddr {
        let sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = sock.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            loop {
                let Ok((n, peer)) = sock.recv_from(&mut buf).await else {
                    break;
                };
                let _ = sock.send_to(&buf[..n], peer).await;
            }
        });
        addr
    }

    /// T024: end-to-end round-trip through the listener.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn udp_listener_round_trip_byte_equal() {
        let echo = spawn_udp_echo().await;
        // Pick a free listen port.
        let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await.unwrap();
        let listen_port = probe.local_addr().unwrap().port();
        drop(probe);

        let stats = RuleStats::new();
        let cancel = CancellationToken::new();
        let cancel_run = cancel.clone();
        let stats_run = Arc::clone(&stats);
        let task = tokio::spawn(async move {
            run_listener(
                RuleId(700),
                listen_port,
                Target::Ip(echo.ip()),
                echo.port(),
                false,
                1024,
                std::time::Duration::ZERO,
                stats_run,
                test_resolver(),
                cancel_run,
                None,
                None,
                None,
                None,
                None,
            )
            .await;
        });

        // Give the listener a beat to bind.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let payload = b"hello-udp";
        client
            .send_to(payload, (Ipv4Addr::LOCALHOST, listen_port))
            .await
            .unwrap();

        let mut buf = [0u8; 64];
        let (n, _from) = tokio::time::timeout(Duration::from_secs(2), client.recv_from(&mut buf))
            .await
            .expect("round-trip timed out")
            .expect("recv ok");
        assert_eq!(&buf[..n], payload);

        // Counters incremented.
        assert_eq!(stats.snapshot_datagrams_in(), 1);
        assert_eq!(stats.snapshot_datagrams_out(), 1);
        assert_eq!(stats.snapshot_active_flows(), 1);

        cancel.cancel();
        task.await.unwrap();
    }

    /// T025: two senders on distinct source ports stay isolated.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn udp_listener_two_sources_isolated_replies() {
        let echo = spawn_udp_echo().await;
        let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await.unwrap();
        let listen_port = probe.local_addr().unwrap().port();
        drop(probe);

        let stats = RuleStats::new();
        let cancel = CancellationToken::new();
        let cancel_run = cancel.clone();
        let stats_run = Arc::clone(&stats);
        let task = tokio::spawn(async move {
            run_listener(
                RuleId(701),
                listen_port,
                Target::Ip(echo.ip()),
                echo.port(),
                false,
                1024,
                std::time::Duration::ZERO,
                stats_run,
                test_resolver(),
                cancel_run,
                None,
                None,
                None,
                None,
                None,
            )
            .await;
        });

        tokio::time::sleep(Duration::from_millis(50)).await;

        let a = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let b = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        // Distinct payloads for distinct sources.
        a.send_to(b"AAAA", (Ipv4Addr::LOCALHOST, listen_port))
            .await
            .unwrap();
        b.send_to(b"BBBBBB", (Ipv4Addr::LOCALHOST, listen_port))
            .await
            .unwrap();

        let mut ba = [0u8; 16];
        let mut bb = [0u8; 16];
        let (na, _) = tokio::time::timeout(Duration::from_secs(2), a.recv_from(&mut ba))
            .await
            .expect("a timed out")
            .unwrap();
        let (nb, _) = tokio::time::timeout(Duration::from_secs(2), b.recv_from(&mut bb))
            .await
            .expect("b timed out")
            .unwrap();
        assert_eq!(&ba[..na], b"AAAA", "source A must receive its own reply");
        assert_eq!(&bb[..nb], b"BBBBBB", "source B must receive its own reply");

        cancel.cancel();
        task.await.unwrap();
    }

    /// T021: a per-rule new_connections_per_sec=1 cap drops the second
    /// new-source first-packet within the same burst window. The first
    /// flow gets through; the second is silently dropped (no upstream
    /// bind) and the per-rule reject counter shows
    /// `RejectReason::UdpFlowRate`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn t021_udp_flow_rate_drops_second_new_source() {
        use crate::forwarder::rate_limit::stats::RateLimitStatsAccumulator;
        use portunus_core::{RateLimit, RejectReason};

        let echo = spawn_udp_echo().await;
        let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await.unwrap();
        let listen_port = probe.local_addr().unwrap().port();
        drop(probe);

        let stats = RuleStats::new();
        let cancel = CancellationToken::new();
        let cancel_run = cancel.clone();
        let stats_run = Arc::clone(&stats);
        let rule_mgr = Arc::new(crate::forwarder::rate_limit::scope::RateLimitScopeManager::new());
        rule_mgr.install(
            RuleId(800),
            Some(&RateLimit {
                new_connections_per_sec: Some(1),
                ..Default::default()
            }),
        );
        let limiter = Arc::new(
            crate::forwarder::rate_limit::scope::RuleRateLimitHandle::new(RuleId(800), rule_mgr),
        );
        let rl_stats = Arc::new(RateLimitStatsAccumulator::new());
        let task_limiter = Arc::clone(&limiter);
        let task_rl_stats = Arc::clone(&rl_stats);
        let task = tokio::spawn(async move {
            run_listener(
                RuleId(800),
                listen_port,
                Target::Ip(echo.ip()),
                echo.port(),
                false,
                1024,
                Duration::ZERO,
                stats_run,
                test_resolver(),
                cancel_run,
                Some(task_limiter),
                Some(task_rl_stats),
                None,
                None,
                None,
            )
            .await;
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        // First source: must be admitted (rate burst = 1).
        let a = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        a.send_to(b"first", (Ipv4Addr::LOCALHOST, listen_port))
            .await
            .unwrap();
        let mut buf = [0u8; 16];
        let (n, _) = tokio::time::timeout(Duration::from_secs(2), a.recv_from(&mut buf))
            .await
            .expect("first source must round-trip")
            .unwrap();
        assert_eq!(&buf[..n], b"first");

        // Second source within the same burst window: rate token is
        // depleted, so the first-packet gate rejects. The send_to call
        // succeeds (UDP is fire-and-forget) but no echo comes back —
        // recv_from must time out.
        let b = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        b.send_to(b"second", (Ipv4Addr::LOCALHOST, listen_port))
            .await
            .unwrap();
        let mut buf2 = [0u8; 16];
        let recv = tokio::time::timeout(Duration::from_millis(300), b.recv_from(&mut buf2)).await;
        assert!(
            recv.is_err(),
            "second source must be dropped on the rate gate, got {recv:?}"
        );

        // Reject counter records UdpFlowRate exactly once.
        assert_eq!(rl_stats.reject_total(RejectReason::UdpFlowRate), 1);
        assert_eq!(rl_stats.reject_total(RejectReason::ConnRate), 0);

        cancel.cancel();
        task.await.unwrap();
    }

    /// T030 / FR-013 (UDP path): when both per-owner and per-rule
    /// flow-rate caps exist and the OWNER cap is the tighter one,
    /// surplus first-packets must reject under `OwnerUdpFlowRate`
    /// and the per-rule flow-rate counter must stay at zero.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn t030_owner_cap_binds_before_rule_cap_on_udp_first_packet() {
        use crate::forwarder::rate_limit::scope::{
            OwnerId, OwnerRateLimitHandle, OwnerRateLimitScopeManager, RuleRateLimitHandle,
        };
        use crate::forwarder::rate_limit::stats::RateLimitStatsAccumulator;
        use portunus_core::{RateLimit, RejectReason};

        let echo = spawn_udp_echo().await;
        let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await.unwrap();
        let listen_port = probe.local_addr().unwrap().port();
        drop(probe);

        // Rule allows 5 new flows/sec, owner allows 1 — owner is the
        // binding ceiling. The first source admits, the second rejects
        // under OwnerUdpFlowRate.
        let rule_mgr = Arc::new(crate::forwarder::rate_limit::scope::RateLimitScopeManager::new());
        rule_mgr.install(
            RuleId(801),
            Some(&RateLimit {
                new_connections_per_sec: Some(5),
                ..Default::default()
            }),
        );
        let rule_limiter = Arc::new(RuleRateLimitHandle::new(RuleId(801), rule_mgr));
        let rule_stats = Arc::new(RateLimitStatsAccumulator::new());
        let owner_mgr = Arc::new(OwnerRateLimitScopeManager::new());
        let owner_id = OwnerId::new("alice");
        owner_mgr.install(
            &owner_id,
            Some(&RateLimit {
                new_connections_per_sec: Some(1),
                ..Default::default()
            }),
        );
        let owner_limiter = Arc::new(OwnerRateLimitHandle::new(owner_id, owner_mgr));
        let owner_stats = Arc::new(RateLimitStatsAccumulator::new());

        let stats = RuleStats::new();
        let cancel = CancellationToken::new();
        let cancel_run = cancel.clone();
        let stats_run = Arc::clone(&stats);
        let task_rule = Arc::clone(&rule_limiter);
        let task_rule_stats = Arc::clone(&rule_stats);
        let task_owner = Arc::clone(&owner_limiter);
        let task_owner_stats = Arc::clone(&owner_stats);
        let task = tokio::spawn(async move {
            run_listener(
                RuleId(801),
                listen_port,
                Target::Ip(echo.ip()),
                echo.port(),
                false,
                1024,
                Duration::ZERO,
                stats_run,
                test_resolver(),
                cancel_run,
                Some(task_rule),
                Some(task_rule_stats),
                Some(task_owner),
                Some(task_owner_stats),
                None,
            )
            .await;
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        // First source admits and round-trips.
        let a = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        a.send_to(b"first", (Ipv4Addr::LOCALHOST, listen_port))
            .await
            .unwrap();
        let mut buf = [0u8; 16];
        let (n, _) = tokio::time::timeout(Duration::from_secs(2), a.recv_from(&mut buf))
            .await
            .expect("first source must round-trip")
            .unwrap();
        assert_eq!(&buf[..n], b"first");

        // Second NEW source within the burst window: owner rate token
        // depleted → reject. Rule still has tokens but FR-013 means
        // the rule's reject counter must stay at zero.
        let b = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        b.send_to(b"second", (Ipv4Addr::LOCALHOST, listen_port))
            .await
            .unwrap();
        let mut buf2 = [0u8; 16];
        let recv = tokio::time::timeout(Duration::from_millis(300), b.recv_from(&mut buf2)).await;
        assert!(
            recv.is_err(),
            "second source must be dropped on the owner gate, got {recv:?}"
        );

        assert_eq!(
            owner_stats.reject_total(RejectReason::OwnerUdpFlowRate),
            1,
            "OwnerUdpFlowRate must record exactly one reject"
        );
        assert_eq!(
            rule_stats.reject_total(RejectReason::UdpFlowRate),
            0,
            "rule flow-rate counter must NOT bump when owner gate refuses (FR-013)"
        );

        cancel.cancel();
        task.await.unwrap();
    }

    /// T072: per-owner `concurrent_connections` cap on UDP. Source A's
    /// first packet occupies the single slot and keeps round-tripping;
    /// source B's first packet must reject under `OwnerConcurrent` while
    /// the owner gauge stays at 1.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn t072_owner_concurrent_cap_drops_second_source_keeps_first_flow_alive() {
        use crate::forwarder::rate_limit::scope::{
            OwnerId, OwnerRateLimitHandle, OwnerRateLimitScopeManager,
        };
        use crate::forwarder::rate_limit::stats::RateLimitStatsAccumulator;
        use portunus_core::{RateLimit, RejectReason};

        let owner_mgr = Arc::new(OwnerRateLimitScopeManager::new());
        let owner_id = OwnerId::new("alice");
        owner_mgr.install(
            &owner_id,
            Some(&RateLimit {
                concurrent_connections: Some(1),
                // NO new_connections_per_sec — concurrent is the only gate.
                ..Default::default()
            }),
        );
        let owner_limiter = Arc::new(OwnerRateLimitHandle::new(owner_id, Arc::clone(&owner_mgr)));
        let owner_stats = Arc::new(RateLimitStatsAccumulator::new());

        let echo = spawn_udp_echo().await;
        let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await.unwrap();
        let listen_port = probe.local_addr().unwrap().port();
        drop(probe);

        let stats = RuleStats::new();
        let cancel = CancellationToken::new();
        let cancel_run = cancel.clone();
        let stats_run = Arc::clone(&stats);
        let task_owner = Arc::clone(&owner_limiter);
        let task_owner_stats = Arc::clone(&owner_stats);
        let task = tokio::spawn(async move {
            run_listener(
                RuleId(901),
                listen_port,
                Target::Ip(echo.ip()),
                echo.port(),
                false,
                1024,
                Duration::ZERO,
                stats_run,
                test_resolver(),
                cancel_run,
                None,
                None,
                Some(task_owner),
                Some(task_owner_stats),
                None,
            )
            .await;
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Source A: first packet admits, flow stays alive.
        let a = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        a.send_to(b"hello-a", (Ipv4Addr::LOCALHOST, listen_port))
            .await
            .unwrap();
        let mut buf = [0u8; 64];
        let (n, _) = tokio::time::timeout(Duration::from_secs(2), a.recv_from(&mut buf))
            .await
            .expect("first source must round-trip")
            .unwrap();
        assert_eq!(&buf[..n], b"hello-a");

        assert_eq!(
            owner_limiter.active_connections(),
            1,
            "first flow occupies the slot"
        );
        assert_eq!(owner_stats.reject_total(RejectReason::OwnerConcurrent), 0);

        // Source B: first packet must drop on the owner concurrent gate.
        let b = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        b.send_to(b"hello-b", (Ipv4Addr::LOCALHOST, listen_port))
            .await
            .unwrap();
        let dropped = tokio::time::timeout(Duration::from_millis(300), b.recv_from(&mut buf)).await;
        assert!(dropped.is_err(), "source B must be dropped, not echoed");

        assert_eq!(
            owner_limiter.active_connections(),
            1,
            "still only source A active"
        );
        assert_eq!(
            owner_stats.reject_total(RejectReason::OwnerConcurrent),
            1,
            "source B increments OwnerConcurrent reject counter"
        );

        cancel.cancel();
        task.await.unwrap();
    }
}

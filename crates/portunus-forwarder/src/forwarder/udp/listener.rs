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
//!   9. first-packet `send().await`, classified per FR-006/FR-007.
//!      (Fresh tokio `UdpSocket` returns spurious `WouldBlock` from
//!      `try_send` until the reactor observes writability; the cold
//!      path awaits writability once so the first datagram is durable.
//!      Subsequent fast-path sends keep `try_send` semantics.)
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
use crate::forwarder::udp::batch::{BatchBufs, recv_batch, send_batch_connected};
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
///
/// On Linux this loop uses batched `recvmmsg(2)` / `sendmmsg(2)`
/// syscalls (see `batch.rs`) to amortize per-packet syscall cost.
/// Run-length grouping by flow means a chatty single-flow workload
/// (e.g. iperf3 UDP) collapses 1 batch of N packets into 1 recvmmsg +
/// 1 sendmmsg. Mixed-flow batches flush per-flow runs as they appear;
/// cold-path (new-flow) packets fall back to the single-packet
/// `handle_datagram` path so the FR-004 admission order stays
/// authoritative.
///
/// On non-Linux platforms the batched helpers return `WouldBlock` and
/// the loop falls through to the original `recv_from` single-packet
/// path, unchanged from v0.4.
pub async fn run_listener<R: Resolve + 'static>(
    cfg: ListenerConfig<R>,
    listener_socket: Arc<UdpSocket>,
) {
    let mut bufs = BatchBufs::new();
    let mut fallback_buf = vec![0u8; UDP_BUFFER_BYTES];
    loop {
        tokio::select! {
            () = cfg.cancel.cancelled() => {
                break;
            }
            ready = listener_socket.readable() => {
                if let Err(e) = ready {
                    warn!(
                        event = "rule.udp_listener_recv_failed",
                        rule_id = %cfg.rule_id,
                        listen_port = cfg.listen_port,
                        error = %e,
                    );
                    continue;
                }
                // Try the batched path first. On Linux this calls
                // recvmmsg; on other platforms (or on any error) we
                // fall back to single-packet recv_from below.
                match recv_batch(&listener_socket, &mut bufs) {
                    Ok(0) => {} // spurious wakeup → just re-loop
                    Ok(n) => {
                        process_batch(&cfg, &listener_socket, &bufs, n).await;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        // Either the readiness event was spurious or
                        // we're on a platform without recvmmsg. Try
                        // a single-packet recv_from once, then loop.
                        match listener_socket.try_recv_from(&mut fallback_buf) {
                            Ok((n, src)) => {
                                handle_datagram(
                                    &cfg,
                                    &listener_socket,
                                    &fallback_buf[..n],
                                    src,
                                ).await;
                            }
                            Err(e2) if e2.kind() == std::io::ErrorKind::WouldBlock => {
                                // Spurious — fall through to next loop iter.
                            }
                            Err(e2) => {
                                warn!(
                                    event = "rule.udp_listener_recv_failed",
                                    rule_id = %cfg.rule_id,
                                    listen_port = cfg.listen_port,
                                    error = %e2,
                                );
                            }
                        }
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
    }
    info!(
        event = "rule.udp_listener_drained",
        rule_id = %cfg.rule_id,
        listen_port = cfg.listen_port,
    );
}

/// Iterate the `n` datagrams freshly delivered by `recv_batch` and
/// dispatch them to the correct path. Consecutive packets that belong
/// to the same live flow are accumulated into a single `sendmmsg`
/// call; flow changes / cold-path packets flush the pending run
/// first.
///
/// Why run-length grouping (not full hashmap-grouping):
/// * Common workloads cluster temporally — iperf3 UDP is single-flow,
///   NAT-style multi-flow tends to interleave in bursts. Run-length
///   captures both with O(batch) work and zero allocations on the
///   hot path.
/// * Reordering: UDP gives no ordering guarantee in general, but
///   sendmmsg preserves the order *within* a single syscall, and the
///   per-flow groups we emit match the order packets arrived in. So
///   we never reorder packets that share a flow.
async fn process_batch<R: Resolve + 'static>(
    cfg: &ListenerConfig<R>,
    listener_socket: &Arc<UdpSocket>,
    bufs: &BatchBufs,
    n: usize,
) {
    // Pending run: shared flow + indices of slots queued for a
    // single sendmmsg. Indices are into `bufs`; sizes are pre-cast to
    // u64 so the flush path doesn't redo the conversion.
    let mut pending: Option<PendingRun> = None;

    for i in 0..n {
        let (payload, src) = bufs.slot(i);
        let key = FlowKey::new(cfg.listen_port, src);
        let payload_len = payload.len();
        let n_u64 = u64::try_from(payload_len).unwrap_or(u64::MAX);

        // Try the fast path: existing Live flow. If we hit it, we may
        // be able to extend a pending run for the same flow.
        if let Some(flow) = cfg.registry.get(key) {
            if flow.cancel.is_cancelled() {
                // Flush previous run, then route this packet through
                // the cold path so the next datagram for this source
                // rebuilds the flow.
                if let Some(run) = pending.take() {
                    flush_run(cfg, bufs, run).await;
                }
                handle_datagram(cfg, listener_socket, payload, src).await;
                continue;
            }
            // Eager-debit the quota BEFORE adding to the pending run.
            // This closes the v1.5.0 hole where batch-build could let
            // up to `batch_size - 1` over-budget packets through
            // because `quota_allows()` only flipped after the late
            // `quota_consume_after_send`. If the debit straddles the
            // boundary, drop this packet (FR-013 silent drop) and
            // every subsequent packet on this flow this batch — but
            // do NOT skip cold-path handling for other flows in the
            // batch.
            if !flow.quota_try_consume(n_u64) {
                continue;
            }
            match &mut pending {
                Some(run) if Arc::ptr_eq(&run.flow.upstream_socket, &flow.upstream_socket) => {
                    run.indices.push(i);
                    run.sizes.push(n_u64);
                    continue;
                }
                _ => {
                    if let Some(run) = pending.take() {
                        flush_run(cfg, bufs, run).await;
                    }
                    pending = Some(PendingRun {
                        flow,
                        indices: vec![i],
                        sizes: vec![n_u64],
                    });
                    continue;
                }
            }
        }

        // Cold path: flush any pending run, then dispatch the new
        // flow through the full FR-004 admission sequence.
        if let Some(run) = pending.take() {
            flush_run(cfg, bufs, run).await;
        }
        handle_datagram(cfg, listener_socket, payload, src).await;
    }

    if let Some(run) = pending.take() {
        flush_run(cfg, bufs, run).await;
    }
}

/// A run of consecutive datagrams (from `process_batch`) that all
/// belong to the same live flow. Flushed via `sendmmsg(2)` on Linux.
struct PendingRun {
    flow: Arc<UdpFlow>,
    /// Indices into `BatchBufs` for the payloads to send.
    indices: Vec<usize>,
    /// Pre-cast byte counts, parallel to `indices`. Saves a re-cast
    /// in the success path.
    sizes: Vec<u64>,
}

/// Send a pending run of packets via batched `sendmmsg` on Linux,
/// falling back to per-packet `try_send` when the batch helper is
/// unavailable (non-Linux) or the kernel reports WouldBlock for the
/// first packet (avoids re-doing the syscall for the whole batch).
async fn flush_run<R: Resolve + 'static>(
    cfg: &ListenerConfig<R>,
    bufs: &BatchBufs,
    run: PendingRun,
) {
    let PendingRun {
        flow,
        indices,
        sizes,
    } = run;
    debug_assert_eq!(indices.len(), sizes.len());
    debug_assert!(!indices.is_empty());

    // Build the payload-slice array. All slices borrow from `bufs`,
    // which lives for the duration of `process_batch` — fine.
    let payloads: Vec<&[u8]> = indices.iter().map(|&i| bufs.slot(i).0).collect();

    match send_batch_connected(&flow.upstream_socket, &payloads) {
        Ok(sent) => {
            // Account for the prefix that the kernel accepted. Quota
            // was eagerly debited in process_batch, so no consume
            // call here — just stats + last_seen.
            for s in sizes.iter().take(sent) {
                flow.bump_inbound(*s).await;
                cfg.stats.inc_datagram_in(cfg.listen_port, *s);
            }
            if sent < indices.len() {
                // Refund the eagerly-debited quota for the unsent
                // tail so per-batch over-debit stays at 0.
                for s in sizes.iter().skip(sent) {
                    flow.quota_restore(*s);
                }
                // Probe-after-partial: sendmmsg drops the tail with
                // no errno, so we don't know whether the kernel
                // bailed on SO_SNDBUF (WouldBlock) or on an ICMP
                // error queue (Evict). Issue one synchronous
                // try_send on the first unsent packet to recover the
                // errno; classify it the same way the fast path
                // does. This keeps v0.4 parity on ICMP-driven
                // eviction (≈ one extra syscall per partial-send
                // event, which is rare).
                let probe_idx = indices[sent];
                let (probe_payload, probe_src) = bufs.slot(probe_idx);
                let probe_size = sizes[sent];
                match flow.upstream_socket.try_send(probe_payload) {
                    Ok(_) => {
                        flow.bump_inbound(probe_size).await;
                        cfg.stats.inc_datagram_in(cfg.listen_port, probe_size);
                        // We refunded probe_size above; re-debit it.
                        let _ = flow.quota_try_consume(probe_size);
                        // Remaining tail (sent + 1 ..) stays dropped;
                        // their quota refunds are correct.
                    }
                    Err(e) => match classify_udp_error(&e) {
                        UdpAction::Evict => {
                            cfg.stats.errors.inc_icmp_evict();
                            info!(
                                event = "rule.udp_flow_evicted_icmp",
                                rule_id = %cfg.rule_id,
                                listen_port = cfg.listen_port,
                                source = %probe_src,
                                error = %e,
                            );
                            let key = FlowKey::new(cfg.listen_port, probe_src);
                            let _ = cfg.registry.remove(key);
                            flow.cancel.cancel();
                        }
                        UdpAction::MessageTooLarge => {
                            cfg.stats.errors.inc_emsgsize();
                            debug!(
                                event = "rule.udp_emsgsize",
                                rule_id = %cfg.rule_id,
                                listen_port = cfg.listen_port,
                                source = %probe_src,
                            );
                        }
                        UdpAction::WouldBlock | UdpAction::Transient => {
                            trace!(
                                event = "rule.udp_upstream_wouldblock",
                                rule_id = %cfg.rule_id,
                                listen_port = cfg.listen_port,
                                dropped = indices.len() - sent,
                            );
                        }
                    },
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
            // Zero packets accepted. Refund everything we
            // eagerly-debited, then fall back to per-packet
            // try_send (which re-debits via quota_consume_after_send
            // on success). This preserves the single-packet
            // WouldBlock semantics from v0.4.
            for s in &sizes {
                flow.quota_restore(*s);
            }
            for (idx, n_u64) in indices.iter().zip(sizes.iter()) {
                let (payload, _src) = bufs.slot(*idx);
                send_single(cfg, &flow, payload, *n_u64).await;
            }
        }
        Err(e) => {
            // Total failure with a non-WouldBlock errno (typically
            // ICMP-class on the FIRST packet — sendmmsg short-
            // circuits before sending anything). Refund the whole
            // run, then classify.
            for s in &sizes {
                flow.quota_restore(*s);
            }
            match classify_udp_error(&e) {
                UdpAction::Evict => {
                    cfg.stats.errors.inc_icmp_evict();
                    info!(
                        event = "rule.udp_flow_evicted_icmp",
                        rule_id = %cfg.rule_id,
                        listen_port = cfg.listen_port,
                        source = %flow.source_addr,
                        error = %e,
                    );
                    let (_payload, src) = bufs.slot(indices[0]);
                    let key = FlowKey::new(cfg.listen_port, src);
                    let _ = cfg.registry.remove(key);
                    flow.cancel.cancel();
                }
                UdpAction::MessageTooLarge => {
                    cfg.stats.errors.inc_emsgsize();
                    debug!(
                        event = "rule.udp_emsgsize",
                        rule_id = %cfg.rule_id,
                        listen_port = cfg.listen_port,
                        source = %flow.source_addr,
                    );
                }
                UdpAction::WouldBlock | UdpAction::Transient => {
                    // Defensive — WouldBlock was matched above, but
                    // classifier owns the source of truth.
                }
            }
        }
    }
}

/// Single-packet send used by both the in-batch flush fallback and
/// the cold path. Mirrors the fast-path arm of `handle_datagram`.
async fn send_single<R: Resolve + 'static>(
    cfg: &ListenerConfig<R>,
    flow: &Arc<UdpFlow>,
    payload: &[u8],
    n_u64: u64,
) {
    match flow.upstream_socket.try_send(payload) {
        Ok(_) => {
            flow.bump_inbound(n_u64).await;
            cfg.stats.inc_datagram_in(cfg.listen_port, n_u64);
            let _ = flow.quota_consume_after_send(n_u64);
        }
        Err(_e) => {
            // Drop; next packet will hit `handle_datagram` and
            // classify properly. We avoid re-classifying here to keep
            // this helper allocation- and branch-light.
        }
    }
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
    if let Some(flow) = cfg.registry.get(key) {
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
                        cfg.stats.errors.inc_icmp_evict();
                        info!(
                            event = "rule.udp_flow_evicted_icmp",
                            rule_id = %cfg.rule_id,
                            listen_port = cfg.listen_port,
                            source = %src,
                            error = %e,
                        );
                        let _ = cfg.registry.remove(key);
                        flow.cancel.cancel();
                    }
                    UdpAction::MessageTooLarge => {
                        cfg.stats.errors.inc_emsgsize();
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
    let reservation = match cfg.registry.try_get_or_reserve(key) {
        TryGetOrReserve::Existing(flow) => {
            // Rare: another listener committed for the same (port, src)
            // between our `get` above and `try_get_or_reserve` here.
            // Treat as if we'd seen the Live flow on the fast path —
            // forward the current datagram through it. Use `send().await`
            // (not `try_send`): the racing cold path may not yet have
            // issued its own first send, so the socket can still be
            // pre-reactor-writability — same race as cold-path step 9.
            if !flow.quota_allows() {
                return;
            }
            if flow.upstream_socket.send(payload).await.is_ok() {
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
                cfg.stats.errors.inc_upstream_connect_failed();
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
    cfg.registry.commit(reservation, Arc::clone(&flow));

    // (8) Hand the flow to the demux. Channel full = back-pressured
    //     demux — rollback so we don't leave a Live slot the demux
    //     never sees (FR-005 invariant).
    if let Err(send_err) = cfg.demux_tx.try_send(DemuxCommand::AddFlow {
        key,
        flow: Arc::clone(&flow),
    }) {
        cfg.stats.errors.inc_addflow_dropped();
        warn!(
            event = "rule.udp_addflow_dropped",
            rule_id = %cfg.rule_id,
            listen_port = cfg.listen_port,
            source = %src,
            reason = ?send_err,
        );
        let _ = cfg.registry.remove(key);
        flow.cancel.cancel();
        return;
    }

    // (9) First-packet send. `send().await` (not `try_send`): a freshly
    //     bind+connect-ed tokio `UdpSocket` has no reactor writability
    //     event yet, so `try_send` returns spurious WouldBlock and the
    //     first datagram of every flow is lost. `send().await` registers
    //     interest and resolves on the next poll. Classify per
    //     FR-006/FR-007: Evict tears the flow down without counting
    //     `datagram_in`; EMSGSIZE / Transient drop this datagram but
    //     keep the flow — demux ReadWait is armed, next packet hits the
    //     fast path.
    match upstream_socket.send(payload).await {
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
                cfg.stats.errors.inc_icmp_evict();
                info!(
                    event = "rule.udp_flow_evicted_icmp",
                    rule_id = %cfg.rule_id,
                    listen_port = cfg.listen_port,
                    source = %src,
                    target = %chosen_target,
                    error = %e,
                    phase = "first_packet",
                );
                let _ = cfg.registry.remove(key);
                flow.cancel.cancel();
            }
            // WouldBlock is unreachable after the `send().await` switch
            // (tokio loops internally on writability), but kept in the
            // pattern as defensive symmetry with the fast-path arms in
            // case the classifier or socket type changes.
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
        assert!(registry.get(key).is_none());
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
            registry.get(k3).is_none(),
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
            registry.get(k4).is_some(),
            "4th source must be admitted after a slot frees up",
        );
        assert_eq!(
            rate_limit.active_connections(),
            2,
            "active_connections must be back at the 2-cap ceiling",
        );
    }
}

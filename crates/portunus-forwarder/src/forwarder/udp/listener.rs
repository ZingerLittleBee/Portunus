//! Per-listener UDP recv loop for the 014-udp-centralized-demux
//! runtime. Owns the listen socket's `recv_from` loop and, on each
//! datagram, applies the FR-004 cold-path admission order. The order is
//! preserved, but the *split* between synchronous and deferred work
//! changed in the #53/#54 hardening pass:
//!
//! Synchronous, in the recv loop (`handle_datagram`), so a spoofed-source
//! flood is bounded by cheap admission BEFORE any allocation or task
//! spawn:
//!   1. existing-flow fast path (registry `get` → quota check →
//!      `try_send` → classify);
//!   2. quota exhaustion short-circuit;
//!   3. per-rule cap reservation (`registry.try_get_or_reserve`) —
//!      `Reserved` proceeds, `CapExhausted` counts `flows_dropped_overflow`,
//!      `Pending` (same-key race) counts `flows_pending_drops` (#54),
//!      `Existing` (Live race) forwards inline.
//!
//! Deferred to a detached task (`complete_cold_flow`) for BOTH IP and DNS
//! targets, so a new-flow burst never head-of-line blocks same-batch
//! datagrams of already-established flows (#53 — see the dispatcher note
//! in `handle_datagram`; only after a `Reserved` outcome do we pay the
//! `payload.to_vec()` + spawn):
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
//! The `Reservation` (RAII) is minted synchronously in `handle_datagram`
//! and moved into the task; it releases the `Slot::Pending` on every
//! early return until `commit`, so a task that is dropped before running
//! (e.g. runtime shutdown) still frees the slot.
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
use crate::forwarder::udp::registry::{FlowKey, Reservation, TryGetOrReserve, UdpFlowRegistry};
use crate::resolver::{LiveResolver, Resolve};

/// IP-layer UDP payload ceiling (FR-013). One static heap buffer per
/// recv loop sized to this value means `recv_from` cannot truncate any
/// well-formed datagram at the proxy layer.
const UDP_BUFFER_BYTES: usize = 65_535;

/// Max datagrams the non-Linux single-packet fallback drains per
/// readiness wake before yielding to the `select!` (so a sustained UDP
/// flood can't starve the `cancel` branch). Sized to match the Linux
/// `recvmmsg` batch so both paths bound per-wake work similarly.
const UDP_DRAIN_PER_WAKE: usize = 32;

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
    // Issue #49: allocate the ~2 MiB batch arena and the 64 KiB
    // single-packet fallback buffer lazily, on this port's first
    // readiness/traffic — NOT at spawn. A large range rule (e.g. 1000
    // listen ports) otherwise pays ~2 GiB up front for ports that may
    // never see a datagram. Idle ports now cost nothing. The
    // steady-state hot path adds one `Option` check (branch-predicted
    // taken after the first wake) per readiness wake.
    let mut bufs: Option<BatchBufs> = None;
    let mut fallback_buf: Option<Vec<u8>> = None;
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
                    // Brief backoff: a persistent readiness error would
                    // otherwise spin this loop at 100% CPU with a log
                    // flood. Mirrors the SNI/TCP accept-error pattern.
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
                // Lazily materialize the batch arena on first readiness
                // for this port (issue #49).
                let bufs = bufs.get_or_insert_with(BatchBufs::new);
                // Try the batched path first. On Linux this calls
                // recvmmsg; on other platforms (or on any error) we
                // fall back to single-packet recv_from below.
                match recv_batch(&listener_socket, bufs) {
                    Ok(0) => {} // spurious wakeup → just re-loop
                    Ok(n) => {
                        process_batch(&cfg, &listener_socket, bufs, n).await;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        // Either the readiness event was spurious or
                        // we're on a platform without recvmmsg. Drain
                        // the socket with single-packet recv_from until
                        // it would block (or we hit the per-wake cap),
                        // so one readiness wake processes the whole
                        // backlog instead of one datagram per wake —
                        // which would otherwise cost a select round-trip
                        // per packet on non-Linux platforms. The cap
                        // mirrors the Linux batch size so a sustained
                        // flood can't starve the `cancel` branch.
                        // Lazily materialize the fallback buffer too (#49).
                        let fallback_buf =
                            fallback_buf.get_or_insert_with(|| vec![0u8; UDP_BUFFER_BYTES]);
                        let mut drained = 0usize;
                        loop {
                            match listener_socket.try_recv_from(fallback_buf) {
                                Ok((n, src)) => {
                                    handle_datagram(
                                        &cfg,
                                        &listener_socket,
                                        &fallback_buf[..n],
                                        src,
                                    ).await;
                                    drained += 1;
                                    if drained >= UDP_DRAIN_PER_WAKE {
                                        // Yield back to the select loop
                                        // so cancellation stays responsive.
                                        break;
                                    }
                                }
                                Err(e2) if e2.kind() == std::io::ErrorKind::WouldBlock => {
                                    // Socket drained — back to the select.
                                    break;
                                }
                                Err(e2) => {
                                    warn!(
                                        event = "rule.udp_listener_recv_failed",
                                        rule_id = %cfg.rule_id,
                                        listen_port = cfg.listen_port,
                                        error = %e2,
                                    );
                                    // Brief backoff so a persistent error
                                    // doesn't spin this drain loop.
                                    tokio::time::sleep(Duration::from_millis(50)).await;
                                    break;
                                }
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
                        // Brief backoff: a persistent non-WouldBlock
                        // recv error would otherwise spin this loop at
                        // 100% CPU with a log flood.
                        tokio::time::sleep(Duration::from_millis(50)).await;
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

    // ---- Cold path admission (synchronous, cheap — run BEFORE any
    //      allocation or task spawn) ----
    //
    // Issue #53/#54: the pre-fix path copied the payload (`to_vec`, up to
    // 64 KiB) and either spawned a task (DNS) or ran the whole setup
    // inline (IP) BEFORE the flow-cap reservation and rate-limit gate. A
    // spoofed-source flood therefore bought one alloc + one spawn per
    // packet, uncapped. We now hoist the two cheap synchronous admission
    // steps (quota short-circuit, cap reservation — both sync; the
    // reservation is a DashMap op) here, so an over-cap / over-budget
    // datagram is dropped without paying for the copy or the task.

    // (FR-004 step 2) Quota exhaustion short-circuit.
    if let Some(q) = cfg.quota.as_ref()
        && q.is_exhausted()
    {
        return;
    }

    // (FR-004 step 3) Reserve a per-rule cap slot. RAII: dropping the
    // reservation without `commit` releases the slot.
    let reservation = match cfg.registry.try_get_or_reserve(key) {
        TryGetOrReserve::Existing(flow) => {
            // Rare: another cold path committed a Live flow for this exact
            // (port, src) between our fast-path `get` above and here.
            // Forward this datagram through it, same as observing the Live
            // flow on the fast path. `send().await` (not `try_send`): the
            // racing cold path may not yet have issued its own first send,
            // so the socket can still be pre-reactor-writability (same race
            // as cold-path step 9).
            if flow.quota_allows() && flow.upstream_socket.send(payload).await.is_ok() {
                flow.bump_inbound(n_u64).await;
                cfg.stats.inc_datagram_in(cfg.listen_port, n_u64);
                let _ = flow.quota_consume_after_send(n_u64);
            }
            // On error / exhausted quota: drop this datagram; the next one
            // hits the fast path and classifies the error there.
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
        TryGetOrReserve::Pending => {
            // A concurrent cold path already holds the Pending slot for
            // this exact (port, src). The rule is NOT at cap — count this
            // separately from overflow (issue #54) and drop the datagram;
            // the next one from this source hits the now-Live fast path.
            cfg.stats.errors.inc_flows_pending_drops();
            debug!(
                event = "rule.udp_flow_pending_drop",
                rule_id = %cfg.rule_id,
                listen_port = cfg.listen_port,
                source = %src,
            );
            return;
        }
    };

    // ---- Cold path (deferred) ----
    //
    // Only now — after a successful reservation — do we pay for the
    // payload copy and the task spawn. Building a new flow means a
    // resolver lookup, an upstream bind+connect, and a first-packet
    // `send().await` (a reactor writability round-trip on a fresh
    // socket). Running any of that inline in the recv loop would
    // head-of-line block same-batch datagrams of every *other*
    // already-established flow behind one brand-new flow.
    //
    // NOTE (behaviour change from v1.5.x): IP targets used to run this
    // setup inline (they resolve synchronously and UDP-`connect` without
    // a network round-trip). They now ALSO defer to a task — the inline
    // first-packet `send().await` still cost a fresh-socket writability
    // round-trip, which HOL-blocked the loop under new-flow bursts. Both
    // target kinds now spawn; the reservation was already taken above so
    // the admission ORDER (FR-004) is preserved. The reservation is moved
    // into the task and released via Drop on any early return before
    // `commit`.
    let ctx = ColdPathCtx::from_listener(cfg);
    let payload_owned = payload.to_vec();
    tokio::spawn(complete_cold_flow(
        ctx,
        reservation,
        key,
        src,
        payload_owned,
    ));

    // `listener_socket` is held by the runtime, but we reference it via
    // the parameter to keep the signature stable for unit tests that
    // pass a hand-rolled socket; suppress the unused-binding warning
    // here without leaking the binding to callers.
    let _ = listener_socket;
}

/// Owned snapshot of the listener state the cold path needs, so the
/// FR-004 flow-setup sequence can run in a detached task (DNS targets)
/// without borrowing `ListenerConfig`. Every field is `Copy` or an
/// `Arc`/`mpsc::Sender` clone — cheap to build per new flow.
struct ColdPathCtx<R: Resolve + 'static> {
    rule_id: RuleId,
    listen_port: u16,
    target: Target,
    target_port: u16,
    prefer_ipv6: bool,
    registry: Arc<UdpFlowRegistry>,
    demux_tx: mpsc::Sender<DemuxCommand>,
    stats: Arc<RuleStats>,
    resolver: Arc<LiveResolver<R>>,
    rate_limit: Option<Arc<RuleRateLimitHandle>>,
    rate_limit_stats: Option<Arc<RateLimitStatsAccumulator>>,
    owner_rate_limit: Option<Arc<OwnerRateLimitHandle>>,
    owner_rate_limit_stats: Option<Arc<RateLimitStatsAccumulator>>,
    quota: Option<Arc<QuotaHandle>>,
}

impl<R: Resolve + 'static> ColdPathCtx<R> {
    fn from_listener(cfg: &ListenerConfig<R>) -> Self {
        Self {
            rule_id: cfg.rule_id,
            listen_port: cfg.listen_port,
            target: cfg.target.clone(),
            target_port: cfg.target_port,
            prefer_ipv6: cfg.prefer_ipv6,
            registry: Arc::clone(&cfg.registry),
            demux_tx: cfg.demux_tx.clone(),
            stats: Arc::clone(&cfg.stats),
            resolver: Arc::clone(&cfg.resolver),
            rate_limit: cfg.rate_limit.clone(),
            rate_limit_stats: cfg.rate_limit_stats.clone(),
            owner_rate_limit: cfg.owner_rate_limit.clone(),
            owner_rate_limit_stats: cfg.owner_rate_limit_stats.clone(),
            quota: cfg.quota.clone(),
        }
    }
}

/// FR-004 cold-path flow setup (steps 4–9), factored out of
/// `handle_datagram` and always run in a detached task (see the
/// dispatcher in `handle_datagram`). Steps 2 (quota short-circuit) and 3
/// (cap reservation) now run synchronously in `handle_datagram` BEFORE
/// the spawn (#53/#54); this function receives the already-held
/// [`Reservation`]. It operates on an owned [`ColdPathCtx`] instead of
/// borrowing `ListenerConfig`, and the payload is owned so both cross the
/// `tokio::spawn` boundary.
///
/// The `reservation` is RAII: every early return below drops it, which
/// releases the `Slot::Pending`. `commit` (step 7) marks it committed so
/// its Drop becomes a no-op.
async fn complete_cold_flow<R: Resolve + 'static>(
    ctx: ColdPathCtx<R>,
    reservation: Reservation,
    key: FlowKey,
    src: SocketAddr,
    payload: Vec<u8>,
) {
    let n_u64 = u64::try_from(payload.len()).unwrap_or(u64::MAX);

    // (4) Layered owner+rule rate-limit gate. Owner first per FR-013.
    //     Reject → silent drop, RAII releases the reservation. On
    //     admit, the returned guards ride the `UdpFlow` Arc for the
    //     flow's lifetime (v0.11 `concurrent_connections` cap).
    let admit_guards = match acquire_first_packet(&ctx, src) {
        AdmitOutcome::Allowed { guards } => guards,
        AdmitOutcome::Rejected => return,
    };

    // (5) Resolve target. Single SocketAddr for IP targets;
    //     multi-A ordered list for DNS targets.
    let resolved = match ctx
        .resolver
        .resolve_target(ctx.rule_id, &ctx.target, ctx.target_port, ctx.prefer_ipv6)
        .await
    {
        Ok((addrs, _src)) if !addrs.is_empty() => addrs,
        Ok((_, _)) => {
            ctx.stats.inc_dns_failure();
            warn!(
                event = "rule.udp_dns_failed",
                rule_id = %ctx.rule_id,
                listen_port = ctx.listen_port,
                source = %src,
                reason = "empty",
            );
            return;
        }
        Err(err) => {
            ctx.stats.inc_dns_failure();
            warn!(
                event = "rule.udp_dns_failed",
                rule_id = %ctx.rule_id,
                listen_port = ctx.listen_port,
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
                    rule_id = %ctx.rule_id,
                    listen_port = ctx.listen_port,
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
                ctx.stats.errors.inc_upstream_connect_failed();
                warn!(
                    event = "rule.udp_upstream_connect_failed",
                    rule_id = %ctx.rule_id,
                    listen_port = ctx.listen_port,
                    source = %src,
                    target = %addr,
                    error = %e,
                );
            }
        }
    }
    let Some((upstream_socket, chosen_target)) = selected else {
        ctx.stats.inc_dns_failure();
        warn!(
            event = "rule.udp_dns_failed",
            rule_id = %ctx.rule_id,
            listen_port = ctx.listen_port,
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
    if let Some(q) = ctx.quota.as_ref() {
        flow = flow.attach_quota(Arc::clone(q));
    }
    flow.attach_admit_guards(admit_guards).await;
    ctx.registry.commit(reservation, Arc::clone(&flow));

    // (8) Hand the flow to the demux. Channel full = back-pressured
    //     demux — rollback so we don't leave a Live slot the demux
    //     never sees (FR-005 invariant).
    if let Err(send_err) = ctx.demux_tx.try_send(DemuxCommand::AddFlow {
        key,
        flow: Arc::clone(&flow),
    }) {
        ctx.stats.errors.inc_addflow_dropped();
        warn!(
            event = "rule.udp_addflow_dropped",
            rule_id = %ctx.rule_id,
            listen_port = ctx.listen_port,
            source = %src,
            reason = ?send_err,
        );
        let _ = ctx.registry.remove(key);
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
    match upstream_socket.send(&payload).await {
        Ok(_) => {
            flow.bump_inbound(n_u64).await;
            ctx.stats.inc_datagram_in(ctx.listen_port, n_u64);
            let _ = flow.quota_consume_after_send(n_u64);
            info!(
                event = "rule.udp_flow_opened",
                rule_id = %ctx.rule_id,
                listen_port = ctx.listen_port,
                source = %src,
                target = %chosen_target,
            );
        }
        Err(e) => match classify_udp_error(&e) {
            UdpAction::Evict => {
                ctx.stats.errors.inc_icmp_evict();
                info!(
                    event = "rule.udp_flow_evicted_icmp",
                    rule_id = %ctx.rule_id,
                    listen_port = ctx.listen_port,
                    source = %src,
                    target = %chosen_target,
                    error = %e,
                    phase = "first_packet",
                );
                let _ = ctx.registry.remove(key);
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
                    rule_id = %ctx.rule_id,
                    listen_port = ctx.listen_port,
                    source = %src,
                    target = %chosen_target,
                    action = ?action,
                    error = %e,
                );
                info!(
                    event = "rule.udp_flow_opened",
                    rule_id = %ctx.rule_id,
                    listen_port = ctx.listen_port,
                    source = %src,
                    target = %chosen_target,
                );
            }
        },
    }
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
    ctx: &ColdPathCtx<R>,
    src: SocketAddr,
) -> AdmitOutcome {
    match try_acquire_layered(ctx.owner_rate_limit.as_ref(), ctx.rate_limit.as_ref(), true) {
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
            if let Some(s) = ctx.owner_rate_limit_stats.as_deref() {
                s.record_reject(reason);
            }
            warn!(
                event = "rule.udp_first_packet_rejected",
                rule_id = %ctx.rule_id,
                listen_port = ctx.listen_port,
                source = %src,
                scope = "owner",
                reason = ?reason,
            );
            AdmitOutcome::Rejected
        }
        LayeredAcquire::RuleRejected(reason) => {
            if let Some(s) = ctx.rate_limit_stats.as_deref() {
                s.record_reject(reason);
            }
            warn!(
                event = "rule.udp_first_packet_rejected",
                rule_id = %ctx.rule_id,
                listen_port = ctx.listen_port,
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

    /// Poll `cond` every 10 ms until it returns true or `budget` elapses.
    /// The cold path now completes flow setup (and any rollback) in a
    /// detached task, so tests that drive `handle_datagram` directly must
    /// wait for that task rather than assert synchronously. The generous
    /// budget keeps this non-flaky under CI scheduling jitter — it only
    /// ever lengthens the wait, never shortens the settle window.
    async fn wait_until(budget: Duration, mut cond: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + budget;
        while std::time::Instant::now() < deadline {
            if cond() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
            tokio::task::yield_now().await;
        }
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

        // The cold-path flow setup now runs in a detached task (both IP
        // and DNS targets defer — #53). `handle_datagram` returns after
        // taking the reservation synchronously; the AddFlow attempt (and
        // its rollback) happen in the spawned task. Poll until the slot
        // is released (generous budget → no flakiness under CI load).
        wait_until(Duration::from_secs(2), || registry.is_empty()).await;

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
        // One of the three must reject at the rate-limit gate; no flow
        // commits for it.
        handle_datagram(&cfg, &listener_sock, b"d3", src3).await;
        // Flow setup and the rejected flow's reservation-Drop now run in
        // detached tasks (#53), so cross-flow admission is scheduling-
        // ordered, not arrival-ordered — WHICH of the three is rejected
        // is nondeterministic (only the reservation, not the rate-limit
        // grant, is arrival-ordered). Assert the invariant that holds
        // regardless: exactly two flows go live and the guard count caps
        // at 2. Wait for both admitted flows to reach the Live state
        // (`get` skips Pending) and the guard count to settle at 2.
        let keys: Vec<FlowKey> = [src1, src2, src3]
            .iter()
            .map(|s| FlowKey::new(listen_addr.port(), *s))
            .collect();
        let live_count = || keys.iter().filter(|k| registry.get(**k).is_some()).count();
        wait_until(Duration::from_secs(2), || {
            live_count() == 2 && rate_limit.active_connections() == 2
        })
        .await;

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
        // Exactly two of the three sources went live; one was rejected.
        let live_keys: Vec<FlowKey> = keys
            .iter()
            .copied()
            .filter(|k| registry.get(*k).is_some())
            .collect();
        assert_eq!(
            live_keys.len(),
            2,
            "exactly two of the three sources must be admitted",
        );

        // Drain pending AddFlow envelopes BEFORE removing the flow.
        // In production the demux task consumes these instantly; the
        // test must do the same so the only Arc<UdpFlow> ref count
        // for the removed flow left in play is the registry's.
        while let Ok(cmd) = demux_rx.try_recv() {
            drop(cmd);
        }

        // Drop one of the live flows → the guard inside
        // `UdpFlow.admit_guards` drops with the Arc, decrementing
        // `active_connections`.
        let removed = registry
            .remove(live_keys[0])
            .expect("a live flow should exist before remove");
        drop(removed);

        // Small yield to let any deferred Drop side effects settle.
        tokio::task::yield_now().await;

        assert_eq!(
            rate_limit.active_connections(),
            1,
            "dropping a flow must release one ActiveGuard",
        );

        // 4th source must now be admitted. Its flow is built in a
        // detached task, so wait for it to go Live.
        handle_datagram(&cfg, &listener_sock, b"d4", src4).await;

        let k4 = FlowKey::new(listen_addr.port(), src4);
        wait_until(Duration::from_secs(2), || registry.get(k4).is_some()).await;
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

    /// Issue #53/#54: the cold-path flow-cap reservation happens
    /// SYNCHRONOUSLY in `handle_datagram`, BEFORE the payload copy and
    /// the task spawn. Flood N distinct sources at `rule_cap = 1`: the
    /// first reserves the sole slot; the other N-1 must be rejected
    /// immediately (`CapExhausted` → `flows_dropped_overflow`) with NO
    /// flow built and NO task spawned for them. Because admission is
    /// synchronous, the drop counters are already settled the instant the
    /// (sequential) recv loop finishes — a task-side reservation could not
    /// guarantee that.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cold_path_reserves_before_spawn_under_flood() {
        let (listener_sock, listen_addr) = bind_loopback().await;
        let (_target_sock, target_addr) = bind_loopback().await;

        // rule_cap = 1: exactly one flow may be admitted rule-wide.
        let registry = UdpFlowRegistry::new(1);
        // Deep channel + live receiver so the ONE admitted flow's AddFlow
        // never fails (keeps the settled slot Live rather than rolled back).
        let (demux_tx, _demux_rx) = mpsc::channel::<DemuxCommand>(64);

        let stats = rule_stats_for(listen_addr.port());
        let cfg = ListenerConfig {
            rule_id: RuleId(9),
            listen_port: listen_addr.port(),
            target: Target::Ip(target_addr.ip()),
            target_port: target_addr.port(),
            prefer_ipv6: false,
            idle_window: Duration::from_secs(30),
            registry: Arc::clone(&registry),
            demux_tx,
            stats: Arc::clone(&stats),
            resolver: make_resolver(),
            rate_limit: None,
            rate_limit_stats: None,
            owner_rate_limit: None,
            owner_rate_limit_stats: None,
            quota: None,
            cancel: CancellationToken::new(),
        };

        const N: u16 = 6;
        let srcs: Vec<SocketAddr> = (0..N)
            .map(|i| SocketAddr::from(([127, 0, 0, 1], 61000 + i)))
            .collect();

        // Drive the recv loop sequentially (single task) so admission for
        // every source completes before we inspect the counters.
        for src in &srcs {
            handle_datagram(&cfg, &listener_sock, b"x", *src).await;
        }

        // Synchronous admission: exactly one slot reserved, N-1 dropped as
        // true cap overflow — counters are already settled, no await.
        assert_eq!(
            registry.len(),
            1,
            "exactly one flow may be reserved at rule_cap=1; got {}",
            registry.len()
        );
        assert_eq!(
            stats.snapshot_flows_dropped_overflow(),
            u64::from(N - 1),
            "the over-cap sources must count as cap overflow, not pending",
        );
        assert_eq!(
            registry.dropped_overflow(),
            u64::from(N - 1),
            "registry overflow counter must match the synchronous drops",
        );
        // No pending-collision drops here: distinct sources at a full cap
        // are overflow, not same-key races.
        assert_eq!(
            stats.errors.snapshot().flows_pending_drops,
            0,
            "distinct-source cap drops must NOT count as pending collisions",
        );
        // None of the over-cap sources built a flow.
        for src in srcs.iter().skip(1) {
            let k = FlowKey::new(listen_addr.port(), *src);
            assert!(
                registry.get(k).is_none(),
                "over-cap source {src} must not have a flow",
            );
        }
    }
}

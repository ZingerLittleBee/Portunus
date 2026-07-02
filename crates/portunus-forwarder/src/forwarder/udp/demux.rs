//! Per-rule reply demux task. Multiplexes all live upstream sockets
//! via `FuturesUnordered<ReadWait>`. Spec: 014-udp-centralized-demux,
//! FR-008 / FR-009 / FR-011 (drain step d/e).
//!
//! The demux owns one heap-allocated 64 KiB buffer that is reused
//! across every drain iteration; per-flow state is the cloned
//! `Arc<UdpFlow>` carried by each pending `ReadWait` future. A single
//! `tokio::select!` arms both the `DemuxCommand` channel and the
//! `FuturesUnordered` of live read-waits. On `Ready` the demux drains
//! up to [`DEMUX_FAIRNESS_BUDGET`] datagrams from the flow before
//! re-arming, preventing one chatty flow from starving the rest of
//! the rule's traffic.
//!
//! v1.6 reply-path batching (issue #46): on Linux the Ready drain uses
//! one `recvmmsg(2)` on the flow's connected upstream socket followed
//! by one `sendmmsg(2)` on the (unconnected) listener socket, instead
//! of up to 32 × (`try_recv` + `try_send_to`) — collapsing as many as
//! 64 syscalls per drained batch into 2. All datagrams in a drain
//! share the same destination (`key.src`), but the send still goes
//! through per-message `msg_name` so the listener socket stays
//! unconnected. On non-Linux platforms the batched helpers return
//! `WouldBlock` and the drain falls back to the per-packet loop that
//! has shipped since v1.5 (FR-008 semantics unchanged either way).
//! Note: the single-demux-task-per-rule ceiling remains — this change
//! is syscall batching only; demux sharding is follow-up work.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;

use futures_util::stream::{FuturesUnordered, StreamExt};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tracing::{debug, info, trace, warn};

use crate::forwarder::stats::RuleStats;
use crate::forwarder::udp::batch::{BATCH_SIZE, BatchBufs, recv_batch_connected, send_batch_to};
use crate::forwarder::udp::error::{UdpAction, classify_udp_error};
use crate::forwarder::udp::flow::UdpFlow;
use crate::forwarder::udp::registry::{FlowKey, UdpFlowRegistry};
use portunus_core::RuleId;

/// Demux drains at most this many datagrams per Ready before re-arming
/// the readable future, to keep one chatty flow from starving others
/// (FR-008).
pub const DEMUX_FAIRNESS_BUDGET: usize = 32;

// The batched drain issues ONE `recvmmsg` of up to `BATCH_SIZE`
// datagrams per Ready. Tie the two constants together at compile time
// so a future `BATCH_SIZE` bump cannot silently blow the fairness
// budget (and a budget bump keeps draining full batches).
const _: () = assert!(
    BATCH_SIZE <= DEMUX_FAIRNESS_BUDGET,
    "batched reply drain must not exceed the demux fairness budget"
);

/// Sized at protocol max so `try_recv` never truncates.
const RECV_BUFFER_BYTES: usize = 65_535;

/// Control-plane message sent to a running demux task.
pub enum DemuxCommand {
    /// Hand a freshly-built flow to the demux. The demux takes
    /// ownership of the cloned `Arc<UdpFlow>` for the lifetime of the
    /// flow; the cold path drops its own clone once the registry is
    /// committed.
    AddFlow { key: FlowKey, flow: Arc<UdpFlow> },
    /// Tear the demux down cleanly. All pending `ReadWait`s are dropped
    /// implicitly when the function returns.
    Shutdown,
}

/// Static configuration handed to [`run_demux`]. All fields are
/// `Arc`-shared so the demux task and the per-listener cold path can
/// see the same registry / listener sockets / stats counters.
pub struct DemuxConfig {
    pub rule_id: RuleId,
    pub registry: Arc<UdpFlowRegistry>,
    pub listener_sockets: Arc<HashMap<u16, Arc<UdpSocket>>>,
    pub stats: Arc<RuleStats>,
}

/// Run the reply-demux loop. Exits cleanly on `DemuxCommand::Shutdown`
/// or when the command channel is closed.
pub async fn run_demux(cfg: DemuxConfig, mut rx: mpsc::Receiver<DemuxCommand>) {
    let mut buf = vec![0u8; RECV_BUFFER_BYTES];
    // Reply-path batch arena (issue #46). One per demux task, reused
    // across every batched drain. On non-Linux it sits idle — the
    // batched drain reports "not handled" and the per-packet loop
    // below takes over.
    let mut bufs = BatchBufs::new();
    let mut readables: FuturesUnordered<ReadWaitFut> = FuturesUnordered::new();

    loop {
        tokio::select! {
            biased;
            cmd = rx.recv() => match cmd {
                Some(DemuxCommand::AddFlow { key, flow }) => {
                    readables.push(read_wait(key, flow));
                }
                Some(DemuxCommand::Shutdown) | None => break,
            },
            Some(outcome) = readables.next(), if !readables.is_empty() => match outcome {
                ReadOutcome::Ready { key, flow } => {
                    // Batched drain first (Linux recvmmsg/sendmmsg);
                    // falls back to the per-packet loop when the
                    // platform (or a spurious wake) reports WouldBlock
                    // before anything was read.
                    if !drain_one_flow_batched(&cfg, key, &flow, &mut bufs).await {
                        drain_one_flow(&cfg, key, &flow, &mut buf).await;
                    }
                    // Re-arm unless this flow was cancelled during drain
                    // (terminal Evict path).
                    if !flow.cancel.is_cancelled() {
                        readables.push(read_wait(key, flow));
                    }
                }
                ReadOutcome::Cancelled => {
                    // Drop the Arc; no re-arm.
                }
            },
        }
    }
}

/// Batched Ready-drain (issue #46): ONE `recvmmsg` of up to
/// [`BATCH_SIZE`] (== the fairness budget) datagrams from the flow's
/// connected upstream socket, then ONE `sendmmsg` on the listener
/// socket with every message addressed to `key.src` via `msg_name`
/// (the listener socket stays unconnected).
///
/// Returns `true` when the drain was handled here (datagrams
/// forwarded / dropped / flow evicted — same FR-008 semantics as
/// [`drain_one_flow`]), or `false` when nothing was read because the
/// recv reported `WouldBlock` — on non-Linux platforms that is always
/// the case (stub), and the caller falls back to the per-packet loop.
///
/// Per-packet semantics preserved exactly:
///  * per delivered datagram: `bump_outbound` + `inc_datagram_out` +
///    `quota_consume_after_send`;
///  * send `WouldBlock` / partial `sendmmsg`: the undelivered tail is
///    dropped with no stats / `last_seen` bump (FR-008 step e);
///  * ICMP-class recv error: evict (registry remove + cancel);
///  * `EMSGSIZE` / `Transient` recv classifications unchanged.
async fn drain_one_flow_batched(
    cfg: &DemuxConfig,
    key: FlowKey,
    flow: &Arc<UdpFlow>,
    bufs: &mut BatchBufs,
) -> bool {
    let Some(listener) = cfg.listener_sockets.get(&key.listen_port).cloned() else {
        // Listener gone — shouldn't happen during normal operation.
        // Handled: the per-packet loop would only repeat this warn.
        warn!(
            event = "rule.udp_demux_missing_listener",
            rule_id = %cfg.rule_id,
            listen_port = key.listen_port,
        );
        return true;
    };
    match recv_batch_connected(&flow.upstream_socket, bufs) {
        // Spurious wakeup with an empty queue — nothing to forward.
        Ok(0) => true,
        Ok(n) => {
            forward_reply_batch(cfg, key, flow, &listener, bufs, n).await;
            true
        }
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
            // Non-Linux stub, or a spurious readiness wake on Linux.
            // Report unhandled so the per-packet loop runs (it exits
            // immediately on the same WouldBlock, so the Linux cost is
            // one extra try_recv on spurious wakes only).
            false
        }
        Err(e) => match classify_udp_error(&e) {
            UdpAction::WouldBlock => false,
            UdpAction::Evict => {
                cfg.stats.errors.inc_icmp_evict();
                info!(
                    event = "rule.udp_flow_evicted_icmp",
                    rule_id = %cfg.rule_id,
                    listen_port = key.listen_port,
                    error = %e,
                );
                let _ = cfg.registry.remove(key);
                flow.cancel.cancel();
                true
            }
            UdpAction::MessageTooLarge => {
                cfg.stats.errors.inc_emsgsize();
                debug!(
                    event = "rule.udp_emsgsize",
                    rule_id = %cfg.rule_id,
                    listen_port = key.listen_port,
                );
                true
            }
            UdpAction::Transient => true,
        },
    }
}

/// Forward `n` freshly-received reply datagrams (slots `0..n` of
/// `bufs`) to `key.src` via one `sendmmsg` on the listener socket, and
/// book per-datagram stats / quota for the delivered prefix.
async fn forward_reply_batch(
    cfg: &DemuxConfig,
    key: FlowKey,
    flow: &Arc<UdpFlow>,
    listener: &UdpSocket,
    bufs: &BatchBufs,
    n: usize,
) {
    let payloads: Vec<&[u8]> = (0..n).map(|i| bufs.payload(i)).collect();
    // All packets in one drain belong to one flow, so every message
    // goes to the same client address. SocketAddr is Copy — a stack
    // array avoids a per-drain heap allocation.
    let dests = [key.src; BATCH_SIZE];

    match send_batch_to(listener, &payloads, &dests[..n]) {
        Ok(sent) => {
            // Book the delivered prefix exactly like the per-packet
            // path: stats + last_seen + post-send quota consume per
            // datagram.
            for payload in payloads.iter().take(sent) {
                let bytes = payload.len() as u64;
                flow.bump_outbound(bytes).await;
                cfg.stats.inc_datagram_out(key.listen_port, bytes);
                let _ = flow.quota_consume_after_send(bytes);
            }
            if sent < n {
                // Partial sendmmsg: the kernel accepted a prefix and
                // hit SO_SNDBUF pressure on the rest. Drop the tail —
                // same semantics as the per-packet WouldBlock drop
                // (no stats / last_seen bump, FR-008 step e).
                cfg.stats.errors.inc_wouldblock();
                trace!(
                    event = "rule.udp_reply_wouldblock",
                    rule_id = %cfg.rule_id,
                    listen_port = key.listen_port,
                    dropped = n - sent,
                );
            }
        }
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
            cfg.stats.errors.inc_wouldblock();
            trace!(
                event = "rule.udp_reply_wouldblock",
                rule_id = %cfg.rule_id,
                listen_port = key.listen_port,
                dropped = n,
            );
            // Drop replies; flow continues. No stats / last_seen
            // bump (FR-008 step e).
        }
        Err(e) => {
            warn!(
                event = "rule.udp_reply_send_failed",
                rule_id = %cfg.rule_id,
                listen_port = key.listen_port,
                error = %e,
            );
            // Listener-side error is rule-level — log + continue.
        }
    }
}

async fn drain_one_flow(cfg: &DemuxConfig, key: FlowKey, flow: &Arc<UdpFlow>, buf: &mut [u8]) {
    let Some(listener) = cfg.listener_sockets.get(&key.listen_port).cloned() else {
        // Listener gone — shouldn't happen during normal operation.
        warn!(
            event = "rule.udp_demux_missing_listener",
            rule_id = %cfg.rule_id,
            listen_port = key.listen_port,
        );
        return;
    };
    for _ in 0..DEMUX_FAIRNESS_BUDGET {
        match flow.upstream_socket.try_recv(buf) {
            Ok(n) => {
                let bytes = n as u64;
                match listener.try_send_to(&buf[..n], key.src) {
                    Ok(_) => {
                        flow.bump_outbound(bytes).await;
                        cfg.stats.inc_datagram_out(key.listen_port, bytes);
                        let _ = flow.quota_consume_after_send(bytes);
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        cfg.stats.errors.inc_wouldblock();
                        trace!(
                            event = "rule.udp_reply_wouldblock",
                            rule_id = %cfg.rule_id,
                            listen_port = key.listen_port,
                        );
                        // Drop reply; flow continues. No stats / last_seen
                        // bump (FR-008 step e).
                    }
                    Err(e) => {
                        warn!(
                            event = "rule.udp_reply_send_failed",
                            rule_id = %cfg.rule_id,
                            listen_port = key.listen_port,
                            error = %e,
                        );
                        // Listener-side error is rule-level — log + continue.
                    }
                }
            }
            Err(e) => match classify_udp_error(&e) {
                UdpAction::WouldBlock => return,
                UdpAction::Evict => {
                    cfg.stats.errors.inc_icmp_evict();
                    info!(
                        event = "rule.udp_flow_evicted_icmp",
                        rule_id = %cfg.rule_id,
                        listen_port = key.listen_port,
                        error = %e,
                    );
                    let _ = cfg.registry.remove(key);
                    flow.cancel.cancel();
                    return;
                }
                UdpAction::MessageTooLarge => {
                    cfg.stats.errors.inc_emsgsize();
                    debug!(
                        event = "rule.udp_emsgsize",
                        rule_id = %cfg.rule_id,
                        listen_port = key.listen_port,
                    );
                    return;
                }
                UdpAction::Transient => {
                    return;
                }
            },
        }
    }
}

enum ReadOutcome {
    Ready { key: FlowKey, flow: Arc<UdpFlow> },
    Cancelled,
}

type ReadWaitFut = std::pin::Pin<Box<dyn std::future::Future<Output = ReadOutcome> + Send>>;

fn read_wait(key: FlowKey, flow: Arc<UdpFlow>) -> ReadWaitFut {
    Box::pin(async move {
        tokio::select! {
            () = flow.cancel.cancelled() => ReadOutcome::Cancelled,
            r = flow.upstream_socket.readable() => match r {
                Ok(()) => ReadOutcome::Ready { key, flow },
                Err(_) => ReadOutcome::Cancelled,
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forwarder::stats::RuleStats;
    use portunus_core::PortRange;
    use std::net::{Ipv4Addr, SocketAddr};
    use std::time::Duration;

    async fn bind_loopback_udp() -> (Arc<UdpSocket>, SocketAddr) {
        let s = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = s.local_addr().unwrap();
        (Arc::new(s), addr)
    }

    fn single_port_stats(port: u16) -> Arc<RuleStats> {
        RuleStats::for_range(PortRange::single(port))
    }

    /// End-to-end: stand up a listener socket, an upstream socket, an
    /// UdpFlow that owns the upstream, then send a "reply" from the
    /// upstream's peer and verify the demux forwards it via the listener
    /// to the original client.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn add_flow_then_reply_reaches_client() {
        let (listener_sock, listener_addr) = bind_loopback_udp().await;
        let mut listener_map = HashMap::new();
        listener_map.insert(listener_addr.port(), Arc::clone(&listener_sock));

        let (client_sock, client_addr) = bind_loopback_udp().await;
        let (target_sock, target_addr) = bind_loopback_udp().await;

        let upstream = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        upstream.connect(target_addr).await.unwrap();
        let upstream = Arc::new(upstream);

        let flow = UdpFlow::for_test_with_socket(client_addr, Arc::clone(&upstream)).await;
        let key = FlowKey::new(listener_addr.port(), client_addr);

        let registry = UdpFlowRegistry::new(4);
        let stats = single_port_stats(listener_addr.port());
        let cfg = DemuxConfig {
            rule_id: RuleId(1),
            registry: Arc::clone(&registry),
            listener_sockets: Arc::new(listener_map),
            stats: Arc::clone(&stats),
        };
        let (tx, rx) = mpsc::channel(8);
        let h = tokio::spawn(run_demux(cfg, rx));

        tx.send(DemuxCommand::AddFlow {
            key,
            flow: Arc::clone(&flow),
        })
        .await
        .unwrap();

        // Send "reply" from target to upstream's ephemeral address.
        target_sock
            .send_to(b"hello", upstream.local_addr().unwrap())
            .await
            .unwrap();

        let mut buf = [0u8; 64];
        let (n, src) =
            tokio::time::timeout(Duration::from_secs(2), client_sock.recv_from(&mut buf))
                .await
                .expect("client should receive forwarded reply within 2s")
                .unwrap();
        assert_eq!(&buf[..n], b"hello");
        assert_eq!(src, listener_addr);

        tx.send(DemuxCommand::Shutdown).await.unwrap();
        h.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_flow_drops_arc_without_re_arm() {
        let (listener_sock, listener_addr) = bind_loopback_udp().await;
        let mut listener_map = HashMap::new();
        listener_map.insert(listener_addr.port(), Arc::clone(&listener_sock));

        let (_target_sock, target_addr) = bind_loopback_udp().await;

        let upstream = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        upstream.connect(target_addr).await.unwrap();
        let upstream = Arc::new(upstream);

        let client_addr: SocketAddr = "127.0.0.1:50000".parse().unwrap();
        let flow = UdpFlow::for_test_with_socket(client_addr, Arc::clone(&upstream)).await;
        let key = FlowKey::new(listener_addr.port(), client_addr);

        let registry = UdpFlowRegistry::new(4);
        let stats = single_port_stats(listener_addr.port());
        let (tx, rx) = mpsc::channel(8);
        let h = tokio::spawn(run_demux(
            DemuxConfig {
                rule_id: RuleId(1),
                registry,
                listener_sockets: Arc::new(listener_map),
                stats,
            },
            rx,
        ));

        tx.send(DemuxCommand::AddFlow {
            key,
            flow: Arc::clone(&flow),
        })
        .await
        .unwrap();
        // Give demux a tick to push the ReadWait future.
        tokio::time::sleep(Duration::from_millis(20)).await;
        // Now cancel the flow. The ReadWait future's `cancelled` branch
        // resolves, demux drops the Arc, and no re-arm happens.
        flow.cancel.cancel();
        // Strong-ref count goes 2 (test + demux) → 1 (test only) once
        // demux drops its clone.
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(10)).await;
            if Arc::strong_count(&flow) == 1 {
                break;
            }
        }
        assert_eq!(
            Arc::strong_count(&flow),
            1,
            "demux must drop its Arc after cancel"
        );

        tx.send(DemuxCommand::Shutdown).await.unwrap();
        h.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drain_budget_caps_at_32_datagrams_per_ready() {
        // Send 100 reply datagrams in one go, observe demux processes
        // them in batches of <=32 (the fairness budget). We can't observe
        // batches directly; instead check that all 100 eventually arrive
        // and the demux did not panic / starve.
        let (listener_sock, listener_addr) = bind_loopback_udp().await;
        let mut listener_map = HashMap::new();
        listener_map.insert(listener_addr.port(), Arc::clone(&listener_sock));

        let (client_sock, client_addr) = bind_loopback_udp().await;
        let (target_sock, target_addr) = bind_loopback_udp().await;

        let upstream = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        upstream.connect(target_addr).await.unwrap();
        let upstream = Arc::new(upstream);
        let upstream_local = upstream.local_addr().unwrap();

        let flow = UdpFlow::for_test_with_socket(client_addr, Arc::clone(&upstream)).await;
        let key = FlowKey::new(listener_addr.port(), client_addr);

        let registry = UdpFlowRegistry::new(4);
        let stats = single_port_stats(listener_addr.port());
        let (tx, rx) = mpsc::channel(8);
        let h = tokio::spawn(run_demux(
            DemuxConfig {
                rule_id: RuleId(1),
                registry,
                listener_sockets: Arc::new(listener_map),
                stats,
            },
            rx,
        ));
        tx.send(DemuxCommand::AddFlow { key, flow }).await.unwrap();

        for i in 0..100u8 {
            target_sock.send_to(&[i], upstream_local).await.unwrap();
        }

        let mut received = std::collections::HashSet::new();
        let mut buf = [0u8; 64];
        for _ in 0..100 {
            let (n, _) =
                tokio::time::timeout(Duration::from_secs(3), client_sock.recv_from(&mut buf))
                    .await
                    .expect("100 replies should arrive within 3s")
                    .unwrap();
            assert_eq!(n, 1);
            received.insert(buf[0]);
        }
        assert_eq!(received.len(), 100);

        tx.send(DemuxCommand::Shutdown).await.unwrap();
        h.await.unwrap();
    }

    /// `drain_one_flow` bails out via the missing-listener guard when the
    /// flow's `listen_port` has no entry in `listener_sockets`. The drain
    /// must `warn!` + return without touching the socket or counters.
    #[tokio::test]
    async fn drain_one_flow_returns_when_listener_missing() {
        // Empty listener map → the `.get(&key.listen_port)` lookup misses.
        let listener_map: HashMap<u16, Arc<UdpSocket>> = HashMap::new();

        let (target_sock, target_addr) = bind_loopback_udp().await;
        let upstream = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        upstream.connect(target_addr).await.unwrap();
        let upstream = Arc::new(upstream);
        let upstream_local = upstream.local_addr().unwrap();

        let client_addr: SocketAddr = "127.0.0.1:50010".parse().unwrap();
        let flow = UdpFlow::for_test_with_socket(client_addr, Arc::clone(&upstream)).await;
        // Use a listen_port that is deliberately absent from the map.
        let key = FlowKey::new(40000, client_addr);

        let registry = UdpFlowRegistry::new(4);
        let stats = single_port_stats(40000);
        let cfg = DemuxConfig {
            rule_id: RuleId(1),
            registry,
            listener_sockets: Arc::new(listener_map),
            stats: Arc::clone(&stats),
        };

        // Queue a reply so a *present* listener would have forwarded it;
        // the missing-listener guard must short-circuit before any recv.
        target_sock.send_to(b"x", upstream_local).await.unwrap();

        let mut buf = vec![0u8; RECV_BUFFER_BYTES];
        drain_one_flow(&cfg, key, &flow, &mut buf).await;

        // No outbound datagram was forwarded; the flow stays live.
        assert_eq!(stats.snapshot_datagrams_out(), 0);
        assert!(!flow.cancel.is_cancelled());
    }

    /// A reply that reads cleanly from the upstream but whose listener-side
    /// `try_send_to` fails with a non-`WouldBlock` error (IPv6 destination
    /// on an IPv4-only listener socket → `EAFNOSUPPORT`/`EINVAL`) lands on
    /// the `warn!` "reply_send_failed" arm. The drain logs + continues; no
    /// outbound stat is booked and the flow is not evicted.
    #[tokio::test]
    async fn drain_one_flow_logs_when_listener_send_fails() {
        let (listener_sock, listener_addr) = bind_loopback_udp().await;
        let mut listener_map = HashMap::new();
        listener_map.insert(listener_addr.port(), Arc::clone(&listener_sock));

        let (target_sock, target_addr) = bind_loopback_udp().await;
        let upstream = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        upstream.connect(target_addr).await.unwrap();
        let upstream = Arc::new(upstream);
        let upstream_local = upstream.local_addr().unwrap();

        // IPv6 client source — an IPv4-bound listener cannot send_to it,
        // so `try_send_to` returns a non-WouldBlock error synchronously.
        let client_addr: SocketAddr = "[::1]:50011".parse().unwrap();
        let flow = UdpFlow::for_test_with_socket(client_addr, Arc::clone(&upstream)).await;
        let key = FlowKey::new(listener_addr.port(), client_addr);

        let registry = UdpFlowRegistry::new(4);
        let stats = single_port_stats(listener_addr.port());
        let cfg = DemuxConfig {
            rule_id: RuleId(1),
            registry,
            listener_sockets: Arc::new(listener_map),
            stats: Arc::clone(&stats),
        };

        // Queue exactly one reply so the recv succeeds and the failing
        // send is attempted once.
        target_sock.send_to(b"reply", upstream_local).await.unwrap();
        // Wait until the datagram is readable so the first `try_recv`
        // inside the drain returns it deterministically.
        tokio::time::timeout(Duration::from_secs(2), upstream.readable())
            .await
            .expect("upstream should become readable")
            .unwrap();

        let mut buf = vec![0u8; RECV_BUFFER_BYTES];
        drain_one_flow(&cfg, key, &flow, &mut buf).await;

        // The send failed, so no outbound bytes/datagrams were booked and
        // the flow was NOT evicted (listener-side error is rule-level).
        assert_eq!(stats.snapshot_datagrams_out(), 0);
        assert_eq!(flow.bytes_out.load(std::sync::atomic::Ordering::Relaxed), 0);
        assert!(!flow.cancel.is_cancelled());
        assert_eq!(stats.errors.snapshot().wouldblock, 0);
    }

    // NOTE: the upstream-ICMP eviction path — a `ConnectionRefused`-class
    // `try_recv` error driving `drain_one_flow` down the `Evict` arm — is not
    // unit-tested here. Provoking a real ICMP port-unreachable depends on
    // kernel/loopback behaviour that is not reliable in hermetic CI sandboxes
    // (it works on macOS but not consistently on Linux runners), which makes
    // such a test flaky. The error classification itself
    // (`ConnectionRefused`/`ConnectionReset`/... -> `UdpAction::Evict`) is
    // covered deterministically by the `classify_udp_error` tests in
    // `udp/error.rs`.

    /// `run_demux` re-arms a live flow after a Ready drain so a second
    /// reply on the same flow is also forwarded. This exercises the
    /// `readables.push(read_wait(..))` re-arm in the `Ready` arm.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_demux_rearms_flow_for_a_second_reply() {
        let (listener_sock, listener_addr) = bind_loopback_udp().await;
        let mut listener_map = HashMap::new();
        listener_map.insert(listener_addr.port(), Arc::clone(&listener_sock));

        let (client_sock, client_addr) = bind_loopback_udp().await;
        let (target_sock, target_addr) = bind_loopback_udp().await;

        let upstream = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        upstream.connect(target_addr).await.unwrap();
        let upstream = Arc::new(upstream);
        let upstream_local = upstream.local_addr().unwrap();

        let flow = UdpFlow::for_test_with_socket(client_addr, Arc::clone(&upstream)).await;
        let key = FlowKey::new(listener_addr.port(), client_addr);

        let registry = UdpFlowRegistry::new(4);
        let stats = single_port_stats(listener_addr.port());
        let (tx, rx) = mpsc::channel(8);
        let h = tokio::spawn(run_demux(
            DemuxConfig {
                rule_id: RuleId(1),
                registry,
                listener_sockets: Arc::new(listener_map),
                stats,
            },
            rx,
        ));
        tx.send(DemuxCommand::AddFlow { key, flow }).await.unwrap();

        let mut buf = [0u8; 64];
        // First reply.
        target_sock.send_to(b"a", upstream_local).await.unwrap();
        let (n, _) = tokio::time::timeout(Duration::from_secs(2), client_sock.recv_from(&mut buf))
            .await
            .expect("first reply forwarded")
            .unwrap();
        assert_eq!(&buf[..n], b"a");

        // Second reply on the SAME flow — only reaches the client if the
        // demux re-armed the flow's read-wait after the first drain.
        target_sock.send_to(b"bb", upstream_local).await.unwrap();
        let (n, _) = tokio::time::timeout(Duration::from_secs(2), client_sock.recv_from(&mut buf))
            .await
            .expect("second reply requires re-arm")
            .unwrap();
        assert_eq!(&buf[..n], b"bb");

        tx.send(DemuxCommand::Shutdown).await.unwrap();
        h.await.unwrap();
    }

    /// `drain_one_flow_batched` bails out via the missing-listener guard
    /// exactly like `drain_one_flow`, reporting the drain as handled
    /// (`true`) so the per-packet fallback does not repeat the warn.
    #[tokio::test]
    async fn drain_one_flow_batched_returns_true_when_listener_missing() {
        let listener_map: HashMap<u16, Arc<UdpSocket>> = HashMap::new();

        let (_target_sock, target_addr) = bind_loopback_udp().await;
        let upstream = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        upstream.connect(target_addr).await.unwrap();
        let upstream = Arc::new(upstream);

        let client_addr: SocketAddr = "127.0.0.1:50012".parse().unwrap();
        let flow = UdpFlow::for_test_with_socket(client_addr, Arc::clone(&upstream)).await;
        let key = FlowKey::new(40001, client_addr);

        let registry = UdpFlowRegistry::new(4);
        let stats = single_port_stats(40001);
        let cfg = DemuxConfig {
            rule_id: RuleId(1),
            registry,
            listener_sockets: Arc::new(listener_map),
            stats: Arc::clone(&stats),
        };

        let mut bufs = BatchBufs::new();
        assert!(
            drain_one_flow_batched(&cfg, key, &flow, &mut bufs).await,
            "missing listener is handled by the batched drain"
        );
        assert_eq!(stats.snapshot_datagrams_out(), 0);
        assert!(!flow.cancel.is_cancelled());
    }

    /// On non-Linux platforms `recv_batch_connected` is a `WouldBlock`
    /// stub, so `drain_one_flow_batched` must report the drain as NOT
    /// handled (`false`) — the run loop then falls back to the
    /// per-packet `drain_one_flow`, which is what actually forwards the
    /// queued reply. This pins the fallback contract the run_demux
    /// round-trip tests rely on.
    #[cfg(not(target_os = "linux"))]
    #[tokio::test]
    async fn drain_one_flow_batched_falls_back_on_non_linux() {
        let (listener_sock, listener_addr) = bind_loopback_udp().await;
        let mut listener_map = HashMap::new();
        listener_map.insert(listener_addr.port(), Arc::clone(&listener_sock));

        let (client_sock, client_addr) = bind_loopback_udp().await;
        let (target_sock, target_addr) = bind_loopback_udp().await;

        let upstream = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        upstream.connect(target_addr).await.unwrap();
        let upstream = Arc::new(upstream);
        let upstream_local = upstream.local_addr().unwrap();

        let flow = UdpFlow::for_test_with_socket(client_addr, Arc::clone(&upstream)).await;
        let key = FlowKey::new(listener_addr.port(), client_addr);

        let registry = UdpFlowRegistry::new(4);
        let stats = single_port_stats(listener_addr.port());
        let cfg = DemuxConfig {
            rule_id: RuleId(1),
            registry,
            listener_sockets: Arc::new(listener_map),
            stats: Arc::clone(&stats),
        };

        // Queue one reply so a Linux batched drain WOULD have consumed
        // it; the stub must leave it unread and return false.
        target_sock.send_to(b"reply", upstream_local).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), upstream.readable())
            .await
            .expect("upstream should become readable")
            .unwrap();

        let mut bufs = BatchBufs::new();
        assert!(
            !drain_one_flow_batched(&cfg, key, &flow, &mut bufs).await,
            "non-Linux batched drain must defer to the per-packet loop"
        );
        assert_eq!(stats.snapshot_datagrams_out(), 0);

        // The per-packet fallback still finds the datagram queued.
        let mut buf = vec![0u8; RECV_BUFFER_BYTES];
        drain_one_flow(&cfg, key, &flow, &mut buf).await;
        let mut out = [0u8; 16];
        let (n, src) =
            tokio::time::timeout(Duration::from_secs(2), client_sock.recv_from(&mut out))
                .await
                .expect("fallback forwards the reply")
                .unwrap();
        assert_eq!(&out[..n], b"reply");
        assert_eq!(src, listener_addr);
    }

    /// Linux: the batched drain consumes queued replies with one
    /// recvmmsg + one sendmmsg and forwards each to the flow's client
    /// through the listener socket, booking per-datagram stats.
    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drain_one_flow_batched_forwards_replies_on_linux() {
        let (listener_sock, listener_addr) = bind_loopback_udp().await;
        let mut listener_map = HashMap::new();
        listener_map.insert(listener_addr.port(), Arc::clone(&listener_sock));

        let (client_sock, client_addr) = bind_loopback_udp().await;
        let (target_sock, target_addr) = bind_loopback_udp().await;

        let upstream = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        upstream.connect(target_addr).await.unwrap();
        let upstream = Arc::new(upstream);
        let upstream_local = upstream.local_addr().unwrap();

        let flow = UdpFlow::for_test_with_socket(client_addr, Arc::clone(&upstream)).await;
        let key = FlowKey::new(listener_addr.port(), client_addr);

        let registry = UdpFlowRegistry::new(4);
        let stats = single_port_stats(listener_addr.port());
        let cfg = DemuxConfig {
            rule_id: RuleId(1),
            registry,
            listener_sockets: Arc::new(listener_map),
            stats: Arc::clone(&stats),
        };

        // Queue several replies, then wait until at least one is
        // deliverable so recvmmsg observes a non-empty queue.
        for i in 0..5u8 {
            target_sock.send_to(&[i, i], upstream_local).await.unwrap();
        }
        tokio::time::timeout(Duration::from_secs(2), upstream.readable())
            .await
            .expect("upstream should become readable")
            .unwrap();

        let mut bufs = BatchBufs::new();
        assert!(
            drain_one_flow_batched(&cfg, key, &flow, &mut bufs).await,
            "Linux batched drain handles the Ready"
        );

        // At least one datagram was received by recvmmsg and forwarded;
        // every forwarded datagram reaches the client via the listener.
        let forwarded = stats.snapshot_datagrams_out();
        assert!(forwarded >= 1, "batched drain must forward >= 1 reply");
        let mut out = [0u8; 16];
        for _ in 0..forwarded {
            let (n, src) =
                tokio::time::timeout(Duration::from_secs(2), client_sock.recv_from(&mut out))
                    .await
                    .expect("forwarded replies reach the client")
                    .unwrap();
            assert_eq!(n, 2);
            assert_eq!(out[0], out[1]);
            assert_eq!(src, listener_addr);
        }
        assert!(!flow.cancel.is_cancelled());
    }
}

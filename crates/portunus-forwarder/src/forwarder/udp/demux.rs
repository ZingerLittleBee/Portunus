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

use std::collections::HashMap;
use std::io;
use std::sync::Arc;

use futures_util::stream::{FuturesUnordered, StreamExt};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tracing::{debug, info, trace, warn};

use crate::forwarder::stats::RuleStats;
use crate::forwarder::udp::error::{UdpAction, classify_udp_error};
use crate::forwarder::udp::flow::UdpFlow;
use crate::forwarder::udp::registry::{FlowKey, UdpFlowRegistry};

/// Demux drains at most this many datagrams per Ready before re-arming
/// the readable future, to keep one chatty flow from starving others
/// (FR-008).
pub const DEMUX_FAIRNESS_BUDGET: usize = 32;

/// Sized at protocol max so `try_recv` never truncates.
const RECV_BUFFER_BYTES: usize = 65_535;

/// Control-plane message sent to a running demux task.
pub enum DemuxCommand {
    /// Hand a freshly-built flow to the demux. The demux takes
    /// ownership of the cloned `Arc<UdpFlow>` for the lifetime of the
    /// flow; the cold path drops its own clone once the registry is
    /// committed.
    AddFlow {
        key: FlowKey,
        flow: Arc<UdpFlow>,
    },
    /// Tear the demux down cleanly. All pending `ReadWait`s are dropped
    /// implicitly when the function returns.
    Shutdown,
}

/// Static configuration handed to [`run_demux`]. All fields are
/// `Arc`-shared so the demux task and the per-listener cold path can
/// see the same registry / listener sockets / stats counters.
pub struct DemuxConfig {
    pub registry: Arc<UdpFlowRegistry>,
    pub listener_sockets: Arc<HashMap<u16, Arc<UdpSocket>>>,
    pub stats: Arc<RuleStats>,
}

/// Run the reply-demux loop. Exits cleanly on `DemuxCommand::Shutdown`
/// or when the command channel is closed.
pub async fn run_demux(cfg: DemuxConfig, mut rx: mpsc::Receiver<DemuxCommand>) {
    let mut buf = vec![0u8; RECV_BUFFER_BYTES];
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
                    drain_one_flow(&cfg, key, &flow, &mut buf).await;
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

async fn drain_one_flow(
    cfg: &DemuxConfig,
    key: FlowKey,
    flow: &Arc<UdpFlow>,
    buf: &mut [u8],
) {
    let Some(listener) = cfg.listener_sockets.get(&key.listen_port).cloned() else {
        // Listener gone — shouldn't happen during normal operation.
        warn!(
            event = "rule.udp_demux_missing_listener",
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
                        trace!(
                            event = "rule.udp_reply_wouldblock",
                            listen_port = key.listen_port,
                        );
                        // Drop reply; flow continues. No stats / last_seen
                        // bump (FR-008 step e).
                    }
                    Err(e) => {
                        warn!(
                            event = "rule.udp_reply_send_failed",
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
                    info!(
                        event = "rule.udp_flow_evicted_icmp",
                        listen_port = key.listen_port,
                        error = %e,
                    );
                    let _ = cfg.registry.remove(key).await;
                    flow.cancel.cancel();
                    return;
                }
                UdpAction::MessageTooLarge => {
                    debug!(
                        event = "rule.udp_emsgsize",
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
        let (n, src) = tokio::time::timeout(Duration::from_secs(2), client_sock.recv_from(&mut buf))
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
}

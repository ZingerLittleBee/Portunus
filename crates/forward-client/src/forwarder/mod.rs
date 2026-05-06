//! Per-rule TCP forwarder: binds the listen port, accepts in a loop, and
//! spawns [`proxy`](proxy::proxy) for each connection.
//!
//! Lifecycle is driven by a [`CancellationToken`]:
//! - cancel → stop accepting new connections immediately (FR-014/FR-016
//!   "stop accepting within 1 s")
//! - then drain in-flight proxies up to `drain_timeout`
//! - return a final activation/teardown outcome to the caller via the
//!   `status_tx` channel — exactly one `Activated`/`Failed` and one
//!   `Removed` per rule lifetime.

pub mod proxy;
pub mod stats;

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::Duration;

use forward_core::RuleId;
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::forwarder::stats::RuleStats;

/// Outcome the forwarder reports back to the control loop. The control loop
/// translates each into a `RuleStatus` message on the bidi gRPC stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleStatusEvent {
    Activated { rule_id: RuleId },
    Failed { rule_id: RuleId, reason: String },
    Removed { rule_id: RuleId },
}

/// One forwarding rule the client should run.
#[derive(Debug, Clone)]
pub struct ClientRule {
    pub rule_id: RuleId,
    pub listen_port: u16,
    pub target_host: String,
    pub target_port: u16,
}

/// Run the forwarder until `cancel` fires. Sends exactly one
/// `Activated|Failed` event during startup and exactly one `Removed` event
/// (only after a successful Activated) when the listener is torn down.
///
/// The listener binds to `0.0.0.0:listen_port` so external machines can
/// reach it (this is the data plane — `data-model.md` does not require
/// loopback-only as the operator HTTP API does). Operators with stricter
/// requirements can run the client behind a host firewall.
#[allow(clippy::too_many_lines)]
pub async fn run(
    rule: ClientRule,
    status_tx: mpsc::Sender<RuleStatusEvent>,
    cancel: CancellationToken,
    drain_timeout: Duration,
    stats: Arc<RuleStats>,
) {
    let bind_addr = (Ipv4Addr::UNSPECIFIED, rule.listen_port);
    let listener = match TcpListener::bind(bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            let reason = classify_bind_error(&e);
            warn!(
                event = "rule.failed",
                rule_id = %rule.rule_id,
                listen_port = rule.listen_port,
                reason = reason,
                error = %e,
            );
            let _ = status_tx
                .send(RuleStatusEvent::Failed {
                    rule_id: rule.rule_id,
                    reason: reason.to_string(),
                })
                .await;
            return;
        }
    };
    info!(
        event = "rule.activated",
        rule_id = %rule.rule_id,
        listen_port = rule.listen_port,
        target = %format!("{}:{}", rule.target_host, rule.target_port),
    );
    if status_tx
        .send(RuleStatusEvent::Activated {
            rule_id: rule.rule_id,
        })
        .await
        .is_err()
    {
        // Control loop hung up before we even reported activated — bail.
        return;
    }

    let mut in_flight: JoinSet<()> = JoinSet::new();
    // `proxy_cancel` is an independent token (NOT a child of `cancel`) so
    // that operator-side rule removal does not immediately tear down
    // in-flight proxies — they get a `drain_timeout` window to finish.
    let proxy_cancel = CancellationToken::new();
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            accept = listener.accept() => match accept {
                Ok((sock, peer)) => {
                    let target_host = rule.target_host.clone();
                    let target_port = rule.target_port;
                    let rule_id = rule.rule_id;
                    let conn_cancel = proxy_cancel.clone();
                    let conn_stats = Arc::clone(&stats);
                    in_flight.spawn(async move {
                        match proxy::proxy(sock, &target_host, target_port, conn_cancel, Some(conn_stats)).await {
                            Ok((bin, bout)) => {
                                info!(
                                    event = "rule.conn_closed",
                                    rule_id = %rule_id,
                                    peer = %peer,
                                    bytes_in = bin,
                                    bytes_out = bout,
                                );
                            }
                            Err(e) => {
                                warn!(
                                    event = "rule.conn_error",
                                    rule_id = %rule_id,
                                    peer = %peer,
                                    error = %e,
                                );
                            }
                        }
                    });
                }
                Err(e) => {
                    // Transient accept error — log and keep looping. A
                    // persistent failure would still be observable here but
                    // we don't try to distinguish.
                    warn!(
                        event = "rule.accept_error",
                        rule_id = %rule.rule_id,
                        error = %e,
                    );
                }
            }
        }
    }

    // Drain phase: stop accept (listener drops at end of function), let
    // in-flight proxies finish naturally up to `drain_timeout`, then fire
    // their per-conn cancel tokens to force-close.
    drop(listener);
    let drain_deadline = tokio::time::sleep(drain_timeout);
    tokio::pin!(drain_deadline);
    loop {
        tokio::select! {
            () = &mut drain_deadline => {
                proxy_cancel.cancel();
                while in_flight.join_next().await.is_some() {}
                break;
            }
            joined = in_flight.join_next() => match joined {
                Some(_) => {
                    if in_flight.is_empty() {
                        break;
                    }
                }
                None => break,
            }
        }
    }

    info!(
        event = "rule.removed",
        rule_id = %rule.rule_id,
    );
    let _ = status_tx
        .send(RuleStatusEvent::Removed {
            rule_id: rule.rule_id,
        })
        .await;
}

fn classify_bind_error(e: &std::io::Error) -> &'static str {
    match e.kind() {
        std::io::ErrorKind::AddrInUse => "port_in_use",
        std::io::ErrorKind::PermissionDenied => "permission_denied",
        _ => "bind_failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn spawn_echo() -> std::net::SocketAddr {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    while let Ok(n) = sock.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        if sock.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        addr
    }

    async fn pick_free_port() -> u16 {
        TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap()
            .local_addr()
            .unwrap()
            .port()
    }

    #[tokio::test]
    async fn run_emits_activated_then_forwards_then_removed() {
        let echo = spawn_echo().await;
        let port = pick_free_port().await;
        let (tx, mut rx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let cancel_run = cancel.clone();
        let task = tokio::spawn(async move {
            run(
                ClientRule {
                    rule_id: RuleId(7),
                    listen_port: port,
                    target_host: echo.ip().to_string(),
                    target_port: echo.port(),
                },
                tx,
                cancel_run,
                Duration::from_secs(2),
                RuleStats::new(),
            )
            .await;
        });

        // Wait for Activated.
        let evt = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(evt, RuleStatusEvent::Activated { rule_id } if rule_id == RuleId(7)));

        // Punch a connection through.
        let mut client = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        client.write_all(b"forwarded").await.unwrap();
        let mut buf = [0u8; 9];
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"forwarded");
        drop(client);

        // Cancel → expect Removed.
        cancel.cancel();
        let evt = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(evt, RuleStatusEvent::Removed { rule_id } if rule_id == RuleId(7)));
        task.await.unwrap();
    }

    #[tokio::test]
    async fn run_reports_port_in_use() {
        // Bind a listener to an OS-chosen port, then try to reuse it.
        let occupy = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await.unwrap();
        let busy_port = occupy.local_addr().unwrap().port();
        let (tx, mut rx) = mpsc::channel(2);
        let cancel = CancellationToken::new();
        run(
            ClientRule {
                rule_id: RuleId(1),
                listen_port: busy_port,
                target_host: "127.0.0.1".into(),
                target_port: 1,
            },
            tx,
            cancel,
            Duration::from_millis(100),
            RuleStats::new(),
        )
        .await;
        let evt = rx.recv().await.unwrap();
        match evt {
            RuleStatusEvent::Failed { rule_id, reason } => {
                assert_eq!(rule_id, RuleId(1));
                assert_eq!(reason, "port_in_use");
            }
            other => panic!("expected Failed{{port_in_use}}, got {other:?}"),
        }
        // No Removed event after a Failed startup.
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn cancel_stops_accept_within_one_second() {
        // FR-014 / FR-016: stop accept within 1 s of remove.
        let echo = spawn_echo().await;
        let port = pick_free_port().await;
        let (tx, mut rx) = mpsc::channel(4);
        let cancel = CancellationToken::new();
        let cancel_run = cancel.clone();
        let task = tokio::spawn(async move {
            run(
                ClientRule {
                    rule_id: RuleId(3),
                    listen_port: port,
                    target_host: echo.ip().to_string(),
                    target_port: echo.port(),
                },
                tx,
                cancel_run,
                Duration::from_millis(500),
                RuleStats::new(),
            )
            .await;
        });
        // Activated event.
        let _ = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();

        let t0 = std::time::Instant::now();
        cancel.cancel();
        // After cancel, a fresh connect MUST be refused within 1 s.
        let stopped = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if TcpStream::connect((Ipv4Addr::LOCALHOST, port))
                    .await
                    .is_err()
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert!(stopped.is_ok(), "listener still accepting 1s after cancel");
        assert!(t0.elapsed() < Duration::from_secs(1));

        // Removed event eventually.
        let _ = tokio::time::timeout(Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap();
        task.await.unwrap();
    }

    /// T041 (US2): a 100 MB stream through the rule arrives byte-equal.
    /// Spec scenario 2: bytes received at the target are byte-for-byte
    /// identical to bytes sent and the connection completes without
    /// truncation.
    #[tokio::test]
    async fn forwards_100mb_byte_equal() {
        let echo = spawn_echo().await;
        let port = pick_free_port().await;
        let (tx, mut rx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let cancel_run = cancel.clone();
        let task = tokio::spawn(async move {
            run(
                ClientRule {
                    rule_id: RuleId(41),
                    listen_port: port,
                    target_host: echo.ip().to_string(),
                    target_port: echo.port(),
                },
                tx,
                cancel_run,
                Duration::from_secs(5),
                RuleStats::new(),
            )
            .await;
        });
        // Wait for Activated.
        let _ = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();

        // 100 MB pseudo-random payload (deterministic so a mismatch points
        // straight at the offending offset).
        let n: usize = 100 * 1024 * 1024;
        let mut sent: Vec<u8> = Vec::with_capacity(n);
        let mut x: u32 = 0xdead_beef;
        for _ in 0..n {
            // xorshift32
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            sent.push((x & 0xff) as u8);
        }

        let conn = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        let (mut rd, mut wr) = conn.into_split();
        let send_payload = sent.clone();
        let writer = tokio::spawn(async move {
            wr.write_all(&send_payload).await.unwrap();
            wr.shutdown().await.unwrap();
        });
        let mut received = Vec::with_capacity(n);
        let read_n = rd.read_to_end(&mut received).await.unwrap();
        writer.await.unwrap();

        assert_eq!(read_n, n, "100MB length mismatch");
        // Compare in chunks so we don't print 100MB on failure.
        for (i, (a, b)) in received.iter().zip(sent.iter()).enumerate() {
            assert_eq!(a, b, "byte mismatch at offset {i}");
        }

        cancel.cancel();
        task.await.unwrap();
    }

    /// T054b (US2 / SC-004): 5 rules × 100 concurrent connections each, all
    /// echoed end-to-end without drop or corruption. Smaller payload + shorter
    /// duration than the 30s spec figure to keep the unit-test runtime sane;
    /// the structural property — many rules + many conns + zero corruption —
    /// is what we're verifying here.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn five_rules_hundred_conns_each_no_corruption() {
        let echo = spawn_echo().await;
        let cancel = CancellationToken::new();

        let mut tasks = Vec::new();
        let mut ports = Vec::new();
        for i in 0..5u32 {
            let port = pick_free_port().await;
            ports.push(port);
            let (tx, mut rx) = mpsc::channel(8);
            let cancel_run = cancel.clone();
            let target_host = echo.ip().to_string();
            let target_port = echo.port();
            tasks.push(tokio::spawn(async move {
                run(
                    ClientRule {
                        rule_id: RuleId(u64::from(i + 100)),
                        listen_port: port,
                        target_host,
                        target_port,
                    },
                    tx,
                    cancel_run,
                    Duration::from_secs(5),
                    RuleStats::new(),
                )
                .await;
            }));
            // Each rule must report Activated.
            let evt = tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .unwrap()
                .unwrap();
            assert!(matches!(evt, RuleStatusEvent::Activated { .. }));
            // Drop the receiver so the rule can keep running without backpressure.
            drop(rx);
        }

        // For each rule, fan out 100 concurrent connections, each sending a
        // 4 KB payload and asserting byte-equal round-trip.
        let conns_per_rule: usize = 100;
        let payload_len: usize = 4096;
        let mut handles = Vec::new();
        for &port in &ports {
            for conn_i in 0..conns_per_rule {
                handles.push(tokio::spawn(async move {
                    let mut sock = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
                        .await
                        .expect("connect");
                    let mut payload = vec![0u8; payload_len];
                    for (i, b) in payload.iter_mut().enumerate() {
                        let v = u8::try_from((i + conn_i) & 0xff).unwrap();
                        *b = v;
                    }
                    let (mut rd, mut wr) = sock.split();
                    let writer = async {
                        wr.write_all(&payload).await.unwrap();
                        wr.shutdown().await.unwrap();
                    };
                    let mut got = Vec::with_capacity(payload_len);
                    let reader = async {
                        rd.read_to_end(&mut got).await.unwrap();
                    };
                    tokio::join!(writer, reader);
                    assert_eq!(got, payload);
                }));
            }
        }
        for h in handles {
            h.await.unwrap();
        }
        cancel.cancel();
        for t in tasks {
            t.await.unwrap();
        }
    }

    /// T042 (US2): after cancel, an in-flight connection survives until the
    /// drain timeout. Spec scenario 5: in-flight connections drain.
    #[tokio::test]
    async fn cancel_drains_in_flight_connection() {
        let echo = spawn_echo().await;
        let port = pick_free_port().await;
        let (tx, mut rx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let cancel_run = cancel.clone();
        let task = tokio::spawn(async move {
            run(
                ClientRule {
                    rule_id: RuleId(42),
                    listen_port: port,
                    target_host: echo.ip().to_string(),
                    target_port: echo.port(),
                },
                tx,
                cancel_run,
                // Generous drain timeout — we'll keep the connection alive
                // by trickling data and assert it can still echo after cancel.
                Duration::from_secs(3),
                RuleStats::new(),
            )
            .await;
        });
        let _ = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();

        let mut conn = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        // Establish a confirmed round-trip before we cancel so we know the
        // forwarder really is in copy_bidirectional.
        conn.write_all(b"warmup").await.unwrap();
        let mut buf = [0u8; 6];
        conn.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"warmup");

        // Now cancel — the listener stops accepting new connections, but our
        // in-flight one should still echo.
        cancel.cancel();
        // Give the cancellation a beat to propagate; drain timeout is 3s, so
        // we have plenty of headroom.
        tokio::time::sleep(Duration::from_millis(200)).await;

        conn.write_all(b"after-cancel").await.unwrap();
        let mut buf = [0u8; 12];
        let echoed = tokio::time::timeout(Duration::from_secs(1), conn.read_exact(&mut buf)).await;
        assert!(echoed.is_ok(), "in-flight read timed out post-cancel");
        echoed.unwrap().unwrap();
        assert_eq!(&buf, b"after-cancel");

        // Fresh connect MUST be refused since the listener is gone.
        let fresh = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).await;
        assert!(fresh.is_err(), "listener still accepting after cancel");

        // Close the in-flight connection so the drain loop returns and the
        // task completes cleanly.
        drop(conn);
        // Removed event arrives after drain.
        let _ = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        task.await.unwrap();
    }
}

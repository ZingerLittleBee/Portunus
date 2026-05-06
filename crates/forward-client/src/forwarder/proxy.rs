//! Bidirectional TCP proxy primitive.
//!
//! Wraps `tokio::io::copy_bidirectional` in a `select!` against a shutdown
//! token so the listener can tear down in-flight connections deterministically
//! during rule removal or process shutdown.

use std::io;
use std::sync::Arc;

use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

use super::stats::RuleStats;

/// Forward `inbound` to `target` (host:port DNS-resolved each call) until
/// either side closes or the shutdown token fires. Returns `(bytes_in,
/// bytes_out)` where `bytes_in` is bytes flowing inbound→outbound (from the
/// outside requester to the target) and `bytes_out` the reverse.
///
/// `target_host` is resolved on every call — intentional in MVP per
/// `data-model.md` ("no caching"). Resolution failure returns
/// `target_resolution_failed`-style `io::Error`.
///
/// `stats` is updated in two passes: `active_connections` is incremented at
/// entry and decremented on exit (RAII via the guard); byte counters get the
/// final tally from `copy_bidirectional` once it returns. Per-direction
/// updates would require `tokio_util::io::InspectReader` shims — overkill for
/// the 5-second sample window the `StatsReport` uses.
pub async fn proxy(
    mut inbound: TcpStream,
    target_host: &str,
    target_port: u16,
    shutdown: CancellationToken,
    stats: Option<Arc<RuleStats>>,
) -> io::Result<(u64, u64)> {
    let target = format!("{target_host}:{target_port}");
    let mut outbound = TcpStream::connect(&target).await?;
    // Disable Nagle to keep latency-sensitive small writes prompt; the kernel
    // still coalesces opportunistically.
    let _ = inbound.set_nodelay(true);
    let _ = outbound.set_nodelay(true);

    let _guard = stats.as_ref().map(|s| ActiveGuard::new(Arc::clone(s)));

    let result = tokio::select! {
        () = shutdown.cancelled() => {
            // Both streams drop at function exit, closing the sockets. We
            // surface a distinct error so the caller can log "drained" rather
            // than "completed".
            Err(io::Error::other("proxy_cancelled"))
        }
        result = tokio::io::copy_bidirectional(&mut inbound, &mut outbound) => {
            result
        }
    };
    if let (Some(s), Ok((bin, bout))) = (stats.as_ref(), result.as_ref()) {
        s.add_in(*bin);
        s.add_out(*bout);
    }
    result
}

struct ActiveGuard {
    stats: Arc<RuleStats>,
}

impl ActiveGuard {
    fn new(stats: Arc<RuleStats>) -> Self {
        stats.inc_active();
        Self { stats }
    }
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.stats.dec_active();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Helper: spawn an echo server on a random port, return its address.
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

    #[tokio::test]
    async fn proxy_forwards_bytes_to_echo_target() {
        let echo = spawn_echo().await;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();
        let cancel = CancellationToken::new();

        // Accept one connection through the proxy.
        let cancel_proxy = cancel.clone();
        let proxy_task = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            proxy(
                sock,
                &echo.ip().to_string(),
                echo.port(),
                cancel_proxy,
                None,
            )
            .await
        });

        // Client side: connect, write, read echoed back.
        let mut client = TcpStream::connect(proxy_addr).await.unwrap();
        client.write_all(b"hello forward").await.unwrap();
        let mut buf = [0u8; 13];
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello forward");
        // Close client → both halves see EOF → proxy returns Ok.
        drop(client);
        let result = proxy_task.await.unwrap();
        let (bin, bout) = result.expect("proxy returns counts");
        assert_eq!(bin, 13, "13 bytes sent inbound→outbound");
        assert_eq!(bout, 13, "13 bytes echoed outbound→inbound");
    }

    #[tokio::test]
    async fn proxy_cancellation_aborts() {
        let echo = spawn_echo().await;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();
        let cancel = CancellationToken::new();

        let cancel_proxy = cancel.clone();
        let proxy_task = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            proxy(
                sock,
                &echo.ip().to_string(),
                echo.port(),
                cancel_proxy,
                None,
            )
            .await
        });

        let _client = TcpStream::connect(proxy_addr).await.unwrap();
        // Idle: copy_bidirectional is parked. Fire cancel.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        cancel.cancel();
        let err = proxy_task.await.unwrap().unwrap_err();
        assert!(
            err.to_string().contains("proxy_cancelled"),
            "expected proxy_cancelled, got {err}"
        );
    }

    #[tokio::test]
    async fn proxy_returns_io_error_on_unreachable_target() {
        // 127.0.0.1:1 should refuse on macOS/Linux dev environments.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();
        let cancel = CancellationToken::new();
        let proxy_task = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            proxy(sock, "127.0.0.1", 1, cancel, None).await
        });
        let _client = TcpStream::connect(proxy_addr).await.unwrap();
        let err = proxy_task.await.unwrap().unwrap_err();
        // Either ConnectionRefused or another connect-time io::Error is fine —
        // both satisfy the spec ("target unreachable surfaces as io::Error").
        assert_ne!(
            err.kind(),
            io::ErrorKind::Other,
            "should be a kernel-level error, got {err}"
        );
    }
}

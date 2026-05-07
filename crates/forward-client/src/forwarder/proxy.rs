//! Bidirectional TCP proxy primitive.
//!
//! Wraps `tokio::io::copy_bidirectional` in a `select!` against a shutdown
//! token so the listener can tear down in-flight connections deterministically
//! during rule removal or process shutdown.

use std::io;
use std::sync::Arc;

use forward_core::{RuleId, Target};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::stats::RuleStats;
use crate::resolver::{ConnectError, LiveResolver, ResolveFailReason, Resolve};

/// Forward `inbound` to `target` (resolved via `resolver` for DNS
/// targets; short-circuited to a direct connect for IP literals)
/// until either side closes or the shutdown token fires. Returns
/// `(bytes_in, bytes_out)` where `bytes_in` is bytes flowing
/// inbound→outbound (from the outside requester to the target) and
/// `bytes_out` the reverse.
///
/// 003-domain-name-forward (T020): the resolver layer owns DNS
/// caching + (in US2) single-flight coalescing + family preference.
/// IP-target rules continue to add zero overhead beyond the v0.2.0
/// hot path because `LiveResolver::connect_target` short-circuits to
/// `TcpStream::connect` (Constitution II / SC-004).
///
/// `stats` is updated in two passes: `active_connections` is incremented at
/// entry and decremented on exit (RAII via the guard); byte counters get the
/// final tally from `copy_bidirectional` once it returns. Per-direction
/// updates would require `tokio_util::io::InspectReader` shims — overkill for
/// the 5-second sample window the `StatsReport` uses.
///
/// `listen_port` identifies which port in the rule's range this
/// connection arrived on (002-port-range-forward, T041). Per-port
/// counters in `stats.per_port` are updated alongside the aggregate;
/// for single-port rules `listen_port` equals the rule's only port and
/// the per-port slot may be empty (graceful degradation).
#[allow(clippy::too_many_arguments)]
pub async fn proxy<R: Resolve>(
    mut inbound: TcpStream,
    resolver: &LiveResolver<R>,
    rule_id: RuleId,
    target: &Target,
    target_port: u16,
    prefer_ipv6: bool,
    shutdown: CancellationToken,
    stats: Option<Arc<RuleStats>>,
    listen_port: u16,
) -> io::Result<(u64, u64)> {
    let mut outbound = match resolver
        .connect_target(rule_id, target, target_port, prefer_ipv6)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            // T034: refuse the inbound socket cleanly (drop closes
            // it; do NOT half-open the proxy) and emit the
            // structured DNS-failure event with rule_id + hostname +
            // classified reason. AllAddrsUnreachable counts as a
            // DNS-side failure per FR-006/FR-008.
            match &e {
                ConnectError::Resolution(reason) => {
                    let hostname_str = match target {
                        Target::Dns(h) => h.as_str().to_string(),
                        Target::Ip(ip) => ip.to_string(),
                    };
                    warn!(
                        event = "rule.dns_failed",
                        rule_id = %rule_id,
                        hostname = %hostname_str,
                        reason = ResolveFailReason::classify(reason).as_str(),
                        detail = %reason,
                    );
                }
                ConnectError::AllAddrsUnreachable { tried, last } => {
                    let hostname_str = match target {
                        Target::Dns(h) => h.as_str().to_string(),
                        Target::Ip(ip) => ip.to_string(),
                    };
                    warn!(
                        event = "rule.dns_failed",
                        rule_id = %rule_id,
                        hostname = %hostname_str,
                        reason = ResolveFailReason::AllAddrsUnreachable.as_str(),
                        tried = *tried,
                        last_error = %last,
                    );
                }
                ConnectError::Dial(_) => {
                    // Pure dial failure on an IP-target rule. Not a
                    // DNS event; let accept_loop log the generic
                    // rule.conn_error fall-through.
                }
            }
            return Err(e.into_io());
        }
    };
    // Disable Nagle to keep latency-sensitive small writes prompt; the kernel
    // still coalesces opportunistically.
    let _ = inbound.set_nodelay(true);
    let _ = outbound.set_nodelay(true);

    let _guard = stats
        .as_ref()
        .map(|s| ActiveGuard::new(Arc::clone(s), listen_port));

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
        s.record_in(listen_port, *bin);
        s.record_out(listen_port, *bout);
    }
    result
}

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
    use crate::resolver::{Resolve, ResolveAnswer, ResolverConfig, ResolverError};
    use forward_core::Hostname;
    use std::net::{IpAddr, Ipv4Addr};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Test-only resolver that panics if invoked. IP-target proxy
    /// calls MUST short-circuit and never touch this.
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
        let resolver = Arc::new(ip_resolver());
        let proxy_task = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            proxy(
                sock,
                resolver.as_ref(),
                RuleId(0),
                &Target::Ip(echo.ip()),
                echo.port(),
                false,
                cancel_proxy,
                None,
                0,
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
        let resolver = Arc::new(ip_resolver());
        let proxy_task = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            proxy(
                sock,
                resolver.as_ref(),
                RuleId(0),
                &Target::Ip(echo.ip()),
                echo.port(),
                false,
                cancel_proxy,
                None,
                0,
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
        let resolver = Arc::new(ip_resolver());
        let proxy_task = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.unwrap();
            proxy(
                sock,
                resolver.as_ref(),
                RuleId(0),
                &Target::Ip(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                1,
                false,
                cancel,
                None,
                0,
            )
            .await
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

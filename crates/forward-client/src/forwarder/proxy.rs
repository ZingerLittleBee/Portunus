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

use super::proxy_protocol::{self, ProxyProtocolPrelude};
use super::stats::RuleStats;
use crate::resolver::{AnswerSource, ConnectError, LiveResolver, Resolve, ResolveFailReason};

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
    inbound: TcpStream,
    resolver: &LiveResolver<R>,
    rule_id: RuleId,
    target: &Target,
    target_port: u16,
    prefer_ipv6: bool,
    shutdown: CancellationToken,
    stats: Option<Arc<RuleStats>>,
    listen_port: u16,
) -> io::Result<(u64, u64)> {
    // Legacy plain-TCP path: no preread bytes to replay.
    proxy_with_preread(
        inbound,
        None,
        resolver,
        rule_id,
        target,
        target_port,
        prefer_ipv6,
        shutdown,
        stats,
        listen_port,
    )
    .await
}

/// 009-tls-sni-routing T041: proxy variant that replays a `preread`
/// buffer to the upstream BEFORE switching to bidirectional copy.
/// The SNI listener (T040) calls this with the captured ClientHello
/// bytes so the upstream sees the byte-identical handshake. The
/// legacy `proxy` shim above passes `None` and stays on the byte-
/// identical v0.7 hot path.
#[allow(clippy::too_many_arguments)]
pub async fn proxy_with_preread<R: Resolve>(
    inbound: TcpStream,
    preread: Option<Vec<u8>>,
    resolver: &LiveResolver<R>,
    rule_id: RuleId,
    target: &Target,
    target_port: u16,
    prefer_ipv6: bool,
    shutdown: CancellationToken,
    stats: Option<Arc<RuleStats>>,
    listen_port: u16,
) -> io::Result<(u64, u64)> {
    proxy_with_preread_and_prelude(
        inbound,
        preread,
        resolver,
        rule_id,
        target,
        target_port,
        prefer_ipv6,
        None,
        shutdown,
        stats,
        listen_port,
    )
    .await
}

/// Variant used by SNI dispatch when an upstream target also requests a
/// PROXY protocol prelude. The injected prelude is written before any preread
/// ClientHello bytes so the upstream sees `PROXY ...\r\n` then TLS.
#[allow(clippy::too_many_arguments)]
pub async fn proxy_with_preread_and_prelude<R: Resolve>(
    mut inbound: TcpStream,
    preread: Option<Vec<u8>>,
    resolver: &LiveResolver<R>,
    rule_id: RuleId,
    target: &Target,
    target_port: u16,
    prefer_ipv6: bool,
    proxy_prelude: Option<ProxyProtocolPrelude>,
    shutdown: CancellationToken,
    stats: Option<Arc<RuleStats>>,
    listen_port: u16,
) -> io::Result<(u64, u64)> {
    let mut outbound = match resolver
        .connect_target(rule_id, target, target_port, prefer_ipv6)
        .await
    {
        Ok((s, source)) => {
            // T047 (US4): a dial that succeeded via the
            // stale-while-error grace window still counts as a DNS
            // failure (FR-005 + FR-008) — the underlying refresh
            // attempt failed, the operator just got lucky that the
            // stale answer still works. The connection is allowed
            // through (graceful degradation), but the metric counts.
            if matches!(source, AnswerSource::Stale)
                && let Some(stats) = stats.as_ref()
            {
                stats.inc_dns_failure();
            }
            s
        }
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
                    if let Some(stats) = stats.as_ref() {
                        stats.inc_dns_failure();
                    }
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
                    if let Some(stats) = stats.as_ref() {
                        stats.inc_dns_failure();
                    }
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

    if let Some(prelude) = proxy_prelude {
        proxy_protocol::write_prelude(&mut outbound, prelude).await?;
    }

    // 009-tls-sni-routing T041: replay the captured ClientHello to the
    // upstream BEFORE switching to bidirectional copy. The bytes count
    // toward the inbound→outbound tally just like a normal write — we
    // bump `record_in` for the preread length so SC-002 byte-equality
    // tests see the same totals regardless of legacy vs. SNI path.
    let mut preread_in: u64 = 0;
    if let Some(buf) = preread.as_ref()
        && !buf.is_empty()
    {
        use tokio::io::AsyncWriteExt;
        outbound.write_all(buf).await?;
        preread_in = buf.len() as u64;
    }

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
        s.record_in(listen_port, *bin + preread_in);
        s.record_out(listen_port, *bout);
    }
    result.map(|(bin, bout)| (bin + preread_in, bout))
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
    async fn proxy_with_preread_writes_proxy_prelude_before_preread() {
        let backend = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let backend_addr = backend.local_addr().unwrap();
        let (captured_tx, captured_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut sock, _) = backend.accept().await.unwrap();
            let mut captured = Vec::new();
            let mut buf = [0u8; 256];
            loop {
                let n = sock.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                captured.extend_from_slice(&buf[..n]);
                if captured.ends_with(b"client-hello") {
                    break;
                }
            }
            captured_tx.send(captured).unwrap();
        });

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let proxy_addr = listener.local_addr().unwrap();
        let resolver = Arc::new(ip_resolver());
        let proxy_task = tokio::spawn(async move {
            let (sock, peer) = listener.accept().await.unwrap();
            let local = sock.local_addr().unwrap();
            proxy_with_preread_and_prelude(
                sock,
                Some(b"client-hello".to_vec()),
                resolver.as_ref(),
                RuleId(10),
                &Target::Ip(backend_addr.ip()),
                backend_addr.port(),
                false,
                Some(crate::forwarder::proxy_protocol::ProxyProtocolPrelude {
                    version: forward_core::ProxyProtocolVersion::V1,
                    source: peer,
                    destination: local,
                }),
                CancellationToken::new(),
                None,
                proxy_addr.port(),
            )
            .await
        });

        let client = TcpStream::connect(proxy_addr).await.unwrap();
        let captured = captured_rx.await.unwrap();
        drop(client);
        let _ = proxy_task.await.unwrap();
        let captured = String::from_utf8(captured).unwrap();
        assert!(captured.starts_with("PROXY TCP4 "));
        assert!(captured.ends_with("client-hello"));
    }

    /// T043 (US4): NXDOMAIN-only path bumps `dns_failures` exactly
    /// once per refused connection. The increment happens at the
    /// proxy boundary (single seam where ConnectError is observed),
    /// not inside the resolver — so tests live with the seam.
    #[tokio::test]
    async fn dns_failures_increments_per_refused_connection() {
        use crate::resolver::ResolverError;
        use crate::resolver::test_support::MockResolver;

        let resolver = Arc::new(LiveResolver::new(
            Arc::new(MockResolver::always_fail(ResolverError::Lookup(
                "no such host".into(),
            ))),
            ResolverConfig::default(),
        ));
        let stats = RuleStats::new();
        let target = Target::Dns(Hostname::new("nx.example").unwrap());

        // Drive M end-user connections; each MUST fail and bump.
        const M: u64 = 5;
        for _ in 0..M {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let proxy_addr = listener.local_addr().unwrap();
            let r = Arc::clone(&resolver);
            let s = Arc::clone(&stats);
            let target = target.clone();
            let proxy_task = tokio::spawn(async move {
                let (sock, _) = listener.accept().await.unwrap();
                let _ = proxy(
                    sock,
                    r.as_ref(),
                    RuleId(0),
                    &target,
                    443,
                    false,
                    CancellationToken::new(),
                    Some(s),
                    0,
                )
                .await;
            });
            // Open and immediately close the client side. proxy()
            // refuses (ConnectError::Resolution) and drops the inbound
            // socket — the client may see write success then EOF on
            // read; we don't care about the data plane here.
            let client = TcpStream::connect(proxy_addr).await.unwrap();
            drop(client);
            proxy_task.await.unwrap();
        }
        assert_eq!(
            stats.snapshot_dns_failures(),
            M,
            "every refused connection MUST bump dns_failures exactly once (FR-008)"
        );
    }

    /// T043 (US4) part 2: a stale-served connection (cache returns
    /// `AnswerSource::Stale` because refresh failed within the grace
    /// window) MUST also bump `dns_failures` even though the dial
    /// succeeded. Per spec § Clarifications and FR-005: the underlying
    /// problem is real, the operator just got lucky on this attempt.
    #[tokio::test]
    async fn dns_failures_increments_on_stale_served_connection() {
        use crate::resolver::ResolverError;
        use crate::resolver::test_support::MockResolver;
        use std::time::Duration;

        let echo = spawn_echo().await;

        // First call returns the working v4 echo with a tiny TTL,
        // then every refresh fails. After the TTL elapses, cache
        // transitions to StaleAfterFailedRefresh on each refresh
        // attempt → returns AnswerSource::Stale within the 30s grace.
        let mock = MockResolver::ok_then_fail(
            vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            Duration::from_millis(1),
            ResolverError::Lookup("upstream down".into()),
        );
        // Critical: lower the cache floor so the 1ms TTL isn't
        // clamped up to the default 5s — otherwise the warmup entry
        // stays Cached and we never reach StaleAfterFailedRefresh.
        let config = ResolverConfig {
            cache_floor: Duration::from_millis(1),
            ..ResolverConfig::default()
        };
        let resolver = Arc::new(LiveResolver::new(Arc::new(mock), config));
        let stats = RuleStats::new();
        let target = Target::Dns(Hostname::new("flaky.example").unwrap());

        async fn drive_one(
            resolver: Arc<LiveResolver<MockResolver>>,
            stats: Arc<RuleStats>,
            target: Target,
            echo_port: u16,
            payload: &[u8],
        ) {
            let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
            let proxy_addr = listener.local_addr().unwrap();
            let task = tokio::spawn(async move {
                let (sock, _) = listener.accept().await.unwrap();
                let _ = proxy(
                    sock,
                    resolver.as_ref(),
                    RuleId(1),
                    &target,
                    echo_port,
                    false,
                    CancellationToken::new(),
                    Some(stats),
                    0,
                )
                .await;
            });
            let mut c = TcpStream::connect(proxy_addr).await.unwrap();
            c.write_all(payload).await.unwrap();
            let mut buf = vec![0u8; payload.len()];
            c.read_exact(&mut buf).await.unwrap();
            drop(c);
            task.await.unwrap();
        }

        // Warmup: Fresh source → no bump.
        drive_one(
            Arc::clone(&resolver),
            Arc::clone(&stats),
            target.clone(),
            echo.port(),
            b"warm",
        )
        .await;
        assert_eq!(
            stats.snapshot_dns_failures(),
            0,
            "warmup (Fresh) connection MUST NOT bump"
        );

        // Wait past the (1ms) TTL so the next get_or_resolve refreshes.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // M stale-served connections. Each refresh fails → cache
        // transitions to StaleAfterFailedRefresh → AnswerSource::Stale
        // → bump.
        const M: u64 = 3;
        for _ in 0..M {
            drive_one(
                Arc::clone(&resolver),
                Arc::clone(&stats),
                target.clone(),
                echo.port(),
                b"stale",
            )
            .await;
        }
        assert_eq!(
            stats.snapshot_dns_failures(),
            M,
            "stale-served connections MUST bump dns_failures (FR-005 + FR-008)"
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

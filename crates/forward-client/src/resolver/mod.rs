//! Client-local DNS resolver layer.
//!
//! Spec: `003-domain-name-forward` — the seam between the proxy hot
//! path and `TcpStream::connect`. IP-target rules skip the resolver
//! entirely. DNS-target rules go through the cache (`cache.rs`),
//! which honors resolver-reported TTL clamped to
//! `[ResolverConfig::cache_floor, cache_ceiling]` (FR-003) and (in
//! US2) coalesces concurrent first-connects via single-flight
//! (FR-012).
//!
//! The trait `Resolve` exists so unit tests can swap in a
//! `MockResolver` / `CountingResolver` without spinning up a real
//! resolver — Constitution III for a network-backed dependency.
//!
//! Live implementation: `LiveResolver` wraps a hickory-resolver
//! `TokioAsyncResolver` configured from `/etc/resolv.conf` (Linux) /
//! the OS-equivalent on macOS, with no DoT/DoH (out of scope per
//! spec § Assumptions).

pub mod cache;

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use forward_core::{Hostname, Target};
use thiserror::Error;
use tokio::net::TcpStream;

use cache::Cache;

/// Process-wide resolver constants. All fields are spec-fixed in
/// v0.3.0 — no CLI/config wire-up in this feature; the struct exists
/// so future work can swap defaults for operator-supplied values
/// without changing call sites.
///
/// `stale_while_error_grace` is a fixed spec budget per FR-005 —
/// not a runtime knob even when future work exposes the cache
/// floor/ceiling.
#[derive(Debug, Clone, Copy)]
pub struct ResolverConfig {
    /// Lower clamp on resolver-reported TTL (FR-003).
    pub cache_floor: Duration,
    /// Upper clamp on resolver-reported TTL (FR-003).
    pub cache_ceiling: Duration,
    /// Stale-while-error window past TTL when fresh resolution
    /// fails (FR-005). Fixed spec budget. Consumed by the
    /// `StaleAfterFailedRefresh` cache state in US2 (T029); held
    /// here in US1 so US2 doesn't need a breaking config change.
    #[allow(dead_code)]
    pub stale_while_error_grace: Duration,
    /// Per-resolver-attempt timeout (Assumptions / SC-003 budget).
    pub attempt_timeout: Duration,
    /// After grace expiry, brief delay before next resolver attempt.
    /// Consumed by the `Failed { retry_after }` cache state in US2.
    #[allow(dead_code)]
    pub negative_cache_retry: Duration,
    /// Cap on `Pending` entries to bound resolver-side load when
    /// many unique names go bad simultaneously (US2 enforces; in
    /// US1 the field is unused but reserved on the public surface
    /// so US2 doesn't need a breaking API change).
    #[allow(dead_code)]
    pub max_concurrent_resolves: usize,
}

impl Default for ResolverConfig {
    fn default() -> Self {
        Self {
            cache_floor: Duration::from_secs(5),
            cache_ceiling: Duration::from_secs(300),
            stale_while_error_grace: Duration::from_secs(30),
            attempt_timeout: Duration::from_secs(3),
            negative_cache_retry: Duration::from_secs(3),
            max_concurrent_resolves: 64,
        }
    }
}

/// Resolver-layer error taxonomy.
#[derive(Debug, Clone, Error)]
pub enum ResolverError {
    #[error("dns_resolution_failed: empty answer set")]
    EmptyAnswer,

    #[error("dns_resolution_failed: {0}")]
    Lookup(String),

    #[error("dns_resolution_failed: attempt timeout after {0:?}")]
    AttemptTimeout(Duration),
}

/// Coarse classification of resolver failures. Fully populated in
/// US2 (T033). Kept here so US1 can declare the stable taxonomy
/// without back-tracking; variants are unused until US2 wires
/// classification into the cache state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum ResolveFailReason {
    NxDomain,
    ServFail,
    AttemptTimeout,
    AllAddrsUnreachable,
    Other,
}

/// One resolver answer. Exposed via the `Resolve` trait so the cache
/// layer doesn't depend on hickory's concrete types.
#[derive(Debug, Clone)]
pub struct ResolveAnswer {
    pub addrs: Vec<IpAddr>,
    /// Resolver-reported TTL **before** clamping. The cache layer
    /// applies the `[cache_floor, cache_ceiling]` clamp.
    pub ttl: Duration,
}

#[async_trait::async_trait]
pub trait Resolve: Send + Sync {
    async fn resolve(&self, name: &Hostname) -> Result<ResolveAnswer, ResolverError>;
}

/// The seam the proxy hot path uses. IP-target rules short-circuit
/// to `TcpStream::connect`; DNS-target rules consult the cache, then
/// dial.
pub struct LiveResolver<R: Resolve> {
    inner: Arc<R>,
    cache: Cache,
    config: ResolverConfig,
}

impl<R: Resolve> LiveResolver<R> {
    pub fn new(inner: Arc<R>, config: ResolverConfig) -> Self {
        Self {
            inner,
            cache: Cache::new(),
            config,
        }
    }

    /// Connect to `target:port`. For IP literals this skips the
    /// resolver entirely (T012 short-circuit). For DNS targets the
    /// cache is consulted; on miss the resolver is invoked once,
    /// the answer is clamped to `[cache_floor, cache_ceiling]` and
    /// stored.
    ///
    /// `_prefer_ipv6` is plumbed through but unused in US1 — the
    /// family-preference logic lands in T040 (US3). For now the
    /// resolver-returned address order is honored as-is.
    pub async fn connect_target(
        &self,
        target: &Target,
        port: u16,
        _prefer_ipv6: bool,
    ) -> io::Result<TcpStream> {
        match target {
            Target::Ip(ip) => TcpStream::connect(SocketAddr::new(*ip, port)).await,
            Target::Dns(name) => {
                let addrs = self
                    .cache
                    .get_or_resolve(name, self.inner.as_ref(), &self.config)
                    .await
                    .map_err(|e| io::Error::other(e.to_string()))?;

                // US1: dial the first address. Multi-A fallback on
                // dial failure lands in US2 (T033a).
                let first = addrs.first().copied().ok_or_else(|| {
                    io::Error::other("dns_resolution_failed: empty answer after cache")
                })?;
                TcpStream::connect(SocketAddr::new(first, port)).await
            }
        }
    }
}

/// hickory-backed `Resolve` impl — reads `/etc/resolv.conf` natively
/// via `system-config`, plays nicely with Tokio.
pub struct HickoryResolver {
    resolver: hickory_resolver::TokioAsyncResolver,
    attempt_timeout: Duration,
}

impl HickoryResolver {
    pub fn from_system(config: &ResolverConfig) -> io::Result<Self> {
        let resolver =
            hickory_resolver::TokioAsyncResolver::tokio_from_system_conf().map_err(|e| {
                io::Error::other(format!(
                    "dns_resolver_init_failed: could not load system resolv.conf: {e}"
                ))
            })?;
        Ok(Self {
            resolver,
            attempt_timeout: config.attempt_timeout,
        })
    }
}

#[async_trait::async_trait]
impl Resolve for HickoryResolver {
    async fn resolve(&self, name: &Hostname) -> Result<ResolveAnswer, ResolverError> {
        let attempt = self.resolver.lookup_ip(name.as_str());
        let lookup = tokio::time::timeout(self.attempt_timeout, attempt)
            .await
            .map_err(|_| ResolverError::AttemptTimeout(self.attempt_timeout))?
            .map_err(|e| ResolverError::Lookup(e.to_string()))?;
        let addrs: Vec<IpAddr> = lookup.iter().collect();
        // hickory exposes per-record TTLs via `valid_until()` (an
        // `Instant`). For the cache contract we want a `Duration`
        // representing the "advertised TTL" the resolver gave us.
        // Use the soonest record's expiry to be conservative; if
        // unavailable, fall back to the configured floor (the cache
        // will clamp to floor/ceiling anyway).
        let ttl = lookup
            .valid_until()
            .checked_duration_since(std::time::Instant::now())
            .unwrap_or(Duration::from_secs(0));
        Ok(ResolveAnswer { addrs, ttl })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// T012: IP-target rules MUST NOT touch the resolver. We pass a
    /// resolver whose `resolve()` panics; if `connect_target` ever
    /// reaches the DNS branch the test fails loudly.
    #[derive(Debug, Default)]
    struct PanickingResolver;

    #[async_trait::async_trait]
    impl Resolve for PanickingResolver {
        async fn resolve(&self, name: &Hostname) -> Result<ResolveAnswer, ResolverError> {
            panic!("PanickingResolver::resolve was called for {name}");
        }
    }

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
    async fn ip_target_short_circuits_resolver() {
        // T012: a Target::Ip(_) call to connect_target reaches the
        // socket directly without invoking PanickingResolver.
        let echo = spawn_echo().await;
        let target = Target::Ip(echo.ip());
        let resolver = LiveResolver::new(Arc::new(PanickingResolver), ResolverConfig::default());
        let mut sock = resolver
            .connect_target(&target, echo.port(), false)
            .await
            .expect("ip target should connect without resolver");
        sock.write_all(b"ping").await.unwrap();
        let mut buf = [0u8; 4];
        sock.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"ping");
    }
}

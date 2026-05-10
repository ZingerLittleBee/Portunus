//! Atomic bind fan-out for range rules (002-port-range-forward, T026).
//!
//! Range rules require all-or-nothing semantics (FR-004): if any port in
//! the requested range is unavailable, every successfully-bound port in
//! the range MUST be released before the failure is reported. The
//! function exists in its own module so it can be unit-tested
//! independently of the forwarder lifecycle (which deals with accept
//! loops, drain timeouts, and event channels).

use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};

use portunus_core::PortRange;
use tokio::net::{TcpListener, TcpSocket};

/// Atomically bind every port in `listen` on IPv4 and IPv6. On any bind
/// failure, every previously-bound listener is dropped before returning,
/// so the kernel observes "no partial state" — operators can re-push the
/// same range after fixing the offending port without waiting for stray
/// `TIME_WAIT` slots.
///
/// The returned vector is grouped in `listen.iter()` order. Each port may
/// contribute both an IPv6 and an IPv4 listener, so callers should treat the
/// port value as the key rather than assuming exactly one listener per port.
///
/// # Errors
///
/// Returns `BindFailure { offending_port, reason }` on the first port
/// that fails to bind. `reason` comes from [`classify_bind_error`] and
/// matches the strings the forwarder reports via `RuleStatusEvent::Failed`.
pub fn bind_all(listen: &PortRange) -> Result<Vec<(u16, TcpListener)>, BindFailure> {
    let mut bound: Vec<(u16, TcpListener)> =
        Vec::with_capacity(usize::try_from(listen.len()).unwrap_or(usize::MAX));
    for port in listen.iter() {
        match bind_port(port) {
            Ok(listeners) => {
                bound.extend(listeners.into_iter().map(|listener| (port, listener)));
            }
            Err(e) => {
                let reason = classify_bind_error(&e);
                // Drop all previously-bound listeners *before* surfacing
                // the error. Dropping a `TcpListener` immediately closes
                // the underlying socket on Linux/macOS, so by the time
                // the caller observes `Err`, every port is already
                // released. (R-001 + FR-004.)
                drop(bound);
                return Err(BindFailure {
                    offending_port: port,
                    reason,
                });
            }
        }
    }
    Ok(bound)
}

fn bind_port(port: u16) -> io::Result<Vec<TcpListener>> {
    let mut listeners = Vec::with_capacity(2);
    match bind_ipv6(port) {
        Ok(listener) => listeners.push(listener),
        Err(err) if should_fallback_to_ipv4(&err) => {
            return bind_ipv4(port).map(|listener| vec![listener]);
        }
        Err(err) => return Err(err),
    }
    listeners.push(bind_ipv4(port)?);
    Ok(listeners)
}

fn bind_ipv6(port: u16) -> io::Result<TcpListener> {
    let socket = TcpSocket::new_v6()?;
    set_ipv6_only(&socket)?;
    socket.bind(SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)))?;
    socket.listen(1024)
}

fn bind_ipv4(port: u16) -> io::Result<TcpListener> {
    let socket = TcpSocket::new_v4()?;
    socket.bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)))?;
    socket.listen(1024)
}

fn set_ipv6_only(socket: &TcpSocket) -> io::Result<()> {
    nix::sys::socket::setsockopt(socket, nix::sys::socket::sockopt::Ipv6V6Only, &true)
        .map_err(io::Error::other)
}

fn should_fallback_to_ipv4(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::AddrNotAvailable | io::ErrorKind::Unsupported
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BindFailure {
    pub offending_port: u16,
    pub reason: &'static str,
}

/// Map a `std::io::Error` from `TcpListener::bind` into the stable
/// reason strings the operator surface keys off. Kept here (rather
/// than in `mod.rs`) so the range path and the legacy single-port path
/// share one classifier.
#[must_use]
pub fn classify_bind_error(e: &std::io::Error) -> &'static str {
    match e.kind() {
        std::io::ErrorKind::AddrInUse => "port_in_use",
        std::io::ErrorKind::PermissionDenied => "permission_denied",
        _ => "bind_failed",
    }
}

/// Process-wide test-only lock used to serialize port-pool exhausting
/// tests across modules (`forwarder::tests` + `forwarder::range::tests`).
/// Without this they race for OS-assigned ephemeral ports under
/// `cargo test`'s parallel runner.
#[cfg(test)]
pub(crate) fn test_port_pool_lock() -> &'static tokio::sync::Mutex<()> {
    use std::sync::OnceLock;
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpStream;

    /// Convenience alias so existing test code reads `port_pool_lock()`.
    fn port_pool_lock() -> &'static tokio::sync::Mutex<()> {
        super::test_port_pool_lock()
    }

    /// Pick `n` consecutive ports that are free for both IPv4 and IPv6.
    async fn pick_consecutive_free(n: u16) -> PortRange {
        for _ in 0..50 {
            // Let the OS pick a candidate base, then release it and verify the
            // whole range through bind_port so IPv4 and IPv6 availability agree
            // with the production path.
            let Ok(probe) = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await else {
                continue;
            };
            let start = probe.local_addr().unwrap().port();
            drop(probe);
            if u32::from(start) + u32::from(n) > 65_536 {
                continue;
            }
            let mut probes: Vec<TcpListener> = Vec::new();
            let mut ok = true;
            for offset in 0..n {
                let Ok(listeners) = bind_port(start + offset) else {
                    ok = false;
                    break;
                };
                probes.extend(listeners);
            }
            if ok {
                drop(probes);
                return PortRange::new(start, start + n - 1).unwrap();
            }
            drop(probes);
        }
        panic!("could not find {n} consecutive free ports after 50 attempts");
    }

    #[tokio::test]
    async fn bind_all_succeeds_for_50_consecutive_ports() {
        let _guard = port_pool_lock().lock().await;
        let range = pick_consecutive_free(50).await;
        let bound = bind_all(&range).unwrap();
        let ports = bound
            .iter()
            .map(|(port, _)| *port)
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(ports.len(), 50);
        assert!(ports.contains(&range.start()));
        assert!(ports.contains(&range.end()));
        for (port, listener) in &bound {
            assert_eq!(listener.local_addr().unwrap().port(), *port);
        }
    }

    #[tokio::test]
    async fn bind_all_releases_all_on_partial_failure() {
        let _guard = port_pool_lock().lock().await;
        // Pick a free range, then occupy a port in the middle. bind_all MUST
        // fail naming that port AND release every other port it bound first.
        let range = pick_consecutive_free(10).await;
        let busy_port = range.start() + 5;
        // Occupy the middle port. We bind 0.0.0.0 to mirror what `bind_all`
        // does, otherwise SO_REUSEADDR on different addresses can let it
        // sneak through on macOS.
        let occupy = TcpListener::bind((Ipv4Addr::UNSPECIFIED, busy_port))
            .await
            .unwrap();

        let err = bind_all(&range).unwrap_err();
        assert_eq!(err.offending_port, busy_port);
        assert_eq!(err.reason, "port_in_use");

        // Every port in the range that bind_all touched MUST be free
        // again. We check the ones BEFORE the offending port (those got
        // bound and then dropped) — the ones after were never touched.
        for p in range.start()..busy_port {
            let probe = TcpListener::bind((Ipv4Addr::UNSPECIFIED, p)).await;
            assert!(probe.is_ok(), "port {p} not released after rollback");
        }

        // Sanity: the still-occupied port refuses fresh binds.
        let still_busy = TcpListener::bind((Ipv4Addr::UNSPECIFIED, busy_port)).await;
        assert!(still_busy.is_err());
        drop(still_busy);

        // Drop the squatter and confirm we can listen on the previously
        // busy port. We don't re-run bind_all on the whole range here —
        // the parallel test pool may have grabbed neighbouring ports
        // while we were running our assertions, and the rollback +
        // single-port-recovery is the spec property under test.
        drop(occupy);
        let recover = TcpListener::bind((Ipv4Addr::UNSPECIFIED, busy_port)).await;
        assert!(recover.is_ok(), "previously-busy port not reusable");
        let recover = recover.unwrap();
        let conn = TcpStream::connect((Ipv4Addr::LOCALHOST, busy_port)).await;
        assert!(conn.is_ok());
        drop(conn);
        drop(recover);
    }

    #[tokio::test]
    async fn bind_all_handles_size_one_range() {
        let _guard = port_pool_lock().lock().await;
        // Degenerate range — same call path as a single-port rule.
        let range = pick_consecutive_free(1).await;
        let bound = bind_all(&range).unwrap();
        assert!(!bound.is_empty());
        assert!(bound.iter().all(|(port, _)| *port == range.start()));
    }

    #[tokio::test]
    async fn bind_all_accepts_ipv6_loopback_connections() {
        let _guard = port_pool_lock().lock().await;
        let range = pick_consecutive_free(1).await;
        let bound = bind_all(&range).unwrap();
        let port = bound[0].0;
        let connect = TcpStream::connect((std::net::Ipv6Addr::LOCALHOST, port)).await;
        assert!(
            connect.is_ok(),
            "dual-stack listener must accept IPv6 clients"
        );
    }
}

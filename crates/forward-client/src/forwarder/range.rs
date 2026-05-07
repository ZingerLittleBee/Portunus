//! Atomic bind fan-out for range rules (002-port-range-forward, T026).
//!
//! Range rules require all-or-nothing semantics (FR-004): if any port in
//! the requested range is unavailable, every successfully-bound port in
//! the range MUST be released before the failure is reported. The
//! function exists in its own module so it can be unit-tested
//! independently of the forwarder lifecycle (which deals with accept
//! loops, drain timeouts, and event channels).

use std::net::Ipv4Addr;

use forward_core::PortRange;
use tokio::net::TcpListener;

/// Atomically bind every port in `listen` to `0.0.0.0:port`. On any bind
/// failure, every previously-bound listener is dropped before returning,
/// so the kernel observes "no partial state" — operators can re-push the
/// same range after fixing the offending port without waiting for stray
/// `TIME_WAIT` slots.
///
/// The returned vector is ordered identically to `listen.iter()`:
/// `[(start, _), (start+1, _), …, (end, _)]`. Callers that want a
/// `HashMap` can fold themselves; we return an ordered `Vec` because the
/// caller (`forwarder::run`) immediately spawns one accept task per
/// listener and benefits from deterministic ordering for log/test
/// readability.
///
/// # Errors
///
/// Returns `BindFailure { offending_port, reason }` on the first port
/// that fails to bind. `reason` comes from [`classify_bind_error`] and
/// matches the strings the forwarder reports via `RuleStatusEvent::Failed`.
pub async fn bind_all(listen: &PortRange) -> Result<Vec<(u16, TcpListener)>, BindFailure> {
    let mut bound: Vec<(u16, TcpListener)> =
        Vec::with_capacity(usize::try_from(listen.len()).unwrap_or(usize::MAX));
    for port in listen.iter() {
        match TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).await {
            Ok(l) => bound.push((port, l)),
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

    /// Pick `n` consecutive free ports. Race-resistant strategy:
    ///   1. Let the OS pick a base port (`bind(0)`).
    ///   2. Hold that listener AND try to bind every subsequent port
    ///      `(base+1)..(base+n-1)` to `0.0.0.0:port` — same address
    ///      family `bind_all` uses, so a successful probe means
    ///      `bind_all` will see the port as free moments later.
    ///   3. If any probe fails, drop everything and retry with a new
    ///      base port (parallel tests grab from the same ephemeral
    ///      pool, so the first attempt sometimes loses the race).
    ///
    /// All probe listeners are held until *after* we resolve the
    /// chosen range, so other tests can't squat in the middle while
    /// the helper is searching.
    async fn pick_consecutive_free(n: u16) -> PortRange {
        for _ in 0..50 {
            // bind(0) on 0.0.0.0 to match bind_all's address family.
            let Ok(probe) = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await else {
                continue;
            };
            let start = probe.local_addr().unwrap().port();
            if u32::from(start) + u32::from(n) > 65_536 {
                drop(probe);
                continue;
            }
            let mut probes: Vec<TcpListener> = vec![probe];
            let mut ok = true;
            for offset in 1..n {
                if let Ok(l) = TcpListener::bind((Ipv4Addr::UNSPECIFIED, start + offset)).await {
                    probes.push(l);
                } else {
                    ok = false;
                    break;
                }
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
        let bound = bind_all(&range).await.unwrap();
        assert_eq!(bound.len(), 50);
        assert_eq!(bound[0].0, range.start());
        assert_eq!(bound[49].0, range.end());
        // Each listener has the right local port.
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

        let err = bind_all(&range).await.unwrap_err();
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
        let bound = bind_all(&range).await.unwrap();
        assert_eq!(bound.len(), 1);
        assert_eq!(bound[0].0, range.start());
    }
}

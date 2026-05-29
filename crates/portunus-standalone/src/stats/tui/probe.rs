//! Client-side TCP-connect latency probing for the stats TUI.
//!
//! Probes are issued only for the selected rule's active target while the
//! Detail tab is visible (see `tui::run_loop`). Nothing here touches the
//! daemon or the UDS wire protocol — the target `host:port` already arrive
//! in `Hello`.

use std::time::{Duration, Instant};

use tokio::net::TcpStream;

use crate::stats::{RuleMeta, TargetMeta};

/// How long a single connect probe may take before it is reported as
/// `Timeout`.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// Minimum spacing between probes. Decoupled from the snapshot refresh so
/// one probe per interval stays negligible load regardless of `refresh_ms`.
pub const PROBE_INTERVAL: Duration = Duration::from_secs(2);

/// Outcome of one TCP connect probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeSample {
    /// Connect succeeded; the value is the measured connect time.
    Ok(Duration),
    /// Connect did not complete within `PROBE_TIMEOUT`.
    Timeout,
    /// Connect failed (refused, unreachable, or DNS error).
    Failed,
}

/// Index of the active (lowest-priority) target, or `None` if the rule has
/// no targets. `min_by_key` returns the first minimum on ties, so the
/// prober and the renderer always agree on the single active row.
#[must_use]
pub fn active_target_index(meta: &RuleMeta) -> Option<usize> {
    meta.targets
        .iter()
        .enumerate()
        .min_by_key(|(_, t)| t.priority)
        .map(|(i, _)| i)
}

/// The active target of a rule, or `None` if it has no targets.
#[must_use]
pub fn active_target(meta: &RuleMeta) -> Option<&TargetMeta> {
    active_target_index(meta).map(|i| &meta.targets[i])
}

/// Measure TCP connect time to `host:port`. `TcpStream::connect` performs
/// DNS resolution internally, so `host` may be a domain or an IP literal;
/// the `(host, port)` tuple form also handles IPv6 literals correctly. The
/// connection is dropped immediately, so the probe leaves no lingering
/// socket.
pub async fn probe_tcp(host: &str, port: u16) -> ProbeSample {
    let start = Instant::now();
    match tokio::time::timeout(PROBE_TIMEOUT, TcpStream::connect((host, port))).await {
        Ok(Ok(_stream)) => ProbeSample::Ok(start.elapsed()),
        Ok(Err(_)) => ProbeSample::Failed,
        Err(_) => ProbeSample::Timeout,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::TargetMeta;

    fn target(host: &str, port: u16, priority: u32) -> TargetMeta {
        TargetMeta {
            host: host.into(),
            port,
            priority,
            proxy_protocol: None,
        }
    }

    fn meta_with(targets: Vec<TargetMeta>) -> RuleMeta {
        RuleMeta {
            id: "r".into(),
            name: "r".into(),
            proto: "tcp".into(),
            listen: "1".into(),
            targets,
            splice_capable: true,
            udp_max_flows: None,
        }
    }

    #[test]
    fn active_target_picks_lowest_priority() {
        let m = meta_with(vec![target("a", 1, 5), target("b", 1, 0), target("c", 1, 3)]);
        assert_eq!(active_target_index(&m), Some(1));
        assert_eq!(active_target(&m).unwrap().host, "b");
    }

    #[test]
    fn active_target_none_when_empty() {
        let m = meta_with(vec![]);
        assert_eq!(active_target_index(&m), None);
        assert!(active_target(&m).is_none());
    }

    #[tokio::test]
    async fn probe_ok_against_loopback_listener() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // Accept in the background so the connect completes.
        tokio::spawn(async move {
            let _ = listener.accept().await;
        });
        let s = probe_tcp(&addr.ip().to_string(), addr.port()).await;
        assert!(matches!(s, ProbeSample::Ok(_)), "got {s:?}");
    }

    #[tokio::test]
    async fn probe_failed_against_closed_port() {
        // Bind then drop to obtain a port nothing listens on.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let s = probe_tcp(&addr.ip().to_string(), addr.port()).await;
        assert!(
            matches!(s, ProbeSample::Failed | ProbeSample::Timeout),
            "got {s:?}"
        );
    }
}

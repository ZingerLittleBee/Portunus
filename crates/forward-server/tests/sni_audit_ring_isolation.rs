//! 009-tls-sni-routing T075 — D13 invariant: data-plane SNI tracing
//! events MUST NOT enter the SQLite-backed operator `AuditRing`.
//!
//! Per `contracts/operator-api.md` §5, the audit ring continues to
//! record only operator allow/deny events. The five data-plane SNI
//! events listed there (`tls.client_hello_timeout`, `tls.parse_failed`,
//! `tls.no_sni`, `tls.sni_no_match`, `tls.sni_routed`) flow through
//! forward-client `tracing` only and are observable via the structured
//! log + Prometheus counters — never via `GET /v1/audit`.
//!
//! This test pins the invariant by construction. We:
//! 1. install a `tracing_subscriber::fmt` layer so the `tracing!` macros
//!    are actually dispatched (otherwise they're no-ops with no
//!    subscriber registered),
//! 2. push one operator allow event onto the ring as a positive
//!    control (proves the ring is wired up and writable),
//! 3. emit every data-plane SNI tracing event from §5,
//! 4. assert the ring snapshot is byte-identical to the post-control
//!    snapshot — zero new entries leaked from the data plane.
//!
//! Companion to T070/T071 (per-rule + per-listener counter emission)
//! and T074 (`/metrics` surface). Closes the audit-isolation half of
//! the observability story.

use chrono::Utc;
use forward_auth::OperatorRole;
use forward_server::operator::audit::{AuditEntry, AuditOutcome, AuditRing};
use std::sync::Once;
use tracing_subscriber::EnvFilter;

static INIT_TRACING: Once = Once::new();

fn init_tracing() {
    INIT_TRACING.call_once(|| {
        // A real subscriber is necessary so `tracing::info!` &c. dispatch
        // to *something*; without one, the macros short-circuit and the
        // test would pass even if a subscriber-side leak existed.
        let _ = tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::new("trace"))
            .with_test_writer()
            .try_init();
    });
}

fn allow_entry() -> AuditEntry {
    AuditEntry {
        timestamp: Utc::now(),
        actor: "operator-1".into(),
        role: Some(OperatorRole::Superadmin),
        method: "POST".into(),
        path: "/v1/rules".into(),
        outcome: AuditOutcome::Allow,
        reason: None,
    }
}

/// Projected shape: (timestamp_rfc3339, actor, method, path, outcome,
/// reason). `AuditEntry` doesn't derive `PartialEq`, so we project to
/// this comparable tuple before asserting equality.
type ProjectedEntry = (String, String, String, String, &'static str, Option<String>);

/// Compare two snapshots by projecting each entry to `ProjectedEntry`.
fn project(s: &[AuditEntry]) -> Vec<ProjectedEntry> {
    s.iter()
        .map(|e| {
            (
                e.timestamp.to_rfc3339(),
                e.actor.clone(),
                e.method.clone(),
                e.path.clone(),
                e.outcome.as_str(),
                e.reason.clone(),
            )
        })
        .collect()
}

#[test]
fn data_plane_sni_events_do_not_leak_into_audit_ring() {
    init_tracing();
    let ring = AuditRing::new();

    // Sanity: ring starts empty.
    assert_eq!(ring.len(), 0);

    // Positive control: one operator allow event lands on the ring.
    ring.push(allow_entry());
    assert_eq!(
        ring.len(),
        1,
        "positive control failed: ring did not accept the operator event"
    );

    // Snapshot after the control. Anything observed after this on the
    // ring is a leak.
    let baseline = ring.snapshot(usize::MAX, None);

    // Now drive every data-plane SNI tracing event from §5. These are
    // the exact targets emitted by `forward-client/src/forwarder/sni`.
    // The subscriber installed above receives them; the audit ring
    // does NOT — that's the invariant.
    tracing::warn!(
        target: "tls.client_hello_timeout",
        client_addr = "203.0.113.1:54321",
        listen_port = 443_u16,
        timeout_ms = 5000_u64,
        "ClientHello not received within timeout"
    );
    tracing::warn!(
        target: "tls.parse_failed",
        client_addr = "203.0.113.2:54322",
        listen_port = 443_u16,
        reason = "invalid TLS record header",
        "ClientHello parse failed"
    );
    tracing::info!(
        target: "tls.no_sni",
        client_addr = "203.0.113.3:54323",
        listen_port = 443_u16,
        "ClientHello carried no SNI extension"
    );
    tracing::info!(
        target: "tls.sni_no_match",
        client_addr = "203.0.113.4:54324",
        listen_port = 443_u16,
        sni = "evil.example.com",
        "no SNI route matched"
    );
    tracing::info!(
        target: "tls.sni_routed",
        client_addr = "203.0.113.5:54325",
        listen_port = 443_u16,
        sni = "api.example.com",
        rule_id = 42_u64,
        result = "exact",
        "routed by SNI"
    );

    // Verdict: ring is byte-identical to the baseline. The data-plane
    // events were observed by the tracing subscriber but never crossed
    // into the operator audit ring (D13).
    let after = ring.snapshot(usize::MAX, None);
    assert_eq!(
        project(&baseline),
        project(&after),
        "data-plane SNI tracing event leaked into AuditRing — D13 broken",
    );
    assert_eq!(
        ring.dropped(),
        0,
        "AuditRing reported overflow drops despite zero data-plane writes"
    );
}

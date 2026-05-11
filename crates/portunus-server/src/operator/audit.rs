//! 006-management-web-ui T008: in-memory audit ring buffer surfaced via
//! `GET /v1/audit`. Source of truth remains the structured tracing log
//! (Constitution IV); this ring is a convenience read of the last 1000
//! `operator.allow` / `operator.deny` events for the Web UI's Audit Log
//! page (FR-010).
//!
//! Capacity: 1000 entries (≈ 200 KB resident, see plan.md "Scale/Scope").
//! Eviction: drop-oldest on overflow; the eviction count is bumped on
//! the `portunus_audit_buffer_drops_total` Prometheus counter so
//! operators can spot a write-heavy ring under burst traffic.
//!
//! Token hygiene (Constitution IV): callers MUST NOT push raw bearer
//! tokens. This is enforced by construction — `AuditEntry` only carries
//! the post-verify `actor` / `role`.
//!
//! 007-multi-target-failover T052: per-target health transitions
//! (`event = "rule.target.health_changed"` from
//! `portunus-client::forwarder::failover::record_failure / record_success`)
//! flow through the structured tracing log alongside the operator
//! allow/deny events recorded here. They are NOT pushed onto this ring
//! because they originate on the client side and are emitted on every
//! Healthy↔Failed transition (potentially many per minute under
//! sustained instability) — putting them on the same drop-oldest ring
//! would evict legitimate operator events under churn. Correlate via
//! the structured log: filter `event = "rule.target.health_changed"`
//! together with `rule_id` to tie a failover to the operator that
//! pushed the rule (whose `actor` is in this ring).

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Utc};
use portunus_auth::OperatorRole;
use prometheus::IntCounter;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Maximum entries retained in the audit ring. Spec
/// `data-model.md` § AuditRing.
pub const AUDIT_RING_CAPACITY: usize = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuditOutcome {
    Allow,
    Deny,
}

impl AuditOutcome {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }

    /// Parse a query-string filter value. `?outcome=allow|deny`.
    /// Returns `None` for unknown values; the caller maps that to
    /// HTTP 422 `invalid_outcome`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "allow" => Some(Self::Allow),
            "deny" => Some(Self::Deny),
            _ => None,
        }
    }
}

/// Single audit log row. Fields mirror `auth_layer`'s structured-log
/// fields so the UI's table column mapping is 1:1.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub timestamp: DateTime<Utc>,
    pub actor: String,
    /// `None` only for failed auth (`_anonymous` deny path) — keeping
    /// the field optional preserves the v0.5 log shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<OperatorRole>,
    pub method: String,
    pub path: String,
    pub outcome: AuditOutcome,
    /// `None` on allow, `RbacError::code()` on deny.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Stable event/action name for non-request audit events. Request
    /// auth rows leave this absent and derive action from `method path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_value: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

#[derive(Clone, Default)]
pub struct AuditRing {
    inner: Arc<Mutex<VecDeque<AuditEntry>>>,
    /// In-process eviction counter (test-friendly snapshot).
    drops: Arc<AtomicU64>,
    /// Optional Prometheus counter — bumped in lockstep with `drops`.
    /// Bound after construction via `bind_drops_metric` so `Metrics`
    /// owns the registry and `AuditRing` doesn't depend on it.
    drops_metric: Arc<Mutex<Option<IntCounter>>>,
    /// 008-sqlite-storage T032 — optional fan-out to the durable
    /// audit writer. When bound, every `push(...)` also records the
    /// entry into the SQLite-backed table via the bounded mpsc queue.
    /// Bound after construction in `serve.rs` so the AppState
    /// constructor stays unchanged.
    durable: Arc<Mutex<Option<crate::store::audit_writer::Handle>>>,
}

impl std::fmt::Debug for AuditRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditRing")
            .field("len", &self.len())
            .field("drops", &self.dropped())
            .finish()
    }
}

impl AuditRing {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 006-management-web-ui T009: stitch a Prometheus counter into
    /// the ring so overflow events appear on `/metrics` as
    /// `portunus_audit_buffer_drops_total`. Idempotent; later calls
    /// replace the previous metric.
    pub fn bind_drops_metric(&self, counter: IntCounter) {
        *self
            .drops_metric
            .lock()
            .expect("AuditRing drops_metric mutex poisoned") = Some(counter);
    }

    /// 008-sqlite-storage T032 — bind the durable audit writer's
    /// handle so every `push(entry)` also records the entry into the
    /// SQLite-backed `audit` table. Idempotent.
    pub fn bind_durable_writer(&self, handle: crate::store::audit_writer::Handle) {
        *self
            .durable
            .lock()
            .expect("AuditRing durable mutex poisoned") = Some(handle);
    }

    /// Push a new entry. On overflow the oldest entry is dropped and
    /// the drop counter is bumped (and mirrored into the Prometheus
    /// counter if one was bound).
    pub fn push(&self, entry: AuditEntry) {
        // 008-sqlite-storage T032: fan out to the durable writer
        // BEFORE the in-memory push so a panic on the ring lock does
        // not lose the durable copy. The durable writer's `record`
        // is non-blocking.
        if let Ok(guard) = self.durable.lock()
            && let Some(h) = guard.as_ref()
        {
            h.record(entry.clone());
        }

        let mut q = self.inner.lock().expect("AuditRing mutex poisoned");
        if q.len() == AUDIT_RING_CAPACITY {
            q.pop_front();
            self.drops.fetch_add(1, Ordering::Relaxed);
            if let Ok(guard) = self.drops_metric.lock()
                && let Some(c) = guard.as_ref()
            {
                c.inc();
            }
        }
        q.push_back(entry);
    }

    /// Newest-first snapshot of the last `limit` entries, optionally
    /// filtered by outcome (server-side filter — keeps the response
    /// small for bandwidth-constrained clients).
    #[must_use]
    pub fn snapshot(&self, limit: usize, outcome_filter: Option<AuditOutcome>) -> Vec<AuditEntry> {
        let q = self.inner.lock().expect("AuditRing mutex poisoned");
        let mut out = Vec::with_capacity(limit.min(q.len()));
        for entry in q.iter().rev() {
            if let Some(o) = outcome_filter
                && entry.outcome != o
            {
                continue;
            }
            out.push(entry.clone());
            if out.len() == limit {
                break;
            }
        }
        out
    }

    /// Cumulative drop count. Test helper.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.drops.load(Ordering::Relaxed)
    }

    /// Current ring length. Test helper.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().expect("AuditRing mutex poisoned").len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(actor: &str, outcome: AuditOutcome) -> AuditEntry {
        AuditEntry {
            timestamp: Utc::now(),
            actor: actor.to_string(),
            role: Some(OperatorRole::Superadmin),
            method: "GET".to_string(),
            path: "/v1/users".to_string(),
            outcome,
            reason: match outcome {
                AuditOutcome::Allow => None,
                AuditOutcome::Deny => Some("port_outside_grant".to_string()),
            },
            action: None,
            resource_kind: None,
            resource_value: None,
            details: None,
        }
    }

    #[test]
    fn push_and_snapshot_newest_first() {
        let ring = AuditRing::new();
        ring.push(entry("alice", AuditOutcome::Allow));
        ring.push(entry("bob", AuditOutcome::Deny));
        ring.push(entry("carol", AuditOutcome::Allow));

        let snap = ring.snapshot(10, None);
        assert_eq!(snap.len(), 3);
        assert_eq!(snap[0].actor, "carol");
        assert_eq!(snap[1].actor, "bob");
        assert_eq!(snap[2].actor, "alice");
    }

    #[test]
    fn outcome_filter_narrows_to_deny() {
        let ring = AuditRing::new();
        ring.push(entry("alice", AuditOutcome::Allow));
        ring.push(entry("bob", AuditOutcome::Deny));
        ring.push(entry("carol", AuditOutcome::Allow));

        let snap = ring.snapshot(10, Some(AuditOutcome::Deny));
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].actor, "bob");
        assert_eq!(snap[0].outcome, AuditOutcome::Deny);
    }

    #[test]
    fn limit_caps_returned_rows() {
        let ring = AuditRing::new();
        for i in 0..50 {
            ring.push(entry(&format!("u{i}"), AuditOutcome::Allow));
        }
        let snap = ring.snapshot(10, None);
        assert_eq!(snap.len(), 10);
        assert_eq!(snap[0].actor, "u49");
    }

    #[test]
    fn overflow_drops_oldest_and_bumps_counter() {
        let ring = AuditRing::new();
        for i in 0..(AUDIT_RING_CAPACITY + 5) {
            ring.push(entry(&format!("u{i}"), AuditOutcome::Allow));
        }
        assert_eq!(ring.len(), AUDIT_RING_CAPACITY);
        assert_eq!(ring.dropped(), 5);
        // Newest entry survives.
        let snap = ring.snapshot(1, None);
        let last_id = format!("u{}", AUDIT_RING_CAPACITY + 4);
        assert_eq!(snap[0].actor, last_id);
    }

    #[test]
    fn outcome_parse_accepts_known_values() {
        assert_eq!(AuditOutcome::parse("allow"), Some(AuditOutcome::Allow));
        assert_eq!(AuditOutcome::parse("deny"), Some(AuditOutcome::Deny));
        assert_eq!(AuditOutcome::parse("banana"), None);
    }
}

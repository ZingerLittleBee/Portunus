//! Defense-in-depth redaction layer for `tracing` events.
//!
//! ## Why this exists
//!
//! Constitution Principle XIII (Operator Safety): structured logs must never
//! leak credentials. The bearer token, server private key, or any other
//! `secret`-named value getting into a log line is a CRITICAL bug. We **also**
//! enforce this by code review and by audit (no `info!`/`warn!`/`error!`
//! call in this codebase currently records a field with these names — see
//! T064 audit notes). This layer is the belt to that suspenders: a runtime
//! check that catches a regression introduced by a future patch.
//!
//! ## How it works
//!
//! [`RedactionLayer`] walks every event with a [`tracing::field::Visit`] that
//! records nothing — it only flips a flag if any field name matches one of
//! the banned substrings (case-insensitive `token`, `secret`, `private_key`).
//! If a violation is observed, the layer emits a single `audit.redaction_violation`
//! event via `eprintln!` so the operator sees it even if a misconfigured
//! formatter swallowed the original line.
//!
//! Importantly the layer does **not** rewrite the original event — it only
//! observes. The right place to redact field *values* is at the point of
//! recording (use `field = "<redacted>"` literally, or strip the value
//! before passing it to a tracing macro). The layer's job is to fail loudly
//! if a developer forgets.

use std::sync::atomic::{AtomicU64, Ordering};

use tracing::Subscriber;
use tracing::field::{Field, Visit};
use tracing::span::Attributes;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

/// Substrings (case-insensitive) that may not appear in a tracing field name.
pub const BANNED_FIELD_SUBSTRINGS: &[&str] = &["token", "secret", "private_key"];

/// True if a field name contains any banned substring (case-insensitive).
#[must_use]
pub fn is_banned_field_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    BANNED_FIELD_SUBSTRINGS.iter().any(|b| lower.contains(b))
}

/// `tracing` Layer that flags banned field names. Composes with the JSON fmt
/// layer in front of it (the JSON formatter still emits the raw event, so
/// tests can inspect both sides).
#[derive(Debug, Default)]
pub struct RedactionLayer {
    /// Total number of events that contained at least one banned field.
    /// Useful in tests to assert "no violation occurred" without parsing
    /// stderr. Increments are `Relaxed` because we only need a final count.
    violations: AtomicU64,
}

impl RedactionLayer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn violation_count(&self) -> u64 {
        self.violations.load(Ordering::Relaxed)
    }
}

impl<S> Layer<S> for RedactionLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = BannedFieldVisitor::default();
        event.record(&mut visitor);
        if let Some(name) = visitor.banned_name {
            self.violations.fetch_add(1, Ordering::Relaxed);
            // Surface a separate event so the violation isn't swallowed by a
            // misconfigured formatter.
            eprintln!(
                "{{\"event\":\"audit.redaction_violation\",\"field\":\"{name}\",\"target\":\"{}\"}}",
                event.metadata().target()
            );
        }
    }

    fn on_new_span(&self, attrs: &Attributes<'_>, _id: &tracing::span::Id, _ctx: Context<'_, S>) {
        let mut visitor = BannedFieldVisitor::default();
        attrs.record(&mut visitor);
        if let Some(name) = visitor.banned_name {
            self.violations.fetch_add(1, Ordering::Relaxed);
            eprintln!(
                "{{\"event\":\"audit.redaction_violation\",\"field\":\"{name}\",\"span\":\"{}\"}}",
                attrs.metadata().name()
            );
        }
    }
}

#[derive(Default)]
struct BannedFieldVisitor {
    banned_name: Option<String>,
}

impl BannedFieldVisitor {
    fn check(&mut self, field: &Field) {
        if self.banned_name.is_some() {
            return;
        }
        if is_banned_field_name(field.name()) {
            self.banned_name = Some(field.name().to_string());
        }
    }
}

impl Visit for BannedFieldVisitor {
    fn record_debug(&mut self, field: &Field, _value: &dyn std::fmt::Debug) {
        self.check(field);
    }
    fn record_str(&mut self, field: &Field, _value: &str) {
        self.check(field);
    }
    fn record_i64(&mut self, field: &Field, _value: i64) {
        self.check(field);
    }
    fn record_u64(&mut self, field: &Field, _value: u64) {
        self.check(field);
    }
    fn record_bool(&mut self, field: &Field, _value: bool) {
        self.check(field);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tracing::info;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    #[test]
    fn substring_match_is_case_insensitive() {
        assert!(is_banned_field_name("token"));
        assert!(is_banned_field_name("auth_token"));
        assert!(is_banned_field_name("BearerToken"));
        assert!(is_banned_field_name("server_secret"));
        assert!(is_banned_field_name("private_key"));
        assert!(!is_banned_field_name("client_name"));
        assert!(!is_banned_field_name("rule_id"));
        assert!(!is_banned_field_name("public_key_fingerprint"));
    }

    /// Drive a real subscriber: assert that emitting an event with a `token`
    /// field bumps the violation counter while a clean event does not.
    #[test]
    fn layer_flags_banned_field_in_real_subscriber() {
        let layer = Arc::new(RedactionLayer::new());
        let layer_for_subscriber = Arc::clone(&layer);
        let subscriber = tracing_subscriber::registry().with(LayerHandle(layer_for_subscriber));
        let _guard = subscriber.set_default();

        info!(event = "audit.something", client_name = "edge-a");
        assert_eq!(layer.violation_count(), 0, "clean event must not flag");

        info!(event = "audit.bad", token = "leaked-bearer-abc123");
        assert_eq!(
            layer.violation_count(),
            1,
            "event with token field must flag"
        );

        info!(event = "audit.also_bad", server_secret = 42_i64);
        assert_eq!(layer.violation_count(), 2);
    }

    /// Tiny wrapper so we can `set_default()` while keeping a handle on the
    /// underlying counter.
    struct LayerHandle(Arc<RedactionLayer>);

    impl<S> Layer<S> for LayerHandle
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_event(&self, event: &tracing::Event<'_>, ctx: Context<'_, S>) {
            self.0.on_event(event, ctx);
        }
    }
}

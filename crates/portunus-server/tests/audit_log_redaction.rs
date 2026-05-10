//! T015 (005-multi-user-rbac, US1) — audit-log redaction.
//!
//! The auth middleware emits structured `tracing` events on every
//! decision (`event = "operator.allow"` / `"operator.deny"`). Constitution
//! Principle IV "no raw credentials in logs" requires that the literal
//! bearer token NEVER appears in any captured record — only the
//! post-verify `OperatorIdentity` (user_id, role) plus method/path.
//!
//! This test wires a `tracing_subscriber` JSON layer to a shared
//! buffer, drives a verify (success) + a verify (rejection) through
//! the live auth middleware, then greps the captured output for the
//! literal raw token. Any leak fails the test loudly.

use std::io;
use std::sync::{Arc, Mutex, MutexGuard};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use portunus_server::clients::ConnectedClients;
use portunus_server::operator::http;
use portunus_server::state::AppState;
use tempfile::TempDir;
use tower::ServiceExt;
use tracing_subscriber::{fmt, layer::SubscriberExt};

const VALID_TOKEN: &str = "T015-valid-43char-XXXXXXXXXXXXXXXXXXXXXXXX";
const INVALID_TOKEN: &str = "T015-invalid-43char-YYYYYYYYYYYYYYYYYYYYYYY";

#[derive(Clone, Default)]
struct SharedBuf {
    inner: Arc<Mutex<Vec<u8>>>,
}

impl SharedBuf {
    fn snapshot(&self) -> String {
        let guard: MutexGuard<'_, Vec<u8>> = self.inner.lock().expect("poisoned");
        String::from_utf8_lossy(&guard).into_owned()
    }
}

impl io::Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.lock().expect("poisoned").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn auth_middleware_does_not_leak_raw_token_in_logs() {
    let buf = SharedBuf::default();
    let buf_for_writer = buf.clone();
    let subscriber = tracing_subscriber::registry().with(
        fmt::layer()
            .json()
            .with_writer(move || buf_for_writer.clone()),
    );
    let _guard = tracing::subscriber::set_default(subscriber);

    let dir = TempDir::new().expect("tempdir");
    let sqlite_store =
        std::sync::Arc::new(portunus_server::store::Store::open(dir.path()).unwrap());
    let tokens = Arc::new(portunus_server::store::token_store::SqliteTokenStore::new(
        std::sync::Arc::clone(&sqlite_store),
    ));
    let operator_store = Arc::new(
        portunus_server::store::operator_store::SqliteOperatorStore::new(std::sync::Arc::clone(
            &sqlite_store,
        )),
    );
    operator_store
        .bootstrap_legacy_superadmin(VALID_TOKEN)
        .expect("bootstrap");
    let state = Arc::new(
        AppState::new(
            tokens,
            operator_store,
            ConnectedClients::default(),
            "127.0.0.1:0",
            "deadbeef",
            "-----BEGIN CERTIFICATE-----\n",
            16,
            std::sync::Arc::clone(&sqlite_store),
        )
        .expect("AppState"),
    );
    let router = http::router(state);

    // Allow path: a valid bearer fires `event = "operator.allow"`.
    let req = Request::builder()
        .method("GET")
        .uri("/v1/rules")
        .header("Authorization", format!("Bearer {VALID_TOKEN}"))
        .body(Body::empty())
        .expect("req");
    let resp = router.clone().oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);

    // Deny path: a garbage bearer fires `event = "operator.deny"`.
    let req = Request::builder()
        .method("GET")
        .uri("/v1/rules")
        .header("Authorization", format!("Bearer {INVALID_TOKEN}"))
        .body(Body::empty())
        .expect("req");
    let resp = router.oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    // Drop the subscriber guard before reading the buffer to flush
    // any tail writes.
    drop(_guard);
    let captured = buf.snapshot();

    // Sanity: we DID see both audit events.
    assert!(
        captured.contains("operator.allow"),
        "expected at least one operator.allow record; got:\n{captured}"
    );
    assert!(
        captured.contains("operator.deny"),
        "expected at least one operator.deny record; got:\n{captured}"
    );

    // The whole point: the raw bearer tokens — both valid and invalid —
    // MUST never appear anywhere in the log buffer.
    assert!(
        !captured.contains(VALID_TOKEN),
        "raw VALID token leaked into logs:\n{captured}"
    );
    assert!(
        !captured.contains(INVALID_TOKEN),
        "raw INVALID token leaked into logs:\n{captured}"
    );

    // The post-verify identity SHOULD appear on the allow record.
    assert!(
        captured.contains("\"actor\":\"_legacy\""),
        "expected actor=_legacy on allow record; got:\n{captured}"
    );
    // The deny record SHOULD carry the reason code from `RbacError::code`.
    assert!(
        captured.contains("credential_invalid"),
        "expected reason=credential_invalid on deny record; got:\n{captured}"
    );
}

//! 006-management-web-ui T020: contract test for `GET /v1/audit`.
//!
//! Mirrors the 8-test plan in
//! `specs/006-management-web-ui/contracts/audit-endpoint.md`.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use portunus_server::clients::ConnectedClients;
use portunus_server::operator::audit::{AuditEntry, AuditOutcome};
use portunus_server::operator::http;
use portunus_server::state::AppState;
use tempfile::TempDir;
use tower::ServiceExt;

const SUPERADMIN_TOKEN: &str = "T020-super";

fn build_router_with_alice() -> (axum::Router, Arc<AppState>, String, TempDir) {
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
        .bootstrap_legacy_superadmin(SUPERADMIN_TOKEN)
        .expect("bootstrap superadmin");
    // Provision alice as a non-superadmin user with an issued credential.
    use std::str::FromStr;
    let alice_id = portunus_auth::UserId::from_str("alice").expect("user id");
    operator_store
        .add_user(portunus_auth::User {
            id: alice_id.clone(),
            display_name: "Alice".to_string(),
            role: portunus_auth::OperatorRole::User,
            disabled: false,
            created_at: chrono::Utc::now(),
        })
        .expect("create alice");
    let (_cred, alice_token) = operator_store
        .issue_credential(&alice_id, Some("alice-default".to_string()))
        .expect("issue alice credential");

    let state = Arc::new(
        AppState::new(
            tokens,
            operator_store,
            ConnectedClients::default(),
            None,
            0,
            "deadbeef",
            include_str!("../src/advertised/testdata/san_fixture.pem"),
            16,
            std::sync::Arc::clone(&sqlite_store),
        )
        .expect("AppState"),
    );
    // 008-sqlite-storage T032 — wire the durable audit writer so the
    // /v1/audit endpoint (which reads from SQLite) can see entries
    // pushed through the auth_layer's AuditRing fan-out.
    let cancel = tokio_util::sync::CancellationToken::new();
    let handle = portunus_server::store::audit_writer::spawn(
        std::sync::Arc::clone(&sqlite_store),
        state.metrics.audit_buffer_drops_total.clone(),
        state.metrics.audit_durable_writer_lag_seconds.clone(),
        cancel,
    );
    state.audit.bind_durable_writer(handle);
    (http::router(state.clone()), state, alice_token, dir)
}

/// Allow the durable audit writer to flush its current batch.
/// `BATCH_MAX_DELAY` is 100 ms — we sleep a touch longer to defeat
/// scheduler jitter on heavily loaded test runners.
async fn flush_audit() {
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
}

fn req(method: &str, uri: &str, bearer: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(t) = bearer {
        b = b.header("Authorization", format!("Bearer {t}"));
    }
    b.body(Body::empty()).expect("req")
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(resp.into_body(), 64 * 1024)
        .await
        .expect("body bytes");
    serde_json::from_slice(&bytes).expect("body json")
}

/// Seed N auditable `allow` rows directly through the ring fan-out.
///
/// This exercises the read endpoint independently of the auth_layer
/// audit *policy* (which no longer records successful reads). Each row
/// is a representative mutation (`POST /v1/rules`), the shape an allow
/// row actually takes now. The middleware policy itself is locked by
/// `successful_read_is_not_audited_but_deny_is`.
fn seed_allow_rows(state: &Arc<AppState>, n: usize) {
    for _ in 0..n {
        state.audit.push(AuditEntry {
            timestamp: chrono::Utc::now(),
            actor: "_legacy".into(),
            role: Some(portunus_auth::OperatorRole::Superadmin),
            method: "POST".into(),
            path: "/v1/rules".into(),
            outcome: AuditOutcome::Allow,
            reason: None,
            action: None,
            resource_kind: None,
            resource_value: None,
            details: None,
        });
    }
}

#[tokio::test]
async fn empty_buffer_returns_empty_array() {
    let (router, state, _alice, _d) = build_router_with_alice();
    // Snapshot the ring directly (no traffic yet → exactly 0 rows).
    assert!(state.audit.is_empty());
    let resp = router
        .oneshot(req("GET", "/v1/audit?limit=10", Some(SUPERADMIN_TOKEN)))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    let arr = v.as_array().expect("array");
    // 008-sqlite-storage: durable writer batches with a ≤100 ms delay,
    // so the audit GET's own row is not yet committed when this same
    // GET reads the SQLite-backed audit table. The row will land on
    // the next read (verified by `audit_reflects_recent_actions_in_order`).
    assert_eq!(arr.len(), 0);
}

#[tokio::test]
async fn buffer_returns_at_most_limit_newest_first() {
    let (router, state, _alice, _d) = build_router_with_alice();
    seed_allow_rows(&state, 5);
    flush_audit().await;
    let resp = router
        .oneshot(req("GET", "/v1/audit?limit=3", Some(SUPERADMIN_TOKEN)))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    let arr = v.as_array().expect("array");
    assert_eq!(arr.len(), 3);
    // Reads are no longer audited, so the only rows are the seeded
    // mutation rows.
    for row in arr {
        assert_eq!(row["path"], "/v1/rules");
    }
}

#[tokio::test]
async fn invalid_limit_returns_422() {
    let (router, _, _alice, _d) = build_router_with_alice();
    let resp = router
        .clone()
        .oneshot(req("GET", "/v1/audit?limit=2000", Some(SUPERADMIN_TOKEN)))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let v = body_json(resp).await;
    assert_eq!(v["error"]["code"], "invalid_limit");

    let resp = router
        .clone()
        .oneshot(req("GET", "/v1/audit?limit=0", Some(SUPERADMIN_TOKEN)))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let resp = router
        .oneshot(req("GET", "/v1/audit?limit=abc", Some(SUPERADMIN_TOKEN)))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body_json(resp).await["error"]["code"], "invalid_limit");
}

#[tokio::test]
async fn outcome_filter_narrows_to_deny() {
    let (router, state, _alice, _d) = build_router_with_alice();
    // Generate one deny by hitting an endpoint with a bogus token.
    let _ = router
        .clone()
        .oneshot(req("GET", "/v1/users", Some("definitely-not-a-real-token")))
        .await
        .expect("oneshot");
    // Plus a few allows.
    seed_allow_rows(&state, 2);
    flush_audit().await;

    let resp = router
        .oneshot(req("GET", "/v1/audit?outcome=deny", Some(SUPERADMIN_TOKEN)))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    let arr = v.as_array().expect("array");
    assert!(!arr.is_empty(), "expected at least one deny row");
    for row in arr {
        assert_eq!(row["outcome"], "deny");
    }
}

#[tokio::test]
async fn invalid_outcome_returns_422() {
    let (router, _, _alice, _d) = build_router_with_alice();
    let resp = router
        .oneshot(req(
            "GET",
            "/v1/audit?outcome=banana",
            Some(SUPERADMIN_TOKEN),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body_json(resp).await["error"]["code"], "invalid_outcome");
}

#[tokio::test]
async fn role_user_returns_403_role_required() {
    let (router, _, alice_token, _d) = build_router_with_alice();
    let resp = router
        .oneshot(req("GET", "/v1/audit", Some(&alice_token)))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let v = body_json(resp).await;
    assert_eq!(v["error"]["code"], "role_required");
}

#[tokio::test]
async fn missing_bearer_returns_401() {
    let (router, _, _alice, _d) = build_router_with_alice();
    let resp = router
        .oneshot(req("GET", "/v1/audit", None))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(body_json(resp).await["error"]["code"], "unauthenticated");
}

#[tokio::test]
async fn audit_reflects_recent_actions_in_order() {
    let (router, state, _alice, _d) = build_router_with_alice();
    // Sequence: 2 auditable allows (mutations) + 1 deny (bogus token).
    // Successful reads are intentionally absent from the audit log.
    seed_allow_rows(&state, 2);
    let _ = router
        .clone()
        .oneshot(req("GET", "/v1/users", Some("not-a-token")))
        .await;
    flush_audit().await;
    let resp = router
        .oneshot(req("GET", "/v1/audit?limit=10", Some(SUPERADMIN_TOKEN)))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    let arr = v.as_array().expect("array");
    // 2 seeded allows + 1 deny.
    assert!(arr.len() >= 3, "expected ≥3 rows, got {}", arr.len());
    // The deny row sits in there.
    assert!(
        arr.iter()
            .any(|r| r["outcome"] == "deny" && r["path"] == "/v1/users"),
        "expected a deny row for /v1/users: {arr:?}"
    );
}

/// Audit-scope policy: a *successful* read-only request produces NO
/// audit row, but a *denied* request always does. This is the core of
/// the noise-reduction change — the dashboard's own polling no longer
/// floods the audit log.
#[tokio::test]
async fn successful_read_is_not_audited_but_deny_is() {
    let (router, _state, _alice, _d) = build_router_with_alice();
    // Several successful reads — none should be audited.
    for _ in 0..5 {
        let resp = router
            .clone()
            .oneshot(req("GET", "/v1/users", Some(SUPERADMIN_TOKEN)))
            .await
            .expect("oneshot");
        assert_eq!(resp.status(), StatusCode::OK);
    }
    // One denied read — must be audited.
    let resp = router
        .clone()
        .oneshot(req("GET", "/v1/users", Some("not-a-real-token")))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    flush_audit().await;

    let resp = router
        .oneshot(req("GET", "/v1/audit?limit=100", Some(SUPERADMIN_TOKEN)))
        .await
        .expect("oneshot");
    let v = body_json(resp).await;
    let arr = v.as_array().expect("array");
    // Exactly one row: the deny. The five successful GET /v1/users reads
    // and the audit GET itself are all reads → not audited.
    assert_eq!(arr.len(), 1, "only the deny should be audited, got {arr:?}");
    assert_eq!(arr[0]["outcome"], "deny");
    assert!(
        arr.iter().all(|r| r["outcome"] == "deny"),
        "no allow rows expected from reads: {arr:?}"
    );
}

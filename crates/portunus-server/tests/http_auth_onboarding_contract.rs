//! Contract tests for unauthenticated auth-status and onboarding endpoints.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode};
use chrono::Utc;
use portunus_auth::token::hash_token;
use portunus_core::fingerprint;
use portunus_server::clients::ConnectedClients;
use portunus_server::operator::http;
use portunus_server::state::AppState;
use serde_json::json;
use tempfile::TempDir;
use tower::ServiceExt;

fn build_router() -> (
    axum::Router,
    Arc<portunus_server::store::operator_store::SqliteOperatorStore>,
    Arc<portunus_server::store::Store>,
    TempDir,
) {
    let dir = TempDir::new().expect("tempdir");
    let sqlite_store =
        Arc::new(portunus_server::store::Store::open(dir.path()).expect("open sqlite store"));
    let tokens = Arc::new(portunus_server::store::token_store::SqliteTokenStore::new(
        Arc::clone(&sqlite_store),
    ));
    let operator_store = Arc::new(
        portunus_server::store::operator_store::SqliteOperatorStore::new(Arc::clone(&sqlite_store)),
    );
    let state = Arc::new(
        AppState::new(
            tokens,
            Arc::clone(&operator_store),
            ConnectedClients::default(),
            "127.0.0.1:0",
            "deadbeef",
            "-----BEGIN CERTIFICATE-----\n",
            16,
            Arc::clone(&sqlite_store),
        )
        .expect("AppState"),
    );
    (http::router(state), operator_store, sqlite_store, dir)
}

fn req(method: Method, uri: &str, body: serde_json::Value) -> Request<Body> {
    let body_bytes = serde_json::to_vec(&body).expect("body");
    let mut builder = Request::builder().method(method.as_str()).uri(uri);
    if method == Method::GET {
        let mut request = builder.body(Body::empty()).expect("request");
        request.extensions_mut().insert(ConnectInfo(
            "127.0.0.1:12345"
                .parse::<std::net::SocketAddr>()
                .expect("socket addr"),
        ));
        return request;
    }
    builder = builder
        .header("content-type", "application/json")
        .header("content-length", body_bytes.len().to_string());
    let mut request = builder.body(Body::from(body_bytes)).expect("request");
    request.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:12345"
            .parse::<std::net::SocketAddr>()
            .expect("socket addr"),
    ));
    request
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(resp.into_body(), 16 * 1024).await.expect("body");
    serde_json::from_slice(&bytes).expect("json")
}

fn seed_setup_token(store: &portunus_server::store::Store, raw: &str) {
    let now = Utc::now();
    let expires_at = now + chrono::Duration::minutes(30);
    let token_hash = fingerprint::hex(&hash_token(raw));
    store
        .with_write_tx(|tx| {
            tx.execute(
                "INSERT INTO onboarding_setup (id, token_hash, issued_at, expires_at) \
                 VALUES (1, ?, ?, ?) \
                 ON CONFLICT(id) DO UPDATE SET \
                    token_hash = excluded.token_hash, \
                    issued_at = excluded.issued_at, \
                    expires_at = excluded.expires_at",
                rusqlite::params![token_hash, now.to_rfc3339(), expires_at.to_rfc3339()],
            )
            .map_err(portunus_server::store::map_rusqlite)?;
            Ok(())
        })
        .expect("seed setup token");
}

#[tokio::test]
async fn get_auth_status_reports_onboarding_required_before_first_superadmin() {
    let (router, _operator_store, _sqlite_store, _dir) = build_router();
    let resp = router
        .oneshot(req(Method::GET, "/v1/auth/status", json!(null)))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body, json!({ "onboarding_required": true }));
}

#[tokio::test]
async fn post_onboarding_requires_valid_setup_token() {
    let (router, _operator_store, _sqlite_store, _dir) = build_router();

    let missing = router
        .clone()
        .oneshot(req(
            Method::POST,
            "/v1/auth/onboarding",
            json!({
                "user_id": "alice",
                "display_name": "Alice",
                "password": "correct horse battery staple",
                "password_confirm": "correct horse battery staple"
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    let missing_body = body_json(missing).await;
    assert_eq!(missing_body["error"]["code"], "setup_token_required");

    let invalid = router
        .oneshot(req(
            Method::POST,
            "/v1/auth/onboarding",
            json!({
                "user_id": "alice",
                "display_name": "Alice",
                "password": "correct horse battery staple",
                "password_confirm": "correct horse battery staple",
                "setup_token": "definitely-wrong"
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(invalid.status(), StatusCode::UNAUTHORIZED);
    let invalid_body = body_json(invalid).await;
    assert_eq!(invalid_body["error"]["code"], "setup_token_invalid");
}

#[tokio::test]
async fn post_onboarding_creates_first_superadmin_once_then_rejects_repeat() {
    let (router, operator_store, sqlite_store, _dir) = build_router();
    let setup_token = "setup-token-success";
    seed_setup_token(&sqlite_store, setup_token);

    let created = router
        .clone()
        .oneshot(req(
            Method::POST,
            "/v1/auth/onboarding",
            json!({
                "user_id": "alice",
                "display_name": "  Alice  ",
                "password": "correct horse battery staple",
                "password_confirm": "correct horse battery staple",
                "setup_token": setup_token
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created_body = body_json(created).await;
    assert_eq!(
        created_body,
        json!({
            "user_id": "alice",
            "display_name": "Alice",
            "role": "superadmin"
        })
    );

    assert_eq!(operator_store.count_superadmins(), 1);
    assert_eq!(operator_store.list_users().len(), 1);
    let credential_count: i64 = sqlite_store
        .with_conn(|conn| {
            conn.query_row("SELECT COUNT(*) FROM credentials", [], |row| row.get(0))
                .map_err(portunus_server::store::map_rusqlite)
        })
        .expect("query credential count");
    assert_eq!(credential_count, 0, "onboarding must not create API tokens");

    let password_hash: Option<String> = sqlite_store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT password_hash FROM users WHERE user_id = 'alice'",
                [],
                |row| row.get(0),
            )
            .map_err(portunus_server::store::map_rusqlite)
        })
        .expect("query password hash");
    assert!(
        password_hash
            .as_deref()
            .is_some_and(|value| value.starts_with("$argon2")),
        "password hash missing or malformed"
    );

    let repeat = router
        .clone()
        .oneshot(req(
            Method::POST,
            "/v1/auth/onboarding",
            json!({
                "user_id": "bob",
                "display_name": "Bob",
                "password": "correct horse battery staple",
                "password_confirm": "correct horse battery staple",
                "setup_token": "unused-after-success"
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(repeat.status(), StatusCode::CONFLICT);
    let repeat_body = body_json(repeat).await;
    assert_eq!(repeat_body["error"]["code"], "already_bootstrapped");

    let status = router
        .oneshot(req(Method::GET, "/v1/auth/status", json!(null)))
        .await
        .expect("oneshot");
    assert_eq!(status.status(), StatusCode::OK);
    let status_body = body_json(status).await;
    assert_eq!(status_body, json!({ "onboarding_required": false }));
}

#[tokio::test]
async fn concurrent_onboarding_allows_only_one_success() {
    let (router, operator_store, sqlite_store, _dir) = build_router();
    let setup_token = "setup-token-concurrent";
    seed_setup_token(&sqlite_store, setup_token);

    let first = router.clone().oneshot(req(
        Method::POST,
        "/v1/auth/onboarding",
        json!({
            "user_id": "alice",
            "display_name": "Alice",
            "password": "correct horse battery staple",
            "password_confirm": "correct horse battery staple",
            "setup_token": setup_token
        }),
    ));
    let second = router.clone().oneshot(req(
        Method::POST,
        "/v1/auth/onboarding",
        json!({
            "user_id": "bob",
            "display_name": "Bob",
            "password": "correct horse battery staple",
            "password_confirm": "correct horse battery staple",
            "setup_token": setup_token
        }),
    ));

    let (first, second) = tokio::join!(first, second);
    let first = first.expect("first oneshot");
    let second = second.expect("second oneshot");
    let statuses = [first.status(), second.status()];
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::CREATED)
            .count(),
        1,
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == StatusCode::CONFLICT)
            .count(),
        1,
    );

    assert_eq!(operator_store.count_superadmins(), 1);
    assert_eq!(operator_store.list_users().len(), 1);
}

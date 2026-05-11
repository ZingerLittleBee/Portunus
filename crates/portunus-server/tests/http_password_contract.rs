//! Contract tests for local password change and reset endpoints.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use chrono::Utc;
use portunus_auth::{UserId, token::hash_token};
use portunus_core::fingerprint;
use portunus_server::clients::ConnectedClients;
use portunus_server::operator::http;
use portunus_server::state::AppState;
use serde_json::json;
use tempfile::TempDir;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery staple";
const ORIGIN: &str = "http://127.0.0.1:7080";

fn build_router() -> (
    axum::Router,
    Arc<AppState>,
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
    (
        http::router(Arc::clone(&state)),
        state,
        operator_store,
        sqlite_store,
        dir,
    )
}

fn request(
    method: Method,
    uri: &str,
    body: serde_json::Value,
    cookie: Option<&str>,
    bearer: Option<&str>,
    csrf: bool,
) -> Request<Body> {
    let body_bytes = serde_json::to_vec(&body).expect("body");
    let mut builder = Request::builder()
        .method(method.as_str())
        .uri(uri)
        .header("content-type", "application/json")
        .header("content-length", body_bytes.len().to_string());
    if let Some(cookie) = cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    if let Some(bearer) = bearer {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    }
    if csrf {
        builder = builder
            .header("x-portunus-csrf", "1")
            .header(header::ORIGIN, ORIGIN);
    }
    let mut request = builder.body(Body::from(body_bytes)).expect("request");
    request.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:12000"
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

async fn create_admin(router: &axum::Router, store: &portunus_server::store::Store, user_id: &str) {
    let setup_token = format!("setup-token-{user_id}");
    seed_setup_token(store, &setup_token);
    let resp = router
        .clone()
        .oneshot(request(
            Method::POST,
            "/v1/auth/onboarding",
            json!({
                "user_id": user_id,
                "display_name": user_id,
                "password": PASSWORD,
                "password_confirm": PASSWORD,
                "setup_token": setup_token,
            }),
            None,
            None,
            false,
        ))
        .await
        .expect("onboarding");
    assert_eq!(resp.status(), StatusCode::CREATED);
}

async fn login_cookie(router: &axum::Router, user_id: &str, password: &str) -> String {
    let resp = router
        .clone()
        .oneshot(request(
            Method::POST,
            "/v1/auth/login",
            json!({ "user_id": user_id, "password": password }),
            None,
            None,
            false,
        ))
        .await
        .expect("login");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    resp.headers()
        .get(header::SET_COOKIE)
        .expect("set-cookie")
        .to_str()
        .expect("cookie string")
        .split(';')
        .next()
        .expect("cookie pair")
        .to_string()
}

#[tokio::test]
async fn self_password_change_requires_current_password() {
    let (router, _state, _operator_store, store, _dir) = build_router();
    create_admin(&router, &store, "admin").await;
    let cookie = login_cookie(&router, "admin", PASSWORD).await;

    let resp = router
        .oneshot(request(
            Method::POST,
            "/v1/users/me/password",
            json!({
                "current_password": "wrong horse battery staple",
                "new_password": "another correct horse battery staple",
                "new_password_confirm": "another correct horse battery staple",
            }),
            Some(&cookie),
            None,
            true,
        ))
        .await
        .expect("self password");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    let body = body_json(resp).await;
    assert_eq!(body["error"]["code"], "invalid_login");
}

#[tokio::test]
async fn admin_reset_revokes_sessions_and_api_tokens_by_default() {
    let (router, _state, operator_store, store, _dir) = build_router();
    create_admin(&router, &store, "admin").await;
    let cookie = login_cookie(&router, "admin", PASSWORD).await;
    let admin_id = "admin".parse::<UserId>().expect("admin user id");
    let (_credential, api_token) = operator_store
        .issue_credential(&admin_id, Some("api".into()))
        .expect("issue api credential");

    let resp = router
        .clone()
        .oneshot(request(
            Method::POST,
            "/v1/users/admin/password",
            json!({"new_password": "changed correct horse battery staple"}),
            Some(&cookie),
            None,
            true,
        ))
        .await
        .expect("admin reset");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["sessions_revoked"], 1);
    assert_eq!(body["api_tokens_revoked"], 1);

    let old_session = router
        .clone()
        .oneshot(request(
            Method::GET,
            "/v1/users/me",
            json!(null),
            Some(&cookie),
            None,
            false,
        ))
        .await
        .expect("users me");
    assert_eq!(old_session.status(), StatusCode::UNAUTHORIZED);

    let old_api_token = router
        .oneshot(request(
            Method::GET,
            "/v1/users/me",
            json!(null),
            None,
            Some(&api_token),
            false,
        ))
        .await
        .expect("users me");
    assert_eq!(old_api_token.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn admin_reset_can_keep_api_tokens_explicitly() {
    let (router, _state, operator_store, store, _dir) = build_router();
    create_admin(&router, &store, "admin").await;
    let cookie = login_cookie(&router, "admin", PASSWORD).await;
    let admin_id = "admin".parse::<UserId>().expect("admin user id");
    let (_credential, api_token) = operator_store
        .issue_credential(&admin_id, Some("api".into()))
        .expect("issue api credential");

    let resp = router
        .clone()
        .oneshot(request(
            Method::POST,
            "/v1/users/admin/password",
            json!({
                "new_password": "changed correct horse battery staple",
                "keep_api_tokens": true
            }),
            Some(&cookie),
            None,
            true,
        ))
        .await
        .expect("admin reset");
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["api_tokens_revoked"], 0);

    let preserved = router
        .oneshot(request(
            Method::GET,
            "/v1/users/me",
            json!(null),
            None,
            Some(&api_token),
            false,
        ))
        .await
        .expect("users me");
    assert_eq!(preserved.status(), StatusCode::OK);
}

#[tokio::test]
async fn admin_reset_writes_audit_event_without_password() {
    let (router, state, _operator_store, store, _dir) = build_router();
    create_admin(&router, &store, "admin").await;
    let cookie = login_cookie(&router, "admin", PASSWORD).await;
    let new_password = "changed correct horse battery staple";

    let resp = router
        .oneshot(request(
            Method::POST,
            "/v1/users/admin/password",
            json!({"new_password": new_password}),
            Some(&cookie),
            None,
            true,
        ))
        .await
        .expect("admin reset");
    assert_eq!(resp.status(), StatusCode::OK);

    let snapshot = state.audit.snapshot(20, None);
    let reset = snapshot
        .iter()
        .find(|entry| entry.action.as_deref() == Some("operator.password_reset"))
        .expect("password reset audit event");
    assert_eq!(reset.actor, "admin");
    assert_eq!(reset.resource_value.as_deref(), Some("admin"));
    let audit_json = serde_json::to_string(reset).expect("audit json");
    assert!(!audit_json.contains(new_password));
    assert!(audit_json.contains("sessions_revoked"));
    assert!(audit_json.contains("api_tokens_revoked"));
}

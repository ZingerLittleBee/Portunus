//! Contract tests for local-password login and Web session cookies.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode, header};
use chrono::Utc;
use portunus_auth::{OperatorRole, User, UserId, token::hash_token};
use portunus_core::fingerprint;
use portunus_server::clients::ConnectedClients;
use portunus_server::operator::http;
use portunus_server::state::AppState;
use serde_json::json;
use tempfile::TempDir;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery staple";

fn build_router(
    secure_cookie: bool,
) -> (
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
    let mut state = AppState::new(
        tokens,
        Arc::clone(&operator_store),
        ConnectedClients::default(),
        "127.0.0.1:0",
        "deadbeef",
        "-----BEGIN CERTIFICATE-----\n",
        16,
        Arc::clone(&sqlite_store),
    )
    .expect("AppState");
    state.operator_http_cookie_secure = secure_cookie;
    (
        http::router(Arc::new(state)),
        operator_store,
        sqlite_store,
        dir,
    )
}

fn req(method: Method, uri: &str, body: serde_json::Value, remote_addr: &str) -> Request<Body> {
    let body_bytes = serde_json::to_vec(&body).expect("body");
    let mut builder = Request::builder().method(method.as_str()).uri(uri);
    if method != Method::GET {
        builder = builder
            .header("content-type", "application/json")
            .header("content-length", body_bytes.len().to_string());
    }
    let mut request = if method == Method::GET {
        builder.body(Body::empty()).expect("request")
    } else {
        builder.body(Body::from(body_bytes)).expect("request")
    };
    request.extensions_mut().insert(ConnectInfo(
        remote_addr
            .parse::<std::net::SocketAddr>()
            .expect("socket addr"),
    ));
    request
}

fn login_req(
    user_id: &str,
    password: &str,
    remote_addr: &str,
    authorization: Option<&str>,
) -> Request<Body> {
    let body = json!({ "user_id": user_id, "password": password });
    let body_bytes = serde_json::to_vec(&body).expect("body");
    let mut builder = Request::builder()
        .method("POST")
        .uri("/v1/auth/login")
        .header("content-type", "application/json")
        .header("content-length", body_bytes.len().to_string())
        .header("user-agent", "portunus-test");
    if let Some(value) = authorization {
        builder = builder.header(header::AUTHORIZATION, value);
    }
    let mut request = builder.body(Body::from(body_bytes)).expect("request");
    request.extensions_mut().insert(ConnectInfo(
        remote_addr
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

async fn create_password_user(
    router: &axum::Router,
    store: &portunus_server::store::Store,
    user_id: &str,
) {
    let setup_token = format!("setup-token-{user_id}");
    seed_setup_token(store, &setup_token);
    let resp = router
        .clone()
        .oneshot(req(
            Method::POST,
            "/v1/auth/onboarding",
            json!({
                "user_id": user_id,
                "display_name": user_id,
                "password": PASSWORD,
                "password_confirm": PASSWORD,
                "setup_token": setup_token,
            }),
            "127.0.0.1:12345",
        ))
        .await
        .expect("onboarding");
    assert_eq!(resp.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn login_sets_http_only_session_cookie() {
    let (router, _operator_store, store, _dir) = build_router(false);
    create_password_user(&router, &store, "admin").await;

    let resp = router
        .oneshot(login_req("admin", PASSWORD, "127.0.0.1:12000", None))
        .await
        .expect("login");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .expect("set-cookie")
        .to_str()
        .expect("cookie string");
    assert!(cookie.contains("portunus_session="));
    assert!(cookie.contains("Path=/"));
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Lax"));
    assert!(cookie.contains("Max-Age=604800"));
    assert!(!cookie.contains("Secure"));

    let secret = cookie
        .split(';')
        .next()
        .and_then(|pair| pair.strip_prefix("portunus_session="))
        .expect("session secret");
    let (session_hash, remote_addr, user_agent): (String, Option<String>, Option<String>) = store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT session_hash, remote_addr, user_agent FROM web_sessions",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(portunus_server::store::map_rusqlite)
        })
        .expect("query web session");
    assert_ne!(session_hash, secret);
    assert_eq!(remote_addr.as_deref(), Some("127.0.0.1"));
    assert_eq!(user_agent.as_deref(), Some("portunus-test"));
}

#[tokio::test]
async fn login_sets_secure_cookie_when_configured() {
    let (router, _operator_store, store, _dir) = build_router(true);
    create_password_user(&router, &store, "admin").await;

    let resp = router
        .oneshot(login_req("admin", PASSWORD, "127.0.0.1:12000", None))
        .await
        .expect("login");
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let cookie = resp
        .headers()
        .get(header::SET_COOKIE)
        .expect("set-cookie")
        .to_str()
        .expect("cookie string");
    assert!(cookie.contains("Secure"));
}

#[tokio::test]
async fn bad_password_returns_generic_401_without_cookie() {
    let (router, _operator_store, store, _dir) = build_router(false);
    create_password_user(&router, &store, "admin").await;

    let resp = router
        .oneshot(login_req(
            "admin",
            "wrong horse battery staple",
            "127.0.0.1:12000",
            None,
        ))
        .await
        .expect("login");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(resp.headers().get(header::SET_COOKIE).is_none());
    let body = body_json(resp).await;
    assert_eq!(body["error"]["code"], "invalid_login");
}

#[tokio::test]
async fn disabled_and_missing_password_users_fail_generically() {
    let (router, operator_store, store, _dir) = build_router(false);
    create_password_user(&router, &store, "admin").await;
    store
        .with_write_tx(|tx| {
            tx.execute("UPDATE users SET disabled = 1 WHERE user_id = 'admin'", [])
                .map_err(portunus_server::store::map_rusqlite)?;
            Ok(())
        })
        .expect("disable admin");

    let disabled = router
        .clone()
        .oneshot(login_req("admin", PASSWORD, "127.0.0.1:12000", None))
        .await
        .expect("login");
    assert_eq!(disabled.status(), StatusCode::UNAUTHORIZED);
    let disabled_body = body_json(disabled).await;
    assert_eq!(disabled_body["error"]["code"], "invalid_login");

    let missing_password_id = "nopassword".parse::<UserId>().expect("user id");
    operator_store
        .add_user(User {
            id: missing_password_id,
            display_name: "No Password".into(),
            role: OperatorRole::Superadmin,
            created_at: Utc::now(),
            disabled: false,
        })
        .expect("add missing password user");
    let missing_password = router
        .oneshot(login_req("nopassword", PASSWORD, "127.0.0.1:12000", None))
        .await
        .expect("login");
    assert_eq!(missing_password.status(), StatusCode::UNAUTHORIZED);
    let missing_body = body_json(missing_password).await;
    assert_eq!(missing_body["error"]["code"], "invalid_login");
}

#[tokio::test]
async fn unknown_user_fails_generically() {
    let (router, _operator_store, store, _dir) = build_router(false);
    create_password_user(&router, &store, "admin").await;

    let resp = router
        .oneshot(login_req("missing", PASSWORD, "127.0.0.1:12000", None))
        .await
        .expect("login");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(resp.headers().get(header::SET_COOKIE).is_none());
    let body = body_json(resp).await;
    assert_eq!(body["error"]["code"], "invalid_login");
}

#[tokio::test]
async fn login_throttle_keys_by_ip_not_ephemeral_port() {
    let (router, _operator_store, store, _dir) = build_router(false);
    create_password_user(&router, &store, "admin").await;

    for port in 12000..12005 {
        let resp = router
            .clone()
            .oneshot(login_req(
                "admin",
                "wrong horse battery staple",
                &format!("127.0.0.1:{port}"),
                None,
            ))
            .await
            .expect("login");
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    let locked = router
        .oneshot(login_req(
            "admin",
            "wrong horse battery staple",
            "127.0.0.1:13000",
            None,
        ))
        .await
        .expect("login");
    assert_eq!(locked.status(), StatusCode::TOO_MANY_REQUESTS);
    let body = body_json(locked).await;
    assert_eq!(body["error"]["code"], "rate_limited");
}

#[tokio::test]
async fn login_ignores_bearer_authorization_without_correct_body() {
    let (router, operator_store, store, _dir) = build_router(false);
    create_password_user(&router, &store, "admin").await;
    let admin_id = "admin".parse::<UserId>().expect("admin user id");
    let (_credential, api_token) = operator_store
        .issue_credential(&admin_id, Some("api".into()))
        .expect("issue api credential");

    let resp = router
        .oneshot(login_req(
            "admin",
            "wrong horse battery staple",
            "127.0.0.1:12000",
            Some(&format!("Bearer {api_token}")),
        ))
        .await
        .expect("login");
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    assert!(resp.headers().get(header::SET_COOKIE).is_none());
}

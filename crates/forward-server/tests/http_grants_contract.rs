//! T028 (005-multi-user-rbac, US2) — `/v1/grants` HTTP contract.
//!
//! Covers add (named + wildcard client, valid/invalid protocols/ranges),
//! list with optional `?user_id` filter, and revoke (no rules to cascade
//! over yet — the cross-cutting test for FR-011 with offline clients
//! lives in T028a below).

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use forward_server::clients::ConnectedClients;
use forward_server::operator::http;
use forward_server::state::AppState;
use serde_json::json;
use tempfile::TempDir;
use tower::ServiceExt;

const SUPER_TOKEN: &str = "T028-super";

fn build_router() -> (axum::Router, TempDir) {
    let dir = TempDir::new().expect("tempdir");
    let sqlite_store = std::sync::Arc::new(forward_server::store::Store::open(dir.path()).unwrap());
    let tokens =
        Arc::new(forward_server::store::token_store::SqliteTokenStore::new(std::sync::Arc::clone(&sqlite_store)));
    let operator_store = Arc::new(
        forward_server::store::operator_store::SqliteOperatorStore::new(std::sync::Arc::clone(&sqlite_store)),
    );
    operator_store
        .bootstrap_legacy_superadmin(SUPER_TOKEN)
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
    (http::router(state), dir)
}

fn req(method: &str, uri: &str, bearer: &str, body: serde_json::Value) -> Request<Body> {
    let body_bytes = serde_json::to_vec(&body).expect("body");
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("Authorization", format!("Bearer {bearer}"));
    if method == "GET" || method == "DELETE" {
        return b.body(Body::empty()).expect("req");
    }
    b = b.header("content-length", body_bytes.len().to_string());
    b.body(Body::from(body_bytes)).expect("req")
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(resp.into_body(), 16384).await.expect("body bytes");
    serde_json::from_slice(&bytes).expect("body json")
}

async fn create_alice(router: &axum::Router) {
    let resp = router
        .clone()
        .oneshot(req(
            "POST",
            "/v1/users",
            SUPER_TOKEN,
            json!({"user_id": "alice", "display_name": "Alice"}),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn post_grant_named_client_happy() {
    let (router, _d) = build_router();
    create_alice(&router).await;

    let resp = router
        .oneshot(req(
            "POST",
            "/v1/grants",
            SUPER_TOKEN,
            json!({
                "user_id": "alice",
                "client": "client-a",
                "listen_port_start": 30000,
                "listen_port_end": 30010,
                "protocols": ["tcp"],
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let v = body_json(resp).await;
    assert_eq!(v["client"], "client-a");
    assert_eq!(v["protocols"], json!(["tcp"]));
}

#[tokio::test]
async fn post_grant_wildcard_client_happy() {
    let (router, _d) = build_router();
    create_alice(&router).await;
    let resp = router
        .oneshot(req(
            "POST",
            "/v1/grants",
            SUPER_TOKEN,
            json!({
                "user_id": "alice",
                "client": "*",
                "listen_port_start": 40000,
                "listen_port_end": 40000,
                "protocols": ["tcp", "udp"],
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::CREATED);
    let v = body_json(resp).await;
    assert_eq!(v["client"], "*");
}

#[tokio::test]
async fn post_grant_empty_protocols_returns_422() {
    let (router, _d) = build_router();
    create_alice(&router).await;
    let resp = router
        .oneshot(req(
            "POST",
            "/v1/grants",
            SUPER_TOKEN,
            json!({
                "user_id": "alice",
                "client": "client-a",
                "listen_port_start": 30000,
                "listen_port_end": 30010,
                "protocols": [],
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn post_grant_inverted_range_returns_422() {
    let (router, _d) = build_router();
    create_alice(&router).await;
    let resp = router
        .oneshot(req(
            "POST",
            "/v1/grants",
            SUPER_TOKEN,
            json!({
                "user_id": "alice",
                "client": "client-a",
                "listen_port_start": 30100,
                "listen_port_end": 30000,
                "protocols": ["tcp"],
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn delete_grant_returns_grant_id_and_no_rules() {
    let (router, _d) = build_router();
    create_alice(&router).await;
    let create_resp = router
        .clone()
        .oneshot(req(
            "POST",
            "/v1/grants",
            SUPER_TOKEN,
            json!({
                "user_id": "alice",
                "client": "client-a",
                "listen_port_start": 30000,
                "listen_port_end": 30005,
                "protocols": ["tcp"],
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(create_resp.status(), StatusCode::CREATED);
    let gid = body_json(create_resp).await["grant_id"]
        .as_str()
        .expect("grant_id")
        .to_string();

    let resp = router
        .oneshot(req(
            "DELETE",
            &format!("/v1/grants/{gid}"),
            SUPER_TOKEN,
            json!(null),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    assert_eq!(v["grant_id"], gid);
    assert_eq!(v["removed_rule_ids"], json!([]));
}

/// T028a (FR-011): a grant created against a client name that has
/// NEVER connected MUST still be valid — the matching push lands as
/// `Pending` (the v0.4 path for offline-client pushes), and the rule
/// transitions on its own once the client connects later. This is
/// the RBAC layer's "client name" half: we don't gate authorisation
/// on `ConnectedClients` membership.
#[tokio::test]
async fn grant_for_offline_client_is_accepted_at_authorisation_time() {
    let (router, _d) = build_router();
    create_alice(&router).await;
    // Create a grant for "client-z", which has never connected.
    let resp = router
        .clone()
        .oneshot(req(
            "POST",
            "/v1/grants",
            SUPER_TOKEN,
            json!({
                "user_id": "alice",
                "client": "client-z",
                "listen_port_start": 35000,
                "listen_port_end": 35000,
                "protocols": ["tcp"],
            }),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Issue alice a credential to push from her own perspective.
    let issue = router
        .clone()
        .oneshot(req(
            "POST",
            "/v1/users/alice/credentials",
            SUPER_TOKEN,
            json!({}),
        ))
        .await
        .expect("oneshot");
    assert_eq!(issue.status(), StatusCode::CREATED);
    let alice_token = body_json(issue).await["token"]
        .as_str()
        .unwrap()
        .to_string();

    // Alice pushes a rule on client-z (offline). RBAC must permit
    // it (the grant covers this exact client + port + protocol).
    // The downstream rule activation will fail with
    // `client_not_connected` (4) — but the failure is POST-RBAC,
    // proving the authorisation layer didn't filter on connectivity.
    let resp = router
        .oneshot(req(
            "POST",
            "/v1/rules",
            &alice_token,
            json!({
                "client": "client-z",
                "listen_port": 35000,
                "target_host": "127.0.0.1",
                "target_port": 9000,
                "protocol": "tcp",
            }),
        ))
        .await
        .expect("oneshot");
    let status = resp.status();
    let v = body_json(resp).await;
    let code = v["error"]["code"].as_str().unwrap_or("");
    assert_ne!(
        status,
        StatusCode::FORBIDDEN,
        "RBAC must allow this push; got {status} body={v}"
    );
    // Implementation detail: the post-RBAC failure for an offline
    // client is `client_not_connected`. Asserting this both pins
    // the v0.4 behaviour (offline clients still surface a clear
    // error) and rules out an accidental RBAC false-positive.
    assert_eq!(
        code, "client_not_connected",
        "expected post-RBAC failure to be client_not_connected; got {status} body={v}"
    );
}

#[tokio::test]
async fn list_grants_filter_by_user_id() {
    let (router, _d) = build_router();
    create_alice(&router).await;
    // Add 2 grants for alice.
    for port in [30000_u16, 31000] {
        router
            .clone()
            .oneshot(req(
                "POST",
                "/v1/grants",
                SUPER_TOKEN,
                json!({
                    "user_id": "alice",
                    "client": "client-a",
                    "listen_port_start": port,
                    "listen_port_end": port,
                    "protocols": ["tcp"],
                }),
            ))
            .await
            .expect("oneshot");
    }

    let resp = router
        .clone()
        .oneshot(req("GET", "/v1/grants", SUPER_TOKEN, json!(null)))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let all = body_json(resp).await;
    assert_eq!(all.as_array().unwrap().len(), 2);

    let resp = router
        .oneshot(req(
            "GET",
            "/v1/grants?user_id=alice",
            SUPER_TOKEN,
            json!(null),
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let filtered = body_json(resp).await;
    assert_eq!(filtered.as_array().unwrap().len(), 2);
}

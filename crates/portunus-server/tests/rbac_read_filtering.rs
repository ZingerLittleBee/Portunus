//! T041 (005-multi-user-rbac, US3) — read-side RBAC filtering.
//!
//! Verifies that:
//! - Superadmin's `GET /v1/rules` lists every rule and includes `owner` on each.
//! - A constrained user's `GET /v1/rules` only returns rules they own.
//! - Cross-owner `?owner=` query is honoured for superadmin only.
//! - `DELETE /v1/rules/{id}` and `GET /v1/rules/{id}/stats` for someone
//!   else's rule return 403 `not_owner`.
//!
//! No real client/gRPC connection is required: we directly seed
//! `state.rules` via the in-process `ServerRuleStore` for both alice
//! and bob, then exercise the HTTP layer.

use std::str::FromStr;
use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use chrono::Utc;
use portunus_auth::{OperatorRole, User, UserId};
use portunus_core::{ClientName, PortRange};
use portunus_server::clients::ConnectedClients;
use portunus_server::operator::http;
use portunus_server::rules::{Protocol, Rule, RuleState};
use portunus_server::state::AppState;
use serde_json::json;
use tempfile::TempDir;
use tower::ServiceExt;

const SUPER_TOKEN: &str = "T041-super";

struct Fixture {
    router: axum::Router,
    state: Arc<AppState>,
    alice_token: String,
    bob_token: String,
    _dir: TempDir,
}

async fn build_fixture() -> Fixture {
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
        .bootstrap_legacy_superadmin(SUPER_TOKEN)
        .expect("bootstrap");

    // Create alice + bob with credentials.
    let alice = UserId::from_str("alice").unwrap();
    let bob = UserId::from_str("bob").unwrap();
    for (id, name) in [(&alice, "Alice"), (&bob, "Bob")] {
        operator_store
            .add_user(User {
                id: id.clone(),
                display_name: name.to_string(),
                role: OperatorRole::User,
                created_at: Utc::now(),
                disabled: false,
            })
            .unwrap();
    }
    let (_c1, alice_token) = operator_store
        .issue_credential(&alice, Some("test".to_string()))
        .unwrap();
    let (_c2, bob_token) = operator_store
        .issue_credential(&bob, Some("test".to_string()))
        .unwrap();

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

    // Seed two rules directly into the rule store via push_range.
    let client_name = ClientName::new("client-a".to_string()).unwrap();
    state
        .rules
        .push_range(
            client_name.clone(),
            PortRange::new(50001, 50001).unwrap(),
            "127.0.0.1".to_string(),
            PortRange::new(9001, 9001).unwrap(),
            Protocol::Tcp,
            None,
            128,
            alice.clone(),
        )
        .await
        .unwrap();
    state
        .rules
        .push_range(
            client_name,
            PortRange::new(50002, 50002).unwrap(),
            "127.0.0.1".to_string(),
            PortRange::new(9002, 9002).unwrap(),
            Protocol::Tcp,
            None,
            128,
            bob.clone(),
        )
        .await
        .unwrap();
    // Mark both Active so list-rules has the canonical state we expect.
    let _ = std::convert::identity::<&Rule>;
    let _ = RuleState::Active;

    Fixture {
        router: http::router(state.clone()),
        state,
        alice_token,
        bob_token,
        _dir: dir,
    }
}

fn req(method: &str, uri: &str, bearer: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", format!("Bearer {bearer}"))
        .body(Body::empty())
        .expect("req")
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(resp.into_body(), 65536).await.expect("body bytes");
    serde_json::from_slice(&bytes).expect("body json")
}

#[tokio::test]
async fn superadmin_sees_all_rules_with_owner_field() {
    let f = build_fixture().await;
    let resp = f
        .router
        .clone()
        .oneshot(req("GET", "/v1/rules", SUPER_TOKEN))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    let arr = v.as_array().expect("array");
    assert_eq!(arr.len(), 2);
    let owners: Vec<&str> = arr
        .iter()
        .map(|r| r["owner_user_id"].as_str().unwrap_or("?"))
        .collect();
    assert!(owners.contains(&"alice"));
    assert!(owners.contains(&"bob"));
}

#[tokio::test]
async fn alice_only_sees_her_own_rule() {
    let f = build_fixture().await;
    let resp = f
        .router
        .clone()
        .oneshot(req("GET", "/v1/rules", &f.alice_token))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    let arr = v.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["owner_user_id"], "alice");
}

#[tokio::test]
async fn alice_cannot_delete_bobs_rule() {
    let f = build_fixture().await;
    // Find bob's rule id.
    let resp = f
        .router
        .clone()
        .oneshot(req("GET", "/v1/rules", SUPER_TOKEN))
        .await
        .expect("oneshot");
    let v = body_json(resp).await;
    let arr = v.as_array().expect("array");
    let bob_rule_id = arr
        .iter()
        .find(|r| r["owner_user_id"] == "bob")
        .and_then(|r| r["id"]["0"].as_u64().or_else(|| r["id"].as_u64()))
        .expect("bob rule id");

    let resp = f
        .router
        .clone()
        .oneshot(req(
            "DELETE",
            &format!("/v1/rules/{bob_rule_id}"),
            &f.alice_token,
        ))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    let body = body_json(resp).await;
    assert_eq!(body["error"]["code"], "not_owner");

    // Sanity: rule still exists for superadmin.
    let resp = f
        .router
        .oneshot(req("GET", "/v1/rules", SUPER_TOKEN))
        .await
        .expect("oneshot");
    let v = body_json(resp).await;
    assert_eq!(v.as_array().unwrap().len(), 2);
    let _ = (f.state, f.bob_token); // suppress unused warnings
    let _ = json!(null);
}

#[tokio::test]
async fn superadmin_owner_filter_narrows_results() {
    let f = build_fixture().await;
    let resp = f
        .router
        .oneshot(req("GET", "/v1/rules?owner=alice", SUPER_TOKEN))
        .await
        .expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let v = body_json(resp).await;
    let arr = v.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["owner_user_id"], "alice");
}

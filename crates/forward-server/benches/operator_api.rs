//! 008-sqlite-storage T027 — operator API criterion bench.
//!
//! Drives `GET /v1/users`, `GET /v1/rules`, and `GET /v1/users/me`
//! through the in-process axum router so we can compare the v0.8
//! SQLite-backed read path to the v0.7 file-store baseline saved at
//! `specs/008-sqlite-storage/baselines/operator_api_v07.json` (SC-004:
//! within 10 % of v0.7 p50 / p99).
//!
//! Run with: `cargo bench -p forward-server --bench operator_api`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use criterion::{Criterion, criterion_group, criterion_main};
use forward_server::clients::ConnectedClients;
use forward_server::operator::http;
use forward_server::state::AppState;
use forward_server::store::Store;
use forward_server::store::operator_store::SqliteOperatorStore;
use forward_server::store::token_store::SqliteTokenStore;
use tempfile::TempDir;
use tokio::runtime::Runtime;
use tower::ServiceExt;

const SUPER: &str = "bench-super";

fn build_router() -> (axum::Router, TempDir) {
    let dir = TempDir::new().unwrap();
    let sqlite = Arc::new(Store::open(dir.path()).unwrap());
    let tokens = Arc::new(SqliteTokenStore::new(sqlite.clone()));
    let operator_store = Arc::new(SqliteOperatorStore::new(sqlite.clone()));
    operator_store.bootstrap_legacy_superadmin(SUPER).unwrap();

    // Seed 32 users so /v1/users has realistic shape.
    use std::str::FromStr;
    for i in 0..32 {
        let id = forward_auth::UserId::from_str(&format!("u{i}")).unwrap();
        operator_store
            .add_user(forward_auth::User {
                id: id.clone(),
                display_name: format!("User {i}"),
                role: forward_auth::OperatorRole::User,
                disabled: false,
                created_at: chrono::Utc::now(),
            })
            .unwrap();
        let _ = operator_store
            .issue_credential(&id, Some(format!("u{i}-default")))
            .unwrap();
    }

    let state = Arc::new(
        AppState::new(
            tokens,
            operator_store,
            ConnectedClients::default(),
            "127.0.0.1:0",
            "deadbeef",
            "-----BEGIN CERTIFICATE-----\n",
            16,
            sqlite,
        )
        .unwrap(),
    );
    (http::router(state), dir)
}

fn req(method: Method, uri: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("Authorization", format!("Bearer {SUPER}"))
        .body(Body::empty())
        .unwrap()
}

fn bench_operator_api(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let (router, _dir) = build_router();

    c.bench_function("operator_api/get_v1_users", |b| {
        b.iter(|| {
            let router = router.clone();
            rt.block_on(async move {
                let resp = router.oneshot(req(Method::GET, "/v1/users")).await.unwrap();
                assert_eq!(resp.status(), StatusCode::OK);
            });
        });
    });

    c.bench_function("operator_api/get_v1_rules", |b| {
        b.iter(|| {
            let router = router.clone();
            rt.block_on(async move {
                let resp = router.oneshot(req(Method::GET, "/v1/rules")).await.unwrap();
                assert_eq!(resp.status(), StatusCode::OK);
            });
        });
    });

    c.bench_function("operator_api/get_v1_users_me", |b| {
        b.iter(|| {
            let router = router.clone();
            rt.block_on(async move {
                let resp = router
                    .oneshot(req(Method::GET, "/v1/users/me"))
                    .await
                    .unwrap();
                assert_eq!(resp.status(), StatusCode::OK);
            });
        });
    });
}

criterion_group!(operator_api, bench_operator_api);
criterion_main!(operator_api);

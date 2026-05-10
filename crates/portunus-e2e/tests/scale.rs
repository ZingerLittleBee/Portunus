//! T038a — 100-client scale test (SC-004a).
//!
//! Spin up one server, provision 100 clients, start them all, and assert
//! that within a generous deadline every one is reported `connected: true`.
//! After that, verify the operator HTTP API can serve the full list in
//! under 1 second.

mod common;

use std::time::{Duration, Instant};

const N_CLIENTS: usize = 100;

#[test]
fn test_100_clients_connected_within_one_second() {
    let server = common::spawn_server(&[]);
    let (_grpc, http) = server
        .wait_listening(Duration::from_secs(5))
        .expect("server should be listening");

    // Phase A: provision + spawn 100 clients.
    let mut bundles = Vec::with_capacity(N_CLIENTS);
    for i in 0..N_CLIENTS {
        let name = format!("edge-{i:03}");
        bundles.push((name.clone(), common::provision_client_http(&http, &name)));
    }

    let mut clients = Vec::with_capacity(N_CLIENTS);
    for (_, path) in &bundles {
        clients.push(common::spawn_client(path, &[]));
    }

    // Phase B: wait until all 100 appear connected. Generous deadline (30s)
    // — SC-004a only requires *querying* in <1s, not boot.
    let all_connected = common::wait_for(Duration::from_secs(30), || {
        let arr = common::list_clients_http(&http);
        let arr = arr.as_array()?;
        let connected_count = arr
            .iter()
            .filter(|v| {
                v.get("connected")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
            })
            .count();
        if connected_count >= N_CLIENTS {
            Some(connected_count)
        } else {
            None
        }
    });
    assert!(
        all_connected.is_some(),
        "expected {N_CLIENTS} clients connected within 30s"
    );

    // Phase C: SC-004a — HTTP `list-clients` for the full 100 must return in
    // under 1 second.
    let start = Instant::now();
    let arr = common::list_clients_http(&http);
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_secs(1),
        "list-clients took {elapsed:?} (must be <1s)"
    );
    assert_eq!(
        arr.as_array().unwrap().len(),
        N_CLIENTS,
        "list-clients should return all {N_CLIENTS} entries"
    );

    // Drop all client handles → kills child processes (Drop impl).
    drop(clients);
}

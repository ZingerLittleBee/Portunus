//! Integration test for the UDS stats server. Spawns a server,
//! connects via UnixStream, reads Hello and one Snapshot, then
//! disconnects.

use std::sync::Arc;
use std::time::Duration;

use portunus_core::{PortRange, RuleId};
use portunus_forwarder::RuleStats;
use portunus_standalone::stats::{Hello, Snapshot, server};
use tempfile::tempdir;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixStream;
use tokio_util::sync::CancellationToken;

#[tokio::test(flavor = "current_thread")]
async fn server_emits_hello_then_snapshots() {
    let dir = tempdir().unwrap();
    let sock = dir.path().join("test.sock");

    let rule_id = RuleId(1);
    let stats = RuleStats::for_range(PortRange::single(2222));

    let registry: server::Registry =
        Arc::new(std::sync::RwLock::new(std::collections::HashMap::from([(
            rule_id,
            server::RuleEntry {
                stats: Arc::clone(&stats),
                meta: server::RuleMetaStatic {
                    name: "test-rule".into(),
                    proto: "tcp".into(),
                    listen: "2222".into(),
                    targets: vec![server::TargetMetaStatic {
                        host: "1.1.1.1".into(),
                        port: 22,
                        priority: 0,
                        proxy_protocol: None,
                    }],
                    splice_capable: true,
                    udp_max_flows: None,
                },
            },
        )])));

    let cancel = CancellationToken::new();
    let started_at_ms: u64 = 12345;
    let handle = server::spawn(
        sock.clone(),
        Arc::clone(&registry),
        Duration::from_millis(250),
        started_at_ms,
        cancel.clone(),
    )
    .unwrap();

    let stream = UnixStream::connect(&sock).await.unwrap();
    let mut reader = BufReader::new(stream).lines();

    let line = tokio::time::timeout(Duration::from_secs(2), reader.next_line())
        .await
        .unwrap()
        .unwrap()
        .expect("hello line");
    let hello: Hello = serde_json::from_str(&line).unwrap();
    assert_eq!(hello.v, 1);
    assert_eq!(hello.rules.len(), 1);
    assert_eq!(hello.rules[0].name, "test-rule");

    stats
        .bytes_in
        .fetch_add(100, std::sync::atomic::Ordering::Relaxed);

    let line = tokio::time::timeout(Duration::from_secs(2), reader.next_line())
        .await
        .unwrap()
        .unwrap()
        .expect("snapshot line");
    let snap: Snapshot = serde_json::from_str(&line).unwrap();
    assert_eq!(snap.r.len(), 1);
    assert_eq!(snap.r[0].bytes_in, 100);

    cancel.cancel();
    let _ = handle.await;
}

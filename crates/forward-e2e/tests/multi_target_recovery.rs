//! End-to-end coverage for 007-multi-target-failover US2 (T027).
//!
//! Exercises Failed→Healthy recovery via the active TCP-connect probe
//! (`health_check_interval_secs: 1`):
//!
//! 1. Push a 2-target rule with both targets reachable and a 1 s
//!    active probe interval.
//! 2. Open a connection — bytes round-trip via the primary.
//! 3. Kill the primary; subsequent new connections should fail over to
//!    the secondary as the passive failure detector + active probe
//!    converge on `Healthy → Failed` for target 0.
//! 4. Restart the primary; the active probe should detect its recovery
//!    within a couple of cycles (≤ 5 s in practice) and the next new
//!    connection should land back on the primary.
//!
//! T030 (in-flight stickiness, FR-011) is a structural invariant —
//! the failover path only calls `select()` on the accept path, never
//! reaches into established connections to swap their outbound. The
//! TCP unit tests in `forwarder::tests` cover the established-
//! connection drain semantics; this test focuses on NEW-connection
//! routing across the recovery transition.

mod common;

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::Mutex;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;

use serde_json::Value;

fn test_lock() -> &'static Mutex<()> {
    static LOCK: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Spawn a tagged echo server: prepends `tag` to each response so the
/// client can tell which target served the connection.
fn spawn_tagged_echo(tag: &'static [u8]) -> (TaggedEcho, u16) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("echo bind");
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = std_mpsc::channel::<()>();
    listener.set_nonblocking(true).unwrap();
    let handle = std::thread::spawn(move || {
        loop {
            // Check for shutdown.
            if rx.try_recv().is_ok() {
                break;
            }
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_nonblocking(false).ok();
                    std::thread::spawn(move || {
                        let mut buf = [0u8; 4096];
                        loop {
                            match stream.read(&mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => {
                                    let mut resp = Vec::with_capacity(tag.len() + n);
                                    resp.extend_from_slice(tag);
                                    resp.extend_from_slice(&buf[..n]);
                                    if stream.write_all(&resp).is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
    });
    (
        TaggedEcho {
            shutdown: tx,
            join: Some(handle),
        },
        port,
    )
}

struct TaggedEcho {
    shutdown: std_mpsc::Sender<()>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl TaggedEcho {
    fn stop(mut self) {
        let _ = self.shutdown.send(());
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn pick_listen_port() -> u16 {
    for _ in 0..50 {
        let probe = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("probe");
        let port = probe.local_addr().unwrap().port();
        drop(probe);
        if let Ok(verify) = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)) {
            drop(verify);
            return port;
        }
    }
    panic!("no free listen port");
}

/// Probe the rule until the response carries `expected_tag` or we time
/// out. Each probe opens a NEW connection (FR-009 — failover applies
/// to NEW connections; existing ones are sticky).
fn wait_for_target(listen_port: u16, expected_tag: &[u8], timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if let Ok(mut conn) = TcpStream::connect((Ipv4Addr::LOCALHOST, listen_port)) {
            conn.set_read_timeout(Some(Duration::from_secs(2))).ok();
            if conn.write_all(b"x").is_ok() {
                let mut buf = [0u8; 8];
                if let Ok(n) = conn.read(&mut buf)
                    && buf[..n].starts_with(expected_tag)
                {
                    return true;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

#[test]
#[allow(clippy::too_many_lines)]
fn active_probe_recovers_primary_after_restart() {
    let _g = test_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let server = common::spawn_server(&[]);
    let (_grpc, http) = server
        .wait_listening(Duration::from_secs(5))
        .expect("server should be listening");
    let bundle = common::provision_client_http(&http, "edge-01");
    let _client = common::spawn_client(&bundle, &[]);
    common::wait_for(Duration::from_secs(5), || {
        let arr = common::list_clients_http(&http);
        arr.as_array()?
            .iter()
            .find(|v| {
                v.get("client_name").and_then(|n| n.as_str()) == Some("edge-01")
                    && v.get("connected").and_then(Value::as_bool).unwrap_or(false)
            })
            .cloned()
    })
    .expect("client connect");

    let (primary_echo, primary_port) = spawn_tagged_echo(b"A:");
    let (secondary_echo, secondary_port) = spawn_tagged_echo(b"B:");
    let listen_port = pick_listen_port();

    let (status, body) = common::push_rule_http_targets(
        &http,
        "edge-01",
        listen_port,
        &[
            ("127.0.0.1", primary_port),
            ("127.0.0.1", secondary_port),
        ],
        Some(1), // 1 s active probe interval
        Some(5),
    );
    assert!(
        status.is_success(),
        "multi-target push should succeed: {status} body={body}"
    );

    // 1) Initial routing — should land on primary (Healthy).
    assert!(
        wait_for_target(listen_port, b"A:", Duration::from_secs(3)),
        "expected initial connect to land on primary"
    );

    // 2) Kill primary; subsequent new connects fail over to secondary.
    primary_echo.stop();
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        wait_for_target(listen_port, b"B:", Duration::from_secs(8)),
        "expected failover to secondary after primary went down"
    );

    // 3) Bring primary back up on the SAME port (best-effort — the
    //    OS may have reassigned it; if so, skip the recovery probe).
    let primary_echo_2 = match TcpListener::bind((Ipv4Addr::LOCALHOST, primary_port)) {
        Ok(l) => Some(spawn_recovery_echo(l, b"A:")),
        Err(_) => None,
    };

    if primary_echo_2.is_some() {
        // 4) Active probe (1 s cadence; needs 2 successes to recover)
        //    + new connect should land back on primary within ~5 s.
        assert!(
            wait_for_target(listen_port, b"A:", Duration::from_secs(10)),
            "expected primary recovery via active probe + 2 successive new connects"
        );
    }

    secondary_echo.stop();
}

/// Stand up an echo server on an already-bound listener (used for the
/// recovery step). Same tagging shape as `spawn_tagged_echo` but the
/// caller hands us the listener.
fn spawn_recovery_echo(listener: TcpListener, tag: &'static [u8]) -> TaggedEcho {
    let (tx, rx) = std_mpsc::channel::<()>();
    listener.set_nonblocking(true).unwrap();
    let handle = std::thread::spawn(move || {
        loop {
            if rx.try_recv().is_ok() {
                break;
            }
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream.set_nonblocking(false).ok();
                    std::thread::spawn(move || {
                        let mut buf = [0u8; 4096];
                        loop {
                            match stream.read(&mut buf) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => {
                                    let mut resp = Vec::with_capacity(tag.len() + n);
                                    resp.extend_from_slice(tag);
                                    resp.extend_from_slice(&buf[..n]);
                                    if stream.write_all(&resp).is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(_) => break,
            }
        }
    });
    TaggedEcho {
        shutdown: tx,
        join: Some(handle),
    }
}

//! UDS stats server. Listens on a Unix-domain socket; per accepted
//! connection, sends a Hello immediately then a Snapshot every
//! `refresh_ms` until the client disconnects or the daemon cancels.
//!
//! Daemon-side state is just the read-only counters held in
//! `RuleStats`; the server never mutates them.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use portunus_core::RuleId;
use portunus_forwarder::RuleStats;
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use tokio::time::{MissedTickBehavior, interval, timeout};
use tokio_util::sync::CancellationToken;

use super::{
    ErrorSnap, Hello, PROTOCOL_VERSION, ProcessSnap, RuleMeta, RuleSnap, Snapshot, TargetMeta,
};

/// Maximum concurrent stats-client connections. The socket is local and
/// group-gated, but an unbounded accept loop lets any same-group process
/// exhaust the daemon by opening connections that each run periodic
/// `/proc` scans. 16 is far above any legitimate operator use.
const MAX_STATS_CLIENTS: usize = 16;

/// Per-write deadline. A stalled or malicious reader must not pin a server
/// task forever; exceeding this drops the connection.
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

pub type Registry = Arc<RwLock<HashMap<RuleId, RuleEntry>>>;

#[derive(Debug, Clone)]
pub struct RuleEntry {
    pub stats: Arc<RuleStats>,
    pub meta: RuleMetaStatic,
}

#[derive(Debug, Clone)]
pub struct RuleMetaStatic {
    pub name: String,
    pub proto: String,
    pub listen: String,
    pub targets: Vec<TargetMetaStatic>,
    pub splice_capable: bool,
    pub udp_max_flows: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct TargetMetaStatic {
    pub host: String,
    pub port: u16,
    pub priority: u32,
    pub proxy_protocol: Option<String>,
}

/// Spawn the UDS server. The returned `JoinHandle` resolves when
/// the supervisor task exits (cancel or hard error).
///
/// # Errors
/// Returns `std::io::Error` if the parent dir cannot be created or
/// the socket cannot be bound.
pub fn spawn(
    socket_path: PathBuf,
    registry: Registry,
    refresh: Duration,
    daemon_started_at_ms: u64,
    cancel: CancellationToken,
) -> std::io::Result<JoinHandle<()>> {
    use std::os::unix::fs::PermissionsExt;

    // Trust model: the stats socket carries infrastructure metadata (rule
    // names, listen ports, upstream targets, byte counters). It is gated to
    // owner + group (0o660), NOT world. Remote access is an operator concern.
    //
    // Ensure parent dir exists and is owner+group only (0o750). Restricting
    // the directory closes the brief window between `bind()` (which creates
    // the socket under the process umask) and the `set_permissions` below:
    // "other" cannot traverse into a 0o750 dir, so the socket is never
    // reachable by world even transiently. Best-effort — a pre-existing dir
    // we do not own may refuse chmod, which is non-fatal.
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
        if let Err(e) = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o750)) {
            tracing::debug!(
                event = "standalone.stats_dir_chmod_skipped",
                path = %parent.display(),
                error = %e,
            );
        }
    }
    // Clean up any stale socket file.
    let _ = std::fs::remove_file(&socket_path);

    let listener = UnixListener::bind(&socket_path)?;
    // 0660 — owner + group rw, world none. Propagate failure: silently
    // leaving the socket at the umask default would risk exposing metadata.
    if let Err(e) = std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o660)) {
        let _ = std::fs::remove_file(&socket_path);
        return Err(e);
    }

    // u128 → u64: any reasonable refresh interval (ms) fits in u64 for centuries.
    #[allow(clippy::cast_possible_truncation)]
    let refresh_ms = refresh.as_millis() as u64;
    tracing::info!(
        event = "standalone.stats_socket_listening",
        path = %socket_path.display(),
        refresh_ms,
    );

    let start_instant = Instant::now();
    let conn_limit = Arc::new(Semaphore::new(MAX_STATS_CLIENTS));

    Ok(tokio::spawn(async move {
        loop {
            tokio::select! {
                () = cancel.cancelled() => {
                    let _ = std::fs::remove_file(&socket_path);
                    break;
                }
                accept = listener.accept() => {
                    let (stream, _addr) = match accept {
                        Ok(v) => v,
                        Err(e) => {
                            tracing::warn!(
                                event = "standalone.stats_accept_error",
                                error = %e,
                            );
                            continue;
                        }
                    };
                    // Bound concurrent clients. The owned permit is held by
                    // the spawned task and released on disconnect.
                    let Ok(permit) = Arc::clone(&conn_limit).try_acquire_owned() else {
                        tracing::warn!(
                            event = "standalone.stats_conn_limit_reached",
                            max = MAX_STATS_CLIENTS,
                        );
                        drop(stream);
                        continue;
                    };
                    let registry = Arc::clone(&registry);
                    let cancel = cancel.clone();
                    tokio::spawn(handle_client(
                        stream,
                        registry,
                        refresh,
                        daemon_started_at_ms,
                        start_instant,
                        cancel,
                        permit,
                    ));
                }
            }
        }
    }))
}

async fn handle_client(
    mut stream: UnixStream,
    registry: Registry,
    refresh: Duration,
    daemon_started_at_ms: u64,
    start_instant: Instant,
    cancel: CancellationToken,
    // Held for the lifetime of the connection; released on return so the
    // `MAX_STATS_CLIENTS` slot frees up.
    _permit: OwnedSemaphorePermit,
) {
    // Send Hello once immediately after accept.
    let hello = build_hello(&registry, daemon_started_at_ms, refresh);
    if let Err(e) = write_line(&mut stream, &hello).await {
        tracing::debug!(
            event = "standalone.stats_client_write_failed",
            stage = "hello",
            error = %e,
        );
        return;
    }

    let mut tick = interval(refresh);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut seq: u64 = 0;
    loop {
        tokio::select! {
            () = cancel.cancelled() => break,
            _ = tick.tick() => {
                seq = seq.wrapping_add(1);
                // Collect process info off the runtime worker (the /proc
                // reads are synchronous, blocking I/O).
                let process = collect_process_info().await;
                let snap = build_snapshot(&registry, start_instant, seq, process);
                if let Err(e) = write_line(&mut stream, &snap).await {
                    tracing::debug!(
                        event = "standalone.stats_client_disconnected",
                        error = %e,
                    );
                    break;
                }
            }
        }
    }
}

async fn write_line<T: serde::Serialize>(
    stream: &mut UnixStream,
    value: &T,
) -> std::io::Result<()> {
    let mut buf = serde_json::to_vec(value).map_err(std::io::Error::other)?;
    buf.push(b'\n');
    // Bound the write so a stalled reader cannot pin this task indefinitely.
    timeout(WRITE_TIMEOUT, async {
        stream.write_all(&buf).await?;
        stream.flush().await
    })
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "stats write timed out"))?
}

fn build_hello(registry: &Registry, daemon_started_at_ms: u64, refresh: Duration) -> Hello {
    // A poisoned lock means a *reader/writer* panicked while holding it; the
    // map data itself is still consistent, so recover the guard rather than
    // cascading the panic into every subsequent snapshot.
    let g = registry
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let rules = g
        .iter()
        .map(|(id, entry)| RuleMeta {
            id: id.to_string(),
            name: entry.meta.name.clone(),
            proto: entry.meta.proto.clone(),
            listen: entry.meta.listen.clone(),
            targets: entry
                .meta
                .targets
                .iter()
                .map(|t| TargetMeta {
                    host: t.host.clone(),
                    port: t.port,
                    priority: t.priority,
                    proxy_protocol: t.proxy_protocol.clone(),
                })
                .collect(),
            splice_capable: entry.meta.splice_capable,
            udp_max_flows: entry.meta.udp_max_flows,
        })
        .collect();
    Hello {
        v: PROTOCOL_VERSION,
        daemon_version: env!("CARGO_PKG_VERSION").to_string(),
        daemon_started_at_ms,
        // u128 → u64: any reasonable refresh interval fits in u64 ms
        // u128 → u64: any reasonable refresh interval (ms) fits in u64 for centuries.
        #[allow(clippy::cast_possible_truncation)]
        refresh_ms: refresh.as_millis() as u64,
        rules,
    }
}

fn build_snapshot(
    registry: &Registry,
    start_instant: Instant,
    seq: u64,
    process: ProcessSnap,
) -> Snapshot {
    // u128 → u64: uptime and wall-clock ms since epoch both fit in u64 for
    // centuries; truncation is intentional and documented.
    #[allow(clippy::cast_possible_truncation)]
    let uptime_ms = start_instant.elapsed().as_millis() as u64;
    #[allow(clippy::cast_possible_truncation)]
    let t_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    // See `build_hello` — recover from poison rather than cascading a panic.
    let g = registry
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let r = g
        .iter()
        .map(|(id, entry)| {
            let s = &entry.stats;
            let err = s.errors.snapshot();
            RuleSnap {
                id: id.to_string(),
                bytes_in: s.bytes_in.load(Ordering::Relaxed),
                out: s.bytes_out.load(Ordering::Relaxed),
                conns_active: s.active_connections.load(Ordering::Relaxed),
                conns_total: s.connections_total.load(Ordering::Relaxed),
                datagrams_in: s.datagrams_in.load(Ordering::Relaxed),
                datagrams_out: s.datagrams_out.load(Ordering::Relaxed),
                flows_active: s.active_flows.load(Ordering::Relaxed),
                target_failovers_total: s.target_failovers_total.load(Ordering::Relaxed),
                err: ErrorSnap {
                    port_in_use: err.port_in_use,
                    upstream_connect_failed: err.upstream_connect_failed,
                    icmp_evict: err.icmp_evict,
                    emsgsize: err.emsgsize,
                    wouldblock: err.wouldblock,
                    addflow_dropped: err.addflow_dropped,
                    flows_pending_drops: err.flows_pending_drops,
                    dns_failures: s.dns_failures.load(Ordering::Relaxed),
                    flows_dropped_overflow: s.flows_dropped_overflow.load(Ordering::Relaxed),
                },
            }
        })
        .collect();

    Snapshot {
        t_ms,
        uptime_ms,
        seq,
        process,
        r,
    }
}

/// Collect process-level metrics without blocking a runtime worker. On
/// Linux the underlying `/proc` reads are synchronous file I/O, so they run
/// on the blocking pool.
#[cfg(target_os = "linux")]
async fn collect_process_info() -> ProcessSnap {
    tokio::task::spawn_blocking(collect_process_info_blocking)
        .await
        .unwrap_or_default()
}

#[cfg(target_os = "linux")]
fn collect_process_info_blocking() -> ProcessSnap {
    let fd_open = std::fs::read_dir("/proc/self/fd")
        .ok()
        .and_then(|d| u32::try_from(d.count()).ok());
    let fd_limit = read_rlimit_nofile_soft();
    let rss_bytes = read_proc_status_rss();
    ProcessSnap {
        fd_open,
        fd_limit,
        rss_bytes,
    }
}

// Kept `async` to match the Linux signature so the call site is uniform.
#[cfg(not(target_os = "linux"))]
#[allow(clippy::unused_async)]
async fn collect_process_info() -> ProcessSnap {
    ProcessSnap::default()
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn read_rlimit_nofile_soft() -> Option<u64> {
    // SAFETY: `getrlimit` is a pure syscall with no memory hazards when
    // given a valid mutable pointer to an initialised `rlimit`.
    let mut rlim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    let rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut rlim) };
    if rc == 0 {
        // libc::rlim_t is u64 on Linux; no cast needed.
        Some(rlim.rlim_cur)
    } else {
        None
    }
}

#[cfg(target_os = "linux")]
fn read_proc_status_rss() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

    /// Build a single-rule registry with one TCP target, mirroring the
    /// integration-test fixture so the helper exercises `build_hello`'s
    /// target-mapping branch.
    fn registry_with_one_rule() -> (Registry, Arc<RuleStats>) {
        let stats = Arc::new(RuleStats::default());
        let registry: Registry = Arc::new(RwLock::new(HashMap::from([(
            RuleId(1),
            RuleEntry {
                stats: Arc::clone(&stats),
                meta: RuleMetaStatic {
                    name: "test-rule".into(),
                    proto: "tcp".into(),
                    listen: "2222".into(),
                    targets: vec![TargetMetaStatic {
                        host: "1.1.1.1".into(),
                        port: 22,
                        priority: 0,
                        proxy_protocol: Some("v2".into()),
                    }],
                    splice_capable: true,
                    udp_max_flows: Some(64),
                },
            },
        )])));
        (registry, stats)
    }

    /// `build_hello` projects the static rule metadata (including targets)
    /// into the wire `Hello`.
    #[test]
    fn build_hello_projects_rule_metadata() {
        let (registry, _stats) = registry_with_one_rule();
        let hello = build_hello(&registry, 12345, Duration::from_millis(250));
        assert_eq!(hello.v, PROTOCOL_VERSION);
        assert_eq!(hello.daemon_started_at_ms, 12345);
        assert_eq!(hello.refresh_ms, 250);
        assert_eq!(hello.rules.len(), 1);
        let rule = &hello.rules[0];
        assert_eq!(rule.id, "1");
        assert_eq!(rule.name, "test-rule");
        assert_eq!(rule.proto, "tcp");
        assert_eq!(rule.listen, "2222");
        assert!(rule.splice_capable);
        assert_eq!(rule.udp_max_flows, Some(64));
        assert_eq!(rule.targets.len(), 1);
        assert_eq!(rule.targets[0].host, "1.1.1.1");
        assert_eq!(rule.targets[0].port, 22);
        assert_eq!(rule.targets[0].proxy_protocol.as_deref(), Some("v2"));
    }

    /// `build_snapshot` reflects the live atomic counters for each rule.
    #[test]
    fn build_snapshot_reflects_live_counters() {
        let (registry, stats) = registry_with_one_rule();
        stats.bytes_in.store(100, Ordering::Relaxed);
        stats.bytes_out.store(200, Ordering::Relaxed);
        stats.errors.port_in_use.store(3, Ordering::Relaxed);
        let snap = build_snapshot(&registry, Instant::now(), 7, ProcessSnap::default());
        assert_eq!(snap.seq, 7);
        assert_eq!(snap.r.len(), 1);
        assert_eq!(snap.r[0].id, "1");
        assert_eq!(snap.r[0].bytes_in, 100);
        assert_eq!(snap.r[0].out, 200);
        assert_eq!(snap.r[0].err.port_in_use, 3);
    }

    /// `write_line` serialises a value as a single newline-terminated JSON
    /// frame and flushes it within the write deadline.
    #[tokio::test]
    async fn write_line_emits_newline_framed_json() {
        let (mut server_side, client_side) = UnixStream::pair().unwrap();
        let hello = Hello {
            v: PROTOCOL_VERSION,
            daemon_version: "test".into(),
            daemon_started_at_ms: 0,
            refresh_ms: 250,
            rules: vec![],
        };
        write_line(&mut server_side, &hello).await.unwrap();
        drop(server_side);

        let mut reader = BufReader::new(client_side);
        let mut line = String::new();
        let n = reader.read_line(&mut line).await.unwrap();
        assert!(n > 0);
        assert!(line.ends_with('\n'));
        let back: Hello = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(back.v, PROTOCOL_VERSION);
    }

    /// `handle_client` returns early when the very first (Hello) write fails
    /// because the peer is already gone, hitting the hello write-error branch.
    #[tokio::test]
    async fn handle_client_returns_when_hello_write_fails() {
        let (server_side, client_side) = UnixStream::pair().unwrap();
        // Drop the peer before handing the stream to the server so the
        // immediate Hello write fails with a broken pipe.
        drop(client_side);

        let (registry, _stats) = registry_with_one_rule();
        let permit = Arc::new(Semaphore::new(1)).try_acquire_owned().unwrap();
        let cancel = CancellationToken::new();

        // Must return promptly rather than entering the tick loop.
        timeout(
            Duration::from_secs(5),
            handle_client(
                server_side,
                registry,
                Duration::from_millis(10),
                0,
                Instant::now(),
                cancel,
                permit,
            ),
        )
        .await
        .expect("handle_client should return after the failed hello write");
    }

    /// `handle_client` breaks out of the tick loop once a snapshot write
    /// fails after the peer disconnects, exercising the disconnect branch.
    #[tokio::test]
    async fn handle_client_breaks_when_snapshot_write_fails() {
        let (server_side, client_side) = UnixStream::pair().unwrap();
        let (registry, _stats) = registry_with_one_rule();
        let permit = Arc::new(Semaphore::new(1)).try_acquire_owned().unwrap();
        let cancel = CancellationToken::new();

        let server = tokio::spawn(handle_client(
            server_side,
            registry,
            Duration::from_millis(5),
            0,
            Instant::now(),
            cancel,
            permit,
        ));

        // Read the Hello frame (proves the first write succeeded), then drop
        // the peer so subsequent snapshot ticks fail and the loop breaks.
        let mut reader = BufReader::new(client_side);
        let mut line = String::new();
        let n = reader.read_line(&mut line).await.unwrap();
        assert!(n > 0);
        drop(reader);

        timeout(Duration::from_secs(5), server)
            .await
            .expect("handle_client should break after the peer disconnects")
            .expect("handle_client task should not panic");
    }

    /// `handle_client` stops when the daemon cancels, even with a live peer.
    #[tokio::test]
    async fn handle_client_stops_on_cancel() {
        let (server_side, client_side) = UnixStream::pair().unwrap();
        let (registry, _stats) = registry_with_one_rule();
        let permit = Arc::new(Semaphore::new(1)).try_acquire_owned().unwrap();
        let cancel = CancellationToken::new();

        let server = tokio::spawn(handle_client(
            server_side,
            registry,
            // Long refresh so the loop is parked on `select!` when cancelled.
            Duration::from_secs(3600),
            0,
            Instant::now(),
            cancel.clone(),
            permit,
        ));

        // Drain the Hello so the server is sitting in the tick `select!`.
        let mut reader = BufReader::new(client_side);
        let mut line = String::new();
        let n = reader.read_line(&mut line).await.unwrap();
        assert!(n > 0);

        cancel.cancel();
        timeout(Duration::from_secs(5), server)
            .await
            .expect("handle_client should return after cancel")
            .expect("handle_client task should not panic");
    }

    /// End-to-end through `spawn`: the server binds the socket, sends a Hello
    /// then a Snapshot, and tears the socket down on cancel.
    #[tokio::test]
    async fn spawn_serves_hello_then_snapshot_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("stats.sock");

        let (registry, stats) = registry_with_one_rule();
        stats.bytes_in.store(42, Ordering::Relaxed);

        let cancel = CancellationToken::new();
        let handle = spawn(
            sock.clone(),
            Arc::clone(&registry),
            Duration::from_millis(20),
            12345,
            cancel.clone(),
        )
        .unwrap();

        let stream = UnixStream::connect(&sock).await.unwrap();
        let mut reader = BufReader::new(stream).lines();

        let hello_line = timeout(Duration::from_secs(5), reader.next_line())
            .await
            .unwrap()
            .unwrap()
            .expect("hello line");
        let hello: Hello = serde_json::from_str(&hello_line).unwrap();
        assert_eq!(hello.v, PROTOCOL_VERSION);
        assert_eq!(hello.rules.len(), 1);

        let snap_line = timeout(Duration::from_secs(5), reader.next_line())
            .await
            .unwrap()
            .unwrap()
            .expect("snapshot line");
        let snap: Snapshot = serde_json::from_str(&snap_line).unwrap();
        assert_eq!(snap.r.len(), 1);
        assert_eq!(snap.r[0].bytes_in, 42);

        cancel.cancel();
        let _ = timeout(Duration::from_secs(5), handle).await;
        // The socket file is removed on cancel.
        assert!(!sock.exists());
    }

    /// Opening more than `MAX_STATS_CLIENTS` live connections trips the
    /// connection-limit branch: the extra connection is accepted then
    /// immediately dropped, so its peer observes EOF without a Hello.
    #[tokio::test]
    async fn spawn_rejects_connections_past_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("stats.sock");

        let (registry, _stats) = registry_with_one_rule();
        let cancel = CancellationToken::new();
        let handle = spawn(
            sock.clone(),
            Arc::clone(&registry),
            // Long refresh so accepted clients stay parked and keep their permit.
            Duration::from_secs(3600),
            0,
            cancel.clone(),
        )
        .unwrap();

        // Saturate the permit pool and confirm each connection got a Hello,
        // which proves its `handle_client` task is live and holding a permit.
        let mut held = Vec::new();
        for _ in 0..MAX_STATS_CLIENTS {
            let stream = UnixStream::connect(&sock).await.unwrap();
            let mut reader = BufReader::new(stream).lines();
            let line = timeout(Duration::from_secs(5), reader.next_line())
                .await
                .unwrap()
                .unwrap()
                .expect("hello line for a permitted client");
            let _: Hello = serde_json::from_str(&line).unwrap();
            held.push(reader);
        }

        // The next connection exceeds the cap: the server drops it without a
        // Hello, so the read returns EOF (empty line / no bytes).
        let over = UnixStream::connect(&sock).await.unwrap();
        let mut over_reader = BufReader::new(over);
        let mut buf = Vec::new();
        let n = timeout(Duration::from_secs(5), over_reader.read_to_end(&mut buf))
            .await
            .expect("over-limit connection should close promptly")
            .unwrap();
        assert_eq!(n, 0, "over-limit connection must receive no Hello bytes");

        cancel.cancel();
        let _ = timeout(Duration::from_secs(5), handle).await;
        drop(held);
    }
}

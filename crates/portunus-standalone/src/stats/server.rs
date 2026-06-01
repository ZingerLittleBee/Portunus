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

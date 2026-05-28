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
use tokio::task::JoinHandle;
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;

use super::{
    ErrorSnap, Hello, PROTOCOL_VERSION, ProcessSnap, RuleMeta, RuleSnap, Snapshot, TargetMeta,
};

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
    // Ensure parent dir exists.
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Clean up any stale socket file.
    let _ = std::fs::remove_file(&socket_path);

    let listener = UnixListener::bind(&socket_path)?;
    // 0660 — owner + group rw, world none.
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o660);
    let _ = std::fs::set_permissions(&socket_path, perms);

    // u128 → u64: any reasonable refresh interval (ms) fits in u64 for centuries.
    #[allow(clippy::cast_possible_truncation)]
    let refresh_ms = refresh.as_millis() as u64;
    tracing::info!(
        event = "standalone.stats_socket_listening",
        path = %socket_path.display(),
        refresh_ms,
    );

    let start_instant = Instant::now();

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
                    let registry = Arc::clone(&registry);
                    let cancel = cancel.clone();
                    tokio::spawn(handle_client(
                        stream,
                        registry,
                        refresh,
                        daemon_started_at_ms,
                        start_instant,
                        cancel,
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
                let snap = build_snapshot(&registry, start_instant, seq);
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
    stream.write_all(&buf).await?;
    stream.flush().await
}

fn build_hello(registry: &Registry, daemon_started_at_ms: u64, refresh: Duration) -> Hello {
    let g = registry.read().expect("registry read lock poisoned");
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

fn build_snapshot(registry: &Registry, start_instant: Instant, seq: u64) -> Snapshot {
    // u128 → u64: uptime and wall-clock ms since epoch both fit in u64 for
    // centuries; truncation is intentional and documented.
    #[allow(clippy::cast_possible_truncation)]
    let uptime_ms = start_instant.elapsed().as_millis() as u64;
    #[allow(clippy::cast_possible_truncation)]
    let t_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let process = collect_process_info();

    let g = registry.read().expect("registry read lock poisoned");
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

#[cfg(target_os = "linux")]
fn collect_process_info() -> ProcessSnap {
    let fd_open = std::fs::read_dir("/proc/self/fd")
        .ok()
        .map(|d| d.count() as u32);
    let fd_limit = read_rlimit_nofile_soft();
    let rss_bytes = read_proc_status_rss();
    ProcessSnap {
        fd_open,
        fd_limit,
        rss_bytes,
    }
}

#[cfg(not(target_os = "linux"))]
fn collect_process_info() -> ProcessSnap {
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
    let rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut rlim) };
    if rc == 0 {
        // libc::rlim_t is u64 on Linux; cast is value-preserving.
        Some(rlim.rlim_cur as u64)
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

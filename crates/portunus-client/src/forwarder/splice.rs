//! Linux TCP zero-copy fast path via `splice(2)` (012-tcp-zero-copy-splice).
//!
//! On Linux, plain TCP rules with no per-rule and no per-owner bandwidth cap
//! are forwarded by moving bytes through a per-connection `pipe2` pair using
//! the [`splice`] syscall, eliminating the userspace `read → memcpy → write`
//! round-trip that bounds throughput on the v1.2.0 path. The
//! [`tokio::io::copy_bidirectional_with_sizes`] userspace path remains the
//! canonical reference and the fallback for non-Linux platforms or
//! ineligible rules.
//!
//! Operator surface is empty: no rule field, no wire field, no Web UI
//! control, no CLI flag. The undocumented `PORTUNUS_DISABLE_SPLICE` env
//! variable is the only off-ramp — intended for triage and bench A/B.
//!
//! See [`specs/012-tcp-zero-copy-splice/spec.md`] and the sibling design
//! artefacts (`plan.md`, `research.md`, `data-model.md`,
//! `contracts/internal-api.md`) for the full contract.

use std::sync::OnceLock;

use portunus_core::RuleId;
use portunus_proto::v1::Protocol;

use super::rate_limit::scope::{OwnerRateLimitHandle, RuleRateLimitHandle};

/// Per-connection context the splice path consults to decide eligibility.
///
/// Built once at connection-acceptance time via [`CopyCtx::build`] and
/// passed by reference into [`eligible`]. Small POD; `Copy` so callers do
/// not need to clone.
//
// `struct_excessive_bools` is allowed here because these flags are the
// natural encoding of the eligibility predicate's gates (FR-001..FR-007);
// collapsing them into a state-machine enum would obscure the per-gate
// test matrix in `eligible_tests`.
//
// Non-Linux builds: `has_sni_replay_done` and `has_proxy_out` are only
// read by the Linux tracing path, so they're dead on darwin/Windows.
#[allow(clippy::struct_excessive_bools)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Clone, Copy, Debug)]
pub(crate) struct CopyCtx {
    /// Rule identifier — used for tracing-event correlation only.
    pub(crate) rule_id: RuleId,
    /// Wire protocol of the rule. Splice only runs for TCP; defensive.
    pub(crate) protocol: Protocol,
    /// `true` if any of {rule.bandwidth_in_bps, rule.bandwidth_out_bps,
    /// owner.bandwidth_in_bps, owner.bandwidth_out_bps} is set. When true
    /// the splice path is ineligible (per-chunk userspace token accounting
    /// is required — see spec FR-001).
    pub(crate) has_bandwidth_cap: bool,
    /// `true` when `PORTUNUS_DISABLE_SPLICE` is set in the process
    /// environment. Cached once at process start via [`disable_splice_env`].
    pub(crate) disable_splice: bool,
    /// `true` if SNI peek+replay (v0.9) has completed for this connection.
    /// Tracing-event field only — splice is only invoked from the
    /// post-prelude site so this is effectively always `true` when reached.
    pub(crate) has_sni_replay_done: bool,
    /// `true` if the target had a PROXY-protocol prelude (v0.10) written.
    /// Tracing-event field only — same reasoning as [`Self::has_sni_replay_done`].
    pub(crate) has_proxy_out: bool,
}

impl CopyCtx {
    /// Build a [`CopyCtx`] from the runtime state available at connection
    /// acceptance time.
    ///
    /// `has_bandwidth_cap` is the OR of {rule, owner} bandwidth-cap presence
    /// — see spec FR-001. Per-rule and per-owner `concurrent_connections` /
    /// `new_connections_per_sec` caps are NOT consulted: those gate at the
    /// accept stage (v0.11) and never touch the data path, so they remain
    /// compatible with the splice fast path.
    ///
    /// `disable_splice` is sourced from [`disable_splice_env`] — the
    /// process-wide kill-switch state cached at first read.
    ///
    /// This function performs no I/O. Each `has_bandwidth_cap` lookup is
    /// an `Arc` deref + an `Option` check on the snapshotted limiter.
    ///
    /// Per spec FR-005 the result is **per-connection** — once built, the
    /// `CopyCtx` is not refreshed mid-connection. A subsequent rule
    /// hot-update via `PUT /v1/rules/{id}` that changes bandwidth-cap
    /// presence does NOT migrate in-flight connections between paths.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(crate) fn build(
        rule_id: RuleId,
        protocol: Protocol,
        rule_handle: Option<&RuleRateLimitHandle>,
        owner_handle: Option<&OwnerRateLimitHandle>,
        has_sni_replay_done: bool,
        has_proxy_out: bool,
    ) -> Self {
        let has_bandwidth_cap = rule_handle.is_some_and(RuleRateLimitHandle::has_bandwidth_cap)
            || owner_handle.is_some_and(OwnerRateLimitHandle::has_bandwidth_cap);
        Self {
            rule_id,
            protocol,
            has_bandwidth_cap,
            disable_splice: disable_splice_env(),
            has_sni_replay_done,
            has_proxy_out,
        }
    }
}

/// Pure-function eligibility predicate.
///
/// **Cross-platform.** Returns `false` on every non-Linux build (compile-time
/// constant) so callers can use the same expression everywhere without
/// platform `cfg`. On Linux, returns `true` iff every gate in the spec's
/// FR-001 / FR-005 predicate passes.
///
/// Guarantees:
///
/// - No I/O, no syscall, no allocation.
/// - Idempotent: repeated calls with the same `ctx` return the same value.
/// - Does **not** re-read the env on every call — the `disable_splice` bit
///   was cached at process start and is part of `ctx`.
#[cfg(target_os = "linux")]
#[inline]
pub(crate) fn eligible(ctx: &CopyCtx) -> bool {
    matches!(ctx.protocol, Protocol::Tcp) && !ctx.disable_splice && !ctx.has_bandwidth_cap
}

#[cfg(not(target_os = "linux"))]
#[inline]
#[allow(dead_code)]
pub(crate) const fn eligible(_ctx: &CopyCtx) -> bool {
    false
}

/// One-time, process-wide cache of the `PORTUNUS_DISABLE_SPLICE` env state.
///
/// Read once on first call and frozen for the lifetime of the process. Any
/// non-empty value of the variable (`"1"`, `"true"`, anything except the
/// empty string) forces the fast path off. Test fixtures bypass this by
/// constructing `CopyCtx { disable_splice: true, .. }` directly rather
/// than mutating the environment.
//
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn disable_splice_env() -> bool {
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED
        .get_or_init(|| std::env::var_os("PORTUNUS_DISABLE_SPLICE").is_some_and(|v| !v.is_empty()))
}

/// Success-return type of [`copy_bidirectional`].
///
/// Equivalent in shape and semantics to the `(u64, u64)` returned by
/// [`tokio::io::copy_bidirectional_with_sizes`]: the values count bytes
/// **delivered** to the destination socket on each direction, never bytes
/// received but not yet delivered. See spec FR-008 and research § R-008.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) struct Transferred {
    /// Bytes delivered downstream → upstream.
    pub(crate) bytes_in: u64,
    /// Bytes delivered upstream → downstream.
    pub(crate) bytes_out: u64,
}

/// Error type of [`copy_bidirectional`].
///
/// The [`Unsupported`](SpliceError::Unsupported) variant is the **only**
/// signal authorising the caller to fall back to the userspace path
/// (`tokio::io::copy_bidirectional_with_sizes`). It is returned only when
/// the first `splice` syscall returns one of the documented "unsupported"
/// errnos and zero bytes have moved into the pipe on either direction
/// (spec FR-006).
///
/// Once any byte has moved, all subsequent errors propagate as [`Io`](
/// SpliceError::Io) — the connection is terminal and the caller MUST
/// NOT retry on the userspace path. Doing so would risk dropping or
/// double-counting bytes already in flight.
#[derive(Debug)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) enum SpliceError {
    /// First `splice` syscall returned an unsupported errno before any
    /// byte moved. Caller may fall back to the userspace path.
    Unsupported {
        /// The errno reported by the kernel. One of `ENOSYS`, `EINVAL`,
        /// `EPERM`, `EOPNOTSUPP`, `ENOTSUP`.
        #[cfg(target_os = "linux")]
        errno: nix::errno::Errno,
    },
    /// Any other I/O error. The connection is terminal — no fallback.
    Io(std::io::Error),
}

#[cfg(target_os = "linux")]
impl From<std::io::Error> for SpliceError {
    fn from(e: std::io::Error) -> Self {
        SpliceError::Io(e)
    }
}

/// Emit the `proxy.splice_selected` `info`-level tracing event the first
/// time a given rule successfully enters the splice path. Subsequent
/// connections on the same rule are silent — the field schema in
/// `contracts/internal-api.md` § 3 is intentionally low-cardinality.
#[cfg(target_os = "linux")]
fn emit_splice_selected(ctx: &CopyCtx, pipe_capacity_bytes: usize) {
    use std::collections::HashSet;
    use std::sync::Mutex;

    static SEEN: OnceLock<Mutex<HashSet<u64>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    let first = {
        let mut guard = seen.lock().unwrap_or_else(|e| e.into_inner());
        guard.insert(ctx.rule_id.0)
    };
    if first {
        tracing::info!(
            event = "proxy.splice_selected",
            rule_id = ctx.rule_id.0,
            pipe_capacity_bytes,
            has_sni_replay_done = ctx.has_sni_replay_done,
            has_proxy_out = ctx.has_proxy_out,
        );
    }
}

// ====================================================================
// Linux-only implementation
// ====================================================================

#[cfg(target_os = "linux")]
mod linux {
    use std::io;
    use std::os::fd::{AsRawFd, OwnedFd, RawFd};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use nix::errno::Errno;
    use nix::fcntl::{FcntlArg, OFlag, fcntl};
    use nix::libc;
    use nix::sys::socket::{Shutdown as NixShutdown, shutdown as nix_shutdown};
    use nix::unistd::pipe2;
    use tokio::io::Interest;
    use tokio::net::TcpStream;

    use super::{CopyCtx, SpliceError, Transferred};

    /// Target pipe capacity per [research.md § R-003](../../specs/012-tcp-zero-copy-splice/research.md).
    /// 1 MiB matches the bench-host `pipe-max-size` default; failure to
    /// reach it is best-effort and degrades to the kernel default.
    const TARGET_PIPE_SIZE: i32 = 1024 * 1024;
    /// Kernel default pipe size on every Linux ≥ 2.6.11. Used as the
    /// fallback `capacity_bytes` when `F_GETPIPE_SZ` itself fails.
    const KERNEL_DEFAULT_PIPE_SIZE: usize = 64 * 1024;

    /// RAII wrapper around a `pipe2(O_NONBLOCK | O_CLOEXEC)` pair.
    ///
    /// `splice` is half-duplex per syscall, so a bidirectional
    /// [`copy_bidirectional`] allocates **two** `PipePair`s and runs both
    /// directions concurrently via `tokio::try_join!`. `F_SETPIPE_SZ` is
    /// applied as a best-effort upgrade to 1 MiB; on failure the pipe
    /// keeps the kernel default and one `proxy.splice_pipe_size_failed`
    /// `debug`-level event is emitted per affected pipe.
    ///
    /// Drop closes both fds via [`OwnedFd`].
    pub(crate) struct PipePair {
        pub(crate) read_fd: OwnedFd,
        pub(crate) write_fd: OwnedFd,
        /// Actual pipe capacity after the best-effort upgrade. Used as
        /// the `len` argument to subsequent `splice` syscalls.
        pub(crate) capacity_bytes: usize,
    }

    impl PipePair {
        /// Allocate a fresh non-blocking, close-on-exec pipe pair and
        /// best-effort enlarge it to [`TARGET_PIPE_SIZE`].
        ///
        /// Returns `Err(io::Error)` only on `pipe2` failure (`ENFILE`,
        /// `EMFILE`); `F_SETPIPE_SZ` / `F_GETPIPE_SZ` failures are logged
        /// and recovered with a fallback capacity.
        pub(crate) fn new(rule_id: u64) -> io::Result<Self> {
            let (read_fd, write_fd): (OwnedFd, OwnedFd) =
                pipe2(OFlag::O_NONBLOCK | OFlag::O_CLOEXEC).map_err(io::Error::from)?;
            let write_raw = write_fd.as_raw_fd();

            let setpipe_result = fcntl(write_raw, FcntlArg::F_SETPIPE_SZ(TARGET_PIPE_SIZE));

            // `F_GETPIPE_SZ` always succeeds on a valid pipe fd; if it
            // does not, fall back to the kernel default (Linux ≥ 2.6.11
            // guarantees 16 pages = 64 KiB).
            let actual_capacity = fcntl(write_raw, FcntlArg::F_GETPIPE_SZ)
                .map(|sz| usize::try_from(sz).unwrap_or(KERNEL_DEFAULT_PIPE_SIZE))
                .unwrap_or(KERNEL_DEFAULT_PIPE_SIZE);

            if let Err(errno) = setpipe_result {
                tracing::debug!(
                    event = "proxy.splice_pipe_size_failed",
                    rule_id,
                    requested_bytes = TARGET_PIPE_SIZE,
                    actual_default_bytes = actual_capacity,
                    errno_name = ?errno,
                );
            }

            Ok(Self {
                read_fd,
                write_fd,
                capacity_bytes: actual_capacity,
            })
        }
    }

    /// Raw `splice(2)` syscall wrapper. The only `unsafe` site in this
    /// module. Returns `Ok(bytes)` (possibly `0` for source EOF), or
    /// `Err(Errno)` for any error including `EAGAIN` / `EINTR`.
    ///
    /// Flags: `SPLICE_F_NONBLOCK | SPLICE_F_MOVE`. `SPLICE_F_MOVE` is the
    /// hint that the kernel may move (rather than copy) pages — even
    /// though current kernels often ignore this hint, it documents the
    /// zero-copy intent and costs nothing to set.
    #[allow(unsafe_code)]
    fn splice_raw(fd_in: RawFd, fd_out: RawFd, len: usize) -> Result<usize, Errno> {
        // SAFETY: `fd_in` / `fd_out` are valid file descriptors borrowed
        // from caller-owned `TcpStream` / `OwnedFd`. `splice` is
        // documented to accept passing `NULL` for `off_in` / `off_out`
        // when the corresponding fd is a pipe or socket. `flags` is a
        // POSIX bitmask we constrain to documented values.
        let n = unsafe {
            libc::splice(
                fd_in,
                std::ptr::null_mut(),
                fd_out,
                std::ptr::null_mut(),
                len,
                (libc::SPLICE_F_NONBLOCK | libc::SPLICE_F_MOVE) as libc::c_uint,
            )
        };
        if n < 0 {
            return Err(Errno::last());
        }
        // `n >= 0` and `splice` returns `ssize_t` whose max is `isize::MAX`,
        // safely fits in `usize`.
        usize::try_from(n).map_err(|_| Errno::EOVERFLOW)
    }

    /// Translate an errno into either an [`Unsupported`](SpliceError::Unsupported)
    /// fallback signal (only when zero bytes have moved on either
    /// direction) or a terminal [`Io`](SpliceError::Io) error.
    ///
    /// Per spec FR-006: only `ENOSYS`, `EINVAL`, `EPERM`,
    /// `EOPNOTSUPP` / `ENOTSUP` are eligible for fallback, and only
    /// before the first byte moves. `EAGAIN` and `EINTR` are NOT routed
    /// here — they are readiness/retry signals handled by the syscall
    /// loop directly.
    fn classify(errno: Errno, moved_any: &AtomicBool) -> SpliceError {
        if moved_any.load(Ordering::Relaxed) {
            return SpliceError::Io(io::Error::from_raw_os_error(errno as i32));
        }
        match errno {
            Errno::ENOSYS | Errno::EINVAL | Errno::EPERM | Errno::EOPNOTSUPP => {
                SpliceError::Unsupported { errno }
            }
            other => SpliceError::Io(io::Error::from_raw_os_error(other as i32)),
        }
    }

    /// Drain `n` bytes from `pipe.read_fd` into `dst`. Loops over
    /// `dst.writable()` + `try_io(WRITABLE, ...)` until all `n` bytes
    /// are emitted. Advances `bytes_out`.
    ///
    /// Errors here are always terminal (`Io`) because at least `n > 0`
    /// bytes have entered the pipe — `moved_any` has already been
    /// flipped by the caller.
    async fn drain_pipe_to(
        dst: &TcpStream,
        pipe_read_fd: RawFd,
        mut remaining: usize,
        bytes_out: &AtomicU64,
    ) -> io::Result<()> {
        while remaining > 0 {
            dst.writable().await?;
            let res = dst.try_io(Interest::WRITABLE, || {
                splice_raw(pipe_read_fd, dst.as_raw_fd(), remaining).map_err(|errno| {
                    // EAGAIN → tokio re-arms; EINTR → retry
                    if errno == Errno::EAGAIN {
                        io::Error::from(io::ErrorKind::WouldBlock)
                    } else {
                        io::Error::from_raw_os_error(errno as i32)
                    }
                })
            });
            match res {
                Ok(n) if n == 0 => {
                    // Should not happen on the pipe-out side unless the
                    // destination has half-closed write. Treat as EOF
                    // for safety.
                    return Err(io::Error::from(io::ErrorKind::WriteZero));
                }
                Ok(n) => {
                    bytes_out.fetch_add(n as u64, Ordering::Relaxed);
                    remaining -= n;
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Err(e) if e.raw_os_error() == Some(libc::EINTR) => continue,
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// One direction of the bidirectional splice loop:
    /// `src → pipe → dst`. Returns `Ok(())` on clean EOF (source
    /// signalled, pipe drained, `dst.shutdown(Write)` called); returns
    /// `Err(SpliceError)` per the contract.
    ///
    /// The first time `src → pipe` returns `Ok(n > 0)` flips
    /// `moved_any` to true — after which no `Unsupported` is permitted
    /// (FR-006).
    async fn splice_dir(
        src: &TcpStream,
        dst: &TcpStream,
        pipe: &PipePair,
        moved_any: &AtomicBool,
        bytes_out: &AtomicU64,
    ) -> Result<(), SpliceError> {
        let pipe_read_fd = pipe.read_fd.as_raw_fd();
        let pipe_write_fd = pipe.write_fd.as_raw_fd();
        let src_fd = src.as_raw_fd();
        let capacity = pipe.capacity_bytes;

        loop {
            // Wait for read readiness on the source socket.
            src.readable().await.map_err(SpliceError::Io)?;

            // src → pipe
            let read_res = src.try_io(Interest::READABLE, || {
                splice_raw(src_fd, pipe_write_fd, capacity).map_err(|errno| {
                    if errno == Errno::EAGAIN {
                        io::Error::from(io::ErrorKind::WouldBlock)
                    } else {
                        // Encode errno verbatim so the caller can
                        // recover the Errno via raw_os_error().
                        io::Error::from_raw_os_error(errno as i32)
                    }
                })
            });

            let n_in = match read_res {
                Ok(0) => {
                    // Source EOF — half-close `dst` write side; this
                    // direction is done. `shutdown(SHUT_WR)` is used
                    // rather than `tokio::AsyncWriteExt::shutdown` so it
                    // composes with the `&TcpStream` shared between the
                    // two splice directions.
                    let _ = nix_shutdown(dst.as_raw_fd(), NixShutdown::Write);
                    return Ok(());
                }
                Ok(n) => {
                    moved_any.store(true, Ordering::Relaxed);
                    n
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Err(e) if e.raw_os_error() == Some(libc::EINTR) => continue,
                Err(e) => {
                    if let Some(raw) = e.raw_os_error() {
                        let errno = Errno::from_raw(raw);
                        return Err(classify(errno, moved_any));
                    }
                    return Err(SpliceError::Io(e));
                }
            };

            // pipe → dst (must drain n_in before next read iteration,
            // otherwise the pipe fills and src→pipe blocks).
            drain_pipe_to(dst, pipe_read_fd, n_in, bytes_out)
                .await
                .map_err(SpliceError::Io)?;
        }
    }

    /// Bidirectional zero-copy forwarding between `downstream` and
    /// `upstream`. See contract in
    /// [`specs/012-tcp-zero-copy-splice/contracts/internal-api.md`](../../specs/012-tcp-zero-copy-splice/contracts/internal-api.md).
    ///
    /// **Pre-condition (callers must satisfy)**: `super::eligible(ctx)`
    /// returned `true`. Internally `debug_assert!`-checks this in dev
    /// builds.
    #[allow(clippy::needless_pass_by_ref_mut)]
    pub(crate) async fn copy_bidirectional(
        downstream: &mut TcpStream,
        upstream: &mut TcpStream,
        ctx: &CopyCtx,
    ) -> Result<Transferred, SpliceError> {
        debug_assert!(
            super::eligible(ctx),
            "splice::copy_bidirectional called when eligible() == false"
        );

        let rule_id = ctx.rule_id.0;
        let pipe_dn_to_up = PipePair::new(rule_id).map_err(SpliceError::Io)?;
        let pipe_up_to_dn = PipePair::new(rule_id).map_err(SpliceError::Io)?;

        // proxy.splice_selected event — emitted at the first successful
        // entry per rule. See contracts/internal-api.md § §3.
        super::emit_splice_selected(ctx, pipe_dn_to_up.capacity_bytes);

        let moved_any = AtomicBool::new(false);
        let bytes_in = AtomicU64::new(0);
        let bytes_out = AtomicU64::new(0);

        let result = tokio::try_join!(
            splice_dir(
                &*downstream,
                &*upstream,
                &pipe_dn_to_up,
                &moved_any,
                &bytes_in
            ),
            splice_dir(
                &*upstream,
                &*downstream,
                &pipe_up_to_dn,
                &moved_any,
                &bytes_out
            ),
        );

        match result {
            Ok(((), ())) => Ok(Transferred {
                bytes_in: bytes_in.load(Ordering::Relaxed),
                bytes_out: bytes_out.load(Ordering::Relaxed),
            }),
            Err(e) => Err(e),
        }
    }
}

#[cfg(target_os = "linux")]
#[allow(unused_imports)]
pub(crate) use linux::{PipePair, copy_bidirectional};

// ====================================================================
// Cross-platform tests
// ====================================================================

#[cfg(test)]
mod eligible_tests {
    //! Truth-table tests for [`eligible`] — pure logic, no I/O, run on
    //! every supported platform. Implementation tests of the splice
    //! syscall itself live in `mod integration` (Linux-only) and land
    //! with T007-T012.

    use super::*;

    fn base_ctx() -> CopyCtx {
        CopyCtx {
            rule_id: RuleId(1),
            protocol: Protocol::Tcp,
            has_bandwidth_cap: false,
            disable_splice: false,
            has_sni_replay_done: false,
            has_proxy_out: false,
        }
    }

    #[test]
    fn baseline_tcp_no_caps_is_eligible_on_linux_only() {
        let ctx = base_ctx();
        assert_eq!(eligible(&ctx), cfg!(target_os = "linux"));
    }

    #[test]
    fn udp_is_never_eligible() {
        let ctx = CopyCtx {
            protocol: Protocol::Udp,
            ..base_ctx()
        };
        assert!(!eligible(&ctx));
    }

    #[test]
    fn bandwidth_cap_forces_userspace() {
        let ctx = CopyCtx {
            has_bandwidth_cap: true,
            ..base_ctx()
        };
        assert!(!eligible(&ctx));
    }

    #[test]
    fn disable_splice_forces_userspace() {
        let ctx = CopyCtx {
            disable_splice: true,
            ..base_ctx()
        };
        assert!(!eligible(&ctx));
    }

    #[test]
    fn sni_replay_done_does_not_affect_eligibility() {
        let ctx = CopyCtx {
            has_sni_replay_done: true,
            ..base_ctx()
        };
        // The field is tracing-metadata only; presence does not gate.
        assert_eq!(eligible(&ctx), cfg!(target_os = "linux"));
    }

    #[test]
    fn proxy_out_does_not_affect_eligibility() {
        let ctx = CopyCtx {
            has_proxy_out: true,
            ..base_ctx()
        };
        // The field is tracing-metadata only; presence does not gate.
        assert_eq!(eligible(&ctx), cfg!(target_os = "linux"));
    }

    #[test]
    fn bandwidth_cap_dominates_other_fields() {
        let ctx = CopyCtx {
            has_bandwidth_cap: true,
            has_sni_replay_done: true,
            has_proxy_out: true,
            disable_splice: false,
            ..base_ctx()
        };
        assert!(!eligible(&ctx));
    }

    #[test]
    fn disable_splice_dominates_other_fields() {
        let ctx = CopyCtx {
            has_bandwidth_cap: false,
            has_sni_replay_done: true,
            has_proxy_out: true,
            disable_splice: true,
            ..base_ctx()
        };
        assert!(!eligible(&ctx));
    }
}

#[cfg(test)]
mod build_tests {
    //! Tests for [`CopyCtx::build`] — verifies the OR semantics of the
    //! rule + owner bandwidth-cap query against real handles. US3
    //! correctness gate (FR-001 / spec acceptance scenarios 1-3 of
    //! User Story 3).

    use std::sync::Arc;

    use portunus_core::RateLimit;

    use super::*;
    use crate::forwarder::rate_limit::scope::{
        OwnerId, OwnerRateLimitScopeManager, RateLimitScopeManager,
    };

    fn rule_handle_with(rl: Option<&RateLimit>) -> RuleRateLimitHandle {
        let mgr = Arc::new(RateLimitScopeManager::new());
        let rid = RuleId(42);
        mgr.install(rid, rl);
        RuleRateLimitHandle::new(rid, mgr)
    }

    fn owner_handle_with(rl: Option<&RateLimit>) -> OwnerRateLimitHandle {
        let mgr = Arc::new(OwnerRateLimitScopeManager::new());
        let oid = OwnerId::new("alice");
        mgr.install(&oid, rl);
        OwnerRateLimitHandle::new(oid, mgr)
    }

    /// T025 — rule with `bandwidth_in_bps` forces userspace.
    #[test]
    fn rule_bandwidth_in_forces_userspace() {
        let rl = RateLimit {
            bandwidth_in_bps: Some(1_000_000),
            ..Default::default()
        };
        let handle = rule_handle_with(Some(&rl));
        let ctx = CopyCtx::build(RuleId(1), Protocol::Tcp, Some(&handle), None, false, false);
        assert!(ctx.has_bandwidth_cap);
        assert!(!eligible(&ctx));
    }

    /// T025 pair — same rule with the bandwidth cap removed becomes
    /// eligible again (on Linux).
    #[test]
    fn rule_without_bandwidth_cap_is_eligible_on_linux_only() {
        let handle = rule_handle_with(None);
        let ctx = CopyCtx::build(RuleId(1), Protocol::Tcp, Some(&handle), None, false, false);
        assert!(!ctx.has_bandwidth_cap);
        // Env-aware: under `PORTUNUS_DISABLE_SPLICE=1` (T029 CI matrix
        // axis) `disable_splice` is true and `eligible` is false.
        assert_eq!(
            eligible(&ctx),
            cfg!(target_os = "linux") && !ctx.disable_splice
        );
    }

    /// T026 — rule with only `concurrent_connections` does NOT force
    /// userspace. Concurrent caps gate at accept time (v0.11) and never
    /// touch the data path.
    #[test]
    fn rule_with_concurrent_only_does_not_force_userspace() {
        let rl = RateLimit {
            concurrent_connections: Some(100),
            ..Default::default()
        };
        let handle = rule_handle_with(Some(&rl));
        let ctx = CopyCtx::build(RuleId(1), Protocol::Tcp, Some(&handle), None, false, false);
        assert!(
            !ctx.has_bandwidth_cap,
            "concurrent_connections alone must not set has_bandwidth_cap"
        );
        // Env-aware: under `PORTUNUS_DISABLE_SPLICE=1` (T029) `eligible`
        // is false even when `has_bandwidth_cap` is false.
        assert_eq!(
            eligible(&ctx),
            cfg!(target_os = "linux") && !ctx.disable_splice
        );
    }

    /// T026 pair — rule with only `new_connections_per_sec` does NOT
    /// force userspace either.
    #[test]
    fn rule_with_new_conn_rate_only_does_not_force_userspace() {
        let rl = RateLimit {
            new_connections_per_sec: Some(50),
            ..Default::default()
        };
        let handle = rule_handle_with(Some(&rl));
        let ctx = CopyCtx::build(RuleId(1), Protocol::Tcp, Some(&handle), None, false, false);
        assert!(!ctx.has_bandwidth_cap);
        // Env-aware: under `PORTUNUS_DISABLE_SPLICE=1` (T029 CI matrix
        // axis) `disable_splice` is true and `eligible` is false.
        assert_eq!(
            eligible(&ctx),
            cfg!(target_os = "linux") && !ctx.disable_splice
        );
    }

    /// T027 — owner bandwidth cap forces userspace even when the rule
    /// has no per-rule cap. Multi-tenant isolation invariant.
    #[test]
    fn owner_bandwidth_cap_forces_userspace() {
        let owner_rl = RateLimit {
            bandwidth_in_bps: Some(5_000_000),
            ..Default::default()
        };
        let owner = owner_handle_with(Some(&owner_rl));
        // No rule-level cap.
        let rule = rule_handle_with(None);
        let ctx = CopyCtx::build(
            RuleId(1),
            Protocol::Tcp,
            Some(&rule),
            Some(&owner),
            false,
            false,
        );
        assert!(ctx.has_bandwidth_cap);
        assert!(!eligible(&ctx));
    }

    /// T027 pair — owner with only `concurrent_connections` does not
    /// force userspace either.
    #[test]
    fn owner_concurrent_only_does_not_force_userspace() {
        let owner_rl = RateLimit {
            concurrent_connections: Some(1000),
            ..Default::default()
        };
        let owner = owner_handle_with(Some(&owner_rl));
        let ctx = CopyCtx::build(RuleId(1), Protocol::Tcp, None, Some(&owner), false, false);
        assert!(!ctx.has_bandwidth_cap);
        // Env-aware: under `PORTUNUS_DISABLE_SPLICE=1` (T029 CI matrix
        // axis) `disable_splice` is true and `eligible` is false.
        assert_eq!(
            eligible(&ctx),
            cfg!(target_os = "linux") && !ctx.disable_splice
        );
    }

    /// Either side (rule OR owner) sets the flag.
    #[test]
    fn rule_or_owner_bandwidth_cap_dominates() {
        // Rule has bw, owner does not → has_bandwidth_cap.
        let rule_rl = RateLimit {
            bandwidth_out_bps: Some(2_000_000),
            ..Default::default()
        };
        let rule = rule_handle_with(Some(&rule_rl));
        let owner = owner_handle_with(None);
        let ctx = CopyCtx::build(
            RuleId(1),
            Protocol::Tcp,
            Some(&rule),
            Some(&owner),
            false,
            false,
        );
        assert!(ctx.has_bandwidth_cap);
        assert!(!eligible(&ctx));
    }

    /// Neither rule nor owner sets a cap → eligible on Linux.
    #[test]
    fn no_caps_anywhere_is_eligible_on_linux_only() {
        let rule = rule_handle_with(None);
        let owner = owner_handle_with(None);
        let ctx = CopyCtx::build(
            RuleId(1),
            Protocol::Tcp,
            Some(&rule),
            Some(&owner),
            false,
            false,
        );
        assert!(!ctx.has_bandwidth_cap);
        // Env-aware: under `PORTUNUS_DISABLE_SPLICE=1` (T029 CI matrix
        // axis) `disable_splice` is true and `eligible` is false.
        assert_eq!(
            eligible(&ctx),
            cfg!(target_os = "linux") && !ctx.disable_splice
        );
    }

    /// `None` handles (the common steady-state for rules with no
    /// rate-limit envelope at all) yield `has_bandwidth_cap: false`.
    #[test]
    fn no_handles_at_all_is_eligible_on_linux_only() {
        let ctx = CopyCtx::build(RuleId(1), Protocol::Tcp, None, None, false, false);
        assert!(!ctx.has_bandwidth_cap);
        // Env-aware: under `PORTUNUS_DISABLE_SPLICE=1` (T029 CI matrix
        // axis) `disable_splice` is true and `eligible` is false.
        assert_eq!(
            eligible(&ctx),
            cfg!(target_os = "linux") && !ctx.disable_splice
        );
    }
}

// ====================================================================
// Linux integration tests (T007, T008, T009, T012)
// ====================================================================

#[cfg(all(test, target_os = "linux"))]
mod integration {
    //! End-to-end exercises of [`linux::copy_bidirectional`] over real
    //! loopback `TcpStream`s. Each test:
    //!
    //! 1. Spawns a "upstream" task bound to `127.0.0.1:0` that
    //!    implements the role-specific behaviour (echo, half-close, etc.).
    //! 2. Spawns a "downstream-driver" task that connects to a separate
    //!    `127.0.0.1:0` listener, the accepted side of which is the
    //!    "downstream" socket the proxy reads from.
    //! 3. The test body itself connects to the upstream listener to
    //!    obtain the "upstream" socket the proxy writes to, then calls
    //!    `linux::copy_bidirectional(&mut downstream, &mut upstream, &ctx)`.

    use std::time::Duration;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use super::linux::copy_bidirectional;
    use super::*;

    fn ctx_eligible() -> CopyCtx {
        CopyCtx {
            rule_id: RuleId(7777),
            protocol: Protocol::Tcp,
            has_bandwidth_cap: false,
            disable_splice: false,
            has_sni_replay_done: false,
            has_proxy_out: false,
        }
    }

    /// Build a loopback `(downstream_proxy_side, downstream_client_side)`
    /// pair: returns the socket the proxy reads from and the socket the
    /// test driver writes to.
    async fn loopback_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let connect_fut = TcpStream::connect(addr);
        let (proxy_side_res, client_side) = tokio::join!(listener.accept(), connect_fut);
        let (proxy_side, _) = proxy_side_res.unwrap();
        let client_side = client_side.unwrap();
        // Disable Nagle to make small writes immediately visible.
        let _ = proxy_side.set_nodelay(true);
        let _ = client_side.set_nodelay(true);
        (proxy_side, client_side)
    }

    /// T007 — 1 MiB round-trips byte-identically through the splice
    /// path. Validates the basic forwarding contract: every byte that
    /// enters one end leaves the other end.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn t007_bidirectional_echo_1mib_round_trips_byte_identical() {
        let (downstream_proxy, mut downstream_client) = loopback_pair().await;
        let (upstream_proxy, upstream_server) = loopback_pair().await;

        // Upstream: echo every byte back.
        let echo_task = tokio::spawn(async move {
            let mut s = upstream_server;
            let mut buf = vec![0u8; 64 * 1024];
            let mut total = 0u64;
            loop {
                let n = s.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                s.write_all(&buf[..n]).await.unwrap();
                total += n as u64;
            }
            // After downstream EOF the splice path half-closes our write
            // side, but we may still want to record total bytes for sanity.
            total
        });

        let payload: Vec<u8> = (0..1024 * 1024).map(|i| (i % 251) as u8).collect();
        let expected = payload.clone();
        let writer_task = tokio::spawn(async move {
            downstream_client.write_all(&payload).await.unwrap();
            // Half-close downstream → upstream so the splice loop on
            // that direction terminates cleanly.
            downstream_client.shutdown().await.unwrap();
            // Read echoed bytes back.
            let mut got = Vec::with_capacity(expected.len());
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let n = downstream_client.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&buf[..n]);
            }
            (got, expected)
        });

        let mut downstream_proxy = downstream_proxy;
        let mut upstream_proxy = upstream_proxy;
        let ctx = ctx_eligible();
        let result = copy_bidirectional(&mut downstream_proxy, &mut upstream_proxy, &ctx).await;

        let transferred = result.expect("splice copy_bidirectional should succeed");
        assert_eq!(
            transferred.bytes_in,
            1024 * 1024,
            "exactly 1 MiB delivered downstream → upstream"
        );

        let (got, expected) = writer_task.await.unwrap();
        assert_eq!(got.len(), expected.len(), "echoed length matches input");
        assert_eq!(got, expected, "echoed bytes are identical");
        assert_eq!(transferred.bytes_out as usize, got.len());

        let _ = echo_task.await;
    }

    /// T008 — Upstream EOF triggers a half-close on downstream. After
    /// the upstream side `shutdown(Write)`s, the downstream client must
    /// observe `read == 0` while still being able to send bytes in the
    /// reverse direction until *it* EOFs.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn t008_upstream_eof_triggers_half_close_downstream() {
        let (downstream_proxy, mut downstream_client) = loopback_pair().await;
        let (upstream_proxy, upstream_server) = loopback_pair().await;

        let upstream_task = tokio::spawn(async move {
            let mut s = upstream_server;
            // Send 4 KiB then half-close write side.
            let outbound = vec![0xABu8; 4096];
            s.write_all(&outbound).await.unwrap();
            s.shutdown().await.unwrap();
            // Continue reading from downstream → us until downstream
            // half-closes (our partner direction).
            let mut buf = vec![0u8; 4096];
            let mut received = Vec::new();
            loop {
                let n = s.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                received.extend_from_slice(&buf[..n]);
            }
            received
        });

        let writer_task = tokio::spawn(async move {
            // Read 4 KiB then expect EOF (read == 0).
            let mut got = vec![0u8; 4096];
            downstream_client.read_exact(&mut got).await.unwrap();
            // Confirm EOF arrives after the 4 KiB.
            let mut tail = [0u8; 16];
            let n = downstream_client.read(&mut tail).await.unwrap();
            assert_eq!(n, 0, "downstream should see EOF after upstream half-close");

            // Now send bytes in the reverse direction.
            let reverse = vec![0xCDu8; 2048];
            downstream_client.write_all(&reverse).await.unwrap();
            downstream_client.shutdown().await.unwrap();
            got
        });

        let mut downstream_proxy = downstream_proxy;
        let mut upstream_proxy = upstream_proxy;
        let transferred =
            copy_bidirectional(&mut downstream_proxy, &mut upstream_proxy, &ctx_eligible())
                .await
                .expect("splice should succeed under half-close");
        assert_eq!(transferred.bytes_out, 4096);
        assert_eq!(transferred.bytes_in, 2048);

        let got = writer_task.await.unwrap();
        assert!(
            got.iter().all(|b| *b == 0xAB),
            "downstream got upstream's bytes verbatim"
        );
        let upstream_received = upstream_task.await.unwrap();
        assert_eq!(upstream_received.len(), 2048);
        assert!(upstream_received.iter().all(|b| *b == 0xCD));
    }

    /// T009 — `F_SETPIPE_SZ` failure (or just success below the target)
    /// is best-effort. The connection still completes. We don't try to
    /// force a `EPERM` here (would require a sysctl write); instead we
    /// simply assert that with the default `pipe-max-size` on the host
    /// (typically 1 MiB on modern distros) `copy_bidirectional` succeeds.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn t009_pipe_size_request_best_effort_does_not_fail_connection() {
        let (downstream_proxy, mut downstream_client) = loopback_pair().await;
        let (upstream_proxy, mut upstream_server) = loopback_pair().await;

        let echo = tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            loop {
                let n = upstream_server.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                upstream_server.write_all(&buf[..n]).await.unwrap();
            }
        });

        let payload = vec![0x42u8; 64 * 1024];
        let expected_len = payload.len();
        let writer = tokio::spawn(async move {
            downstream_client.write_all(&payload).await.unwrap();
            downstream_client.shutdown().await.unwrap();
            let mut got = Vec::with_capacity(expected_len);
            let mut buf = vec![0u8; 4096];
            loop {
                let n = downstream_client.read(&mut buf).await.unwrap();
                if n == 0 {
                    break;
                }
                got.extend_from_slice(&buf[..n]);
            }
            got
        });

        let mut downstream_proxy = downstream_proxy;
        let mut upstream_proxy = upstream_proxy;
        let res = tokio::time::timeout(
            Duration::from_secs(5),
            copy_bidirectional(&mut downstream_proxy, &mut upstream_proxy, &ctx_eligible()),
        )
        .await
        .expect("did not deadlock")
        .expect("splice succeeded");

        assert_eq!(res.bytes_in, 64 * 1024);
        let got = writer.await.unwrap();
        assert_eq!(got.len(), 64 * 1024);
        let _ = echo.await;
    }

    /// T012 — Byte counters match what userspace `copy_bidirectional`
    /// would report for the same payload. Splice's `Transferred` is
    /// shape-equivalent to the tokio `(u64, u64)` return.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn t012_byte_counters_match_userspace_path() {
        // Pass A — splice path
        let splice_counts = {
            let (downstream_proxy, mut downstream_client) = loopback_pair().await;
            let (upstream_proxy, mut upstream_server) = loopback_pair().await;

            let echo = tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let mut total = 0u64;
                loop {
                    let n = upstream_server.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    upstream_server.write_all(&buf[..n]).await.unwrap();
                    total += n as u64;
                }
                total
            });

            let payload = vec![0x55u8; 1024 * 1024];
            let writer = tokio::spawn(async move {
                downstream_client.write_all(&payload).await.unwrap();
                downstream_client.shutdown().await.unwrap();
                let mut got = Vec::with_capacity(payload.len());
                let mut buf = vec![0u8; 8192];
                loop {
                    let n = downstream_client.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    got.extend_from_slice(&buf[..n]);
                }
                got.len() as u64
            });

            let mut downstream_proxy = downstream_proxy;
            let mut upstream_proxy = upstream_proxy;
            let t = copy_bidirectional(&mut downstream_proxy, &mut upstream_proxy, &ctx_eligible())
                .await
                .unwrap();
            let _echo_total = echo.await.unwrap();
            let _writer_got = writer.await.unwrap();
            (t.bytes_in, t.bytes_out)
        };

        // Pass B — userspace path
        let userspace_counts = {
            let (mut downstream_proxy, mut downstream_client) = loopback_pair().await;
            let (mut upstream_proxy, mut upstream_server) = loopback_pair().await;

            let echo = tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                loop {
                    let n = upstream_server.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    upstream_server.write_all(&buf[..n]).await.unwrap();
                }
            });

            let payload = vec![0x55u8; 1024 * 1024];
            let writer = tokio::spawn(async move {
                downstream_client.write_all(&payload).await.unwrap();
                downstream_client.shutdown().await.unwrap();
                let mut buf = vec![0u8; 8192];
                let mut got = 0u64;
                loop {
                    let n = downstream_client.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    got += n as u64;
                }
                got
            });

            let (bytes_in, bytes_out) = tokio::io::copy_bidirectional_with_sizes(
                &mut downstream_proxy,
                &mut upstream_proxy,
                64 * 1024,
                64 * 1024,
            )
            .await
            .unwrap();
            let _ = echo.await;
            let _ = writer.await;
            (bytes_in, bytes_out)
        };

        assert_eq!(
            splice_counts, userspace_counts,
            "splice (bytes_in, bytes_out) must match userspace (bytes_in, bytes_out) bit-for-bit"
        );
    }
}

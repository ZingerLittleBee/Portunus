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

/// Per-connection context the splice path consults to decide eligibility.
///
/// Built once at connection-acceptance time in `proxy.rs` and passed by
/// reference into [`eligible`]. Small POD; `Copy` so callers do not need
/// to clone.
//
// `dead_code` is silenced until the proxy.rs call site lands in T017.
// Every field is read once T017 wires `CopyCtx::build(...)` and the
// eligibility branch.
//
// `struct_excessive_bools` is allowed here because these flags are the
// natural encoding of the eligibility predicate's gates (FR-001..FR-007);
// collapsing them into a state-machine enum would obscure the per-gate
// test matrix in `eligible_tests`.
#[allow(dead_code, clippy::struct_excessive_bools)]
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
    matches!(ctx.protocol, Protocol::Tcp)
        && !ctx.disable_splice
        && !ctx.has_bandwidth_cap
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
// `dead_code` is silenced until the proxy.rs call site lands in T017.
#[allow(dead_code)]
pub(crate) fn disable_splice_env() -> bool {
    static CACHED: OnceLock<bool> = OnceLock::new();
    *CACHED.get_or_init(|| {
        std::env::var_os("PORTUNUS_DISABLE_SPLICE")
            .is_some_and(|v| !v.is_empty())
    })
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

// ====================================================================
// Linux-only implementation
// ====================================================================

#[cfg(target_os = "linux")]
mod linux {
    use std::os::fd::OwnedFd;

    use super::{CopyCtx, SpliceError, Transferred};

    /// RAII wrapper around a `pipe2(O_NONBLOCK | O_CLOEXEC)` pair.
    ///
    /// Created once per connection direction (`splice` is half-duplex per
    /// syscall, so a bidirectional `copy_bidirectional` allocates two
    /// `PipePair`s and runs both directions concurrently via `try_join!`).
    /// [`F_SETPIPE_SZ`] is applied as a best-effort upgrade to 1 MiB; on
    /// failure the pipe keeps the kernel default and a single
    /// `proxy.splice_pipe_size_failed` `debug`-level event is emitted.
    ///
    /// Drop closes both fds (free for [`OwnedFd`]).
    #[allow(dead_code)]
    pub(super) struct PipePair {
        /// Read end of the pipe.
        pub(super) read_fd: OwnedFd,
        /// Write end of the pipe.
        pub(super) write_fd: OwnedFd,
        /// Actual pipe capacity in bytes after the best-effort
        /// `F_SETPIPE_SZ` attempt. Used as the `len` argument to
        /// subsequent `splice` calls.
        pub(super) capacity_bytes: usize,
    }

    /// Bidirectional zero-copy forwarding between `downstream` and
    /// `upstream`. See contract in
    /// `specs/012-tcp-zero-copy-splice/contracts/internal-api.md § §1`.
    ///
    /// **Pre-condition (callers must satisfy)**: `super::eligible(ctx)`
    /// returned `true`. Internally `debug_assert!`-checks this in dev
    /// builds.
    #[allow(dead_code, clippy::needless_pass_by_ref_mut)]
    pub(super) async fn copy_bidirectional(
        downstream: &mut tokio::net::TcpStream,
        upstream: &mut tokio::net::TcpStream,
        ctx: &CopyCtx,
    ) -> Result<Transferred, SpliceError> {
        debug_assert!(
            super::eligible(ctx),
            "splice::copy_bidirectional called when eligible() == false"
        );
        // Body lands in T013-T019 per tasks.md. The signature is the
        // contract; this stub keeps the cross-platform build green
        // until the splice loop, pipe-pair allocation, and tracing
        // wiring land in their respective tasks.
        let _ = (downstream, upstream, ctx);
        Err(SpliceError::Io(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "splice::copy_bidirectional not yet implemented (T013-T019)",
        )))
    }
}

#[cfg(target_os = "linux")]
#[allow(unused_imports)]
pub(crate) use linux::{copy_bidirectional, PipePair};

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

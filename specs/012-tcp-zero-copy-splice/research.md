# Research: TCP Zero-Copy Fast Path

**Phase**: 0
**Status**: Complete — all R-NNN decisions locked.
**Input**: [spec.md](./spec.md), [plan.md](./plan.md)

Each entry: **Decision** / **Rationale** / **Alternatives considered**.

---

## R-001: `splice(2)` over `sendfile(2)` / `MSG_ZEROCOPY` / `tee(2)`

**Decision**: Use `splice(2)` between socket and an intermediate kernel pipe
for bidirectional TCP forwarding.

**Rationale**:
- `splice` is the only Linux primitive that moves bytes between **two
  sockets** with zero userspace copy, via a pipe-pair intermediate buffer.
- `sendfile` requires the source to be a file (or a socket with the
  same-fd workaround that does not avoid the copy). It is the canonical
  static-file-serving primitive, not the canonical proxy primitive.
- `MSG_ZEROCOPY` (Linux 4.14+) is a **send-side** optimization that
  retains user-space memory until the kernel notifies completion via
  `MSG_ERRQUEUE`. It eliminates send-side copies but leaves receive-side
  copies in place, and demands complex retention bookkeeping. Net gain
  for a proxy is much smaller than splice's "no copy in either direction".
- `tee(2)` duplicates a pipe to another pipe without consuming; not what
  we need (we need to consume).

**Alternatives considered**:
- `io_uring` with `IORING_OP_SPLICE` — would let us batch operations and
  eliminate per-op syscall overhead, but requires a Tokio replacement
  (`tokio-uring`) at the runtime layer. Out of scope per spec; revisit if
  splice alone doesn't hit SC-001 with sufficient headroom.
- AF_XDP / DPDK — kernel bypass; would require root/CAP_BPF, conflicts
  with `unsafe_code = "forbid"` workspace lint, and overshoots the
  workload's needs.

---

## R-002: Per-connection pipe pair (no pool)

**Decision**: Each accepted TCP connection allocates its own `pipe2(2)`
pair at the start of `splice::copy_bidirectional`, closed via RAII when
the function returns.

**Rationale**:
- Pool ownership across async boundaries adds complexity (locking,
  sizing, draining) for unclear gain. The connection's lifetime is the
  pipe's natural lifetime.
- `pipe2` is cheap on Linux (~µs).
- Even at 100k concurrent connections, the fd cost (2 fds × 100k =
  200k fds) is modest and operators tuning for that scale already raise
  `RLIMIT_NOFILE`.
- A bug in pool draining could leak data between connections — a
  **tenant-isolation footgun**. Per-connection eliminates that class.

**Alternatives considered**:
- LIFO thread-local pipe pool — saves the `pipe2` syscall (~µs) per
  connection setup. Spec's workload is low-conn-rate / high-throughput,
  so amortized setup cost is negligible relative to per-byte savings.
  Rejected: complexity vs. benefit unfavorable.
- Single shared global pool — multi-tenancy footgun; rejected.

---

## R-003: 1 MiB pipe size target, best-effort `F_SETPIPE_SZ`

**Decision**: Request `F_SETPIPE_SZ` = 1 MiB (`1024 * 1024`) on each
pipe after creation. Failure is logged at `debug` level and ignored
(the pipe continues at the kernel default).

**Rationale**:
- Default Linux pipe size is 16 pages = 64 KiB. With 1 MiB chunks
  (SC-001's bench shape), a 64-KiB pipe forces ~16 syscalls per chunk on
  the producing side and the same on the consuming side, restoring most
  of the userspace path's syscall cost.
- 1 MiB allows a typical bench chunk to fit in one splice-in / one
  splice-out roundtrip.
- The kernel cap is `/proc/sys/fs/pipe-max-size` (default 1 MiB on
  modern kernels, lower on hardened ones). Requesting > cap returns
  EPERM. Operators on hardened kernels still get a working forwarder
  with smaller pipes; only peak throughput is reduced.

**Alternatives considered**:
- Request 8 MiB — likely to fail on default kernels; gain over 1 MiB
  on 1 MiB-chunk workloads is marginal. Rejected.
- Use default — kills SC-001 on the bench. Rejected.
- Require operator to raise `pipe-max-size` — surfaces the optimization
  as operator-visible, violating FR-003. Rejected.

---

## R-004: `TcpStream::try_io` over `tokio::io::unix::AsyncFd`

**Decision**: Use the existing `tokio::net::TcpStream::try_io(Interest,
closure)` + `TcpStream::readable() / writable()` pattern. Do not
introduce `AsyncFd` for either socket.

**Rationale**:
- The socket is already a `TcpStream`. Wrapping its fd in `AsyncFd`
  creates a **second registration** in the Tokio reactor for the same
  fd, which Tokio's documentation cautions against (registration is
  intended to be unique per fd).
- `try_io` was added specifically for the "call a syscall on an
  already-Tokio-managed fd" pattern. It handles `WouldBlock` by clearing
  Tokio's cached readiness so the next `readable()` / `writable()` await
  re-arms correctly. This is exactly what `splice` needs.
- Avoiding `AsyncFd` keeps the splice path free of the ownership
  complications around `AsyncFd::into_inner()` and `into_std()` (per
  reviewer feedback during brainstorming).

**Alternatives considered**:
- `AsyncFd<RawFd>` — works but adds the second-registration risk.
  Rejected on guidance from review.
- Manual `mio` registration — wheel-reinvention; rejected.

---

## R-005: `splice` batch length = pipe capacity

**Decision**: Each `splice(src_sock → pipe_write, …, len, flags)` call
requests `len = pipe_capacity_bytes` (1 MiB target, fallback to actual
kernel size). The reverse direction `splice(pipe_read → dst_sock, …,
n_in, flags)` uses the bytes-in-pipe count returned by the previous
call.

**Rationale**:
- A larger `len` does not force the kernel to move that much; it caps
  the operation. The kernel moves whatever is available up to `len`.
- Asking for the pipe's full capacity maximises the chance of doing
  one syscall per direction per chunk.
- Flags: `SPLICE_F_NONBLOCK | SPLICE_F_MOVE`. `SPLICE_F_MOVE` is the
  hint that the kernel may move (rather than copy) pages — even though
  kernel implementation has since changed and often ignores this hint,
  it's the documented zero-copy intent and costs nothing to set.

**Alternatives considered**:
- Fixed 64 KiB batches (mirroring `PROXY_COPY_BUF_SIZE`) — defeats the
  point of enlarging the pipe; rejected.
- Auto-tune by RTT — adds complexity; rejected for first version.

---

## R-006: Fallback errno set is closed and minimal

**Decision**: `splice` returns translated to `SpliceError::Unsupported`
**only** on `ENOSYS`, `EINVAL`, `EPERM`, `EOPNOTSUPP` / `ENOTSUP`, and
**only** when zero bytes have moved on either direction. All other
errno values (`EPIPE`, `ECONNRESET`, `EBADF`, `ENOSPC`, `EFAULT`, …)
propagate as `SpliceError::Io(io::Error)`. `EAGAIN` / `EWOULDBLOCK` are
handled by the readiness loop (`continue`). `EINTR` is treated as a
retry (`continue`).

**Rationale**:
- The four "unsupported" errnos are the documented signals that the
  kernel / sandbox / file descriptor type rejects splice as an
  operation, not that the operation transiently failed.
- `EOPNOTSUPP` and `ENOTSUP` are required by POSIX to compare equal on
  Linux but `nix` may surface them as distinct constants depending on
  version — we match both.
- After any byte has moved, a sudden `EINVAL` indicates a kernel-level
  state change that we can't recover from without risk of double-counting
  or losing bytes — so we propagate as `Io`, not `Unsupported`.

**Alternatives considered**:
- "Fall back on any error" — corrupts byte counters and risks duplicate
  delivery; rejected.
- "Never fall back" — fails the connection on a host where seccomp
  bans splice, which would be a regression vs. v1.2.0 on the same host;
  rejected.

---

## R-007: Half-close semantics mirror `copy_bidirectional`

**Decision**: When `splice(src → pipe)` returns 0 (peer closed write
half), the implementation drains any remaining pipe content to the
destination, then calls `dst.shutdown(Write).await`. The reverse
direction continues running until it also EOFs or errors. The function
returns `Ok` only when both directions have completed cleanly.

**Rationale**:
- `tokio::io::copy_bidirectional` implements exactly this semantic.
  Any deviation would break protocols (e.g., HTTP/1.1 half-close,
  database close-with-result) that depend on it.
- The two-direction futures run concurrently inside `tokio::try_join!`.

**Alternatives considered**:
- Abort both directions on first EOF — breaks protocols; rejected.
- Half-close on `Ok(0)` without draining the pipe first — drops the
  last batch of in-flight bytes; rejected.

---

## R-008: Byte counter advances on `splice(pipe → dst)` return value

**Decision**: `bytes_in` / `bytes_out` counters (per-rule, fed to
Prometheus and per-rule stats) advance based on the return value of the
**pipe-to-destination** `splice` call, never the source-to-pipe call.

**Rationale**:
- This mirrors `tokio::io::copy_bidirectional_with_sizes`'s contract:
  bytes counted are bytes **delivered** to the destination, not bytes
  **received** from the source. The two values differ during connection
  reset (in-flight bytes never reach the destination).
- Maintains metric continuity (FR-008, SC-004) — operators do not see
  any change in counter semantics when the optimization is enabled.

**Alternatives considered**:
- Count on source-side splice return — overstates delivery during RST;
  rejected.
- Count both and emit a new metric for "in-flight-lost" — adds
  operator-visible surface; rejected per FR-003.

---

## R-009: Bench replicates `data_plane.rs` shape; no `[lib]` target needed

**Decision**: New criterion bench at
`crates/portunus-client/benches/splice_throughput.rs` re-implements the
relevant splice path **inline in the bench file**, mirroring how
`data_plane.rs` re-implements the proxy primitive inline.

**Rationale**:
- `portunus-client` is binary-only by long-standing convention (since
  v0.1.0); benches cannot `use portunus_client::...` because there is no
  library target.
- This is the same constraint that drove T014's deferral in v0.11.
  Solution: copy the shape into the bench, as `data_plane.rs`,
  `sni_route.rs`, `udp_data_plane.rs` already do.
- A "make splice.rs `pub(crate)` and expose via a test-only `[lib]`
  target" workaround would be a workspace-wide convention change.
  Out of scope.

**Alternatives considered**:
- Add `[lib]` target — convention change deferred to a future spec;
  rejected.
- Drop the bench and rely only on integration tests — fails SC-001/SC-002,
  which require quantitative comparison; rejected.

---

## R-010: Tracing event naming under existing `proxy.*` namespace

**Decision**: Three new tracing events:

| Event | Level | Fields |
|---|---|---|
| `proxy.splice_selected` | `info` | `rule_id`, `protocol`, `has_sni`, `has_proxy_out`, `pipe_size_bytes` |
| `proxy.splice_unsupported_fallback` | `warn` | `rule_id`, `errno_name` (string), `errno_value` (i32) |
| `proxy.splice_pipe_size_failed` | `debug` | `rule_id`, `requested_bytes`, `actual_default_bytes`, `errno_name` |

**Rationale**:
- Existing v0.x events use the `proxy.*` prefix
  (e.g., `proxy.completed`, `proxy.connection_error`). New events
  follow the same convention.
- `warn` for unsupported fallback (operators should be alerted once if
  their kernel / sandbox rejects splice, so they can investigate or set
  `PORTUNUS_DISABLE_SPLICE=1` permanently).
- `debug` for pipe-size best-effort failure (informational; doesn't
  affect correctness).
- `info` for selection on the *first* connection per rule (`splice.rs`
  caches an `AtomicBool` per `Rule` to prevent log floods). The cache
  is reset on rule re-push so a re-enabled rule re-logs once.

**Alternatives considered**:
- New `splice.*` namespace — fragments the operator's mental model
  ("why is there a different prefix for the same data-plane events?");
  rejected.
- Emit per-connection `info` — log flood on a busy proxy; rejected.

---

## Open follow-ups (deliberately not blocking v0.12)

These were surfaced during research but are out of scope for this
implementation:

- **F-001**: Investigate whether `MSG_ZEROCOPY` on the send side can
  complement splice on the receive side for very small destinations
  (where pipe overhead dominates). Speculative.
- **F-002**: Per-CPU pipe-pool with `tokio::task_local` for very
  high-conn-rate workloads. Not relevant to spec's workload but worth
  a benchmark before the next perf-touching release.
- **F-003**: When `[lib]` target is added (separately), revisit T014
  for v0.11 and consider the same for splice.

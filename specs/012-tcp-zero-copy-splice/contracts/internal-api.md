# Internal API Contract: `forwarder::splice`

**Phase**: 1
**Status**: Locked
**Audience**: contributors implementing or modifying the splice module
**Input**: [spec.md](../spec.md), [plan.md](../plan.md), [data-model.md](../data-model.md), [research.md](../research.md)

This feature exposes **no external surface** (no wire field, no
operator-API endpoint, no Web UI route, no CLI flag). The "contracts"
here are the **internal seams** between `forwarder::proxy` and
`forwarder::splice`, plus the observable side-effects (tracing events,
existing-metric continuity, env var).

---

## §1. Function signatures (`forwarder::splice`)

These signatures are the contract: changing them requires updating
this document.

```rust
// Available on all platforms; on non-Linux it is a const-fn returning false.
pub(crate) fn eligible(ctx: &CopyCtx) -> bool;

#[cfg(target_os = "linux")]
pub(crate) async fn copy_bidirectional(
    downstream: &mut tokio::net::TcpStream,
    upstream:   &mut tokio::net::TcpStream,
    ctx:        &CopyCtx,
) -> Result<Transferred, SpliceError>;
```

Types (`CopyCtx`, `Transferred`, `SpliceError`) defined in
[data-model.md § Internal Types](../data-model.md#internal-types-all-in-cratesportunus-clientsrcforwardersplicers).

**Contract guarantees**:

| Guarantee | Enforced by |
|---|---|
| `eligible` performs no I/O, no syscall, no allocation. | Unit test calls it 10⁶ times in a tight loop, asserts no allocator activity (heaptrack / counters). |
| `copy_bidirectional` never panics on `Unsupported`. It returns `Err(SpliceError::Unsupported)`. | Integration test injecting `ENOSYS`. |
| `copy_bidirectional` returns `Err(SpliceError::Unsupported)` **only** when zero bytes moved on either direction. | Integration test asserts on `moved_any` flag via injected errno after first successful splice. |
| `copy_bidirectional` updates the per-connection byte counters by the same value that `tokio::io::copy_bidirectional_with_sizes` would for the same byte stream. | Loopback echo test: 1 GiB through with splice, 1 GiB through without; counter values bit-identical. |
| `copy_bidirectional` half-closes per `tokio::io::copy_bidirectional`. | Loopback test: producer EOFs after sending N bytes, verify consumer sees `read = 0` after exactly N bytes. |
| `eligible` returns `false` on non-Linux at compile time (const-fn returning literal `false`). | `cargo check --target x86_64-apple-darwin` confirms the splice module body is not compiled. |

---

## §2. Call-site contract (`forwarder::proxy`)

The **only** call site for `splice::copy_bidirectional` is in
`forwarder/proxy.rs`, immediately before the existing
`tokio::io::copy_bidirectional_with_sizes` call.

**Required structure** (pseudo-code; see plan.md § Project Structure):

```rust
let (bytes_in, bytes_out) = {
    #[cfg(target_os = "linux")]
    {
        if splice::eligible(&ctx) {
            match splice::copy_bidirectional(&mut downstream, &mut upstream, &ctx).await {
                Ok(t)                              => Some((t.bytes_in, t.bytes_out)),
                Err(SpliceError::Unsupported { .. }) => None, // fall through
                Err(SpliceError::Io(e))            => return Err(e.into()),
            }
        } else {
            None
        }
    }
    #[cfg(not(target_os = "linux"))]
    { None }
}.unwrap_or_else(|| { /* userspace path runs */ })
// (real code uses a clearer non-let control flow; this is illustrative.)
```

**Call-site invariants**:

- `splice::copy_bidirectional` is **never** called when `eligible`
  returned `false`. Defensive `debug_assert!` inside the function checks
  this in dev builds.
- `splice::copy_bidirectional` is **never** retried after returning
  `Err(SpliceError::Io)`. The connection terminates with that error.
- After `splice::copy_bidirectional` returns `Err(SpliceError::Unsupported)`,
  the **same** sockets and the **same** rule context are used to run the
  userspace path. The caller MUST NOT close or reopen the sockets between
  the two attempts.

---

## §3. Tracing events (operator-observable)

Three new structured events in the `proxy.*` namespace, emitted by the
splice module:

| Event name | Level | Cardinality | Fields |
|---|---|---|---|
| `proxy.splice_selected` | `info` | **Once per (rule_id, runtime-uptime)** — gated by an `AtomicBool` per rule, reset on rule re-push. | `rule_id: u64`, `protocol: &str` ("tcp"), `has_sni: bool`, `has_proxy_out: bool`, `pipe_size_bytes: usize` |
| `proxy.splice_unsupported_fallback` | `warn` | **Per connection** that takes the fallback. | `rule_id: u64`, `errno_name: &str` (e.g. "ENOSYS"), `errno_value: i32` |
| `proxy.splice_pipe_size_failed` | `debug` | **Per connection** where `F_SETPIPE_SZ` returned non-zero. | `rule_id: u64`, `requested_bytes: usize`, `actual_default_bytes: usize`, `errno_name: &str` |

**Backwards-compatibility guarantees**:

- All existing `proxy.completed`, `proxy.connection_error`, and other
  v0.x `proxy.*` events are emitted identically by both paths. Field
  shapes unchanged.
- No existing event is renamed, removed, or has fields removed.
- New fields are additive only.

---

## §4. Metric continuity

**No new Prometheus metrics.** No new labels on existing metrics.

The contract is: for any rule, the values of these existing series are
**bit-identical** between an optimization-on run and an
optimization-off run of the same workload:

- `portunus_rule_bytes_in_total{rule=...}`
- `portunus_rule_bytes_out_total{rule=...}`
- `portunus_rule_connections_active{rule=...}`
- `portunus_rule_connections_total{rule=...}` and its `result` labels
- All `portunus_rate_limit_*` series (v0.11)
- `portunus_target_failovers_total{rule=...}` (v0.7)

Verified by SC-004 (integration suite double-run).

---

## §5. Environment variable contract

| Name | Read by | When | Effect |
|---|---|---|---|
| `PORTUNUS_DISABLE_SPLICE` | `portunus-client` only | Once, at process start, into `Config::disable_splice` | Any non-empty value (`"1"`, `"true"`, anything non-empty) forces `eligible(&ctx) -> false` for every connection in this process. |

**Documentation policy** (FR-004): this variable is intentionally
**absent from `--help`** and from the public README / configuration
docs. It is referenced **only** in `docs/runbook.md` (or equivalent
troubleshooting page) under "Disabling the Linux fast path for triage".

**Stability**: this is an **internal kill switch**, not an API.
Operators must not script around it for steady-state operations. We
reserve the right to rename or remove it once SC-001 / SC-002 have
multiple production-baked perf cycles behind them.

---

## §6. Out-of-contract (deliberate non-guarantees)

- **No commitment** that any given errno produces the `Unsupported`
  variant other than the four listed in [research.md § R-006](../research.md#r-006-fallback-errno-set-is-closed-and-minimal).
- **No commitment** to a specific pipe size. Operators tuning
  `/proc/sys/fs/pipe-max-size` see different throughput envelopes; the
  optimization adapts (R-003).
- **No commitment** to a specific syscall count per byte. Implementation
  details (batch size, buffer reuse strategy) may evolve as long as
  byte-stability and metric continuity hold.
- **No commitment** to behaviour on hot-reload of a rule's bandwidth
  cap mid-connection. The per-connection `CopyCtx` is built at
  acceptance time and not re-read (FR-005).
- **No commitment** to working on non-`AF_INET` / non-`AF_INET6`
  socket families. Future support for unix-domain rules (if ever) would
  require a separate spec.

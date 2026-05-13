# Data Model: TCP Zero-Copy Fast Path

**Phase**: 1
**Status**: Locked
**Input**: [spec.md](./spec.md), [plan.md](./plan.md), [research.md](./research.md)

The feature is purely **internal to `portunus-client`**. There are no
persistent entities, no wire fields, no Web UI models. The "data model"
here describes the in-memory types introduced and how they relate to
existing state.

---

## Internal Types (all in `crates/portunus-client/src/forwarder/splice.rs`)

### `CopyCtx`

The per-connection context the splice path consults to decide eligibility.
Built once at connection-acceptance time in `proxy.rs` and passed by
reference into the splice path.

| Field | Type | Source | Notes |
|---|---|---|---|
| `rule_id` | `portunus_core::RuleId` | accept-time rule lookup | For tracing event correlation. |
| `protocol` | `Protocol` (enum: `Tcp`, `Udp`) | rule | Splice path only runs when `Tcp`; checked defensively. |
| `has_bandwidth_cap` | `bool` | `rule.rate_limit` OR owner cap | True if **any** of {rule.bandwidth_in_bps, rule.bandwidth_out_bps, owner.bandwidth_in_bps, owner.bandwidth_out_bps} is set. Computed once at accept; not re-read mid-connection. |
| `disable_splice` | `bool` | `PORTUNUS_DISABLE_SPLICE` env, read once at process start, cached in client `Config` | Forces userspace path. |
| `has_sni_replay_done` | `bool` | rule + v0.9 prelude state | Always true when splice path is reached — splice is invoked **after** SNI peek+replay finishes. Field exists for tracing only. |
| `has_proxy_out` | `bool` | `rule.target.proxy_protocol != None` | Tracing only; PROXY prelude is written before splice path runs. |

`CopyCtx` is `Copy` (small POD) — no allocations, no Arc bumps.

### `PipePair`

RAII handle around a `pipe2(O_NONBLOCK | O_CLOEXEC)` allocation, with
best-effort `F_SETPIPE_SZ` applied.

| Field | Type | Notes |
|---|---|---|
| `read_fd` | `OwnedFd` | Read end of the pipe. |
| `write_fd` | `OwnedFd` | Write end. |
| `capacity_bytes` | `usize` | Actual pipe capacity after the `F_SETPIPE_SZ` attempt; used as the `len` arg to `splice`. |

Drop closes both fds. No external owner; the `PipePair` is local to
`splice::copy_bidirectional` for the connection's lifetime.

### `Transferred`

The success-return type of `splice::copy_bidirectional`.

| Field | Type | Meaning |
|---|---|---|
| `bytes_in` | `u64` | Bytes delivered downstream → upstream. |
| `bytes_out` | `u64` | Bytes delivered upstream → downstream. |

Equivalent in shape and semantics to the `(u64, u64)` returned by
`tokio::io::copy_bidirectional_with_sizes` — the only reason for naming
the fields is internal readability.

### `SpliceError`

```rust
enum SpliceError {
    /// First syscall returned an "unsupported" errno before any byte
    /// moved. Caller may safely run the userspace path next.
    Unsupported {
        errno: nix::errno::Errno,
    },
    /// Any other error. May have occurred after bytes moved; caller
    /// MUST NOT attempt userspace fallback.
    Io(std::io::Error),
}
```

The `Unsupported` variant is the **only** signal authorising fallback
(FR-006). `Io` is terminal.

---

## State Machine: per-connection lifecycle

```text
                ┌─────────────────────────────────────────────┐
                │  proxy.rs::proxy() (existing entry point)   │
                │  - TLS accept, SNI peek+replay (009)         │
                │  - PROXY-out prelude write (010)             │
                │  - rate-limit accept-gate (011)              │
                └────────────────┬────────────────────────────┘
                                 │
                                 ▼
                ┌─────────────────────────────────────────────┐
                │  ctx = CopyCtx::build(rule, owner, env)    │
                └────────────────┬────────────────────────────┘
                                 │
                  cfg(linux) && splice::eligible(&ctx)?
                          ┌──────┴──────┐
                          │             │
                          ▼             ▼
            ┌──────────────────┐    ┌──────────────────────┐
            │  splice path     │    │  userspace path      │
            │                  │    │  (existing v1.2.0)   │
            │  alloc PipePair  │    │                      │
            │       │          │    │  copy_bidirectional_ │
            │  emit              │    │    with_sizes(...)   │
            │  proxy.splice_   │    │                      │
            │  selected (once  │    │  returns (in, out)   │
            │  per rule)       │    └──────────┬───────────┘
            │       │          │               │
            │  try_join!(      │               │
            │   splice_dir(↓), │               │
            │   splice_dir(↑)) │               │
            │       │          │               │
            │       │          │               │
            │  First splice    │               │
            │  syscall result?  │               │
            │     ┌─────────┐  │               │
            │     │Ok(n>0)? │──┘               │
            │     ├─────────┤                  │
            │     │Unsupp.? │ → fall through ──┘
            │     │ (before │
            │     │  first  │
            │     │  byte)  │
            │     ├─────────┤
            │     │Io error?│ → propagate Err
            │     └─────────┘
            └──────────────────┘
                          │
                          ▼
                ┌─────────────────────────────────────────────┐
                │  bytes_in / bytes_out updated identically   │
                │  regardless of path                          │
                └─────────────────────────────────────────────┘
```

---

## Interaction with existing state

### Rule (`portunus_core::Rule`)

Unchanged. Fields consulted: `id`, `protocol`, `rate_limit` (for
bandwidth-cap presence check), `target.proxy_protocol` (tracing only),
`sni_pattern` (tracing only). No new fields, no schema migration.

### Owner rate limit (`OwnerRateLimitScopeManager`)

Unchanged. Splice eligibility calls `OwnerRateLimitHandle::snapshot()`
to check whether the owner has any bandwidth cap. Returns
`has_bandwidth_cap: bool` for the `CopyCtx`. No state mutation.

### Per-rule stats (`stats_cache`)

Unchanged. The splice path updates the same `bytes_in / bytes_out`
atomics that `copy_bidirectional_with_sizes` updates via
`forwarder::proxy::proxy`'s post-call accounting. Operators see no
difference in Prometheus / per-rule stats.

### SQLite store

Unchanged. No new tables, no new migration. The rule schema
(currently V005 from v0.11) is not touched.

### Wire / proto

Unchanged. No new fields on `Rule`, `RuleStats`, `StatsReport`,
`RuleUpdate`. No new messages, no new enums. No protobuf compile
change.

### Web UI

Unchanged. No new component, no new label, no new column.

### CLI (`portunus-server`, `portunus-client`)

Unchanged. No new flag, no new subcommand. `PORTUNUS_DISABLE_SPLICE`
is read by `portunus-client` from the environment at startup; it is
**not advertised** in `--help`.

---

## Invariants

These invariants are part of the contract and validated by tests:

1. **`splice::eligible(&ctx)` is a pure function of `ctx`.** No I/O, no
   syscalls, no allocations. Idempotent.
2. **`CopyCtx` is built once per connection.** Re-reading
   `has_bandwidth_cap` mid-connection is **not** part of the contract;
   a rule-cap hot-update via `PUT /v1/rules/{id}` does not migrate
   in-flight connections between paths (FR-005, mirrors v0.11
   hot-reload semantics).
3. **`PipePair::drop` always closes both fds.** No fd leak path exists.
4. **`Transferred::bytes_in / bytes_out` advance only on
   pipe-to-destination splice return.** Source-to-pipe splice does
   not count; verified by unit-level test that injects RST after
   source-side splice succeeds and asserts the counter did not advance.
5. **`SpliceError::Unsupported` is unreachable after `moved_any ==
   true`.** Verified by unit-level test using an injected errno.

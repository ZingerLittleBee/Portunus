# Multi-target failover: splice fast path + live byte counters

Date: 2026-05-29
Crate: `portunus-forwarder` (`forwarder/failover_path.rs`, `forwarder/proxy.rs`)

## Problem

v1.6.1 (`78c9578`, "live bytes_in/out updates on splice fast path")
added a `LiveBytesSink` so `RuleStats::bytes_in/out` update per splice
chunk instead of only at connection close. The fix was wired into the
**single-target** path (`proxy::proxy_with_preread_and_prelude` →
`copy_uncapped` → `splice::copy_bidirectional`).

The **multi-target / failover** path (`failover_path::handle_connection`)
was never updated. Its uncapped branch uses
`tokio::io::copy_bidirectional` (plain userspace, no splice, no sink);
its capped branch uses `copy_bidirectional_with_rate_limit` (no sink).
Bytes are only recorded once, after the copy returns.

Consequence: any rule with 2+ targets (multi-A, priority failover) shows
**frozen `bytes_in/out` for the whole lifetime of a long-lived
connection**, only settling at close — exactly the symptom v1.6.1 set
out to fix. It also means multi-target rules never get the Linux splice
zero-copy fast path at all.

### Reproduction (docker, splice enabled, Linux VM)

| Rule | bytes during a single long-lived flow |
|------|----------------------------------------|
| single-target (`target = "..."`)   | climbs continuously (live) ✓ |
| multi-target (`targets = [...]`)    | frozen at 0 until close ✗ |

`splice_unsupported_fallback` count = 0 in both; `eligible()` =
`TCP && !disable_splice && !has_bandwidth_cap` is true for both — so the
single-target path genuinely splices and live-counts, the multi-target
path never reaches splice.

## Goal

Bring the multi-target uncapped copy to parity with the single-target
path: route it through the splice-capable `copy_uncapped` with a
`LiveBytesSink`. This yields both the zero-copy fast path and live byte
counters for failover rules, with **identical byte totals** (the live
sink flush + post-copy remainder always sum to the full transfer).

Capped multi-target rules stay on `copy_bidirectional_with_rate_limit`
(unchanged) — exactly as the single-target capped branch does, since a
bandwidth cap makes splice ineligible by design.

## Design

`copy_uncapped` already encapsulates "splice if eligible, else userspace
fallback, plus optional live sink". Reuse it instead of duplicating.

1. **`proxy::copy_uncapped` → `pub(crate)`** so the sibling
   `failover_path` module can call it. No signature change.

2. **`failover_path::handle_connection`**: build
   `let live_sink = LiveBytesSink::new(Arc::clone(&stats), listen_port);`
   before the copy select. In the uncapped branch replace
   `tokio::io::copy_bidirectional(&mut inbound, &mut outbound)` with
   ```rust
   crate::forwarder::proxy::copy_uncapped(
       &mut inbound, &mut outbound, rule_id,
       rate_limit.as_deref(), owner_rate_limit.as_deref(),
       None,            // quota: not threaded into the failover path (unchanged)
       false,           // preread: failover has no SNI preread
       false,           // had_proxy_prelude: tracing-only CopyCtx field
       Some(&live_sink),
   ).await
   ```
   The capped branch is untouched.

3. **Post-copy batch record**: mirror the single-target subtraction so
   live-flushed bytes are not double-counted:
   ```rust
   let (live_in, live_out) = live_sink.snapshot_recorded();
   stats.record_in(listen_port, bin.saturating_sub(live_in));
   stats.record_out(listen_port, bout.saturating_sub(live_out));
   ```
   Per-target `state.add_bytes_in/out(*bin/*bout)` keep using the full
   totals — unchanged.

### Why totals stay byte-stable

- Uncapped + Linux + splice eligible: sink flushes deltas live; remainder
  = total − flushed; sum = total.
- Uncapped + splice unsupported→fallback / non-Linux: `copy_uncapped`
  runs the userspace copy and never touches the sink (the sink is only
  threaded inside the `splice::copy_bidirectional` arm), so flushed = 0
  and the post-copy record books the full total — byte-identical to the
  old `tokio::io::copy_bidirectional` path.
- Capped: `copy_uncapped` not called; sink stays 0; full total recorded.

The PROXY-protocol prelude is already written inside
`dial_with_failover` before `handle_connection` copies, so reusing
`copy_uncapped` (which does not write a prelude) changes nothing there.

Quota (013) is not wired into the failover copy today; passing `None`
preserves current behaviour — out of scope for this change.

## Testing

- **No regression (macOS + Linux CI)**: existing e2e multi-target suites
  must stay green — `multi_target_unchanged`, `multi_target_recovery`,
  `multi_target_passive_failover`, `multi_target_cli_wire_through`,
  `standalone_failover`. These assert end-to-end byte delivery /
  failover semantics, so any total-bytes drift fails them.
- **Live behaviour (docker, Linux VM)**: re-run the discriminating probe
  — a single rate-limited long-lived flow through a 2-target rule must
  now show `bytes_out` climbing mid-connection (was frozen at 0).
- `cargo clippy -p portunus-forwarder --all-targets -- -D warnings`,
  `cargo fmt`.

## Non-goals

- Threading quota into the failover copy path.
- Touching the capped (rate-limited) branch.
- Any wire/operator/config surface change.

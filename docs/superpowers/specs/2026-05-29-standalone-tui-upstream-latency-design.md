# Standalone TUI — Upstream Latency Display

**Date:** 2026-05-29
**Branch:** `017-standalone-tui-upstream-latency`
**Status:** Approved (brainstorming)

## Goal

Show the round-trip latency to each rule's active upstream target in the
`portunus-standalone stats` TUI Detail page, so an operator can see at a
glance whether the upstream is reachable and how fast it responds.

## Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Probe method | **TCP connect** (not ICMP) | ICMP needs `CAP_NET_RAW`/root and is firewall-dropped; it measures host reachability, not service reachability. TCP connect-time to `host:port` is privilege-free and measures exactly what a forwarder cares about. |
| Probe location | **TUI client side** | UDS is local, so the TUI and daemon share the same network vantage — daemon-side probing has no accuracy advantage here. Client-side keeps the wire protocol (`Snapshot`/`Hello`) and operator surface unchanged, and only probes while someone is watching. |
| Target scope | **Active target only** | The lowest-priority target (the one currently forwarded to). Minimises probe connections. |
| Probe scope | **Selected rule only, Detail tab only** | RTT is only displayed on the Detail page for the selected rule. Probing elsewhere wastes connections. |
| UDP rules | **Display `—`** | A TCP probe to a UDP service port is meaningless and would mislead with constant `timeout`. |
| Cadence | **Every ~2s, 1s connect timeout** | Decoupled from the snapshot refresh; one probe per interval is negligible load and responsive enough. |
| Presentation | **Current value + colour** | Appended to the active target row in the Targets panel. Green `<50ms` / yellow `<200ms` / red `>=200ms` or timeout. |
| Pause (`p`) | **Keeps probing** | RTT is a live measurement, not historical accumulation; pause only freezes the throughput ring. |

## Architecture & data flow

Entirely inside the stats client process. No daemon changes, no
`Snapshot`/`Hello` field changes, no new config or CLI flag.

1. Target `host:port` already arrives in `Hello.rules[].targets[]`.
2. Probe = `tokio::time::timeout(1s, TcpStream::connect("host:port"))`,
   measuring elapsed connect time. `TcpStream::connect` performs DNS
   resolution internally — no extra dependency (tokio is already a dep).
3. Trigger condition (all three must hold, to floor the load):
   - current tab is **Detail**;
   - `last_probe_at.elapsed() >= 2s`;
   - the selected rule's active target is **TCP** (UDP → skip, show `—`).
4. Non-blocking: each due probe is `tokio::spawn`-ed as a short-lived
   task whose result is returned over an `mpsc` channel. `run_loop`
   drains the channel with `try_recv` each iteration, so the 1s connect
   timeout never blocks key handling or rendering.

Only the **selected** rule's active target is probed. Switching the
selected row refreshes the new rule's value on the next probe tick; a
per-rule cache shows the previous value immediately on switch-back.

## Components

| File | Change |
|------|--------|
| `stats/tui/probe.rs` (new) | `async fn probe_tcp(host, port) -> ProbeSample`; `ProbeSample` enum (`Ok(Duration)` / `Timeout` / `Failed`); `active_target(meta) -> Option<&TargetMeta>` shared by prober and renderer so the "which target is active" rule never drifts. |
| `stats/tui/state.rs` | `AppState` gains `probes: HashMap<String /*rule_id*/, ProbeSample>` and `last_probe_at: Instant`. |
| `stats/tui/mod.rs` | `run_loop` gains probe-trigger logic and `mpsc` result collection. |
| `stats/tui/render.rs` | `render_targets` appends the RTT to the active target row. |
| `stats/tui/format.rs` | `fmt_rtt(sample) -> (String, Color)` with the colour thresholds. |

## Presentation

```
▶ 10.0.0.5:22  prio 0   12ms
  10.0.0.6:22  prio 1
```

- Colour: green `<50ms` / yellow `<200ms` / red `>=200ms` or timeout.
- States: `12ms` (ok) / `timeout` / `…` (first probe in flight) / `—` (UDP).
- Non-active rows show no RTT.

## Edge cases

- **Pause (`p`)**: probing continues (RTT is live, not part of the ring).
- Rule with no targets / selection out of range: skip gracefully, no probe.
- DNS resolution time is included in the measured connect time; the OS
  resolver cache makes this negligible in steady state.

## Testing (Constitution Principle III — real loopback sockets)

- `probe_tcp` against a real loopback `TcpListener` → `Ok(rtt)`.
- `probe_tcp` against a closed/refused port → `Failed`/`Timeout`.
- `render_targets` with an injected `ProbeSample` → asserts the `ms`
  string and colour appear on the active row.
- UDP rule → active row shows `—`, no probe is issued.
- `fmt_rtt` colour-threshold mapping — pure unit test.

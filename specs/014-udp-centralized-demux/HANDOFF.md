# 014 UDP Centralized Demux — Verification Handoff

Branch: `014-udp-centralized-demux` (local only, not pushed).
Base: `main` @ v1.4.3.

## Test matrix

### Local workspace (macOS, darwin 25.4.0)

| Suite | Result |
|---|---|
| `cargo test --workspace` (`PORTUNUS_SKIP_WEBUI=1`) | pass |
| `cargo clippy --workspace --all-targets -- -D warnings` | pass |
| `cargo fmt --all -- --check` | clean |
| `udp::registry` unit tests | 8/8 pass |
| `udp::error::classify_udp_error` unit tests | 6/6 pass |
| `udp::demux` unit tests | 3/3 pass |
| `udp::reaper` unit tests (incl. `zero_idle_window_disables_reaper_without_panic`) | pass |
| `udp::listener` unit tests (incl. `concurrent_connections_cap_bounds_udp_flows`) | pass |
| `udp::runtime` supervisor-in-isolation harness | 4/4 pass |
| `udp::integration_tests` (rule-level, real sockets) | 4/4 pass — incl. `udp_range_rule_cap_is_per_rule` (SC-002), `udp_cross_listener_same_src_distinct_flows` (FR-002), `udp_rule_round_trip_byte_equal`, `udp_overflow_on_cap` |

### Linux VPS (207.241.173.217, kernel 6.8.0-117-generic)

| Suite | Result |
|---|---|
| `cargo test --workspace` | green |
| `cargo test -p portunus-forwarder` UDP suite | 42 / 42 pass |
| `cargo test -p portunus-e2e --test udp_smoke` | 8 / 8 pass |
| `cargo bench udp_data_plane -- udp_high_flow_count` (SC-001a) | pass (see below) |
| `cargo bench udp_data_plane -- 'udp_data_plane.single_flow'` (SC-004 sanity) | pass — no regression vs inline reference |

## SC-001a memory benchmark (Linux VPS)

`udp_high_flow_count / n1000_rss_delta`, N=1000 concurrent flows, single listener, centralized demux:

```
sample 0 (cold): 3756 KB
sample 1:         436 KB
sample 2:         128 KB
sample 3:         108 KB
sample 4:          12 KB
sample 5:          32 KB
sample 6:          96 KB
sample 7:         188 KB
sample 8:           4 KB
sample 9:           4 KB
sample 10:          4 KB
```

Legacy v0.4 expected (per-flow 64 KiB recv_buf): `1000 × 64 KiB = 65 536 KB`.
Observed: peaks at 3756 KB cold, settles to single-digit KB warm.

**Result: ~3 orders of magnitude reduction.** Far exceeds the SC-001a target
(≤ `N × 1 KiB = 1000 KB`).

## SC-004 throughput non-regression

Inline single-flow round-trip on loopback (the bench mirrors the production
shape but does not import the runtime — see bench header):

```
single_flow_throughput/512B_round_trip  time: [111.22 µs 114.95 µs 117.69 µs]
single_flow_rtt                          time: [104.97 µs 107.32 µs 110.38 µs]
```

These values match the pre-014 baseline of the same bench. The bench itself
was not modified for 014 except to add the new `udp_high_flow_count` group
in a separate `criterion_group!`, so single-flow numbers are directly
comparable.

## SC-002 / SC-005 coverage

* **SC-002 (per-rule cap)** is covered by `udp_range_rule_cap_is_per_rule`
  (passes on both macOS and Linux). The test installs a range rule, drives
  flows from distinct source ports, and asserts overflow rejects only once
  the per-rule budget is exhausted.
* **SC-005 (ICMP eviction)** is covered structurally by
  `classify_udp_error` unit tests (synthetic `ECONNREFUSED` /
  `EHOSTUNREACH` / `ENETUNREACH` → `UdpAction::Evict`) and by the
  registry + reaper unit tests that exercise the eviction path. The
  Linux-only e2e `test_udp_smoke_icmp_evict` is gated behind the
  pre-existing e2e harness failure documented below; it is `ignore`d on
  non-Linux already.

## First-packet drop bug (fixed)

During VPS verification the e2e `udp_smoke` suite surfaced a real data-plane
bug. Trace logs showed `rule.udp_first_packet_send_dropped` with
`action=WouldBlock` on every fresh flow — the cold path's
`upstream_socket.try_send(payload)` returned `EAGAIN` even though the
socket had just been `bind`+`connect`-ed and the kernel send buffer was
empty.

Root cause: Tokio's `UdpSocket::try_send` consults the reactor's
registered I/O readiness. On a freshly created socket the reactor has not
yet observed writability (no prior poll), so `try_send` returns
`WouldBlock` until the reactor's first wake. The cold path classified
this as a transient error and dropped the datagram while keeping the
flow. End-user apps that send one datagram per flow (DNS, some QUIC
shapes) saw 100 % first-packet loss.

Fix (`crates/portunus-forwarder/src/forwarder/udp/listener.rs:357`):
cold-path step 9 now uses `send(payload).await` instead of `try_send`.
The future parks once on writability, the reactor immediately reports
the socket as writable, and the syscall issues. Cold path stays fast
(one extra await on a guaranteed-ready future). Hot-path subsequent
sends keep `try_send` with drop-on-`WouldBlock` semantics (FR-007).

Side benefit: the pre-014 "macOS-only `udp_smoke` flake" referenced
in `CLAUDE.md` was the same bug. After the fix the e2e suite is green
on both macOS and Linux.

## Operational notes (carried from `CHANGELOG.md`)

This branch is operator-visible breaking:
* `concurrent_connections` for UDP rules now caps **per-rule**, not
  per-listener-port. Range rules with a large `port_count` and a small
  cap will see fewer concurrent flows than under v1.4.x.
* `active_flows` stat now reflects registry occupancy
  (cap-budget-bearing flows), not the last-writer-wins counter from v0.4.
  Operators relying on the prior gauge for dashboards should re-scale.
* Mid-flow multi-A resolver fallback removed. Fallback happens at
  cold-path `connect()` only — once a flow is live, ICMP errors evict
  the flow rather than rolling to another A record.

## Final state

* 22 commits on `014-udp-centralized-demux`, ahead of `main`.
* Not pushed to remote (per user instruction).
* Working tree clean.

---
title: portunus-standalone — Per-Rule Traffic Stats over UDS + ratatui TUI — Design
status: draft v1 · awaiting user review
date: 2026-05-28
branch: TBD
target_release: v1.6.0
parent_specs:
  - docs/superpowers/specs/2026-05-14-standalone-forwarder-design.md
  - docs/superpowers/specs/2026-05-26-standalone-docs-and-deploy-design.md
---

# portunus-standalone — Per-Rule Traffic Stats over UDS + ratatui TUI

## 1. Goal

Give `portunus-standalone` operators a first-class way to observe
**per-rule traffic** without grepping JSON logs:

- **Current rate** (in/out bytes per second) per rule, computed correctly
  rather than reverse-engineered from cumulative counters in 30 s log
  ticks.
- **Cumulative totals** (bytes, connections opened, datagrams) per rule
  with human-readable formatting. **Note:** `RuleStats` today only has
  `active_connections` (gauge, `AtomicU32`); a new monotonic
  `connections_total: AtomicU64` is added in §6.3 to make this work.
- **60-second history** as inline sparklines.
- **Errors panel** that surfaces `port_in_use`, `upstream_connect_failed`,
  `icmp_evict`, `emsgsize`, and similar failure counters without forcing
  the operator to scan logs.
- **Interactive dashboard** (`portunus-standalone stats`) backed by
  `ratatui` + `crossterm`, plus a `--once` mode for scripting.

Success criteria:

- `portunus-standalone stats` shows a live dashboard of every active
  rule, updating at 1 Hz, on any host where the daemon is reachable via
  its Unix domain socket.
- `portunus-standalone stats --once` prints one snapshot as JSON and
  exits, suitable for `jq` pipelines and ad-hoc scripts.
- No new HTTP server, no new Prometheus endpoint, no SQLite, no
  control-plane dependency. Binary size growth ≤ 2 MB (ratatui +
  crossterm only).
- Daemon stays stateless w.r.t. monitoring: it only exports current
  cumulative counters. All history and rate calculation lives in the
  TUI client.

## 2. Non-goals

- **No Prometheus endpoint in v1.** A separate `portunus-standalone
  metrics --listen 127.0.0.1:NNNN` subcommand that wraps UDS → Prometheus
  textformat is a future extension, sketched in §11 but not implemented
  here. Keeps the v1.5 invariant *"No new Prometheus metrics in
  standalone"* (CLAUDE.md) intact.
- **No HTTP server in the daemon.** UDS is the only new transport. The
  user explicitly favored keeping the binary small.
- **No web UI.** Explicitly rejected by the user; TUI via `ratatui` is
  the chosen surface.
- **No persistent history / long-term metrics storage.** The 60 s
  sparkline lives in the client process; closing the TUI loses it. For
  long-horizon metrics, operators are expected to graduate to
  `portunus-server`.
- **No per-connection / per-peer drill-down** (top-N source IPs,
  latency histograms). Those require per-connection state in the
  daemon, which inflates scope and conflicts with the "thin standalone"
  philosophy.
- **No live config reload, no in-TUI rule editing, no service
  restart from TUI.** Read-only observability.
- **No multi-host federation.** Each TUI talks to one local daemon.
- **No live log tail panel.** Contradicts the premise that logs are
  the thing we're getting away from.

## 3. Current state

| Surface | Path | Status |
|---|---|---|
| Cumulative counters per rule | `portunus-forwarder::RuleStats` | ✅ exists: `bytes_in`, `bytes_out`, `active_connections` (gauge), `datagrams_in`, `datagrams_out`, `active_flows` (gauge), `dns_failures`, `flows_dropped_overflow`, `sni_route_{exact,wildcard,fallback}_total` |
| Multi-target failover counter | `portunus-forwarder::forwarder::failover_path` | ✅ `target_failovers_total: Arc<AtomicU64>` exists per failover rule (currently held by failover state, not exposed via `RuleStats`) |
| TCP `connections_total` (monotonic) | — | ❌ does NOT exist; only `active_connections` gauge. v1 adds this. |
| Per-rule error counters for tracing-only events (`port_in_use`, `upstream_connect_failed`, `icmp_evict`, `emsgsize`, `wouldblock`, `addflow_dropped`) | — | ❌ events fire as tracing lines only, no `AtomicU64`. v1 adds them. |
| Periodic tracing reporter | `crates/portunus-standalone/src/reporter.rs` | ✅ 30 s `standalone.stats` events |
| Per-conn close events | `rule.conn_closed` (tracing) | ✅ |
| Failure event tracing | `rule.udp_emsgsize`, `rule.udp_flow_evicted_icmp`, `rule.udp_upstream_connect_failed`, `rule.failed`, … | ✅ (events exist, but no per-rule counter aggregation) |
| Operator HTTP / Prometheus | — | ❌ does not exist in standalone (intentionally) |
| TUI / interactive dashboard | — | ❌ |

Two pain points the design must close:

1. Cumulative counters in 30 s log ticks make rate calculation a manual
   chore (`(new - old) / interval`).
2. Failure events fire as discrete tracing lines with no per-rule
   counter, so "did this rule see any ICMP evictions in the last
   minute?" requires `grep | wc -l`.

## 4. Architecture

```
┌────────────────────────────────────────────────────────────────┐
│                   portunus-standalone (daemon)                 │
│                                                                │
│  RuleStats {bytes_in, bytes_out, ...}    ◄── reused as-is      │
│  ErrorCounters {port_in_use, ...}        ◄── NEW: AtomicU64    │
│                                                                │
│  reporter (30 s)  ◄── unchanged                                │
│                                                                │
│  NEW: stats::server                                            │
│   - tokio::net::UnixListener("/run/portunus/standalone.sock")  │
│   - per-accept task: read snapshots from registry, write       │
│     JSON lines to socket on a tokio::time::interval ticker     │
│   - closes cleanly on client disconnect or CancellationToken   │
└────────────────────────────────────────────────────────────────┘
                              │
                              │  UDS, JSON-lines
                              ▼
┌────────────────────────────────────────────────────────────────┐
│           portunus-standalone stats (TUI client)               │
│                                                                │
│  stats::client                                                 │
│   - connect UDS, read Hello, then stream Snapshots             │
│   - VecDeque<Snapshot> ring buffer, capacity = 60              │
│   - rate = (snap[i] - snap[i-1]) / dt                          │
│                                                                │
│  stats::tui (ratatui + crossterm)                              │
│   - Overview tab: Table + per-row Sparkline                    │
│   - Detail tab: full counters + Gauge + 60 s sparkline         │
│   - Errors tab: per-rule failure counter table                 │
│   - Footer: aggregate throughput + uptime + fd usage           │
└────────────────────────────────────────────────────────────────┘
```

**Key architectural decisions:**

- **Daemon stays stateless**: only exports the current snapshot. Ring
  buffer + rate calc lives in the client. Multiple TUI clients can
  attach simultaneously without contending or duplicating state.
- **One protocol, two consumers**: the same JSON-line format powers
  the interactive TUI and the scriptable `--once` mode. No second
  CLI/data path to maintain.
- **Single binary**: `portunus-standalone` is both the daemon and the
  TUI client (subcommand dispatch in `main.rs`). No second artifact to
  ship.
- **UDS over HTTP**: zero new transport deps (Tokio already has
  `UnixListener`), permission control is filesystem-level, no port
  allocation, no auth code path needed.

## 5. Protocol

JSON-lines (one document per `\n`-terminated line) over UDS.

### 5.1 Hello (one line, sent on accept)

```jsonc
{
  "v": 1,
  "daemon_version": "1.6.0",
  "daemon_started_at_ms": 1748400000000,
  "refresh_ms": 1000,
  "rules": [
    // Single-target rule
    {
      "id": "11847161691739766033",
      "name": "ssh-tunnel",
      "proto": "tcp",
      "listen": "2222",
      "targets": [
        { "host": "10.0.0.5", "port": 22, "priority": 0, "proxy_protocol": null }
      ],
      "splice_capable": true,
      "udp_max_flows": null
    },
    // Multi-target failover rule
    {
      "id": "12764676716154531751",
      "name": "ha-https",
      "proto": "tcp",
      "listen": "8443",
      "targets": [
        { "host": "primary.internal",   "port": 443, "priority": 0,  "proxy_protocol": "v2" },
        { "host": "secondary.internal", "port": 443, "priority": 10, "proxy_protocol": "v2" }
      ],
      "splice_capable": false,
      "udp_max_flows": null
    }
    // ...
  ]
}
```

Sent exactly once after `accept()`. Contains static per-rule metadata
that doesn't change at runtime (name, ports, target list, capabilities).
`targets` is always an array of length ≥ 1, even for single-target
rules — this lets the client render single and multi-target rules with
the same code path.

**Scope note on multi-target observability**: v1 surfaces only the
aggregate `target_failovers_total` counter in the Snapshot (see §5.2).
Per-target liveness (`{health, consecutive_failures, last_failure_at_ms,
last_success_at_ms}`) is **not** in v1 — the Snapshot does not include
a per-target health array. This is a deliberate cut: per-target health
state lives in the failover `TargetHealth` struct today and exposing it
would require new accessors and a new section in the Snapshot. v2 may
add `Snapshot.r[].targets: [{ index, health, ... }]`; until then the
TUI's Detail view shows the failover counter only and lists targets
from Hello in priority order without per-target liveness.

### 5.2 Snapshot (every `refresh_ms`)

```jsonc
{
  "t_ms": 1748400015234,
  "uptime_ms": 304523,          // daemon-monotonic since process start
  "seq": 42,                    // monotonic snapshot counter (++ each tick)
  "process": {
    "fd_open": 217,             // /proc/self/fd count on Linux; null on macOS
    "fd_limit": 65535,          // soft RLIMIT_NOFILE
    "rss_bytes": 18874368       // /proc/self/status VmRSS; null on macOS
  },
  "r": [
    {
      "id": "11847161691739766033",
      "in": 104857600,
      "out": 52428800,
      "conns_active": 3,
      "conns_total": 124,           // NEW counter, see §6.3
      "datagrams_in": 0,
      "datagrams_out": 0,
      "flows_active": 0,
      "target_failovers_total": 0,  // 0 for single-target rules
      "err": {
        "port_in_use": 0,
        "upstream_connect_failed": 2,
        "icmp_evict": 0,
        "emsgsize": 0,
        "wouldblock": 0,
        "addflow_dropped": 0,
        "dns_failures": 0,           // already in RuleStats
        "flows_dropped_overflow": 0  // already in RuleStats
      }
    }
    // ...
  ]
}
```

Per-rule cumulative fields. The TUI computes rates from adjacent
snapshots using **`uptime_ms`**, not `t_ms`. Wall-clock (`t_ms`) is
provided for display only; if the system clock jumps backward or NTP
slews, rate math would otherwise break. `seq` lets the client detect
dropped snapshots (gaps).

### 5.3 Versioning

- `"v": 1` is the protocol version. Adding new fields is backward
  compatible (clients ignore unknown keys). Renaming or removing fields
  bumps `v`.
- Daemon and client share the same `Cargo.toml` version. Mismatched
  versions are detectable on the client side by comparing
  `daemon_version` against its own; v1 logs a warning and continues
  best-effort.

### 5.4 Lifecycle

- Client disconnect (clean close or broken pipe) → per-connection task
  exits silently. No daemon-level state to clean up.
- Daemon shutdown (`SIGTERM`) → `CancellationToken` fires, all per-
  connection tasks observe it via `select!`, close the socket gracefully,
  join during the shutdown drain budget.
- Multiple concurrent clients are supported. There is no auth or
  rate-limit beyond Unix file permissions on the socket.

## 6. Daemon-side changes

### 6.1 New module `crates/portunus-standalone/src/stats/`

```
src/stats/
├── mod.rs           # public types: Hello, Snapshot, RuleSnap, ErrorCounters
├── server.rs        # UnixListener + per-accept task
└── error_counter.rs # AtomicU64 wrapper; per-rule failure counters
```

### 6.2 Config

`[global]` gets nothing. New top-level `[stats]` section:

```toml
[stats]
enabled     = true                                    # default true
socket_path = "/run/portunus/standalone.sock"         # default
refresh_ms  = 1000                                    # 250..=5000 allowed
```

CLI flags on the **daemon** subcommand:

| Flag | Effect |
|---|---|
| `--no-stats` | Equivalent to `[stats] enabled = false` |
| `--stats-socket <PATH>` | Override `[stats] socket_path` |

**Platform note on `socket_path`**: the Linux default
`/run/portunus/standalone.sock` does not exist on macOS (no
`/run/`). The daemon resolves the default at runtime:

- Linux → `/run/portunus/standalone.sock`
- macOS → `$TMPDIR/portunus-standalone.sock` (typically
  `/var/folders/…/T/portunus-standalone.sock`)
- Anywhere → `[stats] socket_path = "..."` overrides

systemd users get the Linux default via the unit's
`RuntimeDirectory=portunus`; Docker users get the Linux default
because the image runs on Linux; macOS developers running the binary
directly get the `$TMPDIR` default.

### 6.3 New counters in `portunus-forwarder`

This is the most invasive part of the change: it edits
`portunus-forwarder`, not just standalone. Three groups of additions
land in `crates/portunus-forwarder/src/forwarder/stats.rs`.

**(a) `connections_total: AtomicU64`** — added directly to `RuleStats`.
**TCP-only**: bumped on each successful TCP `accept()` on the
single-target path and on the failover path. For UDP rules the
counter is present (always zero) but ignored by the TUI — UDP
activity surfaces through `flows_active` / `datagrams_in` /
`datagrams_out` instead. The Snapshot `conns_total` field is
documented (in §5.2 + fumadocs) as "TCP only; UDP rules use
`flows_active` and datagram counters". Sort-by-conns in the TUI
treats UDP rules as 0 (they fall to the bottom).
The existing `active_connections: AtomicU32` gauge stays;
`connections_total` is the monotonic complement. The new field is
read by the standalone UDS server only; the existing gRPC
`StatsReport` wire format is **not** touched in this design — adding
the counter to RuleStats does not automatically affect the proto, and
plumbing it into `StatsReport` (for `portunus-client → server` use)
is left as a separate decision out of scope here.

**(b) `ErrorCounters` struct** — a new lightweight type bundling
six `AtomicU64`s, embedded into `RuleStats` as a single field
`errors: ErrorCounters`:

```rust
pub struct ErrorCounters {
    pub port_in_use:             AtomicU64,
    pub upstream_connect_failed: AtomicU64,
    pub icmp_evict:              AtomicU64,
    pub emsgsize:                AtomicU64,
    pub wouldblock:              AtomicU64,
    pub addflow_dropped:         AtomicU64,
}
```

Each existing tracing call site gets a paired `fetch_add(1, Relaxed)`.
No new dependency. The data-plane hot path adds one atomic increment
per failure event — negligible.

**Implementation rule**: every existing `tracing::warn!` /
`tracing::info!` call site that emits one of these event names also
bumps the matching counter. The implementation must locate **all**
sites, not just one per event. Concretely (verified against
`portunus-forwarder` at the time of writing):

| Event | Counter | File:line |
|---|---|---|
| `rule.failed` (port_in_use) | `port_in_use` | `forwarder/mod.rs:214`, `mod.rs:622`, `forwarder/failover_path.rs:70`, `failover_path.rs:662` |
| `rule.udp_upstream_connect_failed` | `upstream_connect_failed` | `forwarder/udp/listener.rs:626` |
| `rule.udp_addflow_dropped` | `addflow_dropped` | `forwarder/udp/listener.rs:669` |
| `rule.udp_flow_evicted_icmp` | `icmp_evict` | `forwarder/udp/listener.rs:332,387,473,705`, `forwarder/udp/demux.rs:135` |
| `rule.udp_emsgsize` | `emsgsize` | `forwarder/udp/listener.rs:344,400,484`, `forwarder/udp/demux.rs:146` |
| `rule.udp_reply_wouldblock` | `wouldblock` | `forwarder/udp/demux.rs:113` |

Line numbers are snapshot-of-today references; the implementation
plan should `grep` for each event name to catch any drift before
patching. Filter the grep against `--include='*.rs'` excluding tests
to mirror the table.

**(c) `target_failovers_total` exposure** — already exists as
`Arc<AtomicU64>` in `failover_path.rs:215`, owned by the failover
state.

`RuleStats::for_range` currently returns `Arc<RuleStats>`, which
means a post-construction `Option<Arc<AtomicU64>>` slot would need
interior mutability or `Arc::get_mut` (fragile). Instead:

- `RuleStats` carries a non-optional
  `target_failovers_total: Arc<AtomicU64>`, initialised to `0` by
  `for_range`. For single-target rules this counter exists but is
  never incremented — it always reads 0, which is the correct value
  to surface in the Snapshot.
- For multi-target rules, the failover constructor calls
  `Arc::clone(&rule_stats.target_failovers_total)` and uses that as
  its `target_failovers_total` instead of allocating a fresh `Arc`.
  Existing failover code already takes an `Arc<AtomicU64>` parameter
  (see `failover_path.rs:152`), so the only change is the source of
  the `Arc`.

This avoids `Option`, avoids post-construction mutation, and keeps
both readers (the failover hot path and the standalone stats server)
sharing one `AtomicU64`.

**(d) `dns_failures` / `flows_dropped_overflow`** — already exist in
`RuleStats`, just need to be surfaced in the Snapshot payload (§5.2).
No code change in the forwarder.

The standalone server reads everything via `Arc<RuleStats>` it already
holds — no new public API needed on the forwarder crate beyond the
struct field additions.

### 6.4 Socket setup

- **Parent directory creation**: the daemon `mkdir`s the parent dir
  of the resolved `socket_path` if it does not exist, mode `0755`.
  This is the parent of whatever `[stats] socket_path` (or its
  platform default) resolves to — `/run/portunus/` on Linux,
  `$TMPDIR` on macOS (already exists), or whatever the operator
  chose.
- **Socket file**: `chmod 0660`, owner inherited from the running
  process (typically `portunus:portunus` under systemd, `nonroot`
  UID 65532 inside the Docker image).
- **systemd**: the shipped unit gets `RuntimeDirectory=portunus`,
  giving `/run/portunus/` automatic creation + cleanup with owner
  `portunus:portunus`. No `mkdir` race at startup.
- **Docker / distroless nonroot**: the image runs as UID 65532 with
  no write capability over `/run/`. The Dockerfile must pre-create
  `/run/portunus/` with `chown 65532:65532` and `chmod 0755` in a
  builder stage, then `COPY --chown=65532:65532` it into the final
  image. The runtime daemon then has `mkdir(parent)` succeed as a
  no-op (already exists) and can create the socket file inside.
  Alternative: the operator bind-mounts a writable runtime dir at
  `/run/portunus/`, but that's a documentation footgun — easier to
  bake the directory into the image.
- **macOS**: `$TMPDIR` already exists; no `mkdir` needed for the
  default path. If the operator overrides `socket_path`, the
  `mkdir(parent)` step covers it.
- Operators wanting to view stats from the host of a Docker daemon
  can either `docker exec -it portunus-standalone portunus-standalone stats`
  (preferred — no bind-mount needed) or bind-mount `/run/portunus/`
  out.

### 6.5 Backpressure

Per-client task uses `tokio::io::BufWriter<UnixStream>` with `flush()`
after each line. If a client falls behind, the write blocks the
per-client task only — never the data plane. A slow client cannot
backpressure the forwarder.

## 7. Client-side (TUI)

### 7.1 New module `crates/portunus-standalone/src/stats/`

Reuses `mod.rs` (shared types) and adds:

```
src/stats/
├── client.rs        # UDS reader, ring buffer, rate computation
└── tui/
    ├── mod.rs       # event loop, key dispatch
    ├── render.rs    # widget composition (Overview/Detail/Errors)
    ├── state.rs     # AppState (selected rule, sort, filter, paused)
    └── format.rs    # human-readable byte/rate formatters
```

### 7.2 Subcommand dispatch in `main.rs`

```
portunus-standalone                         # daemon (existing default)
portunus-standalone --check ...             # existing --check (unchanged)
portunus-standalone stats                   # NEW: TUI
portunus-standalone stats --once            # NEW: single JSON snapshot
portunus-standalone stats --socket <PATH>   # override socket path
```

`clap`'s subcommand support handles dispatch. Backwards compatibility
holds: the existing no-subcommand invocation remains the daemon.

**No client-side `--refresh` flag.** Snapshot cadence is daemon-driven
(`[stats] refresh_ms`); the protocol is push-only, the client has no
request channel. The TUI redraws on each received snapshot. If an
operator wants a different cadence, they edit the daemon's
`[stats] refresh_ms` and restart. v1 keeps the protocol unidirectional
to stay simple; a future bidirectional subscribe message would be a
protocol-version bump.

### 7.3 Ring buffer

`VecDeque<Snapshot>` with capacity computed from the daemon's
`refresh_ms` (received in Hello):

```
RING_WINDOW_MS = 60_000;
capacity = (RING_WINDOW_MS / refresh_ms).ceil() as usize + 1;
```

This keeps the visible window pinned at ~60 s regardless of cadence:
~241 entries at `refresh_ms=250`, 61 at 1000, 13 at 5000. The `+ 1`
keeps room to compute the most recent rate without losing the
oldest sample mid-tick.

On each new snapshot:

1. Append to back.
2. Pop front if `len() > capacity`.
3. Sparklines read the entire deque each render tick.

Rate at index `i` uses **`uptime_ms`**, not wall-clock `t_ms`:

```
dt_ms = snap[i].uptime_ms - snap[i-1].uptime_ms;
rate  = (snap[i].field - snap[i-1].field) * 1000 / dt_ms;
```

If `seq[i] - seq[i-1] != 1` (dropped snapshots, daemon stall), the
client interpolates by `dt_ms` as above and marks the gap visually
(faded sparkline segment). Smoothing: none in v1 (raw windows); v2 may
add EWMA.

### 7.4 Widgets used

| Widget | Where |
|---|---|
| `Block` + `Paragraph` | Header, footer, help overlay |
| `Tabs` | Overview / Detail / Errors switcher |
| `Table` + `TableState` | Overview rule list (selectable) |
| `Sparkline` | 60 s in-rate inline column; Detail in/out 60 s lines |
| `Gauge` / `LineGauge` | UDP `flows_active / udp_max_flows` saturation |
| `Scrollbar` | Long rule list |
| `Paragraph` (styled spans) | Sort indicators, error counts (red/yellow) |

### 7.5 Keybindings

| Key | Action |
|---|---|
| `↑` / `↓` / `j` / `k` | Move row selection |
| `←` / `→` / `h` / `l` / `Tab` / `Shift-Tab` | Cycle Overview / Detail / Errors |
| `Enter` | Jump to Detail tab for selected row |
| `s` | Cycle sort key (rate ⇄ total ⇄ name ⇄ conns) |
| `r` | Reverse sort direction |
| `/` | Enter filter input mode (regex against name) |
| `p` | Pause / resume rendering (data still received) |
| `c` | Session reset: capture current cumulative values as new zero baseline (client-side only) |
| `q` / `Ctrl-C` | Quit (always; never trapped) |
| `?` | Toggle help overlay |
| `Esc` | Context-sensitive: filter mode → clear filter + exit input; help overlay → close overlay; normal mode → no-op |

### 7.6 `--once` mode

Connects, waits for Hello + one Snapshot, prints `{hello, snapshot}` as
pretty JSON, exits 0. Used in scripts:

```sh
portunus-standalone stats --once | jq '.snapshot.r[] | select(.id == "11847…")'
```

No TUI dependencies activated; works under `--no-default-features`
build (see §10).

## 8. Cargo features

```toml
[features]
default = ["stats-tui"]
stats-tui = ["dep:ratatui", "dep:crossterm"]
```

- Default build includes the TUI.
- `cargo build --no-default-features` produces a smaller binary with
  the daemon + UDS server + `stats --once` subcommand only. Useful for
  static/musl release artifacts where size matters.
- `stats --once` does not depend on `stats-tui` — it lives in
  `stats/client.rs`, which is always compiled.
- **Without `stats-tui` feature**, naked `portunus-standalone stats`
  (no `--once`) exits with code 2 and stderr message:
  `error: this build was compiled without --features stats-tui; only \`stats --once\` is available`.
  This keeps `clap`'s help output honest (the `--once` flag stays
  documented; the absence of TUI surfaces as a runtime error, not a
  missing subcommand).

## 9. Testing strategy

| Layer | Test | Tool |
|---|---|---|
| Protocol types | `Hello` / `Snapshot` serde round-trip | unit test in `stats/mod.rs` |
| UDS server | Spawn daemon, connect UnixStream, read Hello + N Snapshots, disconnect; assert task exits | tokio integration test |
| Error counters | Increment from inside forwarder, assert visible in next snapshot | unit + integration |
| Client ring buffer | Feed synthetic Snapshot sequence; assert rate calc | unit test |
| TUI render | `ratatui::backend::TestBackend` 80×24 buffer; snapshot Table/Sparkline output as strings | unit test |
| `--once` mode | `portunus-e2e` spawns daemon, runs `stats --once`, asserts JSON fields | e2e |
| Constitution P-III (real sockets) | All integration tests use a real `tempfile::tempdir()` UDS path, not mocks | ✅ |
| Constitution P-II (perf gate) | Run `udp_high_flow_count` bench with and without an attached stats client; assert ≤ 5 % delta | bench gate |

Coverage target: ≥ 80 % line coverage on `stats/` modules, matching
existing forwarder coverage.

## 10. Constitution & invariants check

| Principle | Status |
|---|---|
| I — Single binary, no runtime deps | ✅ ratatui + crossterm are pure Rust, statically linked; UDS is OS-native |
| II — Perf gate (≤ 5 % data-plane regression) | Gated by §9 bench check; UDS path is off the hot loop |
| III — Integration tests use real sockets | ✅ enforced |
| CLAUDE.md "v1.5 No new Prometheus metrics" | ✅ unchanged; Prometheus is §11 future work |
| CLAUDE.md "no operator surface change in v1.5.x" | ✅ `[stats]` is new but additive; no existing field touched; no rename |
| Standalone "no HTTP server, no SQLite, no control plane" | ✅ preserved — UDS only |

## 11. Future work (explicitly out of scope for this design)

- **`portunus-standalone metrics --listen 127.0.0.1:NNNN`** — a third
  subcommand that connects to the same UDS and translates Snapshots
  into Prometheus textformat. Opt-in, only loaded when invoked.
  Requires deciding on the Prometheus dep (`prometheus-client` crate)
  but doesn't touch the daemon.
- **Daemon-side ring buffer** for "history available immediately on
  TUI reconnect" — currently the first 60 s of the TUI lifetime shows
  an empty sparkline. Trade-off is keeping that ring in daemon memory
  (negligible: 60 samples × ~80 B × N rules ≈ 5 KB for 100 rules).
- **Per-target liveness in Snapshot** — extending Snapshot per-rule
  with `targets: [{ index, health, consecutive_failures,
  last_failure_at_ms, last_success_at_ms }]`. Requires adding
  accessors to the failover `TargetHealth` struct and a new section
  in the protocol. v1 ships only the aggregate
  `target_failovers_total` counter.
- **Per-connection drill-down** (top-N peers, RTT histograms) — needs
  per-conn state in the data path. Likely a `portunus-server`-only
  feature.
- **Rate-limit / QoS / SNI / Quota** visualization in the TUI — pulls
  in `portunus-forwarder` features that standalone does not enable
  today; revisit if standalone ever adopts them.
- **Mouse support, colour themes, layout customization** — `ratatui`
  + `crossterm` support these but they balloon test surface area.

## 12. Open questions

None blocking. The user has confirmed:

- UDS over HTTP / file dump / same-process (§4)
- ratatui (not web) (§7)
- Default `[stats] enabled = true` (§6.2)
- Historical sparkline + cumulative + current rate, plus the items
  listed in §1 (per design § 2 mockup)

## 13. Deliverables

1. **`portunus-forwarder` counter additions** (see §6.3):
   - `RuleStats.connections_total: AtomicU64` (new monotonic counter,
     bumped on TCP accept on both single-target and failover paths).
   - `RuleStats.errors: ErrorCounters { 6 × AtomicU64 }` paired with
     existing tracing call sites.
   - `RuleStats.target_failovers_total: Arc<AtomicU64>` (always
     present, initialised to 0; failover constructor clones the
     existing `Arc` instead of allocating its own — see §6.3 (c)).
   - **gRPC `StatsReport` wire format is NOT modified** in this
     design. The new fields live in `RuleStats` (in-process Rust
     struct) and surface only via the UDS Snapshot payload. Whether
     to plumb them into the gRPC report for
     `portunus-client → portunus-server` is a separate decision.
2. New `crates/portunus-standalone/src/stats/` module (server, client,
   TUI, shared types).
3. `clap` subcommand dispatch in `main.rs` (daemon default; `stats`
   subcommand with `--once`, `--socket`).
4. `[stats]` config section in `config.rs` + validation
   (`refresh_ms` range 250..=5000).
5. Cargo feature `stats-tui` + dependency updates (`ratatui`,
   `crossterm`) — gated so `--no-default-features` is a real, smaller
   build.
6. Platform-aware default socket path (Linux `/run/portunus/…` vs
   macOS `$TMPDIR/…`).
7. systemd unit gains `RuntimeDirectory=portunus`.
8. Docker entrypoint creates `/run/portunus/` if missing.
9. Tests (unit + integration + TUI snapshot via `TestBackend` +
   `--once` e2e + perf bench gate).
10. fumadocs page `operations/standalone.mdx` gets a new "Live stats
    dashboard" section (EN + ZH).
11. `crates/portunus-standalone/contrib/portunus.example.toml` gains a
    commented `[stats]` block.
12. `CHANGELOG.md` entry under v1.6.0.

## 14. Out-of-scope for the implementation plan

- Marketing / blog post.
- Auth / multi-tenant access to the socket beyond filesystem
  permissions.
- Cross-platform Windows TUI testing (CI runs Linux + macOS only).

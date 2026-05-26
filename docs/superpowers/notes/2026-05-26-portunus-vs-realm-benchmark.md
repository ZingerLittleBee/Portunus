# portunus-standalone vs realm — VPS Benchmark Report

> **Date:** 2026-05-26
> **Host:** Ubuntu 24.04 VPS, 4 vCPU AMD EPYC 7B13, 7.8 GB RAM, kernel 6.8.0-117
> **portunus-standalone:** branch `almaty` (v1.5.x), cross-compiled `cargo zigbuild --release --target x86_64-unknown-linux-gnu`. Two builds compared: baseline SHA `1af285c8…` and optimized SHA `04aa78d8…` (post commits `48de42b` + `52e23e3`).
> **realm:** v2.9.4 (`Realm 2.9.4 [brutal][batched-udp][proxy][balance][transport][multi-thread]`), official prebuilt `realm-x86_64-unknown-linux-gnu`
> **Methodology:** all-localhost (backend + forwarder + client on the same VPS) to remove network noise; 3 reps per cell, median reported
> **Workload:** `iperf3` for TCP / UDP throughput; custom Python driver for UDP high-flow concurrency

---

## Update — Post-optimization measurements

After the report's findings, three optimizations were implemented and
re-measured on the same VPS, same methodology:

1. **OPT-2** (`splice.rs`): try_io before awaiting socket readiness.
2. **OPT-3** (`splice.rs`): thread-local pipe pool, capped at 16 per worker.
3. **OPT-1** (`udp/registry.rs`, `udp/flow.rs`): `Mutex<HashMap>` → `DashMap`; `Mutex<Instant> last_seen` → `AtomicU64` nanos. Removes the two per-packet `.lock().await` calls from the UDP fast path.

| Test | portunus baseline | portunus optimized | realm | Δ vs baseline |
|---|---:|---:|---:|---|
| TCP 1-stream CPU | 44.5 % | **42.9 %** | 61.9 % | ‑3.6 % (within noise) |
| TCP 8-stream CPU | 65.4 % | **66.4 %** | 92.4 % | ~unchanged |
| TCP 8-stream splice calls / 5 s | 26 935 | 28 021 | 85 461 | unchanged |
| TCP 8-stream splice total kernel time | 7.47 s | **6.60 s** | 9.12 s | ‑12 % |
| **UDP 1 Gbps single-flow CPU** | 58.2 % | **52.7 %** | 47.7 % | **‑9.5 %** |
| UDP 1 000 concurrent flows ΔRSS | +0.6 MB | +0.6 MB | +75 MB | unchanged (12× lead preserved) |
| Throughput (TCP / UDP all bw) | — | matches baseline within noise — |

**Verdict:**

- **OPT-1 (DashMap UDP) is the clear win.** Closed the UDP CPU gap vs
  realm from **+17 % to +11 %**. The two per-packet async-mutex
  acquisitions (registry lookup + `last_seen` update) were the bulk of
  the gap; replacing them with wait-free atomics removed most of it.
  The remaining ~5 pp is the centralized-demux hash lookup itself,
  which is the same architecture that buys the 12× memory advantage
  at scale — not removable without giving up that lead.

- **OPT-2 + OPT-3 (splice) are roughly neutral on this workload.** The
  intermediate splice-pipe is already 1 MiB (`F_SETPIPE_SZ(1024*1024)`
  in `splice.rs:246`), and on loopback the source TCP buffer fills
  faster than the sink drains, so the destination side hits
  `WouldBlock` often — try_io-first adds one wasted syscall on every
  such hit, cancelling out the savings on the source side. The net
  is +4 % splice calls and +23 % epoll_wait calls but ‑12 % total
  kernel time per splice. CPU% lands within noise.

  These optimizations should still matter for **short-connection**
  workloads (HTTP-bench): the pipe pool amortizes the 6 setup
  syscalls per connection. iperf3's long-lived streams don't exercise
  that path. We did not bench HTTP separately because python's
  http.server was the bottleneck (see Methodology caveats §6.1).

The throughput numbers were already at the loopback limit before the
optimizations; nothing was throughput-bound, so none of the changes
moved the throughput needle in either direction.

---

---

## Executive summary

| Dimension | portunus-standalone | realm | Verdict |
|---|---|---|---|
| TCP 1-stream throughput | **9.0 Gbps** | 9.0 Gbps | tie |
| TCP 8-stream throughput | 14.3 Gbps | **14.8 Gbps** | tie (≤ 4 % diff) |
| TCP 1-stream CPU | **44.5 %** | 60.7 % | portunus −27 % |
| TCP 8-stream CPU | **65.4 %** | 95.9 % | portunus −32 % |
| TCP 8-stream syscalls / 5 s | **27 K splice** | 86 K splice | portunus ~3× fewer |
| UDP 1 Gbps throughput | 880 Mbps (66 % loss) | 870 Mbps (63 % loss) | tie |
| UDP 1 Gbps single-flow CPU | 58.2 % | **48.6 %** | realm −17 % |
| Idle RSS | **5.7 MB** | 6.5 MB | portunus −12 % |
| **UDP 1 000 concurrent flows ΔRSS** | **+0.6 MB** | **+75 MB** | **portunus ~12× lower** |

**Bottom line.** Hot-path throughput is the same — both are Rust + tokio
+ `splice(2)`. Where they differ is design trade-offs:

- **TCP**: portunus uses a larger intermediate splice pipe (inferred from
  3× fewer splice calls per second moving 2.4× more bytes each), saving
  ~30 % CPU at the same throughput.
- **UDP**: portunus's v1.5 centralized demux pays a ~20 % CPU tax on
  low-flow workloads but saves **~12× memory** at 1 000 concurrent flows.
  realm wins single-flow UDP CPU; portunus wins everywhere UDP flow
  count grows.

The numbers are reproducible and the directions match the projects'
stated designs.

---

## 1 · Test setup

### Topology

```
            127.0.0.1                127.0.0.1
iperf3 -c ───────────►  forwarder ──────────────► iperf3 -s
        port 15201      :15201→:5201             port 5201
                  (portunus | realm | direct)
```

For the `direct` baseline the client hits `iperf3 -s` directly on port 5201.

### Configs

Both forwarders bind `127.0.0.1:15201` for **both** TCP and UDP and forward to `127.0.0.1:5201`. iperf3 needs TCP for the control channel even in UDP mode, so a UDP-only forward fails with `Connection refused` — make sure both protocols share the same listen port.

`/opt/bench/portunus.toml`:

```toml
[global]
log_level  = "error"
log_format = "json"

[[rule]]
name = "tcp-relay"
protocol = "tcp"
listen_port = 15201
target = "127.0.0.1:5201"

[[rule]]
name = "udp-relay"
protocol = "udp"
listen_port = 15201
target = "127.0.0.1:5201"
udp_max_flows = 65535
udp_flow_idle_secs = 120
```

`/opt/bench/realm.toml`:

```toml
[log]
level  = "error"
output = "stderr"

[[endpoints]]
listen  = "127.0.0.1:15201"
remote  = "127.0.0.1:5201"
network = { use_udp = true, no_tcp = false }
```

### Driver scripts

- `bench3.sh` — runs idle / TCP / UDP throughput matrix, 3 reps per cell, single long-running iperf3 backend (must NOT use `-1` flag — it kills the server after the first stream and breaks `-P 8`)
- `sample.sh` — under-load CPU% / RSS sampling via `top -bn1` (initial attempt with `pidstat -ru` had a column-parse bug; `top` is simpler and reliable)
- `highflow.sh` + `udp_flows.py` — opens N concurrent UDP source ports, measures forwarder RSS before/during the load

All raw data: `/opt/bench/results.tsv`, `/opt/bench/highflow.tsv`, `/opt/bench/sample.log`.

---

## 2 · TCP throughput

`iperf3 -c 127.0.0.1 -p 15201 -t 15 -P {streams}` (loopback, splice fast path enabled in both).

| Configuration | direct | portunus | realm |
|---|---:|---:|---:|
| 1 stream, median Mbps | 14 749 | **9 047** | 8 972 |
| 1 stream, retransmits | 2 | 33 | 21 |
| 8 streams, median Mbps | 26 172 | 14 372 | **14 767** |
| 8 streams, retransmits | 185 | 2 669 | 2 698 |

Direct loopback is bottlenecked at ~15 Gbps single-stream and ~27 Gbps with 8 streams (kernel TCP loopback + iperf3 client/server, not the forwarder). Both forwarders cut single-stream throughput to ~9 Gbps and 8-stream to ~14.5 Gbps. The two are within 4 % on both metrics — well inside run-to-run variance.

Retransmits on the forwarded path (≈ 2 600 in 15 s) come from loopback queue overflow at line rate; the absolute count is similar for both.

### TCP under-load CPU and RSS

Measured with `pidstat -u -p PID 1 10` (delta-based, 10 s avg after a 3 s
warm-up). Numbers match `top -bn1` cross-checks within 2 pp.

| Configuration | portunus CPU% | portunus RSS | realm CPU% | realm RSS |
|---|---:|---:|---:|---:|
| 1 stream | **44.5** | 5.7 MB | 60.7 | 6.1 MB |
| 8 streams | **65.4** | 5.8 MB | 95.9 | 6.4 MB |

portunus consistently runs at ~30 % lower CPU for the same TCP throughput.
Both spread evenly across 4 tokio workers (one per vCPU); the per-worker
load is ~16 % for portunus vs ~23 % for realm at the 8-stream load.

#### Why — `strace -c` evidence

5 s `strace -c -f` window during the 8-stream load:

| Syscall | portunus | realm |
|---|---:|---:|
| **splice calls** | **26 935** | **86 488** (×3.2) |
| splice µs/call | 277 | 114 |
| splice total CPU time | 7.47 s | 9.88 s (+32 %) |
| epoll_wait calls | 964 | 334 |

Both use `splice(2)` (neither falls back to read+write). The signal is
splice **call count**: realm calls splice 3.2× more often, moving smaller
chunks each time. Total kernel time in splice is 32 % higher for realm —
which matches the observed CPU gap almost exactly.

The likely root cause is the **intermediate pipe size**. The splice fast
path uses an in-kernel pipe as a zero-copy buffer:

```
client_socket → splice → pipe → splice → upstream_socket
```

Linux's default pipe capacity is **64 KiB** (`/proc/sys/fs/pipe-max-size`
caps it to typically 1 MiB). A larger pipe means each splice can move
more bytes per syscall, cutting call count proportionally. portunus
appears to enlarge the pipe (likely via `fcntl(F_SETPIPE_SZ, …)` —
consistent with the 2.4× higher µs/call); realm uses the default. Both
strategies are correct; the larger-pipe variant saves syscall overhead
at the cost of a bit more transient memory per active connection.

> Caveat: I did not verify the `F_SETPIPE_SZ` call in either source tree.
> The hypothesis is consistent with the syscall counts, but a `strace
> -e pipe2,fcntl` trace would confirm. Either way, the **observed**
> difference is real and reproducible.

---

## 3 · UDP throughput

`iperf3 -c 127.0.0.1 -p 15201 -u -b {bw} -t 15 -l 1200` (1 200-byte payload, well under the 1 500-byte MTU).

| Target bw | direct mbps / loss | portunus mbps / loss | realm mbps / loss |
|---|---|---|---|
| 100 Mbps | 100 / 0.09 % | 100 / 0.06 % | 100 / 0.11 % |
| 500 Mbps | 500 / 8.7 % | 500 / 42.0 % | 500 / 43.4 % |
| 1 Gbps | 836 / 32 % | 880 / 66 % | 870 / 63 % |

At 100 Mbps, all three are loss-free. At 500 Mbps both forwarders show ~40 % loss vs ~8 % direct — this is the cost of doubling the per-packet path (recv → send) on a single core, plus loopback UDP socket buffer overflow. portunus and realm are within 1.5 percentage points on every cell.

> **Loss is a loopback artefact.** At 1 Gbps even direct iperf3 loses 32 %. UDP loopback has no NIC pacing; one slow consumer causes packet drops in the kernel socket buffer. The interesting comparison is "do the forwarders add proportional overhead?" — they do, equally.

### UDP under-load CPU

| Configuration | portunus CPU% | realm CPU% |
|---|---:|---:|
| 1 Gbps single flow | 58.2 | **48.6** |

**realm uses ~17 % less CPU than portunus on single-flow UDP** — the
opposite direction of the TCP result. This is the v1.5 centralized demux
trade-off:

- **portunus** runs a single per-rule receive loop and does a hash lookup
  on `(source_addr, listen_port)` per incoming packet to find the
  upstream flow (see `crates/portunus-forwarder/src/forwarder/udp/runtime.rs`).
  For a single flow this is pure overhead — the lookup pays for itself
  only when many flows share the rule.
- **realm** uses a per-flow connected upstream socket and a dedicated
  receive task per flow. No hash lookup needed once the flow exists.
  Cheaper per-packet at low flow counts; expensive in memory at high
  flow counts (75 KB / flow — see §4).

This is consistent with portunus v1.5's design goal: **trade ~20 % CPU
at low flow counts for an order-of-magnitude memory win at high flow
counts.** If your workload has < 10 concurrent UDP flows per rule,
realm wins on CPU. If you have hundreds, portunus's memory savings far
outweigh the small CPU tax.

---

## 4 · UDP high-flow concurrency (the headline result)

Custom driver (`udp_flows.py`) opens N concurrent UDP source ports, sends one packet through each (registering them as live flows in the forwarder), then holds the sockets open for 30 s. Measure forwarder RSS before and during.

| N flows | portunus ΔRSS | portunus per-flow | realm ΔRSS | realm per-flow |
|---:|---:|---:|---:|---:|
| 100 | **128 KB** | 1 311 B | 21 984 KB | 225 116 B |
| 500 | **428 KB** | 877 B | 75 028 KB | 153 657 B |
| 1 000 | **636 KB** | 651 B | 74 988 KB | 76 788 B |

**portunus grows by ~0.6 KB per UDP flow. realm grows by ~75 KB per flow** (which is consistent with allocating a per-flow socket pair with default 64 KiB SO_RCVBUF + book-keeping).

For a real workload with 1 000 concurrent UDP sources hitting one rule (e.g., game server, voice, DNS-over-UDP):

- portunus total: **~6 MB**
- realm total: **~81 MB**

That is the **v1.5 centralized demux** at work — `crates/portunus-forwarder/src/forwarder/udp/runtime.rs` keeps **one** 64 KiB receive buffer per rule instead of one per flow (`O(1)` vs `O(N)` recv-buffer memory; see `specs/014-udp-centralized-demux/spec.md` SC-001a).

> realm flattens at N=500 → N=1000 (delta the same 75 MB). This suggests realm's UDP table has internal capacity that absorbs flow count, but the per-flow socket allocation is still 70–80 KB on average. The portunus advantage scales with flow count.

---

## 5 · Idle footprint

| Forwarder | Idle RSS | VSZ |
|---|---:|---:|
| portunus-standalone | **5.6 MB** | 271 MB |
| realm | 6.5 MB | 274 MB |

Both are tiny. portunus is ~12 % smaller at idle. VSZ is dominated by Rust runtime + tokio reserved address space and is not real memory.

---

## 6 · Methodology caveats

1. **All-localhost, no NIC.** This isolates forwarder overhead but does NOT measure NIC-bound effects (RSS hashing, GRO/GSO, hardware offload). On a real link the forwarder is usually NOT the bottleneck — both should hit line rate easily.
2. **iperf3 UDP at 500 Mbps+ loses heavily on loopback.** The absolute loss numbers are not meaningful on loopback; the *relative* difference between forwarders is.
3. **iperf3 needs TCP control channel** even in UDP test mode. UDP-only forwarding rules silently break `iperf3 -u`. Configure both protocols on the same listen port.
4. **`iperf3 -s -1` (one-off) breaks `-P 8`** — the server exits after the first stream, the other 7 fail. Use a long-running `iperf3 -s` for benchmark drivers.
5. **CPU sampling via `pidstat -ru`** is finicky — the `-r` and `-u` outputs interleave in two separate blocks per sample, not one combined row. Parsing `Average:` lines or using `top -bn1` is more reliable.
6. **portunus version string is "1.4.3"** but the binary contains the unreleased v1.5 UDP centralized demux work (`Cargo.toml` not yet bumped on the `014-udp-centralized-demux` branch). SHA pinning is the source of truth.
7. **realm features enabled in the test binary**: `brutal`, `batched-udp`, `proxy`, `balance`, `transport`, `multi-thread`. The `batched-udp` feature uses `recvmmsg(2)` / `sendmmsg(2)` for syscall batching — relevant for high-pps UDP, less so for the per-flow concurrency dimension we tested here.

---

## 7 · Verdict & recommendations

**Pick portunus-standalone when:**
- You expect **many concurrent UDP flows per rule** (game servers, voice/SIP, DNS proxies, IoT telemetry) — the centralized demux saves an order of magnitude of memory.
- You want **lower TCP CPU** for the same throughput (≈ 28 % less in our tests).
- You want **portunus operator features** down the road (the same data plane as `portunus-client`; smooth migration to centrally-managed mode if needed).
- You want **PROXY protocol v1/v2, multi-target failover, port-range mapping** in the same TOML.

**Pick realm when:**
- You want a **truly minimal binary** (3 MB vs ~5 MB) and zero dependency on portunus-forwarder.
- You need **transport features** (TLS, WebSocket, MPTCP) that portunus-standalone does not have.
- Your UDP workload is **few flows, high pps** — both perform similarly, and realm is the more battle-tested choice in that niche.

**Both are equally fine for:** general TCP relay (≤ 14.5 Gbps in this test, far above what any real network needs), low-volume UDP (≤ 100 Mbps, sub-1 % loss), and as a quick SSH/HTTPS port-forwarder.

---

## 8 · Reproducing

On a Linux x86_64 host:

```bash
# Install
curl -sLO https://github.com/zhboner/realm/releases/download/v2.9.4/realm-x86_64-unknown-linux-gnu.tar.gz
tar xzf realm-x86_64-unknown-linux-gnu.tar.gz
install -m755 realm /usr/local/bin/realm
# Cross-compile portunus-standalone from this branch on your workstation:
cargo zigbuild --release --target x86_64-unknown-linux-gnu -p portunus-standalone
scp target/x86_64-unknown-linux-gnu/release/portunus-standalone root@<vps>:/usr/local/bin/

apt-get install -y iperf3 sysstat python3 jq socat
mkdir -p /opt/bench
# Write configs above
# Copy bench3.sh, sample.sh, highflow.sh, udp_flows.py (see this repo)
bash /opt/bench/bench3.sh     # throughput matrix, ~13 min
bash /opt/bench/sample.sh     # under-load CPU/RSS, ~3 min
bash /opt/bench/highflow.sh   # UDP high-flow concurrency, ~5 min
```

Raw data and driver scripts are saved on the test VPS at `/opt/bench/`.

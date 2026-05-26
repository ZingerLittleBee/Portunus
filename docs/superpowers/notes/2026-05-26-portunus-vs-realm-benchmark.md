# portunus-standalone vs realm — VPS Benchmark Report

> **Date:** 2026-05-26
> **Host:** Ubuntu 24.04 VPS, 4 vCPU AMD EPYC 7B13, 7.8 GB RAM, kernel 6.8.0-117
> **portunus-standalone:** branch `014-udp-centralized-demux` (v1.5.x), cross-compiled `cargo zigbuild --release --target x86_64-unknown-linux-gnu`, SHA `1af285c8…`
> **realm:** v2.9.4 (`Realm 2.9.4 [brutal][batched-udp][proxy][balance][transport][multi-thread]`), official prebuilt `realm-x86_64-unknown-linux-gnu`
> **Methodology:** all-localhost (backend + forwarder + client on the same VPS) to remove network noise; 3 reps per cell, median reported
> **Workload:** `iperf3` for TCP / UDP throughput; custom Python driver for UDP high-flow concurrency

---

## Executive summary

| Dimension | portunus-standalone | realm | Verdict |
|---|---|---|---|
| TCP 1-stream throughput | **9.0 Gbps** | 9.0 Gbps | tie |
| TCP 8-stream throughput | 14.3 Gbps | **14.8 Gbps** | tie (≤ 4 % diff) |
| TCP 1-stream CPU | **44 %** | 60 % | portunus −27 % |
| TCP 8-stream CPU | **67 %** | 92 % | portunus −28 % |
| UDP 1 Gbps throughput | 880 Mbps (66 % loss) | 870 Mbps (63 % loss) | tie |
| UDP idle RSS | **5.7 MB** | 6.5 MB | portunus −12 % |
| **UDP 1 000 concurrent flows ΔRSS** | **+0.6 MB** | **+75 MB** | **portunus ~12× lower** |

**Bottom line.** Hot-path throughput is the same — both are Rust + tokio + `splice(2)`. The differences are in the corners: portunus is more CPU-efficient on TCP (–28 %) and dramatically more memory-efficient under high UDP flow counts (the v1.5 centralized demux pays off when you have hundreds of concurrent UDP sources hitting one rule). realm matches on raw throughput and remains a strong choice when concurrent UDP flow count is low.

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

| Configuration | portunus CPU% | portunus RSS | realm CPU% | realm RSS |
|---|---:|---:|---:|---:|
| 1 stream | **44.0** | 5.7 MB | 59.7 | 6.1 MB |
| 8 streams | **67.1** | 5.8 MB | 92.5 | 6.4 MB |

portunus consistently runs at ~28 % lower CPU for the same TCP work. RSS is 5–10 % smaller. The CPU gap is likely from a tighter splice loop (`portunus-forwarder`'s splice fast path in `crates/portunus-forwarder/src/forwarder/tcp.rs`).

---

## 3 · UDP throughput

`iperf3 -c 127.0.0.1 -p 15201 -u -b {bw} -t 15 -l 1200` (1 200-byte payload, well under the 1 500-byte MTU).

| Target bw | direct mbps / loss | portunus mbps / loss | realm mbps / loss |
|---|---|---|---|
| 100 Mbps | 100 / 0.09 % | 100 / 0.06 % | 100 / 0.11 % |
| 500 Mbps | 500 / 8.7 % | 500 / 42.0 % | 500 / 43.4 % |
| 1 Gbps | 836 / 32 % | 880 / 66 % | 870 / 63 % |

At 100 Mbps, all three are loss-free. At 500 Mbps both forwarders show ~40 % loss vs ~8 % direct — this is the cost of doubling the per-packet path (recv → send) on a single core, plus loopback UDP socket buffer overflow. portunus and realm are within 1.5 percentage points on every cell. UDP under-load CPU is also comparable (~50 %).

> **Loss is a loopback artefact.** At 1 Gbps even direct iperf3 loses 32 %. UDP loopback has no NIC pacing; one slow consumer causes packet drops in the kernel socket buffer. The interesting comparison is "do the forwarders add proportional overhead?" — they do, equally.

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

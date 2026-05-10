# Quickstart: UDP forwarding (v0.4.0)

This walkthrough takes you from a clean v0.4.0 build to a forwarding
client that proxies UDP datagrams to an upstream, with the feature's
failure modes (US4 idle eviction, overflow drop) and observability
(per-rule UDP metrics) exercised at the end. Estimated wall-clock
time: **under 2 minutes** on a fresh host pair.

## Prerequisites

- Two hosts (server + client), or one host loopback for the demo.
- `portunus-server` and `portunus-client` v0.4.0 binaries on `$PATH`.
- A UDP echo tool — `ncat -u -l -k -e /bin/cat` works on most
  distributions; macOS users can `brew install nmap` or
  `python3 -c 'import socket;s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM);s.bind(("127.0.0.1",9999))
  while True:
   d,a=s.recvfrom(65535);s.sendto(d,a)'`.

---

## Walkthrough

### 1. Start the upstream UDP echo (the "real" service)

```sh
ncat -u -l -k -e /bin/cat 127.0.0.1 9999  # listens on UDP :9999, echoes back
```

### 2. Provision and start portunus-server + client (one-host)

```sh
portunus-server serve --config-dir /tmp/srv &
SERVER_PID=$!
sleep 1

curl -sS -X POST -H 'content-type: application/json' \
    -d '{"name":"edge-01"}' \
    http://127.0.0.1:7080/v1/clients \
    > /tmp/edge-01.bundle.json

portunus-client --bundle /tmp/edge-01.bundle.json &
CLIENT_PID=$!
sleep 1
```

### 3. Push a UDP rule (the new bit)

```sh
portunus-server push-rule edge-01 6000 127.0.0.1:9999 --protocol udp
# → exit 0, prints rule_id (e.g. 0)
```

### 4. Verify byte-level UDP forwarding works (US1)

```sh
echo 'hello' | ncat -u -w 1 127.0.0.1 6000
# → hello                        (echoed back through the proxy)
```

This is **US1 verified**: a UDP datagram through `:6000` reached the
upstream echo on `:9999` and round-tripped 6 bytes. The
`portunus-client` log shows a `rule.udp_flow_opened` event for the
new flow.

Send from a second source port to confirm flow isolation (US1
acceptance scenario 2):

```sh
( echo 'flow-A' | ncat -u -w 1 -p 40001 127.0.0.1 6000 ) &
( echo 'flow-B' | ncat -u -w 1 -p 40002 127.0.0.1 6000 ) &
wait
# → flow-A
# → flow-B  (each source receives only its own reply)
```

### 5. Verify DNS-target UDP rule (US2)

Add a hosts entry:

```sh
sudo tee -a /etc/hosts <<'EOF'
127.0.0.1   udp.test
EOF
```

Push a DNS-target UDP rule and drive it:

```sh
portunus-server push-rule edge-01 6001 udp.test:9999 --protocol udp
echo 'dns-resolved' | ncat -u -w 1 127.0.0.1 6001
# → dns-resolved
```

The client log shows the same `rule.dns_resolved` event the TCP path
emits — single-flight, TTL-clamped cache, and per-rule DNS-failure
counter all carry over from v0.3.0.

### 6. Verify range-rule UDP (US3)

```sh
portunus-server push-rule edge-01 6010-6019 127.0.0.1:9990-9999 --protocol udp
# → 10 UDP listeners on edge-01, each forwarding to its same-offset
#   upstream port (edge :6010 → upstream :9990, …, edge :6013 → :9993, …)
#   Range rule semantics require equal-length ranges on both sides
#   (carry-over from v0.2.0 — see specs/002-port-range-forward).

# Bring up echoes on the upstream window:
for p in 9990 9991 9992 9993 9994 9995 9996 9997 9998 9999; do
  ncat -u -l -k -e /bin/cat 127.0.0.1 $p &
done

echo 'range-port-3' | ncat -u -w 1 127.0.0.1 6013
# → range-port-3   (lands at upstream :9993)
```

Per-port UDP datagram counters via `--per-port`:

```sh
portunus-server rule-stats <rule_id> --per-port
# {
#   "rule_id": …, "protocol": "udp",
#   "per_port": [
#     { "listen_port": 6010, "datagrams_in": 0, "datagrams_out": 0, "bytes_in": 0, "bytes_out": 0 },
#     …
#     { "listen_port": 6013, "datagrams_in": 1, "datagrams_out": 1, "bytes_in": 13, "bytes_out": 13 },
#     …
#   ]
# }
```

### 7. Verify idle eviction (US4)

```sh
# Send one datagram each from 10 distinct source ports
for p in 50001 50002 50003 50004 50005 50006 50007 50008 50009 50010; do
  echo "src-${p}" | ncat -u -w 0 -p $p 127.0.0.1 6000 &
done
wait

# Inspect active flows (should show 10)
portunus-server rule-stats 0 | grep active_flows
# → active_flows=10

# Wait the configured idle window (default 60s; if you tuned it down
# in server.toml to 30s for testing, wait that long instead)
sleep 75

# active_flows returns to zero
portunus-server rule-stats 0 | grep active_flows
# → active_flows=0
```

This is **US4 verified**: 10 short-lived UDP flows were tracked, then
reaped after the idle window without operator action.

### 8. Verify metric cardinality (one row per rule)

```sh
curl -s 127.0.0.1:7081/metrics | grep -E 'portunus_rule_(active_flows|udp_datagrams|flows_dropped_overflow)'
# → portunus_rule_active_flows{client="edge-01",rule="0"} 0
# → portunus_rule_udp_datagrams_in_total{client="edge-01",rule="0"} 12
# → portunus_rule_udp_datagrams_out_total{client="edge-01",rule="0"} 12
# → (the 1024-port range rule, if pushed, would still be ONE row per collector — SC-004)
```

### 9. Tear down

```sh
portunus-server remove-rule 0
# → metrics rows for rule 0 disappear immediately
curl -s 127.0.0.1:7081/metrics | grep 'rule="0"'
# → (no output)

kill "$CLIENT_PID" "$SERVER_PID"
sudo sed -i '/udp.test$/d' /etc/hosts
```

---

## Verifying SC-001 on a fresh host pair

The same recipe collapsed onto a stopwatch:

| Step                                                         | Wall-clock budget |
|--------------------------------------------------------------|-------------------|
| `portunus-server serve` → `server.listening`                  | < 0.5 s           |
| `POST /v1/clients` → bundle issued                           | < 0.5 s           |
| `portunus-client --bundle …` → control stream up              | < 1.0 s           |
| `push-rule edge-01 6000 127.0.0.1:9999 --protocol udp`       | < 0.1 s           |
| First datagram through proxy (UDP recv → send_to → echo)     | < 0.2 s           |
| **Total**                                                    | **< 2.5 s**       |

SC-001's spec budget is 60 seconds. The recipe above leaves ~57.5 s of
headroom — comfortably catches "we accidentally introduced a
several-second resolver round-trip on first datagram" regressions.

## Verifying SC-002 throughput

Throughput is exercised by the criterion bench, not the quickstart —
but you can sanity-check loopback UDP throughput by hand:

```sh
# Open one flow and pump 60 s of 512-byte datagrams
yes "$(printf '%.0sX' {1..512})" | head -n 3000000 \
  | timeout 60 ncat -u -w 0 127.0.0.1 6000 > /dev/null

# Read final counter values
portunus-server rule-stats 0 | grep datagrams
# → datagrams_in=… datagrams_out=…
```

The rate `datagrams_in / 60` should comfortably exceed 50 000/s on
loopback. The criterion bench is the gate; this is a smoke test.

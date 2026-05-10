# Quickstart — Portunus MVP

**Feature**: 001-tcp-forward-mvp
**Audience**: an operator going from a fresh checkout to "100 MB streamed
through a forwarding rule" in under 5 minutes (the SC-001 target).

This document doubles as the script the e2e test crate (`forward-e2e`)
exercises programmatically.

---

## Prerequisites

- Rust 1.83+ (stable).
- `protoc` available on PATH (or rely on `prost-build`'s vendored copy —
  works out of the box on the typical dev host).
- Two reachable hosts. For local development, two terminals on one host
  with loopback + a second port suffice.

---

## Build

```sh
cargo build --release -p forward-server -p forward-client
# produces:
#   target/release/forward-server
#   target/release/forward-client
```

---

## Step 1 — Start the server (Host A)

```sh
mkdir -p ./srv
./target/release/forward-server --config-dir ./srv \
  --control-listen 0.0.0.0:7443 \
  --operator-http-listen 127.0.0.1:7080 \
  --metrics-listen 127.0.0.1:7081
```

On first launch the server creates `./srv/server.crt`, `./srv/server.key`,
and an empty `./srv/tokens.json`.

It logs (JSON, one line per event):
```
{"timestamp":"…","level":"INFO","event":"process.start",…}
{"timestamp":"…","level":"INFO","event":"tls.cert_generated","fingerprint_sha256":"…"}
{"timestamp":"…","level":"INFO","event":"control.listening","addr":"0.0.0.0:7443"}
{"timestamp":"…","level":"INFO","event":"operator_http.listening","addr":"127.0.0.1:7080"}
{"timestamp":"…","level":"INFO","event":"metrics.listening","addr":"127.0.0.1:7081"}
```

---

## Step 2 — Provision a client (Host A)

```sh
./target/release/forward-server --config-dir ./srv \
  provision-client edge-01 --out ./srv/edge-01.bundle.json
```

Stdout (the path):
```
/abs/path/to/srv/edge-01.bundle.json
```

The bundle file (mode 0600) contains:
```json
{
  "version": 1,
  "client_name": "edge-01",
  "server_endpoint": "<host-A-public-addr>:7443",
  "server_cert_sha256": "f5e7c2a1...",
  "token": "u3VkLm…43chars…"
}
```

Edit `server_endpoint` to the address Host B can reach — defaults to the
hostname returned by the server, which may need adjustment.

Server log (audit):
```
{"event":"audit.provision","request_id":"01HM…","client_name":"edge-01","outcome":"success",…}
```

Transfer `edge-01.bundle.json` to Host B over a channel you trust (`scp`,
`rsync`, etc.).

---

## Step 3 — Start the client (Host B)

```sh
./target/release/forward-client --bundle ./edge-01.bundle.json
```

Client log:
```
{"event":"process.start",…}
{"event":"control.connecting","endpoint":"…:7443",…}
{"event":"control.tls_pinned","fingerprint_sha256":"f5e7c2a1...",…}
{"event":"control.connected",…}
```

Within 5 seconds (SC-001 budget), Host A's server logs:
```
{"event":"client.connected","client_name":"edge-01","remote_addr":"…",…}
```

Verify on Host A:
```sh
./target/release/forward-server --config-dir ./srv list-clients
```
```
NAME      CONNECTED   REMOTE              CONNECTED-AT
edge-01   yes         203.0.113.5:51234   2026-05-06T12:01:30Z
```

✅ User Story 1 acceptance scenario 1 satisfied.

---

## Step 4 — Run a target service (Host C, or anywhere reachable from Host B)

For verification, any TCP server works. Simplest:

```sh
# trivial echo server, in a third terminal somewhere
nc -lk 8080
```

Or an HTTP server: `python3 -m http.server 8080`.

---

## Step 5 — Push a forwarding rule (Host A)

```sh
./target/release/forward-server --config-dir ./srv \
  push-rule edge-01 18080 10.0.0.5:8080
```

Stdout:
```
rule_id=1
```

Server log (in order):
```
{"event":"audit.rule_push","request_id":"01HM…","client_name":"edge-01","rule_id":1,"listen_port":18080,"target":"10.0.0.5:8080","outcome":"success"}
{"event":"rule.activated","client_name":"edge-01","rule_id":1}
```

Client log (Host B):
```
{"event":"rule.received","rule_id":1,"listen_port":18080,…}
{"event":"rule.activated","rule_id":1,"listen_port":18080}
```

✅ User Story 2 acceptance scenario 1 satisfied.

---

## Step 6 — Stream traffic through the rule

From any host reachable to Host B's `18080`:

```sh
# Send 100 MB and verify byte-equality.
dd if=/dev/urandom of=/tmp/in.bin bs=1M count=100
sha256sum /tmp/in.bin

# (assuming `nc -lk 8080 > /tmp/out.bin` on Host C)
nc <host-B> 18080 < /tmp/in.bin

sha256sum /tmp/out.bin   # must match in.bin's hash
```

✅ SC-002 verified by hash match.

Server metrics (Host A, after a stats interval ~5 s):
```sh
curl -s http://127.0.0.1:7081/metrics | grep '^forward_rule'
```
Shows `forward_rule_bytes_in_total{client="edge-01",rule="1"}` ≈ 100 MB.

```sh
./target/release/forward-server --config-dir ./srv rule-stats 1
```
```
rule_id=1 bytes_in=104857600 bytes_out=0 active_connections=0
```

✅ SC-007 verified within ±1 KB tolerance.

---

## Step 7 — Remove the rule, shut down

```sh
./target/release/forward-server --config-dir ./srv remove-rule 1
```

Server log:
```
{"event":"audit.rule_remove","request_id":"…","rule_id":1,"outcome":"success"}
{"event":"rule.removed","client_name":"edge-01","rule_id":1}
```

Client log:
```
{"event":"rule.removed","rule_id":1}
```

`SIGTERM` either binary; both drain in-flight connections up to 30 s, then
exit `0`.

---

## Failure-path quickchecks

### Wrong fingerprint

```sh
# Tamper with the bundle's server_cert_sha256 field, restart client.
./target/release/forward-client --bundle ./tampered-bundle.json
```

Client log:
```
{"event":"control.tls_pinned_mismatch","expected":"…","got":"…"}
```
And exits non-zero. Server logs nothing (handshake never completed).

### Revoke

```sh
./target/release/forward-server --config-dir ./srv revoke edge-01
```

Server log:
```
{"event":"audit.revoke","client_name":"edge-01","outcome":"success"}
{"event":"client.disconnected","client_name":"edge-01","reason":"token_revoked"}
```

Restarting the client with the now-revoked bundle:
```
Client log: {"event":"auth.failure","reason":"token_revoked"}
Server log: {"event":"auth.failure","client_name":"edge-01","reason":"token_revoked"}
```

✅ User Story 1 acceptance scenarios 2 & 3 covered.

### Push to disconnected client

Stop the client, then:
```sh
./target/release/forward-server --config-dir ./srv push-rule edge-01 18080 10.0.0.5:8080
# exit code 4
# stderr: error: client_not_connected
```

✅ User Story 2 acceptance scenario 4 covered.

### Port collision

With a rule already listening on 18080 (or anything else holding it on
Host B):

```sh
./target/release/forward-server --config-dir ./srv push-rule edge-01 18080 10.0.0.5:8080
# exit code 6 (activation_failed)
# stderr: error: activation_failed: port_in_use
./target/release/forward-server --config-dir ./srv list-rules --client edge-01
# rule_id=2 state=Failed(port_in_use) listen_port=18080 …
./target/release/forward-server --config-dir ./srv remove-rule 2
# now the port is free for a new push
```

✅ Q4 lifecycle (no auto-retry; failed rules block port reuse) verified.

---

## End-to-end timing target

A practiced operator running steps 1–6 should finish the round-trip
verification in under 5 minutes from `cargo build --release` to a
matching `sha256sum` (SC-001). The e2e test crate runs the in-process
equivalent in <30 seconds.

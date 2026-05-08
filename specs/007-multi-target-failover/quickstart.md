# Quickstart: Multi-target failover (v0.7.0)

**Feature**: 007-multi-target-failover | **Date**: 2026-05-08

A two-host walkthrough that exercises every primary user story (US1–US4):

1. Push a multi-target rule.
2. Kill the primary upstream and observe automatic failover to the secondary.
3. Restart the primary and observe automatic recovery.
4. Verify per-target stats and the rule-level failover counter via CLI, HTTP, Web UI, and Prometheus.

This walkthrough assumes the v0.6.0 quickstart has been run at least once and the operator already has:

- `forward-server` running on `server-host` with operator HTTP listening on `:7080`.
- `forward-client` running on `edge-host` and registered with the server (so `Hello.client_version >= 0.7.0`).
- A bearer token in `$OPERATOR_TOKEN` with a grant that allows `(client="edge-01", listen_port=8080..=8080, protocol=tcp)`.
- An active probe-friendly L4 endpoint behind `primary.test:80` and `secondary.test:80` — a one-line `nc -lk 80` is enough.

If any of those is missing, run `specs/006-management-web-ui/quickstart.md` first.

---

## Step 1 — Push a multi-target rule

### Via CLI

```sh
forward-server push-rule edge-01 8080 \
    --target primary.test:80 \
    --target secondary.test:80 \
    --health-check-interval-secs 10 \
    --protocol tcp
```

Expected exit code: `0`. Expected last line:

```text
rule 42 activated on edge-01 (listen_port=8080, targets=2)
```

### Via HTTP

```sh
curl -sS -X POST https://server-host:7080/v1/rules \
    -H "Authorization: Bearer $OPERATOR_TOKEN" \
    -H "Content-Type: application/json" \
    -d '{
      "client": "edge-01",
      "listen_port": 8080,
      "listen_port_end": 8080,
      "protocol": "tcp",
      "targets": [
        {"host": "primary.test",   "port": 80, "priority": 0},
        {"host": "secondary.test", "port": 80, "priority": 1}
      ],
      "health_check_interval_secs": 10
    }'
```

Expected response includes `"activation":{"outcome":"activated"}` and the `targets[]` you submitted echoed back.

### Via Web UI

Open `https://server-host:7080/`, log in as the operator, click **Rules → New**, fill in:

- Client: `edge-01`
- Listen port: `8080`
- Protocol: `TCP`
- Target row 0: `primary.test:80`
- Click **Add another target**
- Target row 1: `secondary.test:80`
- Active health check: `10` seconds
- Click **Push rule**

The rule appears in the list with the optional `MT` pill.

## Step 2 — Verify primary is selected and traffic flows

From a third host (or `edge-host` itself):

```sh
echo "ping" | nc -w1 edge-host 8080
```

Whatever `nc -lk 80` is running on `primary.test:80` should print `ping`. The secondary's `nc` should NOT print anything.

Confirm via stats:

```sh
forward-server rule-stats 42 --per-target
```

Expected: `[0] primary.test:80 ... conns=1`, `[1] secondary.test:80 ... conns=0`.

## Step 3 — Trigger failover (US1, P1)

Kill the primary upstream:

```sh
# on whatever host is running `nc -lk 80` for primary.test
pkill -f "nc -lk 80"
```

From the third host, retry the connection:

```sh
echo "ping" | nc -w1 edge-host 8080
```

After the third failed attempt within 30 s (passive failure threshold from FR-008), the next connection attempt lands on `secondary.test:80`. The `nc -lk 80` running for secondary prints `ping`.

Verify the failover counter incremented:

```sh
forward-server rule-stats 42
# expect: target_failovers_total: 1
```

In the Web UI, the `Targets` panel now shows:

- Row 0 (primary): red badge "● Failed", `last_failure_at` timestamp, `consecutive_failures: 3`.
- Row 1 (secondary): green badge "● Healthy", growing `bytes_in/out`.

Verify Prometheus:

```sh
curl -s https://server-host:7080/metrics | grep target_failovers_total
# forward_rule_target_failovers_total{client="edge-01",rule="42"} 1
```

## Step 4 — Recover the primary (US2, P2)

Bring the primary back online:

```sh
nc -lk 80 &     # on the host serving primary.test
```

Within `health_check_interval_secs * 2 = 20` seconds the active probe will rack up two consecutive successes and flip the primary back to `Healthy` (FR-009). The next connection should land on the primary again:

```sh
echo "ping" | nc -w1 edge-host 8080
```

The primary's `nc` prints `ping`. Verify:

```sh
forward-server rule-stats 42
# expect: target_failovers_total: 2  (Healthy→Failed → Failed→Healthy)
```

The Web UI `Targets` panel shows primary back to green "● Healthy", secondary still green "● Healthy" but with its byte counters frozen at the failover-period total. Subsequent traffic increments primary's counters.

## Step 5 — Confirm in-flight connections are not migrated

Open a long-lived connection on the primary, then kill the primary mid-stream:

```sh
# terminal 1: long connection
nc edge-host 8080
# (leave open, type some lines)

# terminal 2: kill primary
pkill -f "nc -lk 80"
```

The in-flight `nc` in terminal 1 hangs — the connection stays bound to the failed primary until the OS closes it (FR-011, "no mid-flight migration"). New connections opened after the kill will go to secondary.

Restart the primary, open a NEW connection, and confirm it lands on the primary.

## Step 6 — Confirm v0.6.0 single-target rules are unchanged

Push a single-target rule via the legacy CLI form on a different listen port:

```sh
forward-server push-rule edge-01 8081 single.test:80 --protocol tcp
forward-server rule-stats 43
# expect: target_failovers_total: 0  (single-target — no failover state)
```

Open the Web UI rule detail page for rule 43 — the `Target` panel renders the "single-target rule — no failover state" note (FR-002 / I-3).

Run the data-plane benchmark to confirm SC-003:

```sh
cd crates/forward-client
cargo bench --bench data_plane -- --baseline v0.6.0
# expect: ≤ 1% regression on the single_target_throughput benchmark
```

## Step 7 — Negative path: malformed pushes

The HTTP layer rejects ambiguous shapes (FR-004):

```sh
# both shapes — should 400 rule_shape_conflict
curl -sS -X POST https://server-host:7080/v1/rules \
    -H "Authorization: Bearer $OPERATOR_TOKEN" \
    -H "Content-Type: application/json" \
    -d '{
      "client":"edge-01","listen_port":8082,"listen_port_end":8082,"protocol":"tcp",
      "target_host":"a.test","target_port":80,
      "targets":[{"host":"b.test","port":80}]
    }' | jq .
# {"error":"rule_shape_conflict",...}

# duplicate (host,port) — should 400 targets_duplicate
curl -sS -X POST https://server-host:7080/v1/rules \
    -H "Authorization: Bearer $OPERATOR_TOKEN" \
    -H "Content-Type: application/json" \
    -d '{
      "client":"edge-01","listen_port":8082,"listen_port_end":8082,"protocol":"tcp",
      "targets":[{"host":"x.test","port":80},{"host":"x.test","port":80}]
    }' | jq .
# {"error":"targets_duplicate",...}
```

## Step 8 — Cleanup

```sh
forward-server remove-rule 42
forward-server remove-rule 43
```

The Web UI rules list updates within the next SSE tick.

---

## Coverage map (which step proves which spec item)

| Step | User Story / FR / SC |
|---|---|
| 1 | US4 (CLI + HTTP + UI surfaces all accept multi-target shape); FR-004; FR-020 |
| 2 | US1 acceptance #3 (priority 0 selected when both healthy) |
| 3 | US1 acceptance #1, #2 (failover after threshold); FR-006, FR-008, FR-010, FR-018; SC-001 |
| 4 | US2 acceptance #1, #3 (recovery + counter reflects 2 transitions); FR-009, FR-011; SC-002 |
| 5 | US2 acceptance #2 (no mid-flight migration); FR-011 |
| 6 | US4 acceptance #1 (v0.6.0 byte-identity); FR-002, FR-003; SC-003 |
| 7 | FR-004, FR-005 (validation) |
| 8 | regression — rule lifecycle still clean after multi-target use |

---

## Troubleshooting

**Failover never happens, even after the primary is killed**:

- Check the active probe interval — if unset, you need ≥ 3 end-user-driven connect failures within 30 s to trip the threshold (FR-008). For a quick walkthrough, set `health_check_interval_secs: 10` so the probe trips on its own within 30 s.
- Check `forward_rule_target_failovers_total` is being scraped — if the counter is incrementing in `/metrics` but the Web UI doesn't update, the SSE channel from spec 006 may be disconnected.

**Recovery never happens, even after primary is back online**:

- Active probe must be enabled on the rule (`health_check_interval_secs`). With passive-only, recovery requires 2 consecutive end-user-driven successes (FR-009) — meaning at least 2 new connections must arrive AND the forwarder must attempt the primary first. The selection rule for all-Failed targets (FR-007) attempts the highest-priority Failed target on every new connection, so after the primary is back you'll recover within 2 new connections.

**`multi_target_unsupported_by_client` error on push**:

- The target client is still on v0.6.0 and doesn't speak the `targets` field. Upgrade the client binary on `edge-host` and reconnect.

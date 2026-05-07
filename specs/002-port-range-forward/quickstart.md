# Quickstart — Port-Range Forwarding (002)

**Feature**: 002-port-range-forward
**Audience**: An operator who already has a working v0.1.0 deployment
(see `specs/001-tcp-forward-mvp/quickstart.md`) and wants to upgrade to
v0.2.0 to use range rules.

---

## TL;DR

```sh
# (one time) upgrade the binaries — no data migration, no config changes
sudo install -m 0755 target/release/forward-server /usr/local/bin/forward-server
sudo install -m 0755 target/release/forward-client /usr/local/bin/forward-client
sudo systemctl restart forward-server forward-client

# push a 51-port range, same offset
forward-server push-rule edge-01 30000-30050 10.0.0.5:30000-30050
# → prints rule_id, e.g. 8

# verify
forward-server list-rules
# → one row showing PORT=30000-30050 SIZE=51 STATE=active

# observe (aggregate, default)
forward-server rule-stats 8

# diagnose a specific port (on-demand, no Prometheus impact)
forward-server rule-stats 8 --per-port

# remove
forward-server remove-rule 8
```

---

## Prerequisites

- A working v0.1.0 install. Specifically:
  - `forward-server` running on the operator host.
  - `forward-client` running on the edge host with a valid
    `client.bundle.json`.
  - Loopback HTTP API reachable on `127.0.0.1:7080` (or whatever
    `operator_http_listen` is set to).
- Both binaries are upgraded together. **Range rules MUST NOT be
  pushed to a v0.1.0 client**; the wire fields are additive but the
  v0.1.0 client only binds the `listen_port` field, silently dropping
  the rest of the range (no migration safety net at the binary
  level — operators must upgrade the client first).
- Optional: review `range_rule_max_ports` in `server.toml`. The
  default `1024` matches the Linux default soft `RLIMIT_NOFILE`. On
  hardened systemd units (the v0.1.0 service file caps `LimitNOFILE`
  through hardening directives), confirm that
  `LimitNOFILE >= range_rule_max_ports + headroom_for_connections`.

---

## Day-1 verification

### Push a range rule

```sh
forward-server push-rule edge-01 30000-30050 10.0.0.5:30000-30050
```

Expected:
- Stdout: a single integer rule_id.
- Server log (journalctl -u forward-server): one
  `event=audit.rule_push` line with
  `range_size=51 listen_port_end=30050`.
- Client log: one `event=rule.activated` line with
  `range_size=51`. Should appear within ~1 s for a 51-port range
  (well under the default `--ack-timeout` of 2 s).

### Drive traffic

```sh
# from any machine that can reach the edge host on port 30025:
echo hello | nc edge-host 30025
# → upstream:30025 should observe the connection
```

Test the offset semantics by sending to `30000`, `30025`, `30050`,
all of which forward to the corresponding upstream port.

### Observe aggregate stats

```sh
forward-server rule-stats <rule_id>
# → one row, bytes_in/bytes_out summed across all 51 ports
```

### Diagnose per-port (on-demand)

```sh
forward-server rule-stats <rule_id> --per-port
# → aggregate row + one row per port (51 rows)
```

This is a CLI-only feature. Prometheus `/metrics` is unaffected: it
still exposes one series per per-rule collector per rule, regardless
of range size (verify via `curl http://127.0.0.1:7081/metrics | grep
forward_rule_bytes_in_total | wc -l` → unchanged after pushing the
range).

### Remove and verify port release

```sh
forward-server remove-rule <rule_id>
# wait the configured drain (default 30 s)

# on the edge host, verify any port in the range is now free:
ss -ltn | grep ':3001[0-9] '
# → should be empty
```

---

## Day-2 ops

### Configuring the cap

In `server.toml`:

```toml
range_rule_max_ports = 4096   # raise; only meaningful if LimitNOFILE is also raised
```

Restart the server (`systemctl restart forward-server`) for the new
value to apply. Existing rules are unaffected; the cap is checked at
push time only.

### Conflict semantics

A push that overlaps any port already bound by another `Active` /
`Failed` rule for the same client returns exit code `5`
(`port_in_use`) with a message naming the offending port:

```sh
$ forward-server push-rule edge-01 30005-30015 10.0.0.5:40005-40015
error: port_in_use (port 30005 already in use by rule 8 on edge-01)
$ echo $?
5
```

This is the same exit code single-port `port_in_use` returns; v0.1.0
operator scripts that handle exit 5 work unchanged.

### Re-pushing a removed range

After `remove-rule`, the freed ports become available again immediately
(after the drain completes). A re-push using any subset of those ports
succeeds.

### Rolling back a v0.2.0 server to v0.1.0

Safe **iff** no range rules have been pushed since the upgrade. If
you're unsure, before downgrade run:

```sh
forward-server list-rules --format json | jq '.[] | select(.range_size > 1)'
```

If empty, downgrade is safe. Otherwise remove the range rules first.
(This applies only when the server persists rules to disk; the v0.1.0
shipping default is in-memory rules, in which case a server restart
loses all rules anyway.)

---

## Verifying SC-001 on a fresh host pair

This is the planned acceptance test, mirroring spec 001's SC-001
"5-minute fresh deploy" budget but for a 100-port range:

```sh
# 1. provision client on the server
forward-server provision-client edge-01 --out edge-01.bundle.json

# 2. SCP the bundle to the edge host
scp edge-01.bundle.json edge-host:/etc/forward/client.bundle.json

# 3. start the client
ssh edge-host 'systemctl start forward-client'

# 4. push a 100-port range
time forward-server push-rule edge-01 30000-30099 10.0.0.5:30000-30099
# → real time should be sub-second on a healthy stream

# 5. drive a connection through any port in the range
time echo ping | nc edge-host 30050
```

Total wall clock from step 1 to step 5 should be well under 5 minutes
on a fresh host pair (matching SC-001 from spec 001).

---

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| `exit 3 (invalid_target): range_inverted` | listen end < listen start, or target similarly | Re-check the `start-end` syntax |
| `exit 3: exceeds_cap (101 > 100)` | range size > `range_rule_max_ports` | Raise the cap (with corresponding `LimitNOFILE` raise) or split the rule |
| `exit 5: port_in_use (port 30025 already in use by rule 7)` | overlap with an existing rule | Remove the overlapping rule first, or pick a non-overlapping range |
| Range rule activates but some ports show 0 bytes in `--per-port` | normal — ports without traffic show zeros | Drive traffic; the per-port snapshot updates every `stats_report_interval_secs` |
| `rule.failed reason=permission_denied:30025` | ports < 1024 need `CAP_NET_BIND_SERVICE` | The systemd unit already grants this for forward-client; verify with `systemctl show forward-client \| grep AmbientCap` |

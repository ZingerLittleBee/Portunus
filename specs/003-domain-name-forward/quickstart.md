# Quickstart: Domain-name forwarding targets (v0.3.0)

This walkthrough takes you from a clean v0.3.0 build to a forwarding
client that proxies traffic to a DNS-named upstream, with the
feature's failure modes (US2) and observability (US4) exercised at the
end. Estimated wall-clock time: **under 2 minutes** on a fresh host
pair.

## Prerequisites

- Two hosts (server + client), or one host loopback for the demo. The
  recipe below uses a single Linux host with everything on loopback —
  topology is not load-bearing for this feature.
- `portunus-server` and `portunus-client` v0.3.0 binaries on `$PATH`.
- A way to control DNS for one test name. The recipe uses `/etc/hosts`
  for a hermetic demo; in production this is your normal DNS provider.

---

## Walkthrough

### 1. Set up the demo DNS entry

Add a hosts entry mapping a test name to localhost (so the demo runs
without external DNS):

```sh
sudo tee -a /etc/hosts <<'EOF'
127.0.0.1   echo.test
EOF
```

### 2. Start the upstream echo server (the "real" service)

In one terminal:

```sh
ncat -lk 41000 -c 'cat'        # listens on :41000, echoes inbound bytes
```

### 3. Provision and start the portunus-server + client (one-host)

```sh
portunus-server serve --config-dir /tmp/srv &
SERVER_PID=$!

# Wait for server.listening
sleep 1

# Provision a client
curl -sS -X POST -H 'content-type: application/json' \
    -d '{"name":"edge-01"}' \
    http://127.0.0.1:7080/v1/clients \
    > /tmp/edge-01.bundle.json

portunus-client --bundle /tmp/edge-01.bundle.json &
CLIENT_PID=$!

# Wait for the client to connect
sleep 1
```

### 4. Push a DNS-target rule (the new bit)

```sh
portunus-server push-rule edge-01 8080 echo.test:41000
# → exit 0, prints rule_id (e.g. 0)
```

The `push-rule` call returns immediately. **No DNS query has fired
yet** — resolution is lazy on first connection (FR-002).

### 5. Verify byte-level forwarding works through the DNS-target rule

```sh
echo 'hello' | ncat 127.0.0.1 8080
# → hello                  (echoed back through the proxy)
```

This is **US1 verified**: an end-user connection through `:8080`
reached `echo.test` (resolved to `127.0.0.1`) and round-tripped 6
bytes. The first call also primed the resolver cache (FR-003).

### 6. Verify FR-002 invariant: rule still works under DNS failure (US2)

Break DNS for `echo.test` by removing the hosts entry:

```sh
sudo sed -i '/echo.test$/d' /etc/hosts
```

Within ~1 cache-window (or sooner if you bounce the cache by
restarting the client), fresh connections start failing fast — but
the **rule stays Active**:

```sh
portunus-server list-rules
# → rule 0   client edge-01   listen 127.0.0.1:8080   target echo.test:41000   status Active

echo 'hello' | timeout 5 ncat 127.0.0.1 8080
# → connection refused within 3 s (SC-003), exit 1
```

The DNS-failure counter has bumped:

```sh
curl -sS http://127.0.0.1:7081/metrics | grep dns_failures
# → portunus_rule_dns_failures_total{client="edge-01",rule="0"} 1
```

This is **US2 + US4 verified** — the rule absorbed the DNS outage,
end-user connections fail with a bounded latency, and the operator
sees the failed rule in the metrics dashboard, not the log stream.

Now restore DNS:

```sh
sudo tee -a /etc/hosts <<'EOF'
127.0.0.1   echo.test
EOF
```

…and the next connection succeeds again with no operator action
against the client (US2 acceptance scenario 3):

```sh
echo 'hello' | ncat 127.0.0.1 8080
# → hello
```

### 7. (Optional) Verify the IPv6 opt-in (US3)

Add a dual-stack hosts entry:

```sh
sudo tee -a /etc/hosts <<'EOF'
127.0.0.1   dual.test
::1         dual.test
EOF
```

Push two rules to the same name, one default, one IPv6-preferred:

```sh
portunus-server push-rule edge-01 9080 dual.test:41000
portunus-server push-rule edge-01 9081 dual.test:41000 --prefer-ipv6
```

Drive each through `ncat` and inspect `portunus-client` logs for the
`rule.dns_resolved.chosen_addr` fields — the first will show
`127.0.0.1`, the second `::1`. Same DNS name, two rules, two address
families — observably different (US3 acceptance scenarios 1 & 2).

### 8. Tear down

```sh
kill "$CLIENT_PID" "$SERVER_PID"
sudo sed -i '/echo.test$/d' /etc/hosts
sudo sed -i '/dual.test$/d' /etc/hosts
```

---

## Verifying SC-001 on a fresh host pair

The same recipe collapsed onto a stopwatch:

| Step                                                | Wall-clock budget |
|-----------------------------------------------------|-------------------|
| `portunus-server serve` → `server.listening`         | < 0.5 s           |
| `POST /v1/clients` → bundle issued                  | < 0.5 s           |
| `portunus-client --bundle …` → control stream up     | < 1.0 s           |
| `push-rule edge-01 8080 echo.test:41000`            | < 0.1 s           |
| First byte through proxy (DNS resolved + connected) | < 1.0 s           |
| **Total**                                           | **< 3 s**         |

SC-001's spec budget is 60 seconds. The recipe above leaves ~57 s of
headroom — comfortably catches "we accidentally introduced a
several-second resolver round-trip on first connect" regressions.

## Verifying SC-002 (cache propagation)

```sh
# Push rule pointing at name → IP_v1
echo "127.0.0.1   moving.test" | sudo tee -a /etc/hosts
portunus-server push-rule edge-01 8888 moving.test:41000
echo 'hello' | ncat 127.0.0.1 8888           # primes the cache, IP_v1

# Switch the name to IP_v2 (still loopback, but a different listener)
sudo sed -i 's/127.0.0.1   moving.test/127.0.0.2   moving.test/' /etc/hosts
ncat -lk 127.0.0.2 41000 -c 'cat' &

# Wait at most one cache-ceiling (5 min default)
sleep 300

echo 'hello' | ncat 127.0.0.1 8888
# → hello   (now reaching 127.0.0.2:41000 via the re-resolved name)
```

SC-002's spec budget is "at most one configured cache ceiling, with
zero operator action" — the recipe above gives that exactly.

## Verifying SC-006 (Prometheus cardinality)

After driving any number of failures through any number of rules:

```sh
curl -sS http://127.0.0.1:7081/metrics \
    | grep '^portunus_rule_dns_failures_total' \
    | wc -l
# → exactly one row per rule that has ever attempted DNS resolution
```

Add 50 rules, drive 1000 connections each through deliberately broken
DNS names, and the row count stays at 50 — never exploding by attempt
count or resolved-address count.

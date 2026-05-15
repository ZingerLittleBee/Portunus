# Local Multi-User Demo Harness — Design

**Date:** 2026-05-16
**Status:** Approved (pending written-spec review)
**Topic:** A repeatable, manually-driven local environment that stands up
`portunus-server` + multiple `portunus-client` edges across multiple RBAC
users, pushes real forwarding rules, and verifies real data forwarding
end-to-end through locally-started upstream services.

## Purpose

Provide a one-command local environment to exercise and observe the full
control + data plane across a realistic multi-tenant shape:

- **Data plane:** real TCP traffic flows `client:listen → upstream service`
  and back, verified with a real connect/send/echo round-trip (no synthetic
  load-loop simulator).
- **Control plane:** the server pushes per-owner rules and reports
  per-owner / per-rule connection and byte statistics; RBAC isolation is
  exercised (wrong-token push rejected, cross-tenant read returns 403).

This is a **manual demo / debugging environment**, not an automated test
added to `cargo test`. It stays running after a one-shot correctness pass so
the operator can keep poking at it by hand.

## Non-Goals (YAGNI)

- No synthetic payload load-loop / throughput simulator.
- No throughput or latency benchmarking (that is `cargo bench` territory).
- No changes to the `portunus-e2e` crate.
- No daemonization / service install.
- No new Rust code — pure orchestration of existing CLI subcommands.

## Project Model (confirmed)

- **client** is the data plane: it listens locally and forwards to a
  target. Data does **not** flow through the server.
- **server** is the control plane: pushes rules into the data-dir, monitors
  client liveness, and serves per-owner statistics via the operator HTTP
  API.
- RBAC: `_superadmin` bootstraps; `user-add` creates a tenant user;
  `grant-add` authorizes a user to push rules to a named client within a
  listen-port range and protocol set; `credential-issue` mints that user's
  bearer token. Rules are owned by the issuing user; cross-tenant reads
  return 403.
- Default endpoint alignment requires no ephemeral-port discovery: gRPC
  control listen defaults to `127.0.0.1:7443`, operator HTTP to
  `127.0.0.1:7080`, and `provision-client` advertises `127.0.0.1:7443` —
  all three align out of the box.

## Topology (defaults: 3 users × 2 rules = 6 rules)

```
DATA_DIR=/tmp/portunus-demo
server: gRPC 127.0.0.1:7443   operator HTTP 127.0.0.1:7080

user1 (independent edge-1)  grant: tcp 18001-18002
    rule 18001 -> 127.0.0.1:19001   (nc -lk : real upstream service)
    rule 18002 -> 127.0.0.1:19002
user2 (independent edge-2)  grant: tcp 18003-18004
    rule 18003 -> 127.0.0.1:19003
    rule 18004 -> 127.0.0.1:19004
user3 (independent edge-3)  grant: tcp 18005-18006
    rule 18005 -> 127.0.0.1:19005
    rule 18006 -> 127.0.0.1:19006
```

Each user owns an independent `portunus-client` (maximum isolation, closest
to a real multi-tenant deployment). Upstream "intranet services" are real
processes started locally by the harness as `nc -lk <port>` echo
listeners — real services that are really connected to, not a fabricated
load stream.

## Deliverables

- `scripts/demo.sh` — the orchestration script.
- `Makefile` target `demo:` with a `##` help comment matching existing
  Makefile style; body delegates to `bash scripts/demo.sh $(DEMO_ARGS)`.

No other files change. The harness uses an isolated `/tmp/portunus-demo`
data dir so it never collides with `make dev`'s `/tmp/portunus-dev`.

## Startup Sequence (scripts/demo.sh)

Runs in the foreground, holds the terminal, and tears everything down on
exit via `trap 'kill 0' INT TERM EXIT` (plus a `pkill -P` sweep for any
`nc -lk` children that escape the process group).

1. Dependency preflight: assert `nc`, `jq`, `curl`, `cargo` exist; clear
   error if any is missing.
2. Reset `/tmp/portunus-demo` (skipped with `--keep`); run
   `bootstrap-superadmin --name ops`, capture the superadmin bearer token.
3. Start `serve` in the background (gRPC `127.0.0.1:7443`, operator HTTP
   `127.0.0.1:7080`); poll the server log for the `server.listening`
   structured event with a timeout.
4. Start one `nc -lk 1900X` per rule (default 6) as the real upstream
   services; record PIDs.
5. For each user (default 3): `user-add` → `grant-add` for that user's
   own listen-port range → `credential-issue`, capturing each user's
   bearer token.
6. For each user: `provision-client edge-N` → start `portunus-client
   --bundle …` in the background → poll operator HTTP `GET /v1/clients`
   until `edge-N` reports `connected: true` (timeout-guarded).
7. For each user: push that user's rules via the operator HTTP API using
   **that user's own token**. Includes one negative RBAC assertion: a push
   with the wrong user's token onto a foreign listen-port range must be
   rejected.
8. **One-shot real verification pass:** for each of the listen ports, open
   a real TCP connection, send a unique marker string, and assert the exact
   marker echoes back → per-rule PASS/FAIL line.
9. Pull `/v1/rules/<rule_id>/stats` per rule using the owning user's token;
   print per-owner / per-rule `bytes_in` / `bytes_out` and `connected`.
   Assert a cross-tenant stats read returns 403.
10. Print an operations cheat-sheet (each user's token, each rule_id, the
    listen-port table, log file paths, stop instructions) and `wait` to
    hold the environment open for manual interaction.

## Manual Interaction (after the one-shot pass)

The cheat-sheet tells the operator exactly how to keep testing by hand:

```
demo ready
  data plane:   nc 127.0.0.1 18001   (type text -> echoed back)
  monitoring:   curl -s -H "Authorization: Bearer $USER1_TOKEN" \
                  http://127.0.0.1:7080/v1/rules/<rule_id>/stats | jq
  logs:         server -> /tmp/portunus-demo/server.log
                edge-N -> /tmp/portunus-demo/edge-N.log
  stop:         Ctrl-C in this terminal (tears down server/clients/nc)
```

Tokens and rule ids are captured by the script and substituted into the
printed instructions so nothing has to be looked up by hand.

## Script Behavior Details

- `set -euo pipefail`.
- Every background child PID is recorded; `trap 'kill 0' INT TERM EXIT` is
  the primary teardown, with a `pkill -P $$` sweep as a backstop for
  `nc -lk` listeners.
- Readiness is polled with explicit timeouts (server listening ≤ 10 s;
  each client `connected` ≤ 10 s). On timeout the relevant `.log` tail is
  dumped and the script exits non-zero.
- Configurable flags (all defaulted):
  `--users N` (default 3),
  `--rules-per-user K` (default 2),
  `--base-listen P` (default 18001; targets derive as 19001+),
  `--keep` (reuse an existing `/tmp/portunus-demo`, skip bootstrap),
  `--disable-splice` (inject `PORTUNUS_DISABLE_SPLICE=1` into server +
  client children for future v1.3.0 fast-path A/B triage).
- Isolated data dir `/tmp/portunus-demo`; never touches `make dev` state.

## Error Handling

- Missing dependency → fail fast with the missing tool named.
- Server not listening within timeout → dump `server.log` tail, exit 1.
- A client never reaching `connected` → dump that `edge-N.log` tail,
  exit 1.
- A rule push failing, or the negative RBAC assertion not being rejected,
  or the one-shot echo mismatch, or the cross-tenant 403 assertion failing
  → printed as a FAIL line; the script still proceeds to print the
  cheat-sheet and hold open (so the operator can investigate live) but
  records overall non-zero status for the eventual exit.

## Verification of the Harness Itself

- Run `make demo`; expect 6 PASS lines, the negative RBAC push rejected,
  and the cross-tenant read returning 403.
- Manually `nc 127.0.0.1 18001`, type text, confirm echo; re-query that
  rule's stats and confirm `bytes_in`/`bytes_out` increased.
- Ctrl-C; confirm no `portunus-server`, `portunus-client`, or `nc -lk`
  processes survive.
- Re-run with `--keep` and confirm it reuses the existing data dir without
  re-bootstrapping.

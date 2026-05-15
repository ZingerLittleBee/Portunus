# Local Multi-User Demo Harness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build `scripts/demo.sh` + a `make demo` target that stands up `portunus-server` plus N RBAC users each owning an independent `portunus-client` edge with K real forwarding rules, verifies real end-to-end TCP forwarding through locally-started echo upstreams, exercises RBAC isolation, then holds the environment open for manual use.

**Architecture:** Pure shell orchestration of existing `portunus-server` / `portunus-client` CLI subcommands. No new Rust code. The script is structured as sourceable bash functions guarded by a `BASH_SOURCE` main check so each function is unit-testable in isolation before the full pipeline is wired in the final task.

**Tech Stack:** bash, `cargo`, `portunus-server`/`portunus-client` CLIs, operator HTTP API (`curl` + `jq`), a real echo upstream (`socat` if present, else an embedded `python3` TCP echo), bash `/dev/tcp` for round-trip verification.

---

## Key Facts (verified against the codebase — do not re-derive)

- **Binaries:** built with `cargo build`. `portunus-server`'s `build.rs` errors if `webui/dist/index.html` is missing **unless `PORTUNUS_SKIP_WEBUI=1` is set at build time**. So build with `PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-server -p portunus-client`. Default profile is `release` → binaries at `target/release/portunus-{server,client}`. Honor `CARGO_PROFILE=dev` → `target/debug/...`.
- **Endpoint defaults align with no ephemeral discovery:** gRPC control listen defaults to `127.0.0.1:7443`; `provision-client` advertises `127.0.0.1:7443`; operator HTTP is set explicitly via `serve --operator-http-listen 127.0.0.1:7080`.
- **Operator CLI auth:** `user-add`, `grant-add`, `credential-issue` authenticate to `--http-endpoint` (default `127.0.0.1:7080`) via the `PORTUNUS_OPERATOR_TOKEN` env var (they call the live server's HTTP API — safe while the server runs). Missing → stderr `error: unauthenticated (set PORTUNUS_OPERATOR_TOKEN ...)`.
- **CRITICAL — offline CLIs vs live server:** `provision-client` and `push-rule` use the OFFLINE path (`build_offline_state` → opens `state.db` directly). Run while the server is live they fail with `store: store_in_use: <data-dir>/state.db`, and even if they didn't they would desync the server's in-memory token/rule cache. While the server is running, ALL provisioning and rule pushes MUST go through the HTTP API (`POST /v1/clients`, `POST /v1/rules`) — exactly as `crates/portunus-e2e/tests/common/mod.rs` (`provision_client_http`, `push_rule_http`) does. The harness keeps the server live for `connected`/stats monitoring, so it uses HTTP throughout.
- **`bootstrap-superadmin` stdout** on success: a single line `superadmin user_id=_superadmin token=<43-char>` (token is URL-safe base64: `[A-Za-z0-9_-]`).
- **`credential-issue --format json`** (default) prints the raw API JSON; the token is at `.token` (43-char, returned exactly once). Per `specs/005-multi-user-rbac/contracts/operator-api.md` `POST /v1/users/{id}/credentials`.
- **`grant-add`** requires `--user-id --client <name> --listen-port-start --listen-port-end [--protocols tcp]`.
- **Provision via HTTP:** `POST /v1/clients` (bearer = superadmin token) body `{"name":"<edge>","address":"127.0.0.1"}` → response body IS the credential bundle JSON; write it to `<edge>.bundle.json`.
- **Push rule via HTTP:** `POST /v1/rules` (bearer = owning user's token → rule owned by that user) body `{"client","listen_port","target_host","target_port","protocol":"tcp"}`. A 2xx means success; 403 means RBAC denied (used for the negative check).
- **Rule listing / stats:** operator HTTP `GET /v1/rules?client=<name>` (bearer = owner token) returns the owner's rules; `GET /v1/rules/<rule_id>/stats` returns `bytes_in`/`bytes_out`. Cross-tenant read returns HTTP 403. (Confirmed by `crates/portunus-e2e/tests/rbac_smoke.rs` and `tests/common/mod.rs`.)
- **Echo upstream correctness:** macOS/BSD `nc -lk` does **not** echo received bytes back, so it cannot serve as the round-trip upstream. Use `socat TCP-LISTEN:<port>,bind=127.0.0.1,reuseaddr,fork EXEC:cat` when `socat` exists, else an embedded `python3` threaded TCP echo. This is a deliberate refinement of the design's "nc -lk" wording; the intent (a real upstream service that really echoes) is preserved.
- **Isolated state dir:** `/tmp/portunus-demo` (never `/tmp/portunus-dev`, which `make dev` owns).

## Flags (final, supersedes any informal list)

`--users N` (default 3), `--rules-per-user K` (default 2), `--base-listen P` (default 18001; upstream ports derive as `P + 1000 + idx` i.e. 19001+), `--keep` (reuse existing `/tmp/portunus-demo`, skip wipe + bootstrap), `--disable-splice` (inject `PORTUNUS_DISABLE_SPLICE=1` into server + client children), `--no-wait` (run the full pipeline + one-shot verification, print summary, exit with status instead of holding open — used by automated self-check and every verification step below), `--dry-run` (print resolved topology and exit 0 without starting anything). Unknown flag → print usage to stderr, exit 2.

## File Structure

- **Create `scripts/demo.sh`** — the entire harness. Sourceable: all logic in functions; the last line is `if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then main "$@"; fi` so tests can `source` it without executing.
- **Modify `Makefile`** — add `DEMO_ARGS ?=`, a `demo:` target with a `##` help comment, and add `demo` to `.PHONY`.

No other files change.

---

### Task 1: Scaffold — flags, preflight, dry-run

**Files:**
- Create: `scripts/demo.sh`

- [ ] **Step 1: Write the script scaffold**

Create `scripts/demo.sh` with exactly this content:

```bash
#!/usr/bin/env bash
# Local multi-user demo harness for portunus.
# Stands up server + N users each with an independent edge client and K
# real forwarding rules, verifies real end-to-end TCP forwarding, then
# holds the environment open for manual use. See
# docs/superpowers/specs/2026-05-16-local-demo-harness-design.md
set -euo pipefail

# ---- defaults --------------------------------------------------------------
USERS=3
RULES_PER_USER=2
BASE_LISTEN=18001
KEEP=0
DISABLE_SPLICE=0
NO_WAIT=0
DRY_RUN=0

DATA_DIR=/tmp/portunus-demo
GRPC_ENDPOINT=127.0.0.1:7443
HTTP_ENDPOINT=127.0.0.1:7080
CARGO_PROFILE="${CARGO_PROFILE:-release}"

usage() {
  cat >&2 <<'EOF'
usage: scripts/demo.sh [options]
  --users N              number of RBAC users / edges (default 3)
  --rules-per-user K     rules per user (default 2)
  --base-listen P        first client listen port (default 18001)
  --keep                 reuse existing /tmp/portunus-demo, skip bootstrap
  --disable-splice       inject PORTUNUS_DISABLE_SPLICE=1 into children
  --no-wait              run + verify + exit (do not hold open)
  --dry-run              print resolved topology and exit
EOF
}

parse_args() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --users) USERS="$2"; shift 2;;
      --rules-per-user) RULES_PER_USER="$2"; shift 2;;
      --base-listen) BASE_LISTEN="$2"; shift 2;;
      --keep) KEEP=1; shift;;
      --disable-splice) DISABLE_SPLICE=1; shift;;
      --no-wait) NO_WAIT=1; shift;;
      --dry-run) DRY_RUN=1; shift;;
      -h|--help) usage; exit 0;;
      *) echo "error: unknown argument: $1" >&2; usage; exit 2;;
    esac
  done
}

log()  { printf '[demo] %s\n' "$*" >&2; }
die()  { printf '[demo] FATAL: %s\n' "$*" >&2; exit 1; }

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "missing required command: $1"
}

preflight() {
  require_cmd cargo
  require_cmd curl
  require_cmd jq
  if ! command -v socat >/dev/null 2>&1 \
     && ! command -v python3 >/dev/null 2>&1; then
    die "need either 'socat' or 'python3' for the echo upstream"
  fi
}

# Print the resolved topology (used by --dry-run and the cheat-sheet).
print_topology() {
  log "topology:"
  log "  data-dir   = ${DATA_DIR}"
  log "  gRPC       = ${GRPC_ENDPOINT}"
  log "  operator   = http://${HTTP_ENDPOINT}"
  log "  users      = ${USERS}  rules/user = ${RULES_PER_USER}"
  local u r idx listen target
  idx=0
  for ((u = 1; u <= USERS; u++)); do
    for ((r = 1; r <= RULES_PER_USER; r++)); do
      listen=$((BASE_LISTEN + idx))
      target=$((BASE_LISTEN + 1000 + idx))
      log "  user${u}/edge-${u}  rule listen ${listen} -> 127.0.0.1:${target}"
      idx=$((idx + 1))
    done
  done
}

main() {
  parse_args "$@"
  if [[ "${DRY_RUN}" == "1" ]]; then
    print_topology
    exit 0
  fi
  preflight
  print_topology
}

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  main "$@"
fi
```

- [ ] **Step 2: Make it executable and syntax-check**

Run:
```bash
chmod +x scripts/demo.sh
bash -n scripts/demo.sh && echo SYNTAX_OK
command -v shellcheck >/dev/null 2>&1 && shellcheck scripts/demo.sh || echo "shellcheck not installed (skipped)"
```
Expected: prints `SYNTAX_OK`; shellcheck (if installed) reports no errors.

- [ ] **Step 3: Verify dry-run and bad-arg behavior**

Run:
```bash
scripts/demo.sh --dry-run --users 2 --rules-per-user 2
echo "exit=$?"
scripts/demo.sh --bogus; echo "exit=$?"
```
Expected: first prints a 4-rule topology (`user1/edge-1`, `user2/edge-2`, listen 18001-18004 → 19001-19004) and `exit=0`; second prints `error: unknown argument: --bogus` + usage and `exit=2`.

- [ ] **Step 4: Commit**

```bash
git add scripts/demo.sh
git commit -m "feat(demo): scaffold local demo harness (flags, preflight, dry-run)"
```

---

### Task 2: Process lifecycle + echo upstream + round-trip helpers

**Files:**
- Modify: `scripts/demo.sh`

- [ ] **Step 1: Add lifecycle/echo/roundtrip helpers**

In `scripts/demo.sh`, insert the following block immediately **after** the `require_cmd()` function and **before** `preflight()`:

```bash
# ---- process lifecycle -----------------------------------------------------
declare -a CHILD_PIDS=()

track() { CHILD_PIDS+=("$1"); }

cleanup() {
  local pid
  for pid in "${CHILD_PIDS[@]+"${CHILD_PIDS[@]}"}"; do
    [[ -n "${pid}" ]] && kill "${pid}" 2>/dev/null || true
  done
  # Backstop: kill anything still in this script's process group.
  pkill -P $$ 2>/dev/null || true
}
trap cleanup INT TERM EXIT

# Generic readiness poller. Usage: wait_ready <timeout_s> <desc> <cmd...>
# Succeeds when <cmd...> exits 0; fails (exit 1) after timeout.
wait_ready() {
  local timeout="$1" desc="$2"; shift 2
  local deadline
  deadline=$(( $(date +%s) + timeout ))
  while (( $(date +%s) < deadline )); do
    if "$@" >/dev/null 2>&1; then return 0; fi
    sleep 0.3
  done
  log "timeout (${timeout}s) waiting for: ${desc}"
  return 1
}

# ---- echo upstream ---------------------------------------------------------
# Starts a real TCP echo service on 127.0.0.1:<port> in the background and
# track()s its PID. Prefers socat; falls back to an embedded python3 echo.
PY_ECHO='import socket,threading,sys
p=int(sys.argv[1])
s=socket.socket(); s.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1)
s.bind(("127.0.0.1",p)); s.listen(64)
def h(c):
    try:
        while True:
            d=c.recv(65536)
            if not d: break
            c.sendall(d)
    finally:
        c.close()
while True:
    c,_=s.accept()
    threading.Thread(target=h,args=(c,),daemon=True).start()'

start_echo_upstream() {
  local port="$1"
  if command -v socat >/dev/null 2>&1; then
    socat "TCP-LISTEN:${port},bind=127.0.0.1,reuseaddr,fork" EXEC:cat \
      >>"${DATA_DIR}/upstream-${port}.log" 2>&1 &
  else
    python3 -c "${PY_ECHO}" "${port}" \
      >>"${DATA_DIR}/upstream-${port}.log" 2>&1 &
  fi
  track "$!"
}

# ---- TCP round-trip --------------------------------------------------------
# Connects to 127.0.0.1:<port>, sends a unique marker line, and returns 0
# iff the exact marker echoes back within 3s. Uses bash /dev/tcp.
tcp_roundtrip() {
  local port="$1" marker="$2" line
  exec 3<>"/dev/tcp/127.0.0.1/${port}" || return 1
  printf '%s\n' "${marker}" >&3 || { exec 3>&- 3<&-; return 1; }
  IFS= read -r -t 3 line <&3 || { exec 3>&- 3<&-; return 1; }
  exec 3>&- 3<&-
  [[ "${line}" == "${marker}" ]]
}
```

- [ ] **Step 2: Syntax-check**

Run:
```bash
bash -n scripts/demo.sh && echo SYNTAX_OK
command -v shellcheck >/dev/null 2>&1 && shellcheck scripts/demo.sh || true
```
Expected: `SYNTAX_OK`.

- [ ] **Step 3: Functionally test the echo + round-trip helpers in isolation**

Run:
```bash
mkdir -p /tmp/portunus-demo
bash -c '
  source scripts/demo.sh
  start_echo_upstream 19099
  wait_ready 5 "echo:19099" bash -c "exec 3<>/dev/tcp/127.0.0.1/19099" \
    || { echo HELPER_FAIL; exit 1; }
  if tcp_roundtrip 19099 "marker-abc-123"; then
    echo HELPER_OK
  else
    echo HELPER_FAIL
  fi
'
```
Expected: prints `HELPER_OK`. The `EXIT` trap fires on subshell end and kills the echo upstream — confirm none survive:
```bash
pgrep -fl 'socat .*19099|python3 -c .*19099' || echo NO_LEAK
```
Expected: `NO_LEAK`.

- [ ] **Step 4: Commit**

```bash
git add scripts/demo.sh
git commit -m "feat(demo): add lifecycle, echo-upstream, and tcp round-trip helpers"
```

---

### Task 3: Build binaries, bootstrap superadmin, start server

**Files:**
- Modify: `scripts/demo.sh`

- [ ] **Step 1: Add build/bootstrap/server functions**

In `scripts/demo.sh`, insert the following block immediately **before** the `main()` function:

```bash
# ---- binaries --------------------------------------------------------------
SERVER_BIN=""
CLIENT_BIN=""
SUPERADMIN_TOKEN=""

resolve_bins() {
  local sub="release"
  [[ "${CARGO_PROFILE}" == "dev" ]] && sub="debug"
  SERVER_BIN="target/${sub}/portunus-server"
  CLIENT_BIN="target/${sub}/portunus-client"
}

build_binaries() {
  log "building binaries (profile=${CARGO_PROFILE}, PORTUNUS_SKIP_WEBUI=1)..."
  local flags=()
  [[ "${CARGO_PROFILE}" == "release" ]] && flags+=(--release)
  PORTUNUS_SKIP_WEBUI=1 cargo build "${flags[@]+"${flags[@]}"}" \
    -p portunus-server -p portunus-client
  [[ -x "${SERVER_BIN}" ]] || die "server binary missing: ${SERVER_BIN}"
  [[ -x "${CLIENT_BIN}" ]] || die "client binary missing: ${CLIENT_BIN}"
}

# ---- data dir + bootstrap --------------------------------------------------
prepare_data_dir() {
  if [[ "${KEEP}" == "1" ]]; then
    [[ -d "${DATA_DIR}" ]] || die "--keep set but ${DATA_DIR} does not exist"
    log "reusing existing data dir ${DATA_DIR}"
  else
    rm -rf "${DATA_DIR}"
    mkdir -p "${DATA_DIR}"
  fi
}

bootstrap_superadmin() {
  if [[ "${KEEP}" == "1" && -f "${DATA_DIR}/.superadmin-token" ]]; then
    SUPERADMIN_TOKEN="$(cat "${DATA_DIR}/.superadmin-token")"
    log "reusing captured superadmin token"
    return 0
  fi
  local out
  out="$("${SERVER_BIN}" --data-dir "${DATA_DIR}" \
        bootstrap-superadmin --name ops 2>>"${DATA_DIR}/server.log")"
  SUPERADMIN_TOKEN="$(printf '%s\n' "${out}" \
    | grep -oE 'token=[A-Za-z0-9_-]{20,}' | head -1 | cut -d= -f2)"
  [[ -n "${SUPERADMIN_TOKEN}" ]] \
    || die "could not parse superadmin token from: ${out} (check ${DATA_DIR}/server.log)"
  printf '%s' "${SUPERADMIN_TOKEN}" >"${DATA_DIR}/.superadmin-token"
}

check_ports() {
  local endpoint host port
  for endpoint in "${GRPC_ENDPOINT}" "${HTTP_ENDPOINT}"; do
    host="${endpoint%:*}"
    port="${endpoint##*:}"
    if (exec 3<>"/dev/tcp/${host}/${port}") 2>/dev/null; then
      die "port ${port} (${endpoint}) already in use — kill the stale portunus-server first"
    fi
  done
}

# ---- server ----------------------------------------------------------------
start_server() {
  log "starting server (gRPC ${GRPC_ENDPOINT}, http ${HTTP_ENDPOINT})..."
  local extra_env=(PORTUNUS_SKIP_WEBUI=1)
  [[ "${DISABLE_SPLICE}" == "1" ]] && extra_env+=(PORTUNUS_DISABLE_SPLICE=1)
  env "${extra_env[@]}" "${SERVER_BIN}" --data-dir "${DATA_DIR}" \
    serve --operator-http-listen "${HTTP_ENDPOINT}" \
    >>"${DATA_DIR}/server.log" 2>&1 &
  track "$!"
}

server_listening() {
  grep -q 'server.listening' "${DATA_DIR}/server.log" 2>/dev/null
}

wait_server() {
  wait_ready 15 "server.listening" server_listening || {
    log "--- server.log (tail) ---"; tail -n 30 "${DATA_DIR}/server.log" >&2
    die "server did not become ready"
  }
}
```

- [ ] **Step 2: Wire these into `main()` (temporary, behind --no-wait early-exit for testing)**

Replace the body of `main()` with:

```bash
main() {
  parse_args "$@"
  if [[ "${DRY_RUN}" == "1" ]]; then print_topology; exit 0; fi
  preflight
  check_ports
  resolve_bins
  print_topology
  build_binaries
  prepare_data_dir
  bootstrap_superadmin
  start_server
  wait_server
  log "server ready; superadmin token captured (${#SUPERADMIN_TOKEN} chars)"
  if [[ "${NO_WAIT}" == "1" ]]; then
    log "stopping here (--no-wait, pipeline incomplete: through Task 3)"
    exit 0
  fi
}
```

- [ ] **Step 3: Syntax-check**

Run: `bash -n scripts/demo.sh && echo SYNTAX_OK`
Expected: `SYNTAX_OK`.

- [ ] **Step 4: Functionally verify build + bootstrap + server readiness**

Run:
```bash
scripts/demo.sh --no-wait --users 1 --rules-per-user 1
echo "exit=$?"
grep -c 'server.listening' /tmp/portunus-demo/server.log
test -s /tmp/portunus-demo/.superadmin-token && echo TOKEN_CAPTURED
pgrep -fl portunus-server || echo NO_SERVER_LEAK
```
Expected: build runs, `exit=0`, `server.listening` count ≥ 1, `TOKEN_CAPTURED`, and `NO_SERVER_LEAK` (the EXIT trap killed the server).

- [ ] **Step 5: Commit**

```bash
git add scripts/demo.sh
git commit -m "feat(demo): build binaries, bootstrap superadmin, start server"
```

---

### Task 4: Create users, grants, and per-user credentials

**Files:**
- Modify: `scripts/demo.sh`

- [ ] **Step 1: Add the user-provisioning function**

In `scripts/demo.sh`, insert immediately **before** `main()`:

```bash
# ---- users / grants / credentials -----------------------------------------
# USER_TOKENS[i] = bearer token for useri (1-based); EDGE_NAMES[i]=edge-i
declare -a USER_TOKENS=()
declare -a EDGE_NAMES=()

# First listen port for user u (1-based), contiguous K-port block.
user_listen_start() { echo $(( BASE_LISTEN + (($1 - 1) * RULES_PER_USER) )); }
user_listen_end()   { echo $(( $(user_listen_start "$1") + RULES_PER_USER - 1 )); }

add_users() {
  local u uid edge port_start port_end tok
  for ((u = 1; u <= USERS; u++)); do
    uid="user${u}"
    edge="edge-${u}"
    EDGE_NAMES[u]="${edge}"
    port_start="$(user_listen_start "${u}")"
    port_end="$(user_listen_end "${u}")"

    PORTUNUS_OPERATOR_TOKEN="${SUPERADMIN_TOKEN}" \
      "${SERVER_BIN}" --data-dir "${DATA_DIR}" \
      user-add "${uid}" --display-name "Demo ${uid}" \
      --http-endpoint "${HTTP_ENDPOINT}" >/dev/null 2>>"${DATA_DIR}/server.log"

    PORTUNUS_OPERATOR_TOKEN="${SUPERADMIN_TOKEN}" \
      "${SERVER_BIN}" --data-dir "${DATA_DIR}" \
      grant-add --user-id "${uid}" --client "${edge}" \
      --listen-port-start "${port_start}" --listen-port-end "${port_end}" \
      --protocols tcp --http-endpoint "${HTTP_ENDPOINT}" >/dev/null 2>>"${DATA_DIR}/server.log"

    tok="$(PORTUNUS_OPERATOR_TOKEN="${SUPERADMIN_TOKEN}" \
      "${SERVER_BIN}" --data-dir "${DATA_DIR}" \
      credential-issue "${uid}" --format json \
      --http-endpoint "${HTTP_ENDPOINT}" 2>>"${DATA_DIR}/server.log" | jq -r '.token')"
    [[ -n "${tok}" && "${tok}" != "null" ]] \
      || die "no token for ${uid} (check ${DATA_DIR}/server.log)"
    USER_TOKENS[u]="${tok}"
    log "provisioned ${uid} (grant ${edge} tcp ${port_start}-${port_end})"
  done
}
```

- [ ] **Step 2: Wire into `main()`**

In `main()`, replace the `if [[ "${NO_WAIT}" == "1" ]]; then ... exit 0; fi` block with:

```bash
  add_users
  log "users provisioned: ${USERS}"
  if [[ "${NO_WAIT}" == "1" ]]; then
    log "stopping here (--no-wait, pipeline incomplete: through Task 4)"
    exit 0
  fi
```

- [ ] **Step 3: Syntax-check**

Run: `bash -n scripts/demo.sh && echo SYNTAX_OK`
Expected: `SYNTAX_OK`.

- [ ] **Step 4: Functionally verify users + grants exist**

Run:
```bash
scripts/demo.sh --no-wait --users 2 --rules-per-user 2
echo "exit=$?"
TOK=$(cat /tmp/portunus-demo/.superadmin-token)
curl -s -H "Authorization: Bearer $TOK" \
  http://127.0.0.1:7080/v1/users | jq -r '.[].user_id' 2>/dev/null | sort
```
Expected: `exit=0`; the user list includes `user1` and `user2` (and `_superadmin`). (The server is killed by the trap when the script exits; this curl runs against the still-up server only if it raced — acceptable; the authoritative check is that the script reached "users provisioned: 2" with `exit=0`. If the curl returns nothing because the server already exited, re-run and read `/tmp/portunus-demo/server.log` for `user_add` audit lines instead.)

- [ ] **Step 5: Commit**

```bash
git add scripts/demo.sh
git commit -m "feat(demo): create users, grants, per-user credentials"
```

---

### Task 5: Provision and start edge clients, wait for connected

**Files:**
- Modify: `scripts/demo.sh`

- [ ] **Step 1: Add edge provisioning/start/wait functions**

In `scripts/demo.sh`, insert immediately **before** `main()`:

```bash
# ---- edge clients ----------------------------------------------------------
edge_connected() {
  local edge="$1"
  curl -s -H "Authorization: Bearer ${SUPERADMIN_TOKEN}" \
    "http://${HTTP_ENDPOINT}/v1/clients" 2>/dev/null \
    | jq -e --arg n "${edge}" \
        'any(.[]; .client_name == $n and .connected == true)' \
        >/dev/null 2>&1
}

start_edges() {
  local u edge bundle code
  for ((u = 1; u <= USERS; u++)); do
    edge="${EDGE_NAMES[u]}"
    bundle="${DATA_DIR}/${edge}.bundle.json"
    # Provision via the LIVE server's HTTP API. The offline `provision-client`
    # CLI opens state.db directly — it fails with `store_in_use` while the
    # server holds the SQLite lock, and would also desync the server's
    # in-memory token cache (per crates/portunus-e2e/tests/common/mod.rs).
    code="$(curl -s -o "${bundle}" -w '%{http_code}' \
      -X POST -H "Authorization: Bearer ${SUPERADMIN_TOKEN}" \
      -H 'Content-Type: application/json' \
      -d "{\"name\":\"${edge}\",\"address\":\"127.0.0.1\"}" \
      "http://${HTTP_ENDPOINT}/v1/clients")"
    [[ "${code}" == 2?? ]] \
      || die "provision ${edge} failed (HTTP ${code}); see ${bundle}"

    local extra_env=()
    [[ "${DISABLE_SPLICE}" == "1" ]] && extra_env+=(PORTUNUS_DISABLE_SPLICE=1)
    env "${extra_env[@]+"${extra_env[@]}"}" "${CLIENT_BIN}" --bundle "${bundle}" \
      >>"${DATA_DIR}/${edge}.log" 2>&1 &
    track "$!"
  done
  for ((u = 1; u <= USERS; u++)); do
    edge="${EDGE_NAMES[u]}"
    wait_ready 15 "${edge} connected" edge_connected "${edge}" || {
      log "--- ${edge}.log (tail) ---"
      tail -n 30 "${DATA_DIR}/${edge}.log" >&2
      die "${edge} never reported connected"
    }
    log "${edge} connected"
  done
}
```

- [ ] **Step 2: Wire into `main()`**

In `main()`, replace the Task-4 `if [[ "${NO_WAIT}" == "1" ]]; then ... fi` block with:

```bash
  start_edges
  log "all ${USERS} edges connected"
  if [[ "${NO_WAIT}" == "1" ]]; then
    log "stopping here (--no-wait, pipeline incomplete: through Task 5)"
    exit 0
  fi
```

- [ ] **Step 3: Syntax-check**

Run: `bash -n scripts/demo.sh && echo SYNTAX_OK`
Expected: `SYNTAX_OK`.

- [ ] **Step 4: Functionally verify all edges connect**

Run:
```bash
scripts/demo.sh --no-wait --users 3 --rules-per-user 2
echo "exit=$?"
grep -c 'connected' /tmp/portunus-demo/edge-1.log /tmp/portunus-demo/edge-3.log
```
Expected: `exit=0`; the script log shows `edge-1 connected` … `edge-3 connected` and `all 3 edges connected`. (If a client fails to connect the script dies non-zero with the tailed `edge-N.log` — that is the failure signal.)

- [ ] **Step 5: Commit**

```bash
git add scripts/demo.sh
git commit -m "feat(demo): provision and start edge clients, await connected"
```

---

### Task 6: Start echo upstreams, push rules, negative RBAC check

**Files:**
- Modify: `scripts/demo.sh`

- [ ] **Step 1: Add upstream + rule-push + negative-RBAC functions**

In `scripts/demo.sh`, insert immediately **before** `main()`:

```bash
# ---- rules -----------------------------------------------------------------
# RULE_LISTEN[g], RULE_TARGET[g], RULE_USER[g], RULE_EDGE[g], RULE_ID[g]
declare -a RULE_LISTEN=() RULE_TARGET=() RULE_USER=() RULE_EDGE=() RULE_ID=()

start_upstreams() {
  local u r idx listen target
  idx=0
  for ((u = 1; u <= USERS; u++)); do
    for ((r = 1; r <= RULES_PER_USER; r++)); do
      listen=$((BASE_LISTEN + idx))
      target=$((BASE_LISTEN + 1000 + idx))
      start_echo_upstream "${target}"
      RULE_LISTEN[idx]="${listen}"
      RULE_TARGET[idx]="${target}"
      RULE_USER[idx]="${u}"
      RULE_EDGE[idx]="${EDGE_NAMES[u]}"
      idx=$((idx + 1))
    done
  done
  # Give every echo listener a moment to bind.
  local p
  for p in "${RULE_TARGET[@]}"; do
    wait_ready 5 "echo:${p}" bash -c "exec 3<>/dev/tcp/127.0.0.1/${p}" \
      || die "echo upstream ${p} did not bind"
  done
}

push_rules() {
  local g u tok edge listen target code
  for g in "${!RULE_LISTEN[@]}"; do
    u="${RULE_USER[g]}"
    tok="${USER_TOKENS[u]}"
    edge="${RULE_EDGE[g]}"
    listen="${RULE_LISTEN[g]}"
    target="${RULE_TARGET[g]}"
    # Push via the LIVE server's HTTP API using the owning user's token so
    # the rule is owned by that user (RBAC). The offline `push-rule` CLI
    # conflicts with the server's SQLite lock and bypasses per-user auth.
    code="$(curl -s -o /dev/null -w '%{http_code}' \
      -X POST -H "Authorization: Bearer ${tok}" \
      -H 'Content-Type: application/json' \
      -d "{\"client\":\"${edge}\",\"listen_port\":${listen},\"target_host\":\"127.0.0.1\",\"target_port\":${target},\"protocol\":\"tcp\"}" \
      "http://${HTTP_ENDPOINT}/v1/rules")"
    [[ "${code}" == 2?? ]] \
      || die "push rule failed: ${edge}:${listen} user${u} (HTTP ${code})"
  done
  # Resolve rule ids per owner via the operator HTTP API.
  for g in "${!RULE_LISTEN[@]}"; do
    u="${RULE_USER[g]}"
    tok="${USER_TOKENS[u]}"
    edge="${RULE_EDGE[g]}"
    listen="${RULE_LISTEN[g]}"
    RULE_ID[g]="$(curl -s -H "Authorization: Bearer ${tok}" \
      "http://${HTTP_ENDPOINT}/v1/rules?client=${edge}" \
      | jq -r --argjson lp "${listen}" \
          '.[] | select((.listen_port // .listen) == $lp) | .rule_id // .id' \
      | head -1)"
    [[ -n "${RULE_ID[g]}" && "${RULE_ID[g]}" != "null" ]] \
      || die "could not resolve rule_id for ${edge}:${listen}"
  done
  log "pushed ${#RULE_LISTEN[@]} rules across ${USERS} users"
}

# user1's token attempting to push into user2's granted port range MUST be
# rejected. Requires USERS >= 2; skipped otherwise.
assert_negative_rbac() {
  if (( USERS < 2 )); then
    log "negative RBAC check skipped (need >= 2 users)"
    return 0
  fi
  local foreign_listen code
  foreign_listen="$(user_listen_start 2)"
  code="$(curl -s -o /dev/null -w '%{http_code}' \
    -X POST -H "Authorization: Bearer ${USER_TOKENS[1]}" \
    -H 'Content-Type: application/json' \
    -d "{\"client\":\"${EDGE_NAMES[2]}\",\"listen_port\":${foreign_listen},\"target_host\":\"127.0.0.1\",\"target_port\":1,\"protocol\":\"tcp\"}" \
    "http://${HTTP_ENDPOINT}/v1/rules")"
  if [[ "${code}" == 2?? ]]; then
    die "RBAC FAIL: user1 token pushed into user2's range (${foreign_listen}, HTTP ${code})"
  fi
  log "negative RBAC OK: cross-user push rejected (HTTP ${code})"
}
```

- [ ] **Step 2: Wire into `main()`**

In `main()`, replace the Task-5 `if [[ "${NO_WAIT}" == "1" ]]; then ... fi` block with:

```bash
  start_upstreams
  push_rules
  assert_negative_rbac
  log "rules active; RBAC isolation verified"
  if [[ "${NO_WAIT}" == "1" ]]; then
    log "stopping here (--no-wait, pipeline incomplete: through Task 6)"
    exit 0
  fi
```

- [ ] **Step 3: Syntax-check**

Run: `bash -n scripts/demo.sh && echo SYNTAX_OK`
Expected: `SYNTAX_OK`.

- [ ] **Step 4: Functionally verify rules push + negative RBAC**

Run:
```bash
scripts/demo.sh --no-wait --users 3 --rules-per-user 2
echo "exit=$?"
```
Expected: `exit=0`; log contains `pushed 6 rules across 3 users` and `negative RBAC OK: cross-user push rejected`. A non-zero exit with `RBAC FAIL` means isolation is broken (real finding, not a script bug — stop and report).

- [ ] **Step 5: Commit**

```bash
git add scripts/demo.sh
git commit -m "feat(demo): start echo upstreams, push per-user rules, RBAC check"
```

---

### Task 7: End-to-end verification, stats, cross-tenant 403, hold open

**Files:**
- Modify: `scripts/demo.sh`

- [ ] **Step 1: Add verification / stats / cheat-sheet functions**

In `scripts/demo.sh`, insert immediately **before** `main()`:

```bash
# ---- end-to-end verification ----------------------------------------------
OVERALL_STATUS=0

verify_forwarding() {
  local g listen marker
  for g in "${!RULE_LISTEN[@]}"; do
    listen="${RULE_LISTEN[g]}"
    marker="demo-$(date +%s)-${listen}-$$"
    if tcp_roundtrip "${listen}" "${marker}"; then
      log "PASS  listen ${listen} (user${RULE_USER[g]}/${RULE_EDGE[g]}) echo ok"
    else
      log "FAIL  listen ${listen} (user${RULE_USER[g]}/${RULE_EDGE[g]}) no echo"
      OVERALL_STATUS=1
    fi
  done
}

print_stats() {
  local g u tok rid body bin bout
  for g in "${!RULE_LISTEN[@]}"; do
    u="${RULE_USER[g]}"; tok="${USER_TOKENS[u]}"; rid="${RULE_ID[g]}"
    body="$(curl -s -H "Authorization: Bearer ${tok}" \
      "http://${HTTP_ENDPOINT}/v1/rules/${rid}/stats")"
    bin="$(printf '%s' "${body}" | jq -r '.bytes_in // 0')"
    bout="$(printf '%s' "${body}" | jq -r '.bytes_out // 0')"
    log "stats user${u}/${RULE_EDGE[g]} listen ${RULE_LISTEN[g]}" \
        "rule=${rid} bytes_in=${bin} bytes_out=${bout}"
  done
}

# user2's token reading user1's rule stats MUST return HTTP 403.
assert_cross_tenant_403() {
  if (( USERS < 2 )); then
    log "cross-tenant 403 check skipped (need >= 2 users)"
    return 0
  fi
  local victim_rid code
  victim_rid="${RULE_ID[0]}"   # owned by user1
  code="$(curl -s -o /dev/null -w '%{http_code}' \
    -H "Authorization: Bearer ${USER_TOKENS[2]}" \
    "http://${HTTP_ENDPOINT}/v1/rules/${victim_rid}/stats")"
  if [[ "${code}" == "403" ]]; then
    log "cross-tenant 403 OK: user2 denied reading user1's rule stats"
  else
    log "RBAC FAIL: cross-tenant read returned ${code}, expected 403"
    OVERALL_STATUS=1
  fi
}

print_cheatsheet() {
  local g u
  log "==================== demo ready ===================="
  print_topology
  for ((u = 1; u <= USERS; u++)); do
    log "user${u} token: ${USER_TOKENS[u]}"
  done
  for g in "${!RULE_LISTEN[@]}"; do
    log "rule ${RULE_ID[g]}  listen ${RULE_LISTEN[g]}  owner user${RULE_USER[g]}"
  done
  log "data plane:   nc 127.0.0.1 ${RULE_LISTEN[0]}   (type text -> echoed)"
  log "monitoring:   curl -s -H \"Authorization: Bearer <user-token>\" \\"
  log "                http://${HTTP_ENDPOINT}/v1/rules/<rule_id>/stats | jq"
  log "logs:         ${DATA_DIR}/server.log , ${DATA_DIR}/edge-*.log"
  log "stop:         Ctrl-C here (tears down server / clients / upstreams)"
  log "===================================================="
}
```

- [ ] **Step 2: Finalize `main()`**

In `main()`, replace the Task-6 `if [[ "${NO_WAIT}" == "1" ]]; then ... fi` block with:

```bash
  verify_forwarding
  print_stats
  assert_cross_tenant_403
  print_cheatsheet
  if [[ "${NO_WAIT}" == "1" ]]; then
    log "exiting (--no-wait); overall status=${OVERALL_STATUS}"
    exit "${OVERALL_STATUS}"
  fi
  log "holding environment open — press Ctrl-C to stop"
  wait
```

- [ ] **Step 3: Syntax-check**

Run: `bash -n scripts/demo.sh && echo SYNTAX_OK`
Expected: `SYNTAX_OK`.

- [ ] **Step 4: Full automated self-check (--no-wait)**

Run:
```bash
scripts/demo.sh --no-wait --users 3 --rules-per-user 2
echo "exit=$?"
```
Expected: 6 `PASS  listen …` lines (no `FAIL`), 6 `stats …` lines, `negative RBAC OK`, `cross-tenant 403 OK`, and `exit=0`.

- [ ] **Step 5: Verify hold-open + clean teardown**

Run:
```bash
( scripts/demo.sh --users 2 --rules-per-user 1 & echo $! >/tmp/demo.pid ) ; sleep 25
cat /tmp/portunus-demo/server.log | grep -q server.listening && echo SERVER_UP
kill -INT "$(cat /tmp/demo.pid)" 2>/dev/null || true
sleep 2
pgrep -fl 'portunus-server|portunus-client' && echo LEAK || echo CLEAN_TEARDOWN
```
Expected: `SERVER_UP`, then `CLEAN_TEARDOWN` (the `INT` trap killed all children). If `LEAK`, the trap/`pkill` path needs fixing before proceeding.

- [ ] **Step 6: Commit**

```bash
git add scripts/demo.sh
git commit -m "feat(demo): end-to-end echo verification, per-owner stats, 403 check, hold open"
```

---

### Task 8: Makefile integration

**Files:**
- Modify: `Makefile`

- [ ] **Step 1: Add `demo` to `.PHONY` and a `DEMO_ARGS` variable**

In `Makefile`, the `.PHONY` line currently reads:

```makefile
.PHONY: help setup webui-install webui-build server-build bootstrap \
        dev-bootstrap serve serve-docker dev backend ui test test-csrf clean
```

Change it to add `demo`:

```makefile
.PHONY: help setup webui-install webui-build server-build bootstrap \
        dev-bootstrap serve serve-docker dev backend ui test test-csrf clean \
        demo
```

Then, immediately **after** the line `CARGO_PROFILE ?= release`, add:

```makefile
DEMO_ARGS   ?=
```

- [ ] **Step 2: Add the `demo` target**

At the end of `Makefile`, append:

```makefile
## --- demo -------------------------------------------------------------------

# Stand up a full multi-user demo: server + N RBAC users each with an
# independent edge client and K real forwarding rules to local echo
# upstreams. Verifies real end-to-end TCP forwarding + RBAC isolation,
# then holds the environment open (Ctrl-C tears everything down). Uses an
# isolated /tmp/portunus-demo data dir — does not touch `make dev` state.
# Override args, e.g.: make demo DEMO_ARGS="--users 5 --rules-per-user 3"
demo:  ## Multi-user demo: server + N edges + K rules, verify + hold open
	@bash scripts/demo.sh $(DEMO_ARGS)
```

- [ ] **Step 3: Verify the help line and Makefile parse**

Run:
```bash
make help | grep demo
```
Expected: a line like `  demo             Multi-user demo: server + N edges + K rules, verify + hold open`.

- [ ] **Step 4: Full end-to-end via make (automated mode)**

Run:
```bash
make demo DEMO_ARGS="--no-wait --users 3 --rules-per-user 2"
echo "exit=$?"
```
Expected: 6 `PASS` lines, `negative RBAC OK`, `cross-tenant 403 OK`, `exit=0`.

- [ ] **Step 5: Manual smoke (interactive, optional but recommended)**

Run `make demo` in one terminal; wait for `demo ready`. In another terminal:
```bash
printf 'hello-demo\n' | nc 127.0.0.1 18001
```
Expected: `hello-demo` echoes back. Then re-query that rule's stats with the printed user1 token and confirm `bytes_in`/`bytes_out` > 0. Ctrl-C the `make demo` terminal; confirm `pgrep -fl 'portunus-server|portunus-client'` is empty.

- [ ] **Step 6: Commit**

```bash
git add Makefile
git commit -m "feat(demo): add 'make demo' target wiring scripts/demo.sh"
```

---

## Self-Review

**Spec coverage:**

- Deliverables `scripts/demo.sh` + `make demo` → Tasks 1-7, Task 8. ✓
- Topology 3 users × independent edge × 2 rules (configurable) → Task 1 flags, Tasks 4-6. ✓
- Real upstream services (not synthetic load) → Task 2 echo helper, Task 6 `start_upstreams`. ✓ (echo via socat/python3; rationale documented under Key Facts — supersedes the spec's literal "nc -lk", same intent).
- No synthetic load loop → none added; verification is a one-shot round-trip. ✓
- Startup sequence steps 1-10 → Tasks 3-7 in order. ✓
- Per-user grants + per-user tokens + per-user rule push → Task 4, Task 6 `push_rules`. ✓
- Negative RBAC (wrong-token push rejected) → Task 6 `assert_negative_rbac`. ✓
- One-shot real verification PASS/FAIL → Task 7 `verify_forwarding`. ✓
- Per-owner/per-rule stats printed → Task 7 `print_stats`. ✓
- Cross-tenant read 403 → Task 7 `assert_cross_tenant_403`. ✓
- Cheat-sheet with tokens/rule ids/ports/logs/stop → Task 7 `print_cheatsheet`. ✓
- Hold open, `trap 'kill 0'`-style teardown, `pkill` backstop → Task 2 `cleanup`/trap, Task 7 `wait`. ✓
- Flags `--users --rules-per-user --base-listen --keep --disable-splice` → Task 1. `--no-wait` and `--dry-run` added as documented testability flags (noted in plan). ✓
- Timeout-guarded readiness + log dump on failure → Task 2 `wait_ready`, Tasks 3/5. ✓
- Isolated `/tmp/portunus-demo`, no collision with `make dev` → Task 1 default, Task 3 `prepare_data_dir`. ✓
- Dependency preflight → Task 1 `preflight`. ✓

**Placeholder scan:** No `TBD`/`TODO`/"similar to"/"add error handling" — every step has complete code or exact commands with expected output. ✓

**Type/identifier consistency:** `SUPERADMIN_TOKEN`, `USER_TOKENS`, `EDGE_NAMES`, `RULE_LISTEN/TARGET/USER/EDGE/ID`, `OVERALL_STATUS`, `track`, `cleanup`, `wait_ready`, `tcp_roundtrip`, `start_echo_upstream`, `resolve_bins`, `SERVER_BIN`/`CLIENT_BIN` are defined once and referenced with identical names/arities across tasks. `main()` is progressively edited; each task states exactly which block it replaces. ✓

**Note on Task 4 verification:** the curl check races the EXIT trap; the plan calls this out and gives the authoritative signal (script reaching its log line with `exit=0`) plus a log-based fallback — not a placeholder, a documented limitation of `--no-wait` self-checks for mid-pipeline tasks.

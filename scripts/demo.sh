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
      "http://${HTTP_ENDPOINT}/v1/clients")" || true
    [[ "${code}" == 2?? ]] \
      || die "provision ${edge} failed (HTTP ${code:-curl-error}); see ${bundle}"

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
      "http://${HTTP_ENDPOINT}/v1/rules")" || true
    [[ "${code}" == 2?? ]] \
      || die "push rule failed: ${edge}:${listen} user${u} (HTTP ${code:-curl-error})"
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
    "http://${HTTP_ENDPOINT}/v1/rules")" || true
  if [[ "${code}" == 2?? ]]; then
    die "RBAC FAIL: user1 token pushed into user2's range (${foreign_listen}, HTTP ${code})"
  fi
  [[ "${code}" == 4?? ]] \
    || die "negative RBAC inconclusive: expected 4xx denial, got HTTP ${code:-curl-error}"
  log "negative RBAC OK: cross-user push rejected (HTTP ${code})"
}

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
  add_users
  log "users provisioned: ${USERS}"
  start_edges
  log "all ${USERS} edges connected"
  start_upstreams
  push_rules
  assert_negative_rbac
  log "rules active; RBAC isolation verified"
  if [[ "${NO_WAIT}" == "1" ]]; then
    log "stopping here (--no-wait, pipeline incomplete: through Task 6)"
    exit 0
  fi
}

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  main "$@"
fi

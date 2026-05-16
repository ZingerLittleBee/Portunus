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
WEBUI_ENDPOINT=127.0.0.1:5173
CARGO_PROFILE="${CARGO_PROFILE:-release}"
DEMO_TRAFFIC_SEED_MINUTES="${PORTUNUS_DEMO_TRAFFIC_SEED_MINUTES:-18}"

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
  while (( $(date +%s) <= deadline )); do
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

traffic_profile_for_index() {
  case $(( $1 % 3 )) in
    0) printf 'download' ;;
    1) printf 'video' ;;
    *) printf 'intermittent' ;;
  esac
}

preflight() {
  require_cmd cargo
  require_cmd curl
  require_cmd jq
  require_cmd python3
  if [[ "${NO_WAIT}" != "1" ]]; then
    require_cmd pnpm
  fi
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
  log "  web UI     = http://localhost:${WEBUI_ENDPOINT##*:}"
  log "  users      = ${USERS}  rules/user = ${RULES_PER_USER}"
  local u r idx listen target profile
  idx=0
  for ((u = 1; u <= USERS; u++)); do
    for ((r = 1; r <= RULES_PER_USER; r++)); do
      listen=$((BASE_LISTEN + idx))
      target=$((BASE_LISTEN + 1000 + idx))
      profile="$(traffic_profile_for_index "${idx}")"
      log "  user${u}/edge-${u}  rule listen ${listen} -> 127.0.0.1:${target}  traffic=${profile}"
      idx=$((idx + 1))
    done
  done
}

# ---- binaries --------------------------------------------------------------
SERVER_BIN=""
CLIENT_BIN=""
SUPERADMIN_TOKEN=""
SUPERADMIN_DEMO_PASSWORD="${PORTUNUS_DEMO_PASSWORD:-portunus-demo-password}"

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

reset_superadmin_password() {
  local out
  out="$(printf '%s\n' "${SUPERADMIN_DEMO_PASSWORD}" \
        | "${SERVER_BIN}" --data-dir "${DATA_DIR}" \
          reset-password _superadmin --password-stdin --keep-api-tokens \
          2>>"${DATA_DIR}/server.log")"
  printf '%s\n' "${out}" | grep -q '^password_reset=ok ' \
    || die "could not reset superadmin demo password from: ${out} (check ${DATA_DIR}/server.log)"
}

check_ports() {
  local endpoint host port endpoints
  endpoints=("${GRPC_ENDPOINT}" "${HTTP_ENDPOINT}")
  if [[ "${NO_WAIT}" != "1" ]]; then
    endpoints+=("${WEBUI_ENDPOINT}")
  fi
  for endpoint in "${endpoints[@]}"; do
    host="${endpoint%:*}"
    port="${endpoint##*:}"
    if (exec 3<>"/dev/tcp/${host}/${port}") 2>/dev/null; then
      die "port ${port} (${endpoint}) already in use — kill the stale demo/dev process first"
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
  local code
  code="$(curl -s -o /dev/null -w '%{http_code}' \
    "http://${HTTP_ENDPOINT}/v1/clients" 2>/dev/null || true)"
  [[ "${code}" =~ ^[234][0-9][0-9]$ ]]
}

wait_server() {
  wait_ready 15 "server.listening" server_listening || {
    log "--- server.log (tail) ---"; tail -n 30 "${DATA_DIR}/server.log" >&2
    die "server did not become ready"
  }
}

# ---- web UI ----------------------------------------------------------------
start_webui() {
  if [[ "${NO_WAIT}" == "1" ]]; then
    return 0
  fi
  log "starting Web UI (Vite http://localhost:${WEBUI_ENDPOINT##*:})..."
  pnpm --dir webui dev --host "${WEBUI_ENDPOINT%:*}" \
    --port "${WEBUI_ENDPOINT##*:}" --strictPort \
    >>"${DATA_DIR}/webui.log" 2>&1 &
  track "$!"
}

webui_ready() {
  grep -q "http://127.0.0.1:${WEBUI_ENDPOINT##*:}/" "${DATA_DIR}/webui.log" 2>/dev/null
}

wait_webui() {
  if [[ "${NO_WAIT}" == "1" ]]; then
    return 0
  fi
  wait_ready 15 "web UI on ${WEBUI_ENDPOINT}" webui_ready || {
    log "--- webui.log (tail) ---"; tail -n 30 "${DATA_DIR}/webui.log" >&2
    die "web UI did not become ready"
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
      --http-endpoint "${HTTP_ENDPOINT}" >/dev/null 2>>"${DATA_DIR}/server.log" || [[ "${KEEP}" == "1" ]]

    PORTUNUS_OPERATOR_TOKEN="${SUPERADMIN_TOKEN}" \
      "${SERVER_BIN}" --data-dir "${DATA_DIR}" \
      grant-add --user-id "${uid}" --client "${edge}" \
      --listen-port-start "${port_start}" --listen-port-end "${port_end}" \
      --protocols tcp --http-endpoint "${HTTP_ENDPOINT}" >/dev/null 2>>"${DATA_DIR}/server.log" || [[ "${KEEP}" == "1" ]]

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
    if [[ "${KEEP}" == "1" && -s "${bundle}" ]] \
       && jq -e '.token' "${bundle}" >/dev/null 2>&1; then
      log "reusing existing bundle for ${edge}"
    else
      code="$(curl -s -o "${bundle}" -w '%{http_code}' \
        -X POST -H "Authorization: Bearer ${SUPERADMIN_TOKEN}" \
        -H 'Content-Type: application/json' \
        -d "{\"name\":\"${edge}\",\"address\":\"127.0.0.1\"}" \
        "http://${HTTP_ENDPOINT}/v1/clients")" || true
      [[ "${code}" == 2?? ]] \
        || die "provision ${edge} failed (HTTP ${code:-curl-error}); see ${bundle}"
    fi

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
    [[ "${code}" == 2?? || "${KEEP}" == "1" ]] \
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
      | head -1)" || true
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

# ---- demo traffic ----------------------------------------------------------
seed_demo_traffic_history() {
  if (( DEMO_TRAFFIC_SEED_MINUTES <= 0 )); then
    log "traffic history seed skipped (PORTUNUS_DEMO_TRAFFIC_SEED_MINUTES=${DEMO_TRAFFIC_SEED_MINUTES})"
    return 0
  fi

  local db="${DATA_DIR}/state.db"
  [[ -f "${db}" ]] || die "cannot seed traffic history; missing ${db}"

  local -a entries=()
  local g u edge profile
  for g in "${!RULE_LISTEN[@]}"; do
    u="${RULE_USER[g]}"
    edge="${RULE_EDGE[g]}"
    profile="$(traffic_profile_for_index "${g}")"
    entries+=("user${u}:${edge}:${profile}")
  done

  python3 - "${db}" "${DEMO_TRAFFIC_SEED_MINUTES}" "${entries[@]+"${entries[@]}"}" <<'PY'
import sqlite3
import sys
import time

db = sys.argv[1]
minutes = int(sys.argv[2])
entries = sys.argv[3:]
now = int(time.time())
current_minute = now - (now % 60)
start = current_minute - ((minutes - 1) * 60)

def shaped_bytes(profile: str, minute_index: int, rule_index: int) -> int:
    phase = minute_index + rule_index
    if profile == "download":
        if phase % 7 == 0 or minute_index == minutes - 1:
            return (42 + (phase % 5) * 6) * 1024 * 1024
        return 32 * 1024
    if profile == "video":
        return (1800 + (phase % 4) * 220) * 1024
    if profile == "intermittent":
        if phase % 5 in (0, 1):
            return (384 + (phase % 3) * 160) * 1024
        return 0
    raise ValueError(f"unknown traffic profile: {profile}")

conn = sqlite3.connect(db, timeout=10)
conn.execute("PRAGMA busy_timeout = 10000")
for minute_index in range(minutes):
    ts = start + minute_index * 60
    for rule_index, raw in enumerate(entries):
        user_id, client_name, profile = raw.split(":", 2)
        bytes_in = shaped_bytes(profile, minute_index, rule_index)
        bytes_out = bytes_in
        conn.execute(
            """
            INSERT INTO traffic_samples_1m
                (user_id, client_name, ts_minute, bytes_in, bytes_out)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(user_id, client_name, ts_minute) DO UPDATE
                SET bytes_in = bytes_in + excluded.bytes_in,
                    bytes_out = bytes_out + excluded.bytes_out
            """,
            (user_id, client_name, ts, bytes_in, bytes_out),
        )
conn.commit()
conn.close()
print(f"seeded {minutes} minutes for {len(entries)} demo rules")
PY
}

drive_demo_transfer() {
  local listen="$1" total_bytes="$2" chunk_bytes="$3" pause_seconds="$4"
  python3 - "${listen}" "${total_bytes}" "${chunk_bytes}" "${pause_seconds}" <<'PY'
import socket
import sys
import time

port = int(sys.argv[1])
total = int(sys.argv[2])
chunk = int(sys.argv[3])
pause = float(sys.argv[4])
base = b"portunus-demo-traffic:"
payload = (base * ((chunk // len(base)) + 1))[:chunk]
remaining = total

with socket.create_connection(("127.0.0.1", port), timeout=5) as sock:
    sock.settimeout(5)
    while remaining > 0:
        n = min(chunk, remaining)
        block = payload[:n]
        sock.sendall(block)
        received = 0
        while received < n:
            data = sock.recv(min(65536, n - received))
            if not data:
                raise RuntimeError("echo connection closed before payload returned")
            received += len(data)
        remaining -= n
        if pause > 0:
            time.sleep(pause)
PY
}

run_demo_traffic_loop() {
  local listen="$1" profile="$2"
  log "traffic generator ${profile} started on listen ${listen}"
  while true; do
    case "${profile}" in
      download)
        drive_demo_transfer "${listen}" $((24 * 1024 * 1024)) 65536 0 || true
        sleep 35
        ;;
      video)
        drive_demo_transfer "${listen}" $((384 * 1024)) 65536 0 || true
        sleep 2
        ;;
      intermittent)
        local burst
        for burst in 1 2 3; do
          drive_demo_transfer "${listen}" $((128 * 1024)) 32768 0 || true
          sleep 1
        done
        sleep 14
        ;;
      *)
        log "unknown traffic profile ${profile} for listen ${listen}"
        sleep 30
        ;;
    esac
  done
}

start_demo_traffic() {
  seed_demo_traffic_history | while IFS= read -r line; do log "${line}"; done

  local g listen profile
  for g in "${!RULE_LISTEN[@]}"; do
    listen="${RULE_LISTEN[g]}"
    profile="$(traffic_profile_for_index "${g}")"
    run_demo_traffic_loop "${listen}" "${profile}" \
      >>"${DATA_DIR}/traffic-${listen}.log" 2>&1 &
    track "$!"
    log "live traffic ${profile} -> listen ${listen}"
  done
}

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
      "http://${HTTP_ENDPOINT}/v1/rules/${rid}/stats")" || true
    bin="$(printf '%s' "${body}" | jq -r '.bytes_in // 0' 2>/dev/null || echo 0)"
    bout="$(printf '%s' "${body}" | jq -r '.bytes_out // 0' 2>/dev/null || echo 0)"
    log "stats user${u}/${RULE_EDGE[g]} listen ${RULE_LISTEN[g]}" \
        "rule=${rid} bytes_in=${bin} bytes_out=${bout}"
  done
}

any_rule_stats_nonzero() {
  local g u tok rid body bin bout
  for g in "${!RULE_LISTEN[@]}"; do
    u="${RULE_USER[g]}"; tok="${USER_TOKENS[u]}"; rid="${RULE_ID[g]}"
    body="$(curl -s -H "Authorization: Bearer ${tok}" \
      "http://${HTTP_ENDPOINT}/v1/rules/${rid}/stats")" || true
    bin="$(printf '%s' "${body}" | jq -r '.bytes_in // 0' 2>/dev/null || echo 0)"
    bout="$(printf '%s' "${body}" | jq -r '.bytes_out // 0' 2>/dev/null || echo 0)"
    if (( bin > 0 || bout > 0 )); then
      return 0
    fi
  done
  return 1
}

wait_initial_live_stats() {
  if wait_ready 12 "initial live traffic stats" any_rule_stats_nonzero; then
    return 0
  fi
  log "initial live stats not visible yet; historical chart samples are already seeded"
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
    "http://${HTTP_ENDPOINT}/v1/rules/${victim_rid}/stats")" || true
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
  log "operator login: http://${HTTP_ENDPOINT}"
  log "Web UI:         http://localhost:${WEBUI_ENDPOINT##*:}"
  log "  user_id:  _superadmin"
  log "  password: ${SUPERADMIN_DEMO_PASSWORD}"
  for ((u = 1; u <= USERS; u++)); do
    log "user${u} token: ${USER_TOKENS[u]}"
  done
  for g in "${!RULE_LISTEN[@]}"; do
    log "rule ${RULE_ID[g]}  listen ${RULE_LISTEN[g]}  owner user${RULE_USER[g]}  traffic=$(traffic_profile_for_index "${g}")"
  done
  log "data plane:   nc 127.0.0.1 ${RULE_LISTEN[0]}   (type text -> echoed)"
  log "demo traffic: download bursts, video-like segment pulls, intermittent bursts"
  log "monitoring:   curl -s -H \"Authorization: Bearer <user-token>\" \\"
  log "                http://${HTTP_ENDPOINT}/v1/rules/<rule_id>/stats | jq"
  log "logs:         ${DATA_DIR}/server.log , ${DATA_DIR}/webui.log , ${DATA_DIR}/edge-*.log"
  log "stop:         Ctrl-C here (tears down server / Web UI / clients / upstreams)"
  log "===================================================="
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
  reset_superadmin_password
  start_server
  wait_server
  start_webui
  wait_webui
  log "server ready; superadmin token captured (${#SUPERADMIN_TOKEN} chars)"
  add_users
  log "users provisioned: ${USERS}"
  start_edges
  log "all ${USERS} edges connected"
  start_upstreams
  push_rules
  assert_negative_rbac
  log "rules active; RBAC isolation verified"
  verify_forwarding
  start_demo_traffic
  wait_initial_live_stats
  print_stats
  assert_cross_tenant_403
  print_cheatsheet
  if [[ "${NO_WAIT}" == "1" ]]; then
    log "exiting (--no-wait); overall status=${OVERALL_STATUS}"
    exit "${OVERALL_STATUS}"
  fi
  log "holding environment open — press Ctrl-C to stop"
  wait
}

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  main "$@"
fi

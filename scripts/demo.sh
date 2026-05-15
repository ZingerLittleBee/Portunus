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

if [[ "${BASH_SOURCE[0]}" == "${0}" ]]; then
  main "$@"
fi

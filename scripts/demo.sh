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

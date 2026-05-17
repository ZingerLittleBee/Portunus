#!/usr/bin/env bash
# Portunus lifecycle manager: install/uninstall/upgrade/status/service/
# config/env for client and server, binary+systemd or Docker Compose.
#
#   curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | bash -s -- client
#   curl -fsSL .../scripts/install.sh | bash        # interactive menu
#
set -euo pipefail

# ─── Guard ────────────────────────────────────────────────────────────
if [ -z "${BASH_VERSINFO:-}" ] || [ "${BASH_VERSINFO[0]:-0}" -lt 4 ]; then
  echo "Portunus installer requires bash 4.0+ (found ${BASH_VERSION:-unknown})." >&2
  echo "On macOS: 'brew install bash' then run it with that bash." >&2
  exit 1
fi

SELF_SCRIPT=""
case "${BASH_SOURCE[0]:-}" in
  "" | bash | sh | -bash | -sh) ;;
  *) [ -r "${BASH_SOURCE[0]}" ] && SELF_SCRIPT="$(cd "$(dirname "${BASH_SOURCE[0]}")" 2>/dev/null && pwd)/$(basename "${BASH_SOURCE[0]}")" || true ;;
esac

# ─── Constants ────────────────────────────────────────────────────────
REPO="ZingerLittleBee/Portunus"
RAW_BASE="https://raw.githubusercontent.com/${REPO}/main"
DEFAULT_BIN_DIR="/usr/local/bin"
DOCS_FEATURE_URL="https://github.com/${REPO}/blob/main/docs/content/docs/features/advertised-endpoint.mdx"

# ─── Globals ──────────────────────────────────────────────────────────
VERB=""           # install|uninstall|upgrade|status|service|config|env
ROLE=""           # client|server
DEPLOY=""         # binary|docker
VERSION=""        # user-supplied version (may have leading v)
BIN_DIR="$DEFAULT_BIN_DIR"
COMPOSE_DIR=""
WANT_SYSTEMD="no"
ADVERTISED=""     # advertised endpoint host:port ("" = unset/auto)
DATA_DIR=""
OP_HTTP_LISTEN=""
SERVICE_ACTION="" # start|stop|restart
CONFIG_OP=""      # get|set
CONFIG_KEY=""
CONFIG_VALUE=""
ASSUME_YES="no"
PURGE="no"
DRY_RUN="no"
LANG_CODE="${PORTUNUS_LANG:-}"
tag=""
artifact_version=""
resolved_version=""
os=""
arch=""
target=""

die() { echo "error: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "missing required tool: $1"; }

# ─── Platform ─────────────────────────────────────────────────────────
detect_platform() {
  os="$(uname -s | tr '[:upper:]' '[:lower:]')"
  arch="$(uname -m)"
  case "$arch" in
    x86_64|amd64) arch="x86_64" ;;
    aarch64|arm64) arch="aarch64" ;;
    *) die "unsupported arch: $arch" ;;
  esac
  case "$os" in
    linux) target="${arch}-unknown-linux-gnu" ;;
    darwin) target="${arch}-apple-darwin" ;;
    *) die "unsupported os: $os" ;;
  esac
}

resolve_version_static() {
  if [ -n "$VERSION" ]; then
    case "$VERSION" in
      v*) tag="$VERSION"; artifact_version="${VERSION#v}" ;;
      *)  tag="v$VERSION"; artifact_version="$VERSION" ;;
    esac
    resolved_version="$artifact_version"
  else
    resolved_version="<latest, resolved at run time>"; tag=""; artifact_version=""
  fi
}

rel() { echo "https://github.com/${REPO}/releases/download/${tag}/$1"; }

# ─── Plan / dry-run ───────────────────────────────────────────────────
print_plan() {
  local asset checksums
  asset="portunus-${artifact_version:-<latest>}-${target}.tar.gz"
  checksums="portunus-${artifact_version:-<latest>}-checksums.txt"
  echo "portunus install (dry-run)"
  echo "role:             ${ROLE}"
  echo "os:               ${os}"
  echo "arch:             ${arch}"
  echo "target:           ${target}"
  echo "tag:              ${tag:-<latest, resolved at run time>}"
  echo "artifact_version: ${resolved_version}"
  if [ -n "$artifact_version" ]; then
    echo "download_url:     $(rel "$asset")"
    echo "checksums_url:    $(rel "$checksums")"
  else
    echo "download_url:     <github releases/latest, resolved at run time>"
    echo "checksums_url:    <github releases/latest, resolved at run time>"
  fi
  echo "deploy:           ${DEPLOY:-binary}"
  echo "bin_dir:          ${BIN_DIR}"
  echo "systemd:          ${WANT_SYSTEMD}"
  echo "advertised:       ${ADVERTISED:-<unset, runtime auto>}"
  echo "actions:          download+verify+install portunus-${ROLE} -> ${BIN_DIR}$( [ "$WANT_SYSTEMD" = yes ] && echo ' + systemd unit' )"
}

# ─── Arg parse + dispatch (minimal; expanded in Task 2) ───────────────
parse_args() {
  while [ $# -gt 0 ]; do
    case "$1" in
      client|server) ROLE="$1"; VERB="${VERB:-install}" ;;
      install|uninstall|upgrade|status|service|config|env) VERB="$1" ;;
      --version) shift; [ $# -gt 0 ] || die "--version needs a value"; VERSION="$1" ;;
      --bin-dir) shift; [ $# -gt 0 ] || die "--bin-dir needs a value"; BIN_DIR="$1" ;;
      --systemd) WANT_SYSTEMD="yes" ;;
      --yes) ASSUME_YES="yes" ;;
      --dry-run) DRY_RUN="yes" ;;
      -h|--help) echo "usage: install.sh <client|server|install|uninstall|upgrade|status|service|config|env> [flags]"; exit 0 ;;
      *) die "unknown argument: $1" ;;
    esac
    shift
  done
}

main() {
  parse_args "$@"
  [ -n "$ROLE" ] || die "role required: client or server"
  detect_platform
  resolve_version_static
  if [ "$DRY_RUN" = "yes" ]; then print_plan; exit 0; fi
  die "non-dry-run install not yet implemented (scaffold task)"
}

main "$@"

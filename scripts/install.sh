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

# ─── i18n ─────────────────────────────────────────────────────────────
declare -A MSG_EN MSG_ZH
MSG_EN=(
  [menu_title]="Portunus Manager"
  [menu_install]="  [1] Install"
  [menu_uninstall]="  [2] Uninstall"
  [menu_upgrade]="  [3] Upgrade"
  [menu_status]="  [4] Status"
  [menu_service]="  [5] Service (start/stop/restart)"
  [menu_config]="  [6] Config"
  [menu_env]="  [7] Env"
  [menu_exit]="  [0] Exit"
  [menu_select]="Select [0-7]: "
  [lang_prompt]="Select language [1] English [2] 中文: "
  [ask_role]="Install which role? [1] server [2] client: "
  [ask_deploy]="Deploy form? [1] binary+systemd [2] docker compose: "
  [ask_version]="Version (blank = latest): "
  [ask_bindir]="Install dir [%s]: "
  [ask_advertised]="Advertised endpoint host:port (blank = auto; cert SAN must cover it; see %s): "
  [ask_datadir]="Server data dir (blank = default): "
  [ask_ophttp]="Operator HTTP listen (blank = default): "
  [confirm_proceed]="Proceed? [Y/n]: "
  [confirm_uninstall]="Uninstall portunus-%s (%s)? [y/N]: "
  [confirm_purge_typed]="Type 'purge' to also delete data at %s: "
  [need_role]="role required: client or server"
  [no_install_found]="No Portunus install detected (no .install-meta and no probe match)."
  [done_next]="Done. Next steps:"
  [restart_now]="Apply now (restart service)? [y/N]: "
  [upgrade_current]="Already at %s; nothing to upgrade."
  [unknown_config_key]="unknown config key: %s (allowed: advertised-endpoint data-dir operator-http-listen version-pin)"
)
MSG_ZH=(
  [menu_title]="Portunus 管理器"
  [menu_install]="  [1] 安装    Install"
  [menu_uninstall]="  [2] 卸载    Uninstall"
  [menu_upgrade]="  [3] 升级    Upgrade"
  [menu_status]="  [4] 状态    Status"
  [menu_service]="  [5] 服务控制 Service (start/stop/restart)"
  [menu_config]="  [6] 配置    Config"
  [menu_env]="  [7] 环境变量 Env"
  [menu_exit]="  [0] 退出    Exit"
  [menu_select]="选择 [0-7]: "
  [lang_prompt]="选择语言 [1] English [2] 中文: "
  [ask_role]="安装哪个角色? [1] server [2] client: "
  [ask_deploy]="部署方式? [1] 二进制+systemd [2] docker compose: "
  [ask_version]="版本 (留空=最新): "
  [ask_bindir]="安装目录 [%s]: "
  [ask_advertised]="通告地址 host:port (留空=自动; 证书 SAN 须覆盖该 host; 参见 %s): "
  [ask_datadir]="服务端 data 目录 (留空=默认): "
  [ask_ophttp]="Operator HTTP 监听 (留空=默认): "
  [confirm_proceed]="继续? [Y/n]: "
  [confirm_uninstall]="卸载 portunus-%s (%s)? [y/N]: "
  [confirm_purge_typed]="输入 'purge' 以同时删除数据 %s: "
  [need_role]="必须指定角色: client 或 server"
  [no_install_found]="未检测到 Portunus 安装 (无 .install-meta 且探测未命中)。"
  [done_next]="完成。后续步骤:"
  [restart_now]="现在生效 (重启服务)? [y/N]: "
  [upgrade_current]="已是 %s; 无需升级。"
  [unknown_config_key]="未知配置键: %s (允许: advertised-endpoint data-dir operator-http-listen version-pin)"
)

resolve_lang() {
  if [ -z "$LANG_CODE" ]; then
    case "${LC_ALL:-${LANG:-}}" in zh*|*zh_*) LANG_CODE="zh" ;; *) LANG_CODE="" ;; esac
  fi
  case "$LANG_CODE" in zh|en) ;; *) LANG_CODE="" ;; esac
}

t() {
  local key="$1"; shift || true
  local val
  if [ "${LANG_CODE:-en}" = "zh" ]; then val="${MSG_ZH[$key]:-}"; else val="${MSG_EN[$key]:-}"; fi
  [ -n "$val" ] || val="${MSG_EN[$key]:-$key}"
  # shellcheck disable=SC2059
  printf "$val" "$@"
}

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
      --lang) shift; [ $# -gt 0 ] || die "--lang needs a value"; LANG_CODE="$1" ;;
      --print-i18n-keys) shift; resolve_lang; if [ "${1:-en}" = zh ]; then for k in "${!MSG_ZH[@]}"; do echo "$k"; done; else for k in "${!MSG_EN[@]}"; do echo "$k"; done; fi; exit 0 ;;
      --print-i18n) shift; [ $# -gt 0 ] || die "--print-i18n needs a key"; resolve_lang; t "$1"; echo; exit 0 ;;
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

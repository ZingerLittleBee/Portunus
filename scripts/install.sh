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
# Convention: every '%' in a message value MUST be a '%s' directive with a matching t() arg (values are used as printf format strings).
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
  local key="${1:-}"; shift || true
  local val
  if [ "${LANG_CODE:-en}" = "zh" ]; then val="${MSG_ZH[$key]:-}"; else val="${MSG_EN[$key]:-}"; fi
  [ -n "$val" ] || val="${MSG_EN[$key]:-$key}"
  # shellcheck disable=SC2059
  printf "$val" "$@"
}

# ─── Meta ─────────────────────────────────────────────────────────────
meta_path_for() {
  # echo the .install-meta path for current ROLE/DEPLOY/paths.
  if [ "${DEPLOY:-binary}" = "docker" ]; then
    echo "${COMPOSE_DIR:-$PWD}/.install-meta"
  elif [ "$ROLE" = "server" ]; then
    echo "${DATA_DIR:-/var/lib/portunus}/.install-meta"
  else
    echo "/etc/portunus/.install-meta"
  fi
}

meta_write() {
  local f="$1"; shift
  local dir; dir="$(dirname "$f")"
  [ "$DRY_RUN" = yes ] && { echo "would write meta: $f ($*)"; return 0; }
  mkdir -p "$dir" 2>/dev/null || true
  : > "$f"
  local kv
  for kv in "$@"; do printf '%s\n' "$kv" >> "$f"; done
  printf 'installed_at=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >> "$f"
  printf 'installer_version=%s\n' "${SELF_SCRIPT:-pipe}" >> "$f"
}

meta_read() {
  local f="$1" key="$2" line
  [ -r "$f" ] || return 1
  while IFS= read -r line; do
    case "$line" in "${key}="*) printf '%s\n' "${line#*=}"; return 0 ;; esac
  done < "$f"
  return 1
}

detect_deploy() {
  local hint="${1:-}" f
  if [ -n "$hint" ]; then
    for f in "$hint"/compose.yml "$hint"/compose.yaml "$hint"/docker-compose.yml "$hint"/docker-compose.yaml; do
      [ -f "$f" ] && { echo "docker"; return 0; }
    done
  fi
  if [ -f /etc/systemd/system/portunus-server.service ] || [ -f /etc/systemd/system/portunus-client.service ]; then
    echo "binary"; return 0
  fi
  if command -v portunus-server >/dev/null 2>&1 || command -v portunus-client >/dev/null 2>&1; then
    echo "binary"; return 0
  fi
  echo ""; return 0
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
  if [ "$ROLE" = "server" ] && [ "${DEPLOY:-binary}" != "docker" ]; then
    echo "drop-in:          /etc/systemd/system/portunus-server.service.d/10-portunus.conf"
    echo "data_dir:         ${DATA_DIR:-/var/lib/portunus}"
    echo "op_http_listen:   ${OP_HTTP_LISTEN:-<default>}"
  fi
  echo "actions:          download+verify+install portunus-${ROLE} -> ${BIN_DIR}$( [ "$WANT_SYSTEMD" = yes ] && echo ' + systemd unit' )"
}

# ─── Download / install (binary) ──────────────────────────────────────
resolve_latest_tag() {
  need curl; need sed
  tag="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
    | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1)"
  [ -n "$tag" ] || die "could not resolve latest release tag"
  artifact_version="${tag#v}"; resolved_version="$artifact_version"
}

maybe_sudo() { if [ -w "$1" ] || [ "$(id -u)" = "0" ]; then SUDO=""; else SUDO="sudo"; fi; }

install_binary() {
  need curl; need tar
  [ -n "$tag" ] || resolve_latest_tag
  local asset checksums tmp src expected actual
  asset="portunus-${artifact_version}-${target}.tar.gz"
  checksums="portunus-${artifact_version}-checksums.txt"
  tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' RETURN
  echo "→ downloading ${asset} (${tag})"
  curl -fsSL "$(rel "$asset")" -o "$tmp/$asset" || die "download failed: $asset"
  curl -fsSL "$(rel "$checksums")" -o "$tmp/$checksums" || die "download failed: $checksums"
  echo "→ verifying sha256"
  expected="$(grep -F " ${asset}" "$tmp/$checksums" | awk '{print $1}')"
  [ -n "$expected" ] || die "no checksum entry for $asset"
  if command -v sha256sum >/dev/null 2>&1; then actual="$(sha256sum "$tmp/$asset" | awk '{print $1}')"
  else actual="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"; fi
  [ "$expected" = "$actual" ] || die "checksum mismatch for $asset"
  tar -xzf "$tmp/$asset" -C "$tmp"
  src="$tmp/portunus-${artifact_version}-${target}/portunus-${ROLE}"
  [ -f "$src" ] || die "binary not found in archive: portunus-${ROLE}"
  maybe_sudo "$BIN_DIR"
  echo "→ installing portunus-${ROLE} to ${BIN_DIR}"
  ${SUDO:-} install -m 0755 "$src" "${BIN_DIR}/portunus-${ROLE}"
}

# ─── Systemd ──────────────────────────────────────────────────────────
install_systemd_unit() {
  if [ "$os" != "linux" ] || ! command -v systemctl >/dev/null 2>&1; then
    echo "warning: --systemd ignored (not Linux or systemctl missing)" >&2; return 0
  fi
  local unit tmp; unit="portunus-${ROLE}.service"; tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' RETURN
  curl -fsSL "${RAW_BASE}/deploy/systemd/${unit}" -o "$tmp/$unit" || die "unit download failed"
  if [ "$ROLE" = "client" ]; then
    id portunus-client >/dev/null 2>&1 || sudo useradd --system --no-create-home --shell /usr/sbin/nologin portunus-client
    sudo install -d -o root -g portunus-client -m 0750 /etc/portunus
  else
    id portunus-server >/dev/null 2>&1 || sudo useradd --system --no-create-home --shell /usr/sbin/nologin portunus-server
    sudo install -d -o portunus-server -g portunus-server -m 0750 "${DATA_DIR:-/var/lib/portunus}"
  fi
  sudo install -m 0644 "$tmp/$unit" "/etc/systemd/system/$unit"
}

# ─── Server config (drop-in / .env) ───────────────────────────────────
write_server_dropin() {
  local d="/etc/systemd/system/portunus-server.service.d" f
  f="$d/10-portunus.conf"
  sudo install -d -m 0755 "$d"
  {
    echo "[Service]"
    [ -n "$ADVERTISED" ] && echo "Environment=PORTUNUS_ADVERTISED_ENDPOINT=${ADVERTISED}"
  } | sudo tee "$f" >/dev/null
  sudo systemctl daemon-reload || true
  echo "→ wrote $f"
}

# ─── Arg parse + dispatch (minimal; expanded in Task 2) ───────────────
parse_args() {
  while [ $# -gt 0 ]; do
    case "$1" in
      client|server) ROLE="$1"; [ -z "$VERB" ] && VERB="install" ;;
      install|uninstall|upgrade|status|service|config|env) VERB="$1" ;;
      start|stop|restart) SERVICE_ACTION="$1" ;;
      get|set) CONFIG_OP="$1" ;;
      --version) shift; [ $# -gt 0 ] || die "--version needs a value"; VERSION="$1" ;;
      --bin-dir) shift; [ $# -gt 0 ] || die "--bin-dir needs a value"; BIN_DIR="$1" ;;
      --compose-dir) shift; [ $# -gt 0 ] || die "--compose-dir needs a value"; COMPOSE_DIR="$1" ;;
      --deploy) shift; case "${1:-}" in binary|docker) DEPLOY="$1" ;; *) die "--deploy must be binary|docker" ;; esac ;;
      --advertised-endpoint) shift; [ $# -gt 0 ] || die "--advertised-endpoint needs a value"; ADVERTISED="$1" ;;
      --data-dir) shift; [ $# -gt 0 ] || die "--data-dir needs a value"; DATA_DIR="$1" ;;
      --operator-http-listen) shift; [ $# -gt 0 ] || die "--operator-http-listen needs a value"; OP_HTTP_LISTEN="$1" ;;
      --lang) shift; [ $# -gt 0 ] || die "--lang needs a value"; LANG_CODE="$1" ;;
      --systemd) WANT_SYSTEMD="yes" ;;
      --yes) ASSUME_YES="yes" ;;
      --purge) PURGE="yes" ;;
      --dry-run) DRY_RUN="yes" ;;
      --print-i18n-keys) shift; resolve_lang; if [ "${1:-en}" = zh ]; then for k in "${!MSG_ZH[@]}"; do echo "$k"; done; else for k in "${!MSG_EN[@]}"; do echo "$k"; done; fi; exit 0 ;;
      --print-i18n) shift; [ $# -gt 0 ] || die "--print-i18n needs a key"; resolve_lang; t "$1"; echo; exit 0 ;;
      -h|--help) echo "usage: install.sh <client|server|install|uninstall|upgrade|status|service|config|env> [start|stop|restart] [get|set key [value]] [--version V] [--deploy binary|docker] [--bin-dir D] [--compose-dir D] [--advertised-endpoint H:P] [--data-dir D] [--operator-http-listen A] [--systemd] [--lang en|zh] [--yes] [--purge] [--dry-run]"; exit 0 ;;
      --meta-write) shift; f="$1"; shift; meta_write "$f" "$@"; exit 0 ;;
      --meta-read) shift; f="$1"; k="$2"; meta_read "$f" "$k"; exit $? ;;
      --detect-deploy) shift; detect_deploy "${1:-}"; exit 0 ;;
      *) if [ "$VERB" = config ] && [ -z "$CONFIG_KEY" ]; then CONFIG_KEY="$1"; elif [ "$VERB" = config ] && [ -z "$CONFIG_VALUE" ]; then CONFIG_VALUE="$1"; else die "unknown argument: $1"; fi ;;
    esac
    shift
  done
}

is_interactive() {
  if [ -t 0 ]; then return 0; fi
  if [ -r /dev/tty ] && { exec 3</dev/tty; } 2>/dev/null; then exec 3<&-; return 0; fi
  return 1
}

main() {
  parse_args "$@"
  resolve_lang
  # No actionable verb/role and a TTY ⇒ interactive menu (Task 7).
  if [ -z "$VERB" ] && [ -z "$ROLE" ]; then
    if is_interactive; then run_menu; exit $?; fi
    die "no command given and no terminal; run 'install.sh -h' or pass a role"
  fi
  [ -n "$VERB" ] || VERB="install"
  detect_platform
  resolve_version_static
  if [ "$DRY_RUN" = "yes" ]; then
    if [ "$VERB" = "install" ]; then [ -n "$ROLE" ] || die "$(t need_role)"; print_plan; exit 0; fi
    echo "verb: ${VERB} (dry-run; no side effects)"; exit 0
  fi
  dispatch_verb
}

run_menu() { die "interactive menu not yet implemented"; }
dispatch_verb() {
  case "$VERB" in
    install)
      [ -n "$ROLE" ] || die "$(t need_role)"
      [ -z "$DEPLOY" ] && DEPLOY="binary"
      if [ "$DEPLOY" = "docker" ]; then install_docker; else
        install_binary
        [ "$WANT_SYSTEMD" = yes ] && install_systemd_unit
        [ "$ROLE" = "server" ] && [ "$WANT_SYSTEMD" = yes ] && write_server_dropin
      fi
      meta_write "$(meta_path_for)" "role=$ROLE" "deploy=$DEPLOY" "version=$resolved_version" "lang=${LANG_CODE:-en}" "advertised_endpoint_set=$([ -n "$ADVERTISED" ] && echo yes || echo no)"
      echo; echo "$(t done_next)"
      ;;
    uninstall|upgrade|status|service|config|env) lifecycle_"$VERB" ;;
    *) die "verb '${VERB}' not yet implemented" ;;
  esac
}

install_docker() { die "docker install not yet implemented"; }
lifecycle_uninstall() { die "uninstall not yet implemented"; }
lifecycle_upgrade()   { die "upgrade not yet implemented"; }
lifecycle_status()    { die "status not yet implemented"; }
lifecycle_service()   { die "service not yet implemented"; }
lifecycle_config()    { die "config not yet implemented"; }
lifecycle_env()       { die "env not yet implemented"; }

main "$@"

#!/bin/sh
# Portunus lifecycle manager: install/uninstall/upgrade/status/service/
# config/env for client/server/standalone, binary+systemd|openrc or Docker.
#
#   curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sh -s -- standalone
#   curl -fsSL .../scripts/install.sh | sh -s -- server --advertised-endpoint host:7443
#
# POSIX sh. The only non-POSIX builtin relied upon is `local`, which dash,
# busybox ash, and ksh all provide.
# shellcheck disable=SC3043  # 'local' is provided by dash/busybox-ash/ksh
set -eu

# When piped (curl|sh) $0 is the shell name; when run as a file it is the
# path. Only a readable file path yields local templates.
SELF_SCRIPT=""
case "${0:-}" in
  ""|sh|-sh|dash|-dash|bash|-bash|ash|-ash) ;;
  *) [ -r "$0" ] && SELF_SCRIPT="$(cd "$(dirname "$0")" 2>/dev/null && pwd)/$(basename "$0")" || true ;;
esac

# ─── Constants ────────────────────────────────────────────────────────
REPO="ZingerLittleBee/Portunus"
RAW_BASE="https://raw.githubusercontent.com/${REPO}/main"
DEFAULT_BIN_DIR="/usr/local/bin"

# ─── Globals ──────────────────────────────────────────────────────────
VERB=""           # install|uninstall|upgrade|status|service|config|env
ROLE=""           # client|server|standalone
DEPLOY=""         # binary|docker
VERSION=""        # user-supplied version (may have leading v)
BIN_DIR="$DEFAULT_BIN_DIR"
COMPOSE_DIR=""
NO_SERVICE="no"   # --no-service: install but do not enable/start
CONFIG_PATH=""    # --config PATH (standalone only): file the service reads
INIT=""           # systemd|openrc|none — set by detect_init()
SUDO=""           # set in main(): "" when root, "sudo" otherwise
ADVERTISED=""     # advertised endpoint host:port ("" = unset/auto)
DATA_DIR=""
OP_HTTP_LISTEN=""
SERVICE_ACTION="" # start|stop|restart
CONFIG_OP=""      # get|set
CONFIG_KEY=""
CONFIG_VALUE=""
RESTART="no"
PURGE="no"
DRY_RUN="no"
ENROLL_URI=""     # --enroll '<uri>' (client/binary only): one-time enrollment URI
tag=""
artifact_version=""
resolved_version=""
os=""
arch=""
target=""
DETECTED_IP=""        # last detect_public_ip() result
DETECTED_PROV=""      # provenance token: prov_detected|prov_nic|prov_loopback
DOMAIN=""             # optional HTTPS domain for host Caddy
ACME_EMAIL=""         # optional Let's Encrypt account email
SKIP_DNS_CHECK="no"   # --skip-dns-check
CADDYFILE="/etc/caddy/Caddyfile"
PRINT_EFF="no"               # --effective-advertised seam

die() { echo "error: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "missing required tool: $1"; }

# Space-separated list of temp dirs removed on exit (paths contain no spaces).
CLEANUP_DIRS=""
_cleanup() { for d in $CLEANUP_DIRS; do [ -n "$d" ] && rm -rf "$d"; done; return 0; }
trap _cleanup EXIT
track_tmp() { CLEANUP_DIRS="$CLEANUP_DIRS $1"; }

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
    linux) target="${arch}-unknown-linux-musl" ;;
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

# ─── Init-system abstraction (per-init driver groups behind svc) ──────
detect_init() {
  if command -v systemctl >/dev/null 2>&1 && [ -d /run/systemd/system ]; then INIT=systemd
  elif command -v rc-service >/dev/null 2>&1 || command -v openrc >/dev/null 2>&1; then INIT=openrc
  else INIT=none; fi
}
svc() { _op="$1"; shift 2>/dev/null || true; "${INIT}_${_op}" "$@"; }

svc_user_for() { case "$1" in standalone) echo portunus ;; client) echo portunus-client ;; server) echo portunus-server ;; esac; }
config_default_for() { case "$1" in standalone) echo /etc/portunus/standalone.toml ;; *) echo "" ;; esac; }

server_extra_args() {
  _a=""
  [ -n "${OP_HTTP_LISTEN:-}" ] && _a="$_a --operator-http-listen $OP_HTTP_LISTEN"
  [ -n "${ADVERTISED:-}" ] && _a="$_a --advertised-endpoint $ADVERTISED"
  printf '%s' "${_a# }"
}

# Create the per-role system user + base dirs (idempotent; useradd or busybox adduser).
ensure_svc_user() {
  _r="$1"; _u="$(svc_user_for "$_r")"
  if ! id "$_u" >/dev/null 2>&1; then
    # useradd (Debian/RHEL) creates a matching primary group on its own.
    # busybox adduser does NOT, so create the group first and bind to it —
    # otherwise `chown root:$_u` later has no group and the 0640 config
    # becomes unreadable by the service user.
    ${SUDO:-} useradd --system --no-create-home --shell /usr/sbin/nologin "$_u" 2>/dev/null \
      || { ${SUDO:-} addgroup -S "$_u" 2>/dev/null; \
           ${SUDO:-} adduser -S -D -H -s /sbin/nologin -G "$_u" "$_u" 2>/dev/null; } \
      || die "failed to create system user $_u"
  fi
  case "$_r" in
    server) ${SUDO:-} install -d -o "$_u" -g "$_u" -m 0750 "${DATA_DIR:-/var/lib/portunus}" 2>/dev/null \
              || ${SUDO:-} mkdir -p "${DATA_DIR:-/var/lib/portunus}" ;;
    *)      ${SUDO:-} install -d -o root -g "$_u" -m 0750 /etc/portunus 2>/dev/null \
              || ${SUDO:-} mkdir -p /etc/portunus ;;
  esac
}

# Prepare the config dir and fix perms on a user-authored config — never
# seed one. The standalone binary exits (code 2) if the file is absent, so
# the operator must create it first (see the docs); we only make sure the
# service user can read whatever they wrote.
apply_config_path() {
  _r="$1"; _p="$2"; _u="$3"
  [ -z "$_p" ] && return 0
  ${SUDO:-} mkdir -p "$(dirname "$_p")"
  if [ -f "$_p" ]; then
    ${SUDO:-} chown "root:$_u" "$_p" 2>/dev/null || true
    ${SUDO:-} chmod 0640 "$_p" 2>/dev/null || true
    ${SUDO:-} su -s /bin/sh "$_u" -c "test -r '$_p'" 2>/dev/null \
      || echo "warning: $_u may not be able to read $_p (check directory permissions)" >&2
  fi
}

# Enroll the client and place its bundle where the service reads it.
# Runs after the binary + service unit are installed; dies on failure so we
# never enable a service that would crash-loop on a missing bundle.
# Re-enrollment: if the service is already active, restart it so the new
# credentials take effect (a fresh install is started by the caller).
place_client_bundle() {
  _uri="$1"
  ensure_svc_user client   # idempotent: guarantees portunus-client + /etc/portunus
  # Stage the bundle (it holds a bearer token) in a tracked temp dir: the
  # EXIT trap removes it on normal exit, and the explicit rm -rf below
  # clears it on every success/failure path. (A hard kill mid-enroll could
  # leave it; it is mode 0600 inside a 0700 mktemp -d.)
  _tmpd="$(mktemp -d)" || die "failed to create temp dir for bundle"
  track_tmp "$_tmpd"
  _tmp="$_tmpd/client.bundle.json"
  if ! "${BIN_DIR}/portunus-client" enroll "$_uri" --out "$_tmp" >/dev/null; then
    rm -rf "$_tmpd"
    die "Enrollment failed; the binary and service are installed — retry with a fresh enroll link."
  fi
  ${SUDO:-} install -o root -g portunus-client -m 0640 "$_tmp" /etc/portunus/client.bundle.json \
    || { rm -rf "$_tmpd"; die "failed to place client bundle"; }
  rm -rf "$_tmpd"
  if command -v systemctl >/dev/null 2>&1 && ${SUDO:-} systemctl is-active --quiet portunus-client 2>/dev/null; then
    ${SUDO:-} systemctl restart portunus-client || true
  elif command -v rc-service >/dev/null 2>&1 && rc-service portunus-client status >/dev/null 2>&1; then
    ${SUDO:-} rc-service portunus-client restart || true
  fi
  echo "Enrollment bundle placed at /etc/portunus/client.bundle.json"
}

# systemd drop-in body for a custom standalone config path (empty otherwise).
render_config_dropin() {
  _r="$1"; _p="$2"
  [ "$_r" = standalone ] || return 0
  [ "$_p" = "$(config_default_for "$_r")" ] && return 0
  printf '[Service]\nExecStart=\nExecStart=/usr/local/bin/portunus-standalone --config %s\n' "$_p"
}

# OpenRC /etc/conf.d body per role.
render_confd() {
  case "$1" in
    standalone) printf 'cfgfile="%s"\n' "${2:-/etc/portunus/standalone.toml}" ;;
    client)     printf 'bundle="%s"\n' "/etc/portunus/client.bundle.json" ;;
    server)     printf 'datadir="%s"\nserver_args="%s"\n' "${DATA_DIR:-/var/lib/portunus}" "$(server_extra_args)" ;;
  esac
}

openrc_url() {
  case "$1" in
    standalone) printf '%s\n' "${RAW_BASE}/crates/portunus-standalone/contrib/portunus-standalone.openrc" ;;
    *)          printf '%s\n' "${RAW_BASE}/deploy/openrc/portunus-$1.openrc" ;;
  esac
}
render_openrc() {  # role -> init.d script (local template else curl)
  _lp=""
  if [ -n "${SELF_SCRIPT:-}" ]; then
    case "$1" in
      standalone) _lp="$(dirname "$SELF_SCRIPT")/../crates/portunus-standalone/contrib/portunus-standalone.openrc" ;;
      *)          _lp="$(dirname "$SELF_SCRIPT")/../deploy/openrc/portunus-$1.openrc" ;;
    esac
  fi
  if [ -n "$_lp" ] && [ -r "$_lp" ]; then cat "$_lp"
  else curl -fsSL "$(openrc_url "$1")" || die "failed to fetch OpenRC service for $1"; fi
}

# ── systemd driver ──
systemd_install() {
  ensure_svc_user "$1"
  install_systemd_unit "$1"
  apply_config_path "$1" "$2" "$(svc_user_for "$1")"
  _dp="$(render_config_dropin "$1" "$2")"
  if [ -n "$_dp" ]; then
    ${SUDO:-} mkdir -p "/etc/systemd/system/portunus-$1.service.d"
    printf '%s' "$_dp" | ${SUDO:-} tee "/etc/systemd/system/portunus-$1.service.d/10-config.conf" >/dev/null
    ${SUDO:-} systemctl daemon-reload || true
  fi
}
systemd_enable_start() { ${SUDO:-} systemctl enable --now "portunus-$1.service"; }
systemd_start()   { ${SUDO:-} systemctl start "portunus-$1.service"; }
systemd_stop()    { ${SUDO:-} systemctl stop "portunus-$1.service" 2>/dev/null || true; }
systemd_disable() { ${SUDO:-} systemctl disable "portunus-$1.service" 2>/dev/null || true; }
systemd_restart() { ${SUDO:-} systemctl restart "portunus-$1.service"; }
systemd_status()  { ${SUDO:-} systemctl --no-pager status "portunus-$1.service" 2>/dev/null || ${SUDO:-} systemctl is-active "portunus-$1.service" 2>/dev/null || true; }
systemd_remove()  {
  ${SUDO:-} rm -f "/etc/systemd/system/portunus-$1.service"
  ${SUDO:-} rm -rf "/etc/systemd/system/portunus-$1.service.d"
  ${SUDO:-} systemctl daemon-reload 2>/dev/null || true
}

# ── openrc driver ──
openrc_install() {
  command -v rc-service >/dev/null 2>&1 || command -v rc-update >/dev/null 2>&1 || die "OpenRC tools (rc-service/rc-update) missing"
  ensure_svc_user "$1"
  render_openrc "$1" | ${SUDO:-} tee "/etc/init.d/portunus-$1" >/dev/null
  ${SUDO:-} chmod 0755 "/etc/init.d/portunus-$1"
  apply_config_path "$1" "$2" "$(svc_user_for "$1")"
  render_confd "$1" "$2" | ${SUDO:-} tee "/etc/conf.d/portunus-$1" >/dev/null
}
openrc_enable_start() { ${SUDO:-} rc-update add "portunus-$1" default 2>/dev/null || true; ${SUDO:-} rc-service "portunus-$1" start; }
openrc_start()   { ${SUDO:-} rc-service "portunus-$1" start; }
openrc_stop()    { ${SUDO:-} rc-service "portunus-$1" stop 2>/dev/null || true; }
openrc_disable() { ${SUDO:-} rc-update del "portunus-$1" default 2>/dev/null || true; }
openrc_restart() { ${SUDO:-} rc-service "portunus-$1" restart; }
openrc_status()  { ${SUDO:-} rc-service "portunus-$1" status 2>/dev/null || true; }
openrc_remove()  { ${SUDO:-} rm -f "/etc/init.d/portunus-$1" "/etc/conf.d/portunus-$1"; }

# ── none driver (no supported init: binary + config only) ──
none_install()      { ensure_svc_user "$1" 2>/dev/null || true; apply_config_path "$1" "$2" "$(svc_user_for "$1")" 2>/dev/null || true; }
none_enable_start() { printf '  no supported init system; run it in the background manually:\n  nohup %s --config %s > /var/log/portunus.log 2>&1 &\n' "/usr/local/bin/portunus-$1" "${2:-$(config_default_for "$1")}"; }
none_start()   { :; }
none_stop()    { :; }
none_disable() { :; }
none_restart() { :; }
none_status()  { echo "no service manager detected (init=none); not managed"; }
none_remove()  { :; }

# Decide whether `install` should enable+start the service now. Standalone
# needs an operator-authored config — the binary exits (code 2) without one,
# so we don't auto-start it until the file exists (the docs guide creating it
# first). server/client always start unless --no-service / no init manager.
service_should_start() {
  [ "$NO_SERVICE" = yes ] && return 1
  [ "$INIT" = none ] && return 1
  [ "$ROLE" = standalone ] && [ ! -f "${CONFIG_PATH:-/etc/portunus/standalone.toml}" ] && return 1
  return 0
}

# ─── Plan / dry-run ───────────────────────────────────────────────────
print_plan() {
  local asset checksums
  detect_init
  asset="portunus-${artifact_version:-<latest>}-${target}.tar.gz"
  checksums="portunus-${artifact_version:-<latest>}-checksums.txt"
  echo "portunus install (dry-run)"
  echo "role:             ${ROLE}"
  [ "$ROLE" = client ] && [ -n "$ENROLL_URI" ] && echo "enroll_uri:       ${ENROLL_URI%%\?*} (code redacted)"
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
  if [ "${DEPLOY:-binary}" = "docker" ]; then
    echo "compose_dir:      ${COMPOSE_DIR:-$PWD}"
    echo "env_file:         ${COMPOSE_DIR:-$PWD}/.env"
  fi
  echo "bin_dir:          ${BIN_DIR}"
  [ "$ROLE" = "standalone" ] && [ "${DEPLOY:-binary}" != "docker" ] && echo "config:           ${CONFIG_PATH:-/etc/portunus/standalone.toml} (you create it; service exits if absent)"
  echo "init:             ${INIT:-?}"
  echo "service:          $([ "$NO_SERVICE" = yes ] && echo 'install only (--no-service)' || echo 'install + start')"
  echo "advertised:       ${ADVERTISED:-<unset, runtime auto>}"
  if [ "$ROLE" = "server" ] && [ "${DEPLOY:-binary}" != "docker" ]; then
    echo "drop-in:          /etc/systemd/system/portunus-server.service.d/10-portunus.conf"
    echo "data_dir:         ${DATA_DIR:-/var/lib/portunus}"
    echo "op_http_listen:   ${OP_HTTP_LISTEN:-<default>}"
  fi
  echo "actions:          download+verify+install portunus-${ROLE} -> ${BIN_DIR}$([ "$NO_SERVICE" != yes ] && [ "$INIT" != none ] && echo " + ${INIT} service")"
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
  tmp="$(mktemp -d)"; track_tmp "$tmp"
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
  # User creation + config seeding are handled by the init driver
  # (ensure_svc_user / apply_config_path) so systemd and OpenRC share one path.
}

# ─── Systemd ──────────────────────────────────────────────────────────
install_systemd_unit() {  # install_systemd_unit <role> — place the unit only
  _role="$1"
  if [ "$os" != "linux" ] || ! command -v systemctl >/dev/null 2>&1; then
    echo "warning: systemd unit skipped (not Linux or systemctl missing)" >&2; return 0
  fi
  maybe_sudo "/etc/systemd/system"
  local unit tmp; unit="portunus-${_role}.service"; tmp="$(mktemp -d)"; track_tmp "$tmp"
  if [ "$_role" = "standalone" ]; then
    local self_dir=""
    [ -n "${SELF_SCRIPT:-}" ] && self_dir="$(dirname "$SELF_SCRIPT")"
    if [ -n "$self_dir" ] && [ -r "$self_dir/../crates/portunus-standalone/contrib/portunus-standalone.service" ]; then
      cp "$self_dir/../crates/portunus-standalone/contrib/portunus-standalone.service" "$tmp/$unit"
    else
      curl -fsSL "${RAW_BASE}/crates/portunus-standalone/contrib/portunus-standalone.service" -o "$tmp/$unit" \
        || die "failed to fetch portunus-standalone.service"
    fi
  else
    curl -fsSL "${RAW_BASE}/deploy/systemd/${unit}" -o "$tmp/$unit" || die "unit download failed"
  fi
  ${SUDO:-} install -m 0644 "$tmp/$unit" "/etc/systemd/system/$unit"
  ${SUDO:-} systemctl daemon-reload || true
}

# ─── Server config (systemd drop-in / openrc conf.d) ──────────────────
# The server's scoped flags live in the systemd ExecStart override (systemd)
# or /etc/conf.d/portunus-server's datadir=/server_args= (openrc). These paths
# are root-owned; `config set` writes them via sudo. PORTUNUS_TEST_CONFIG_ROOT
# (a directory prefix) is a test-only seam: when set, the files are written
# under it WITHOUT sudo and WITHOUT systemctl, so `config get/set` can be
# exercised unprivileged in CI. Production leaves it unset → real paths + sudo.
systemd_dropin_dir()  { printf '%s/etc/systemd/system/portunus-server.service.d' "${PORTUNUS_TEST_CONFIG_ROOT:-}"; }
systemd_dropin_file() { printf '%s/10-portunus.conf' "$(systemd_dropin_dir)"; }
confd_file()          { printf '%s/etc/conf.d/portunus-server' "${PORTUNUS_TEST_CONFIG_ROOT:-}"; }
# Honor the $SUDO convention (empty as root, "sudo" otherwise) like every other
# privileged call — a hardcoded `sudo` breaks on root-but-no-sudo hosts (Alpine,
# minimal openrc/musl images). Test seam set ⇒ write directly, no sudo.
config_sudo() { if [ -n "${PORTUNUS_TEST_CONFIG_ROOT:-}" ]; then "$@"; else ${SUDO:-} "$@"; fi }

# Pure: emit the systemd drop-in body for the currently-set scoped values.
render_dropin() {
  # The server has no env binding for these; an inert Environment= line
  # is ignored. Emit a real ExecStart= override (cleared then re-set —
  # the standard systemd drop-in idiom) so the flags actually take.
  local dd="${DATA_DIR:-/var/lib/portunus}" args
  args="--data-dir ${dd} serve"
  [ -n "$OP_HTTP_LISTEN" ] && args="${args} --operator-http-listen ${OP_HTTP_LISTEN}"
  [ -n "$ADVERTISED" ]     && args="${args} --advertised-endpoint ${ADVERTISED}"
  printf '[Service]\nExecStart=\nExecStart=/usr/local/bin/portunus-server %s\n' "$args"
  return 0
}
write_server_dropin() {
  local d f; d="$(systemd_dropin_dir)"; f="$(systemd_dropin_file)"
  config_sudo install -d -m 0755 "$d"
  render_dropin | config_sudo tee "$f" >/dev/null
  [ -n "${PORTUNUS_TEST_CONFIG_ROOT:-}" ] || sudo systemctl daemon-reload || true
  echo "→ wrote $f"
}
# openrc counterpart: rewrite /etc/conf.d/portunus-server from the current
# scoped globals (datadir= + server_args=), mirroring openrc_install's seed.
write_server_confd() {
  local f; f="$(confd_file)"
  config_sudo install -d -m 0755 "$(dirname "$f")"
  render_confd server | config_sudo tee "$f" >/dev/null
  echo "→ wrote $f"
}
# Load the server's current scoped values (DATA_DIR/OP_HTTP_LISTEN/ADVERTISED)
# from the live binary config so `config get/set` reflects what is in effect.
# systemd → the ExecStart override flags; openrc → datadir= + server_args=.
hydrate_binary_config() {  # $1 = init (systemd|openrc|none)
  DATA_DIR=""; OP_HTTP_LISTEN=""; ADVERTISED=""
  if [ "$1" = openrc ]; then
    # Tolerant parse: the canonical form is quoted (datadir="…"), but a
    # hand-edited conf.d may carry a trailing comment or an unquoted value —
    # both are valid POSIX-shell. Capture the quoted body first (ignoring
    # anything after the closing quote), else an unquoted bareword. Missing a
    # custom datadir here would silently revert it to the default on rewrite.
    local conf sargs; conf="$(confd_file)"
    DATA_DIR="$(sed -n 's/^datadir="\([^"]*\)".*/\1/p' "$conf" 2>/dev/null | tail -1)"
    [ -n "$DATA_DIR" ] || DATA_DIR="$(sed -n 's/^datadir=\([^"#[:space:]][^#[:space:]]*\).*/\1/p' "$conf" 2>/dev/null | tail -1)"
    sargs="$(sed -n 's/^server_args="\([^"]*\)".*/\1/p' "$conf" 2>/dev/null | tail -1)"
    OP_HTTP_LISTEN="$(flag_value_from "$sargs" --operator-http-listen)"
    ADVERTISED="$(flag_value_from "$sargs" --advertised-endpoint)"
  else
    local line; line="$(grep -E '^ExecStart=.*portunus-server' "$(systemd_dropin_file)" 2>/dev/null | tail -1 || true)"
    DATA_DIR="$(flag_value_from "$line" --data-dir)"
    OP_HTTP_LISTEN="$(flag_value_from "$line" --operator-http-listen)"
    ADVERTISED="$(flag_value_from "$line" --advertised-endpoint)"
  fi
}

# Guided onboarding for a systemd server install. The generic one-liner
# (just `status:`) left users unsure the service was even running and with
# no path to the Web UI / first-admin token; this walks them through it.
print_server_next_steps() {
  local host="127.0.0.1" port="7080" h url
  if [ -n "$OP_HTTP_LISTEN" ]; then
    port="${OP_HTTP_LISTEN##*:}"; h="${OP_HTTP_LISTEN%:*}"
    # A wildcard bind (0.0.0.0 / ::) is still reached over loopback locally.
    [ -n "$h" ] && [ "$h" != "0.0.0.0" ] && [ "$h" != "::" ] && host="$h"
  fi
  url="http://${host}:${port}"
  if service_should_start; then printf '✓ Portunus server %s is installed and running.\n' "$resolved_version"
  else printf '✓ Portunus server %s is installed (service not started: --no-service).\n' "$resolved_version"
       echo "  Start it first:  sudo systemctl enable --now portunus-server"; fi
  echo
  echo "Next steps:"
  printf '  1) Get the onboarding setup token (first run only, to create the first admin):\n       sudo journalctl -u portunus-server | grep '\''onboarding setup token'\''\n'
  printf '  2) Open the Web UI in a browser:  %s\n       (bound to loopback by design — not reachable from the public network)\n' "$url"
  printf '       Remote server? From your own machine, tunnel first, then open the URL locally:\n       ssh -L %s:127.0.0.1:%s <user>@<this-server>\n' "$port" "$port"
  echo "  3) Paste the token in the browser, then create the first _superadmin account."
  echo
  printf 'Handy commands:\n  status:  install.sh status\n  logs:    sudo journalctl -u portunus-server -f\n  stop:    sudo systemctl stop portunus-server\n'
}

# Actionable post-install hints. The service is started by default; these
# cover the cases where it was NOT (--no-service, no init manager, or a
# standalone install whose config the operator still has to create).
print_next_steps() {
  # systemd server gets the guided block above; everything else keeps the
  # compact hints (other inits use commands the rich block can't assume).
  if [ "$ROLE" = server ] && [ "${DEPLOY:-binary}" != docker ] && [ "${INIT:-}" = systemd ]; then
    print_server_next_steps
    return
  fi
  echo "Done. Next steps:"
  if [ "${DEPLOY:-binary}" = "docker" ]; then
    printf '  manage:  (cd %s && docker compose ps)\n' "${COMPOSE_DIR:-$PWD}"
  else
    _cfg="${CONFIG_PATH:-/etc/portunus/standalone.toml}"
    if [ "$ROLE" = "standalone" ]; then
      if [ -f "$_cfg" ]; then printf '  edit:    sudoedit %s\n' "$_cfg"
      else printf "  create config first: write your forwarding rules to %s (the service exits and won't start without it)\n" "$_cfg"; fi
    fi
    # If we did not start the service, show how to start it.
    if ! service_should_start; then
      case "${INIT:-}" in
        openrc) printf '  start:   sudo rc-update add portunus-%s default && sudo rc-service portunus-%s start\n' "$ROLE" "$ROLE" ;;
        none)   none_enable_start "$ROLE" "$_cfg" ;;
        *)      printf '  start:   sudo systemctl enable --now portunus-%s\n' "$ROLE" ;;
      esac
    fi
  fi
  echo "  status:  install.sh status"
}

# ─── Docker ───────────────────────────────────────────────────────────
compose_cmd() {
  if docker compose version >/dev/null 2>&1; then echo "docker compose";
  elif command -v docker-compose >/dev/null 2>&1; then echo "docker-compose";
  else die "docker compose v2 (or docker-compose) required"; fi
}

write_compose_env() {
  local dir="$1" f="$1/.env"
  mkdir -p "$dir"
  : > "$f"
  [ -n "$ADVERTISED" ]      && echo "PORTUNUS_ADVERTISED_ENDPOINT=${ADVERTISED}" >> "$f"
  [ -n "$OP_HTTP_LISTEN" ]  && echo "PORTUNUS_OPERATOR_HTTP_LISTEN=${OP_HTTP_LISTEN}" >> "$f"
  if [ "$ROLE" = "client" ]; then
    # First boot self-enrolls from this URI; paste the one-time enroll URI
    # from the Web UI Clients page (or `portunus-server enroll-client`).
    echo "# set PORTUNUS_ENROLL_URI before 'docker compose up' (one-time URI)" >> "$f"
    echo "PORTUNUS_ENROLL_URI=" >> "$f"
  fi
  echo "→ wrote $f"
}

write_compose_file() {
  local dir="$1" f="$1/compose.yml" port; port="$(op_http_port)"
  mkdir -p "$dir"
  if [ "$ROLE" = "standalone" ]; then
    # The compose file bind-mounts ./portunus.toml read-only into the
    # container. We never seed it — a missing source would make Docker
    # create a bogus *directory* at that path. Require the operator to
    # author it first (see the standalone docs).
    if [ ! -f "$dir/portunus.toml" ]; then
      die "create ${dir}/portunus.toml first — docker mounts it at /etc/portunus/standalone.toml (example: ${RAW_BASE}/crates/portunus-standalone/contrib/portunus.example.toml)"
    fi
    # The standalone GHCR image is published by release.yml (tags
    # :<version> and :latest), so emit an image-based compose that
    # bind-mounts the operator-authored portunus.toml read-only. No
    # source tree or local build is required on the target host.
    [ -f "$f" ] && { echo "→ keeping existing $f"; return 0; }
    cat > "$f" <<YAML
services:
  standalone:
    image: ghcr.io/zingerlittlebee/portunus-standalone:${artifact_version:-latest}
    container_name: portunus-standalone
    network_mode: host
    volumes:
      - ./portunus.toml:/etc/portunus/standalone.toml:ro
    cap_add:
      - NET_BIND_SERVICE
    restart: unless-stopped
YAML
    echo "→ wrote $f"
    return 0
  fi
  if [ "$ROLE" = "client" ]; then
    # The client image self-enrolls on first boot from PORTUNUS_ENROLL_URI
    # (set it in .env) into the named volume at /etc/portunus, then runs.
    # Host networking lets pushed-rule listeners bind on the edge host.
    [ -f "$f" ] && { echo "→ keeping existing $f"; return 0; }
    cat > "$f" <<YAML
services:
  client:
    image: ghcr.io/zingerlittlebee/portunus-client:${artifact_version:-latest}
    container_name: portunus-client
    network_mode: host
    environment:
      - PORTUNUS_ENROLL_URI=\${PORTUNUS_ENROLL_URI:-}
    volumes:
      - portunus-client:/etc/portunus
    restart: unless-stopped

volumes:
  portunus-client:
YAML
    echo "→ wrote $f"
    return 0
  fi
  [ -f "$f" ] && { echo "→ keeping existing $f"; return 0; }
  # The server's --operator-http-listen has no env binding and defaults
  # to container-internal 127.0.0.1, which Docker's published port (and
  # host Caddy) cannot reach. Override the image CMD to bind 0.0.0.0
  # inside the container (mirrors deploy/docker/docker-compose.yml); the
  # host only publishes 127.0.0.1:<port> so it stays loopback-exposed.
  local advcmd=""
  [ -n "$ADVERTISED" ] && advcmd=", \"--advertised-endpoint\", \"${ADVERTISED}\""
  cat > "$f" <<YAML
services:
  server:
    image: ghcr.io/zingerlittlebee/portunus-${ROLE}:${artifact_version:-latest}
    container_name: portunus-${ROLE}
    env_file: [ .env ]
    command: ["--data-dir", "/var/lib/portunus", "serve", "--operator-http-listen", "0.0.0.0:${port}"${advcmd}]
    ports:
      - "7443:7443"
      - "127.0.0.1:${port}:${port}"
    volumes:
      - portunus-data:/var/lib/portunus
    restart: unless-stopped
volumes:
  portunus-data:
    name: portunus-data
YAML
  echo "→ wrote $f"
}

install_docker() {
  need docker
  local dir dc; dir="${COMPOSE_DIR:-$PWD}"; dc="$(compose_cmd)"
  [ -n "$tag" ] || resolve_latest_tag
  write_compose_file "$dir"
  write_compose_env "$dir"
  # Record the deploy BEFORE pull/up: a port conflict that fails
  # `up -d` must not leave compose files on disk with no .install-meta.
  meta_write "$dir/.install-meta" "role=$ROLE" "deploy=docker" \
    "version=${artifact_version:-$resolved_version}" \
    "advertised_endpoint_set=$([ -n "$ADVERTISED" ] && echo yes || echo no)"
  ( cd "$dir" && $dc pull && $dc up -d )
}

# ─── Usage / help (layered: short by default, full via --help-all) ────
print_usage() {
  cat <<'USAGE'
Portunus installer & lifecycle manager (flag-driven, non-interactive)

Usage:
  install.sh <role> [options]    install a role
  install.sh <verb> [options]    manage an existing install

Roles:
  standalone   forward ports/traffic on THIS machine (no control plane)
  server       run a control panel for many nodes (with Web UI)
  client       connect THIS machine to an existing control panel

Manage verbs:
  status                         show what is installed and running
  service start|stop|restart     control the service
  upgrade                        upgrade to the latest release
  config get|set <key> [value]   view/change a server config key
  env                            print all server config keys + values
  uninstall [--purge]            remove (--purge also deletes data)
  domain <fqdn>                  set up HTTPS via Caddy (server)

Options:
  --version V                    install a specific version (default: latest)
  --deploy binary|docker         deployment form (default: binary)
  --bin-dir D                    binary install dir (default: /usr/local/bin)
  --compose-dir D                docker compose project dir (default: cwd)
  --enroll '<uri>'               (client) self-enroll during install
  --domain FQDN                  (server) HTTPS via Caddy + Let's Encrypt
  --acme-email A                 (server --domain) ACME contact email
  --skip-dns-check               (server --domain) skip the DNS pre-check
  --data-dir D                   (server) data directory
  --advertised-endpoint H:P      (server) endpoint clients dial
  --operator-http-listen A       (server) operator HTTP bind (default 127.0.0.1:7080)
  --config PATH                  (standalone) config file the service reads
  --no-service                   install but do not enable/start the service
  --restart                      (config set) restart the service to apply
  --purge                        (uninstall) also delete the data dir / volume
  --dry-run                      print the plan and change nothing

Examples:
  install.sh server --advertised-endpoint panel.example:7443
  install.sh server --deploy docker --compose-dir ~/portunus
  install.sh server --domain panel.example.com --acme-email ops@example.com
  install.sh client --enroll 'portunus://panel.example.com:7443/enroll?...'
  install.sh standalone --config /etc/portunus/standalone.toml
  install.sh status
  install.sh service restart
  install.sh upgrade
  install.sh config set advertised-endpoint panel.example:7443 --restart
  install.sh config get advertised-endpoint
  install.sh uninstall --purge

More:  install.sh --help-all     adds automation/CI seam flags
USAGE
}

print_usage_all() {
  print_usage
  cat <<'USAGE'

Automation / CI seams (stable; exercised by scripts/install.test.sh):
  --effective-advertised         print the resolved advertised endpoint, exit
  --detect-deploy [DIR]          print binary|docker for DIR/host, exit
  --detect-init                  print systemd|openrc|none, exit
  --detect-ip                    print "<ip> <provenance>", exit
  --resolve-meta                 print the resolved .install-meta path, exit
  --meta-write FILE k=v...       write an install-meta file, exit
  --meta-read FILE KEY           read one key from an install-meta file, exit
  --valid-endpoint H:P           exit 0/1 on host:port validity
  --valid-fqdn FQDN              exit 0/1 on FQDN validity
  --valid-email ADDR             exit 0/1 on ACME-email validity
  --render-dropin                print the systemd ExecStart drop-in, exit
  --render-caddy FQDN [PORT]     print the managed Caddy block, exit
  --render-openrc ROLE           print the OpenRC init script, exit
  --render-confd ROLE [CFG]      print the OpenRC conf.d body, exit
  --render-config-dropin ROLE CFG  print the standalone config drop-in, exit
USAGE
}

# ─── Arg parse + dispatch ─────────────────────────────────────────────
parse_args() {
  while [ $# -gt 0 ]; do
    case "$1" in
      client|server|standalone) ROLE="$1"; [ -z "$VERB" ] && VERB="install" ;;
      install|uninstall|upgrade|status|service|config|env|domain) VERB="$1" ;;
      start|stop|restart) SERVICE_ACTION="$1" ;;
      get|set) CONFIG_OP="$1" ;;
      --version) shift; [ $# -gt 0 ] || die "--version needs a value"; VERSION="$1" ;;
      --bin-dir) shift; [ $# -gt 0 ] || die "--bin-dir needs a value"; BIN_DIR="$1" ;;
      --compose-dir) shift; [ $# -gt 0 ] || die "--compose-dir needs a value"; COMPOSE_DIR="$1" ;;
      --deploy) shift; case "${1:-}" in binary|docker) DEPLOY="$1" ;; *) die "--deploy must be binary|docker" ;; esac ;;
      --advertised-endpoint) shift; [ $# -gt 0 ] || die "--advertised-endpoint needs a value"; ADVERTISED="$1" ;;
      --domain) shift; [ $# -gt 0 ] || die "--domain needs a value"; DOMAIN="$1" ;;
      --acme-email) shift; [ $# -gt 0 ] || die "--acme-email needs a value"; ACME_EMAIL="$1" ;;
      --skip-dns-check) SKIP_DNS_CHECK="yes" ;;
      --data-dir) shift; [ $# -gt 0 ] || die "--data-dir needs a value"; DATA_DIR="$1" ;;
      --operator-http-listen) shift; [ $# -gt 0 ] || die "--operator-http-listen needs a value"; OP_HTTP_LISTEN="$1" ;;
      --no-service) NO_SERVICE="yes" ;;
      --config) shift; [ $# -gt 0 ] || die "--config needs a value"; CONFIG_PATH="$1" ;;
      --enroll) shift; [ $# -gt 0 ] || die "--enroll needs a value"; ENROLL_URI="$1" ;;
      --restart) RESTART="yes" ;;
      --purge) PURGE="yes" ;;
      --dry-run) DRY_RUN="yes" ;;
      -h|--help) print_usage; exit 0 ;;
      --help-all) print_usage_all; exit 0 ;;
      --meta-write) shift; f="$1"; shift; meta_write "$f" "$@"; exit 0 ;;
      --meta-read) shift; f="$1"; k="$2"; meta_read "$f" "$k"; exit $? ;;
      --detect-deploy) shift; detect_deploy "${1:-}"; exit 0 ;;
      --detect-ip) detect_public_ip; printf '%s %s\n' "$DETECTED_IP" "$DETECTED_PROV"; exit 0 ;;
      --valid-fqdn) shift; valid_fqdn "${1:-}" && exit 0 || exit 1 ;;
      --valid-email) shift; valid_email "${1:-}" && exit 0 || exit 1 ;;
      --render-caddy) shift; DOMAIN="${1:-}"; render_caddy_block "${2:-7080}"; exit 0 ;;
      --render-dropin) render_dropin; exit 0 ;;
      --detect-init) detect_init; printf '%s\n' "$INIT"; exit 0 ;;
      --render-openrc) shift 2>/dev/null || true; render_openrc "${1:-standalone}"; exit 0 ;;
      --render-confd) shift 2>/dev/null || true; render_confd "${1:-standalone}" "${2:-/etc/portunus/standalone.toml}"; exit 0 ;;
      --render-config-dropin) shift 2>/dev/null || true; render_config_dropin "${1:-standalone}" "${2:-/etc/portunus/standalone.toml}"; exit 0 ;;
      --effective-advertised) PRINT_EFF=yes ;;
      --valid-endpoint) shift; valid_host_port "${1:-}" && exit 0 || exit 1 ;;
      --resolve-meta) current_meta_file && exit 0 || exit 1 ;;
      *) if [ "$VERB" = domain ] && [ -z "$DOMAIN" ]; then DOMAIN="$1";
         elif [ "$VERB" = config ] && [ -z "$CONFIG_KEY" ]; then CONFIG_KEY="$1";
         elif [ "$VERB" = config ] && [ -z "$CONFIG_VALUE" ]; then CONFIG_VALUE="$1";
         else die "unknown argument: $1"; fi ;;
    esac
    shift
  done
}

main() {
  parse_args "$@"
  # No actionable verb/role: this is a flag-only CLI — print usage and exit.
  if [ -z "$VERB" ] && [ -z "$ROLE" ]; then
    print_usage >&2
    exit 2
  fi
  [ -n "$VERB" ] || VERB="install"
  [ "$(id -u)" = 0 ] && SUDO="" || SUDO="sudo"
  detect_platform
  resolve_version_static
  [ -n "$DOMAIN" ] && [ -n "$ROLE" ] && [ "$ROLE" != server ] && die "--domain is server-only"
  [ -n "$ENROLL_URI" ] && [ "$ROLE" != client ] && die "--enroll is client-only"
  [ -n "$ENROLL_URI" ] && [ "$DEPLOY" = docker ] && die "--enroll is binary-only (for Docker pass PORTUNUS_ENROLL_URI to the container)"
  # Reject a malformed ACME email before it can reach the root-written
  # Caddyfile as a `tls <email>` directive (Caddy directive injection).
  if [ -n "$ACME_EMAIL" ] && ! valid_email "$ACME_EMAIL"; then
    die "invalid --acme-email '$ACME_EMAIL' — expected a single-line address like ops@example.com"
  fi
  apply_advertised_default
  apply_install_defaults
  if [ "$PRINT_EFF" = yes ]; then printf '%s\n' "$ADVERTISED"; exit 0; fi
  if [ "$DRY_RUN" = "yes" ]; then
    case "$VERB" in
      install) [ -n "$ROLE" ] || die "role required: client, server, or standalone"; print_plan; exit 0 ;;
      config)
        # Mirror the real path's server-only role guard so --dry-run does not
        # report a config op as valid for a client/standalone install.
        _dmf="$(current_meta_file 2>/dev/null || true)"
        _drr="$([ -n "$_dmf" ] && meta_read "$_dmf" role 2>/dev/null || echo server)"
        case "${_drr:-server}" in server) ;; *) echo "config get/set applies to the server role only (standalone: edit /etc/portunus/standalone.toml directly; client has no such knobs)" >&2; exit 2 ;; esac
        validate_config_key; echo "verb: config ${CONFIG_OP:-get} ${CONFIG_KEY} (dry-run)"; exit 0 ;;
      *) echo "verb: ${VERB} (dry-run; no side effects)"; exit 0 ;;
    esac
  fi
  dispatch_verb
}

# Seed the advertised-endpoint default. Public probe → local NIC →
# loopback. Sets DETECTED_IP + DETECTED_PROV (a provenance token,
# surfaced only by the --detect-ip seam). Never fatal.
# PORTUNUS_SKIP_IP_PROBE=1 skips the external probe (offline/test/CI).
valid_ip() { case "$1" in ""|*[!0-9a-fA-F.:]*) return 1 ;; *[.:]*) return 0 ;; *) return 1 ;; esac; }
detect_public_ip() {
  [ -n "$DETECTED_IP" ] && return 0
  local ip=""
  if [ "${PORTUNUS_SKIP_IP_PROBE:-0}" != 1 ] && command -v curl >/dev/null 2>&1; then
    local u
    for u in https://api.ipify.org https://ifconfig.me/ip https://icanhazip.com; do
      ip="$(curl -fsS --max-time 3 "$u" 2>/dev/null | tr -d '[:space:]' || true)"
      if valid_ip "$ip"; then DETECTED_IP="$ip"; DETECTED_PROV="prov_detected"; return 0; fi
    done
  fi
  ip=""
  if command -v ip >/dev/null 2>&1; then
    ip="$(ip route get 1.1.1.1 2>/dev/null | sed -n 's/.* src \([0-9.]*\).*/\1/p' | head -1 || true)"
  fi
  if [ -z "$ip" ] && command -v hostname >/dev/null 2>&1; then
    ip="$(hostname -I 2>/dev/null | tr ' ' '\n' | grep -v '^127\.' | head -1 || true)"
  fi
  if valid_ip "$ip"; then DETECTED_IP="$ip"; DETECTED_PROV="prov_nic"; return 0; fi
  DETECTED_IP="127.0.0.1"; DETECTED_PROV="prov_loopback"; return 0
}

# ─── Caddy / HTTPS ────────────────────────────────────────────────────
valid_fqdn() {
  case "$1" in
    ""|*[!a-zA-Z0-9.-]*) return 1 ;;
    .*|-*|*.|*-|*..*) return 1 ;;
  esac
  case "$1" in *.*) return 0 ;; *) return 1 ;; esac
}

valid_email() {
  # Single-line ACME contact email. Rejects empty, embedded whitespace,
  # newlines, control characters, and Caddyfile metacharacters so the
  # value cannot inject directives when emitted into the managed Caddy
  # block (`tls <email>`). Structural check only — no deliverability
  # guarantee. Mirrors valid_fqdn for the domain part.
  case "$1" in
    ""|*[!a-zA-Z0-9.@_+-]*) return 1 ;;   # empty or an illegal character
    @*|*@|*@*@*) return 1 ;;               # empty local/domain, or not exactly one @
  esac
  case "${1#*@}" in
    .*|-*|*.|*-|*..*) return 1 ;;
    *.*) return 0 ;;
    *) return 1 ;;
  esac
}

op_http_port() {  # echo the loopback port Caddy must proxy to
  local p="${OP_HTTP_LISTEN##*:}"
  case "$p" in ''|*[!0-9]*) echo 7080 ;; *) echo "$p" ;; esac
}

dns_a_records() {
  if command -v getent >/dev/null 2>&1; then
    getent ahostsv4 "$1" 2>/dev/null | awk '{print $1}' | sort -u
  elif command -v dig >/dev/null 2>&1; then
    dig +short A "$1" 2>/dev/null | sed '/^$/d'
  fi
}

dns_points_here() {  # $1 domain ; uses detect_public_ip
  local d="$1" a ip
  detect_public_ip; ip="$DETECTED_IP"
  printf 'Checking %s resolves to this server (%s)…\n' "$d" "$ip"
  a="$(dns_a_records "$d" | tr '\n' ' ')"
  if printf '%s' "$a" | grep -qw "$ip"; then
    printf 'DNS OK: %s → %s\n' "$d" "$ip"; return 0
  fi
  printf 'DNS for %s does not point here. A record(s): %s ; this server: %s\n' \
    "$d" "${a:-none}" "$ip"
  die "DNS for $d must point to $ip (or pass --skip-dns-check)"
}

render_caddy_block() {  # $1 op-http port ; prints managed block
  local port="$1"
  # The ACME contact email is emitted as a per-site `tls` directive rather
  # than a global `{ email }` options block: a global block is only valid as
  # the very first thing in a Caddyfile, so appending one after existing site
  # blocks makes Caddy refuse to start ("Unexpected '}' ... no matching
  # opening brace"). The `tls <email>` form is valid anywhere and keeps the
  # managed block self-contained and position-independent.
  echo "# >>> portunus >>>"
  echo "${DOMAIN} {"
  [ -n "$ACME_EMAIL" ] && echo "    tls ${ACME_EMAIL}"
  echo "    reverse_proxy 127.0.0.1:${port}"
  echo "}"
  echo "# <<< portunus <<<"
}

ensure_caddy() {
  command -v caddy >/dev/null 2>&1 && return 0
  echo "Installing Caddy…"
  local id="" like=""
  [ -r /etc/os-release ] && . /etc/os-release && id="${ID:-}" && like="${ID_LIKE:-}"
  case "$id $like" in
    *debian*|*ubuntu*)
      sudo apt-get install -y -qq debian-keyring debian-archive-keyring apt-transport-https curl >/dev/null 2>&1 || true
      curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' | sudo gpg --dearmor --batch --yes -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
      curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' | sudo tee /etc/apt/sources.list.d/caddy-stable.list >/dev/null
      sudo chmod o+r /usr/share/keyrings/caddy-stable-archive-keyring.gpg /etc/apt/sources.list.d/caddy-stable.list
      sudo apt-get update -qq >/dev/null 2>&1 || true
      sudo apt-get install -y -qq caddy >/dev/null 2>&1 || die "caddy install failed (apt)" ;;
    *fedora*|*rhel*|*centos*)
      if command -v dnf >/dev/null 2>&1; then sudo dnf copr enable -y @caddy/caddy >/dev/null 2>&1 && sudo dnf install -y -q caddy >/dev/null 2>&1 || die "caddy install failed (dnf)"
      else sudo yum copr enable -y @caddy/caddy >/dev/null 2>&1 && sudo yum install -y -q caddy >/dev/null 2>&1 || die "caddy install failed (yum)"; fi ;;
    *) die "cannot auto-install Caddy on this distro; install it and add:\n  ${DOMAIN} {\n      reverse_proxy 127.0.0.1:$(op_http_port)\n  }" ;;
  esac
}

write_caddy_block() {
  local port; port="$(op_http_port)"
  sudo install -d -m 0755 "$(dirname "$CADDYFILE")"
  if [ -f "$CADDYFILE" ]; then
    sudo cp "$CADDYFILE" "${CADDYFILE}.portunus.$(date +%Y%m%d%H%M%S).bak"
    # A fresh apt/dnf install ships a boilerplate Caddyfile whose `:80`
    # file-server site binds port 80 (colliding with the ACME HTTP-01
    # challenge / HTTP→HTTPS redirect Caddy needs for the domain) and carries
    # no operator config. Replace it wholesale — the copy above is the backup.
    # A user-authored Caddyfile is preserved; only the managed block is
    # rewritten in place.
    if sudo grep -q 'root \* /usr/share/caddy' "$CADDYFILE" 2>/dev/null; then
      sudo sh -c ": > '$CADDYFILE'"
    else
      sudo sed -i '/^# >>> portunus >>>$/,/^# <<< portunus <<<$/d' "$CADDYFILE"
    fi
  fi
  render_caddy_block "$port" | sudo tee -a "$CADDYFILE" >/dev/null
}

caddy_reload() {
  if command -v systemctl >/dev/null 2>&1; then
    sudo systemctl enable caddy >/dev/null 2>&1 || true
    sudo systemctl restart caddy
  else
    caddy reload --config "$CADDYFILE" 2>/dev/null || echo "start Caddy manually: caddy run --config $CADDYFILE"
  fi
}

verify_https() {
  local d="$1"
  printf 'Verifying https://%s/ (Let'\''s Encrypt issuance can take ~30s)…\n' "$d"
  for _ in $(seq 1 12); do
    if curl -fsS --max-time 5 -o /dev/null "https://${d}/" 2>/dev/null; then
      printf 'HTTPS ready: https://%s/\n' "$d"; return 0
    fi
    sleep 5
  done
  printf 'Could not verify https://%s/ yet. Check: journalctl -u caddy -e ; DNS propagation.\n' "$d"; return 0
}

setup_caddy_domain() {
  [ "$ROLE" = server ] || die "domain/HTTPS is server-only"
  valid_fqdn "$DOMAIN" || die "invalid domain '$DOMAIN' — expected an FQDN like portunus.example.com"
  if [ "$DRY_RUN" = yes ]; then
    echo "domain:           $DOMAIN"
    echo "reverse_proxy:    127.0.0.1:$(op_http_port)"
    echo "caddyfile:        $CADDYFILE"
    echo "dns_precheck:     $([ "$SKIP_DNS_CHECK" = yes ] && echo skipped || echo enabled)"
    return 0
  fi
  [ "$SKIP_DNS_CHECK" = yes ] || dns_points_here "$DOMAIN"
  ensure_caddy
  write_caddy_block
  caddy_reload
  printf 'Caddy configured for %s\n' "$DOMAIN"
  verify_https "$DOMAIN"
  echo "Note: the web UI is now publicly reachable over HTTPS; it stays protected by operator login/token."
}

lifecycle_domain() {
  local mf; mf="$(current_meta_file)" || die "No Portunus install detected (no .install-meta and no probe match)."
  ROLE="$(meta_read "$mf" role || echo server)"
  [ "$ROLE" = server ] || die "domain/HTTPS is server-only"
  [ -n "$DOMAIN" ] || die "usage: install.sh domain <fqdn>"
  OP_HTTP_LISTEN="$(meta_read "$mf" op_http_listen 2>/dev/null || echo '')"
  DEPLOY="$(meta_read "$mf" deploy || echo binary)"
  setup_caddy_domain
  apply_advertised_default
  if [ "$DRY_RUN" != yes ] && [ -n "$ADVERTISED" ]; then
    if [ "$DEPLOY" = docker ]; then
      COMPOSE_DIR="$(dirname "$mf")"
      [ -f "$COMPOSE_DIR/compose.yml" ] && sudo cp "$COMPOSE_DIR/compose.yml" "$COMPOSE_DIR/compose.yml.portunus.$(date +%Y%m%d%H%M%S).bak" && rm -f "$COMPOSE_DIR/compose.yml"
      write_compose_file "$COMPOSE_DIR"; write_compose_env "$COMPOSE_DIR"
      ( cd "$COMPOSE_DIR" && $(compose_cmd) up -d ) || true
    else
      write_server_dropin
      command -v systemctl >/dev/null 2>&1 && sudo systemctl restart "portunus-$ROLE" 2>/dev/null || true
    fi
    echo "→ advertised endpoint set to ${ADVERTISED}; the server re-aligns its gRPC cert SAN on restart."
    echo "→ existing client bundles must be re-issued: portunus-server enroll-client <name>"
  fi
  if [ "$DRY_RUN" != yes ]; then
    meta_write "$mf" "role=$ROLE" "deploy=$DEPLOY" \
      "version=$(meta_read "$mf" version || echo '?')" \
      "advertised_endpoint_set=$(meta_read "$mf" advertised_endpoint_set || echo no)" \
      "domain=$DOMAIN"
  fi
}

# Explicit --advertised-endpoint wins; otherwise a server --domain
# derives advertised = <domain>:7443. Idempotent; safe to call twice.
apply_advertised_default() {
  [ "$ROLE" = server ] || return 0
  [ -n "$DOMAIN" ] || return 0
  [ -z "$ADVERTISED" ] || return 0
  ADVERTISED="${DOMAIN}:7443"
  return 0
}

apply_install_defaults() {
  [ "${VERB:-install}" = "install" ] || return 0
  [ -n "${ROLE:-}" ] || return 0
  if [ -n "$CONFIG_PATH" ] && [ "$ROLE" != standalone ]; then
    die "--config is only valid for the standalone role (client uses --bundle, server uses --data-dir)"
  fi
  [ -z "$CONFIG_PATH" ] && [ "$ROLE" = standalone ] && CONFIG_PATH="/etc/portunus/standalone.toml"
  # A service unit embeds this path, so it must be absolute — resolve a
  # relative --config against the invoking cwd.
  case "$CONFIG_PATH" in
    ""|/*) : ;;
    *) CONFIG_PATH="$(pwd)/$CONFIG_PATH" ;;
  esac
  if [ -z "${DEPLOY:-}" ]; then
    DEPLOY="binary"
    return 0
  fi
}

dispatch_verb() {
  case "$VERB" in
    install)
      [ -n "$ROLE" ] || die "role required: client, server, or standalone"
      [ -z "$DEPLOY" ] && DEPLOY="binary"
      if [ "$DEPLOY" = "docker" ]; then
        install_docker          # writes its own .install-meta pre-up
      else
        install_binary
        detect_init
        svc install "$ROLE" "$CONFIG_PATH"
        [ "$ROLE" = "server" ] && [ "$INIT" = systemd ] && write_server_dropin
        # Record the install BEFORE attempting to start: the binary, unit,
        # and config are already on disk, so even if enable/start fails the
        # deploy is recoverable via uninstall/upgrade/status.
        meta_write "$(meta_path_for)" "role=$ROLE" "deploy=$DEPLOY" "version=$resolved_version" "init=$INIT" "advertised_endpoint_set=$([ -n "$ADVERTISED" ] && echo yes || echo no)"
        if [ "$ROLE" = client ] && [ -n "$ENROLL_URI" ]; then
          place_client_bundle "$ENROLL_URI"
        fi
        if service_should_start; then svc enable_start "$ROLE"; fi
      fi
      if [ "$ROLE" = server ] && [ -n "$DOMAIN" ]; then
        meta_write "$(meta_path_for)" "role=$ROLE" "deploy=$DEPLOY" "version=$resolved_version" "advertised_endpoint_set=$([ -n "$ADVERTISED" ] && echo yes || echo no)" "domain=$DOMAIN"
        setup_caddy_domain
      fi
      echo; print_next_steps
      ;;
    uninstall|upgrade|status|service|config|env|domain) lifecycle_"$VERB" ;;
    *) die "verb '${VERB}' not yet implemented" ;;
  esac
}

# ─── Lifecycle ────────────────────────────────────────────────────────
SCOPED_KEYS="advertised-endpoint data-dir operator-http-listen"
validate_config_key() {
  [ -n "$CONFIG_KEY" ] || die "config key required (allowed: $SCOPED_KEYS)"
  case " $SCOPED_KEYS " in *" $CONFIG_KEY "*) ;; *) die "unknown config key: $CONFIG_KEY (allowed: advertised-endpoint data-dir operator-http-listen)" ;; esac
}

current_meta_file() {
  # An explicit --compose-dir is a hard scope: resolve ONLY its meta and
  # never fall back to a system binary path. Otherwise a destructive
  # `uninstall --purge --compose-dir X` could silently target
  # /var/lib/portunus when X has no meta.
  local f
  if [ -n "${COMPOSE_DIR:-}" ]; then
    f="$COMPOSE_DIR/.install-meta"
    [ -r "$f" ] && { echo "$f"; return 0; }
    return 1
  fi
  # No explicit compose-dir: cwd (Docker user's pwd) then system paths.
  for f in "$PWD/.install-meta" "/var/lib/portunus/.install-meta" "/etc/portunus/.install-meta"; do
    [ -r "$f" ] && { echo "$f"; return 0; }
  done
  return 1
}

resolved_deploy() {
  local mf; mf="$(current_meta_file 2>/dev/null || true)"
  if [ -n "$mf" ]; then meta_read "$mf" deploy || echo "binary"; else detect_deploy "${COMPOSE_DIR:-}"; fi
}

lifecycle_status() {
  local mf d; mf="$(current_meta_file 2>/dev/null || true)"
  if [ -z "$mf" ]; then echo "No Portunus install detected (no .install-meta and no probe match)."; return 0; fi
  d="$(meta_read "$mf" deploy || echo binary)"
  echo "meta:    $mf"
  echo "role:    $(meta_read "$mf" role || echo '?')"
  echo "deploy:  $d"
  echo "version: $(meta_read "$mf" version || echo '?')"
  echo "advertised_endpoint_set: $(meta_read "$mf" advertised_endpoint_set || echo '?')"
  if [ "$d" = docker ]; then ( cd "$(dirname "$mf")" && $(compose_cmd) ps ) 2>/dev/null || true
  else
    _i="$(meta_read "$mf" init 2>/dev/null || true)"; case "$_i" in systemd|openrc|none) INIT="$_i" ;; *) detect_init ;; esac
    svc status "$(meta_read "$mf" role || echo server)" 2>/dev/null || true
  fi
}

lifecycle_service() {
  [ -n "$SERVICE_ACTION" ] || die "service action required: start|stop|restart"
  local d r mf; mf="$(current_meta_file)" || die "No Portunus install detected (no .install-meta and no probe match)."
  d="$(meta_read "$mf" deploy || echo binary)"; r="$(meta_read "$mf" role || echo server)"
  if [ "$d" = docker ]; then
    ( cd "$(dirname "$mf")" && case "$SERVICE_ACTION" in start) $(compose_cmd) up -d ;; stop) $(compose_cmd) stop ;; restart) $(compose_cmd) restart ;; esac )
  else
    [ "$(id -u)" = 0 ] && SUDO="" || SUDO="sudo"
    _i="$(meta_read "$mf" init 2>/dev/null || true)"; case "$_i" in systemd|openrc|none) INIT="$_i" ;; *) detect_init ;; esac
    case "$SERVICE_ACTION" in start) svc start "$r" ;; stop) svc stop "$r" ;; restart) svc restart "$r" ;; esac
  fi
}

lifecycle_upgrade() {
  local mf cur; mf="$(current_meta_file)" || die "No Portunus install detected (no .install-meta and no probe match)."
  ROLE="$(meta_read "$mf" role || echo server)"; DEPLOY="$(meta_read "$mf" deploy || echo binary)"
  cur="$(meta_read "$mf" version || echo 0)"
  detect_platform; resolve_latest_tag
  if [ "$cur" = "$artifact_version" ]; then echo "Already at $cur; nothing to upgrade."; return 0; fi
  if [ "$DEPLOY" = docker ]; then
    COMPOSE_DIR="$(dirname "$mf")"
    # Bump the pinned image tag in the existing compose file. write_compose_file
    # preserves an operator's compose.yml verbatim, so without this the upgrade
    # would re-pull the OLD tag and only the recorded meta version would change.
    _cf="$COMPOSE_DIR/compose.yml"; [ -f "$_cf" ] || _cf="$COMPOSE_DIR/compose.yaml"
    [ -f "$_cf" ] && sed -i "s#\(ghcr.io/zingerlittlebee/portunus-[a-z]*:\)[^[:space:]\"]*#\1${artifact_version}#g" "$_cf"
    install_docker
  else
    [ "$(id -u)" = 0 ] && SUDO="" || SUDO="sudo"
    _i="$(meta_read "$mf" init 2>/dev/null || true)"; case "$_i" in systemd|openrc|none) INIT="$_i" ;; *) detect_init ;; esac
    install_binary; svc restart "$ROLE" 2>/dev/null || true
  fi
  meta_write "$mf" "role=$ROLE" "deploy=$DEPLOY" "version=$artifact_version" "init=$INIT"
}

lifecycle_uninstall() {
  local mf r d; mf="$(current_meta_file)" || die "No Portunus install detected (no .install-meta and no probe match)."
  r="$(meta_read "$mf" role || echo server)"; d="$(meta_read "$mf" deploy || echo binary)"
  if [ "$d" = docker ]; then ( cd "$(dirname "$mf")" && $(compose_cmd) down )
  else
    [ "$(id -u)" = 0 ] && SUDO="" || SUDO="sudo"
    _i="$(meta_read "$mf" init 2>/dev/null || true)"; case "$_i" in systemd|openrc|none) INIT="$_i" ;; *) detect_init ;; esac
    svc stop "$r" 2>/dev/null || true
    svc disable "$r" 2>/dev/null || true
    svc remove "$r" 2>/dev/null || true
    ${SUDO:-} rm -f "/usr/local/bin/portunus-$r"
    ${SUDO:-} rm -f "/etc/systemd/system/portunus-server.service.d/10-portunus.conf" 2>/dev/null || true
    command -v systemctl >/dev/null 2>&1 && ${SUDO:-} systemctl daemon-reload 2>/dev/null || true
  fi
  if [ -f "$CADDYFILE" ] && grep -q '^# >>> portunus >>>$' "$CADDYFILE" 2>/dev/null; then
    sudo cp "$CADDYFILE" "${CADDYFILE}.portunus.$(date +%Y%m%d%H%M%S).bak"
    sudo sed -i '/^# >>> portunus >>>$/,/^# <<< portunus <<<$/d' "$CADDYFILE"
    command -v systemctl >/dev/null 2>&1 && sudo systemctl reload caddy 2>/dev/null || true
    echo "→ removed Caddy block from $CADDYFILE"
  fi
  if [ "$PURGE" = yes ]; then
    local dd; dd="$(dirname "$mf")"
    sudo rm -rf "$dd"; echo "→ purged $dd"
  fi
  rm -f "$mf" 2>/dev/null || true
}

# Extract the token following a CLI flag from a command string. Handles both
# the systemd ExecStart form (`--flag value`) and the compose `command:` form
# (`"--flag", "value"`) by normalizing quotes/commas/brackets to spaces first.
flag_value_from() {  # $1 = text, $2 = flag (e.g. --advertised-endpoint)
  printf '%s' "$1" | tr ',"[]' '    ' | awk -v f="$2" '{for(i=1;i<=NF;i++) if($i==f){print $(i+1); exit}}'
}

# Map a scoped config key to the server CLI flag it controls.
config_key_flag() {  # $1 = key
  case "$1" in
    advertised-endpoint)  echo "--advertised-endpoint" ;;
    operator-http-listen) echo "--operator-http-listen" ;;
    data-dir)             echo "--data-dir" ;;
  esac
}

# Validate a `config set` value before it is rendered into a CLI arg (systemd
# ExecStart, openrc server_args, or the compose JSON command array). Reject
# anything that could break out of the JSON string, inject a systemd directive
# (a newline), or smuggle shell/quoting metacharacters. host:port keys get the
# stricter valid_host_port; data-dir must be an absolute, path-safe string.
valid_config_value() {  # $1 key, $2 value
  [ -n "$2" ] || return 1
  case "$1" in
    advertised-endpoint|operator-http-listen)
      case "$2" in *:*) ;; *) return 1 ;; esac   # require an explicit :port
      valid_host_port "$2" ;;
    data-dir)
      case "$2" in /*) ;; *) return 1 ;; esac     # absolute path only
      case "$2" in *[!A-Za-z0-9._/-]*) return 1 ;; *) return 0 ;; esac ;;
    *) return 1 ;;
  esac
}

# The server consumes advertised-endpoint / operator-http-listen / data-dir as
# CLI flags ONLY — no env binding — so config reads/writes where the flags
# actually live: the systemd ExecStart override or openrc conf.d server_args=
# (binary), or the compose `command:` array (docker). config set re-renders
# from the install primitive, hydrating siblings so one key changes in isolation.
# Scoped keys are server-only, so the verb applies to the server role only.
lifecycle_config() {
  local mf; mf="$(current_meta_file)" || die "No Portunus install detected (no .install-meta and no probe match)."
  # A role-less meta defaults to server (parity with status/upgrade/uninstall);
  # only a concrete non-server role is rejected.
  local _r; _r="$(meta_read "$mf" role 2>/dev/null || echo server)"
  case "${_r:-server}" in server) ;; *) echo "config get/set applies to the server role only (standalone: edit /etc/portunus/standalone.toml directly; client has no such knobs)" >&2; return 2 ;; esac
  validate_config_key
  local d dir flag v
  d="$(meta_read "$mf" deploy || echo binary)"; dir="$(dirname "$mf")"
  flag="$(config_key_flag "$CONFIG_KEY")"

  if [ "$d" = docker ]; then
    # Operate on the file docker compose v2 actually uses: compose.yaml wins
    # over compose.yml. The installer only ever writes compose.yml, so normally
    # only one exists; this matters only if an operator added a second file.
    local src line; src="$dir/compose.yaml"; [ -f "$src" ] || src="$dir/compose.yml"
    line="$(grep -E '^[[:space:]]*command:' "$src" 2>/dev/null || true)"
    if [ "${CONFIG_OP:-get}" = get ]; then
      v="$(flag_value_from "$line" "$flag")"
      if [ -n "$v" ]; then printf '%s\n' "$v"; else printf '%s\n' '<unset>'; fi
      return 0
    fi
    [ -n "$CONFIG_VALUE" ] || die "config set needs a value"
    # data-dir maps to a fixed in-container volume path; changing it would
    # desync the command from the volume mount, so it is not config-settable.
    [ "$CONFIG_KEY" = data-dir ] && { echo "data-dir is fixed to the in-container volume path for Docker deploys and cannot be changed via config; edit the volume mount in compose.yml instead." >&2; return 2; }
    valid_config_value "$CONFIG_KEY" "$CONFIG_VALUE" || { echo "invalid value for $CONFIG_KEY: $CONFIG_VALUE" >&2; return 2; }
    # Hydrate from the compose so only the requested key changes, preserving the
    # pinned image tag and keeping the published port in sync with
    # operator-http-listen when we regenerate from the managed template.
    ROLE="$(meta_read "$mf" role || echo server)"
    OP_HTTP_LISTEN="$(flag_value_from "$line" --operator-http-listen)"
    ADVERTISED="$(flag_value_from "$line" --advertised-endpoint)"
    artifact_version="$(sed -n 's#.*ghcr.io/zingerlittlebee/portunus-[a-z]*:\([^"[:space:]]*\).*#\1#p' "$src" 2>/dev/null | head -1)"
    [ -n "$artifact_version" ] || artifact_version="$(meta_read "$mf" version 2>/dev/null || true)"
    [ -n "$artifact_version" ] || die "cannot determine the current image tag in $src"
    case "$CONFIG_KEY" in
      advertised-endpoint)  ADVERTISED="$CONFIG_VALUE" ;;
      operator-http-listen) OP_HTTP_LISTEN="$CONFIG_VALUE" ;;
    esac
    # Regenerating overwrites operator hand-edits to the managed compose. Back
    # up EVERY compose file present first (so nothing is lost and so a stale
    # compose.yml cannot keep write_compose_file from regenerating), then
    # regenerate and restore the operator's effective filename.
    local _stamp _cf; _stamp="$(date +%Y%m%d%H%M%S)"
    for _cf in "$dir/compose.yml" "$dir/compose.yaml"; do
      if [ -f "$_cf" ]; then cp "$_cf" "${_cf}.portunus.${_stamp}.bak" 2>/dev/null || true; fi
    done
    rm -f "$dir/compose.yml" "$dir/compose.yaml"
    write_compose_file "$dir"; write_compose_env "$dir"
    [ "$src" = "$dir/compose.yaml" ] && [ -f "$dir/compose.yml" ] && mv "$dir/compose.yml" "$dir/compose.yaml"
    echo "→ set ${CONFIG_KEY}=${CONFIG_VALUE}"
    # `compose restart` keeps the old command; only `up -d` recreates with it.
    if [ "$RESTART" = yes ]; then ( cd "$dir" && $(compose_cmd) up -d )
    else echo "→ restart to apply: re-run with --restart (recreates the container)"; fi
    return 0
  fi

  # Binary: the flags live in the systemd ExecStart override OR the openrc
  # conf.d, so config must follow the recorded init system — not assume systemd.
  local init _i; _i="$(meta_read "$mf" init 2>/dev/null || true)"
  case "$_i" in systemd|openrc|none) INIT="$_i" ;; *) detect_init ;; esac
  init="$INIT"
  hydrate_binary_config "$init"
  if [ "${CONFIG_OP:-get}" = get ]; then
    case "$CONFIG_KEY" in
      advertised-endpoint)  v="$ADVERTISED" ;;
      operator-http-listen) v="$OP_HTTP_LISTEN" ;;
      data-dir)             v="$DATA_DIR" ;;
    esac
    if [ -n "$v" ]; then printf '%s\n' "$v"; else printf '%s\n' '<unset>'; fi
    return 0
  fi
  [ -n "$CONFIG_VALUE" ] || die "config set needs a value"
  valid_config_value "$CONFIG_KEY" "$CONFIG_VALUE" || { echo "invalid value for $CONFIG_KEY: $CONFIG_VALUE" >&2; return 2; }
  case "$CONFIG_KEY" in
    advertised-endpoint)  ADVERTISED="$CONFIG_VALUE" ;;
    operator-http-listen) OP_HTTP_LISTEN="$CONFIG_VALUE" ;;
    data-dir)             DATA_DIR="$CONFIG_VALUE" ;;
  esac
  [ "$(id -u)" = 0 ] && SUDO="" || SUDO="sudo"   # config_sudo honors $SUDO
  # Back up the target before regenerating so a hand-edit (esp. an openrc
  # conf.d the operator annotated) is recoverable, mirroring the docker path.
  local tgt; [ "$init" = openrc ] && tgt="$(confd_file)" || tgt="$(systemd_dropin_file)"
  # Best-effort backup: a failed cp (e.g. declined sudo) must NOT abort under
  # set -e before the write — `|| true`, matching the docker path.
  if [ -f "$tgt" ]; then config_sudo cp "$tgt" "${tgt}.portunus.$(date +%Y%m%d%H%M%S).bak" 2>/dev/null || true; fi
  if [ "$init" = openrc ]; then write_server_confd; else write_server_dropin; fi
  echo "→ set ${CONFIG_KEY}=${CONFIG_VALUE}"
  if [ "$RESTART" = yes ]; then SERVICE_ACTION=restart; lifecycle_service
  else echo "→ restart to apply: install.sh service restart (or re-run with --restart)"; fi
}

lifecycle_env() { CONFIG_OP="get"; for CONFIG_KEY in advertised-endpoint operator-http-listen data-dir; do printf '%s=' "$CONFIG_KEY"; lifecycle_config; done; }


# Light host:port sanity (authoritative SAN/grammar check is the server's).
valid_host_port() {
  case "$1" in
    "") return 0 ;;  # blank ⇒ auto (tier-3/4 at runtime)
    *[!A-Za-z0-9.:_-]*) return 1 ;;
  esac
  case "$1" in
    *:[0-9]|*:[0-9][0-9]|*:[0-9][0-9][0-9]|*:[0-9][0-9][0-9][0-9]|*:[0-9][0-9][0-9][0-9][0-9])
      [ -n "${1%:*}" ] && return 0 ;;
  esac
  return 1
}

main "$@"

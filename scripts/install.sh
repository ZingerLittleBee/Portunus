#!/bin/sh
# Portunus installer. Downloads a release binary (and optionally installs
# the hardened systemd unit) for one role.
#
#   curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sh -s -- client
#   curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sudo sh -s -- server --systemd
#
# Flags: --version <X.Y.Z|vX.Y.Z>  --bin-dir DIR  --systemd  --yes  --dry-run
set -eu

REPO="ZingerLittleBee/Portunus"
RAW_BASE="https://raw.githubusercontent.com/${REPO}/main"
ROLE=""
VERSION=""
BIN_DIR="/usr/local/bin"
WANT_SYSTEMD="no"
ASSUME_YES="no"  # forward-compat stub; no interactive prompts exist yet
DRY_RUN="no"

die() { echo "error: $*" >&2; exit 1; }

while [ $# -gt 0 ]; do
  case "$1" in
    client|server) ROLE="$1" ;;
    --version) shift; [ $# -gt 0 ] || die "--version needs a value"; VERSION="$1" ;;
    --bin-dir) shift; [ $# -gt 0 ] || die "--bin-dir needs a value"; BIN_DIR="$1" ;;
    --systemd) WANT_SYSTEMD="yes" ;;
    --yes) ASSUME_YES="yes" ;;
    --dry-run) DRY_RUN="yes" ;;
    -h|--help) echo "usage: install.sh <client|server> [--version V] [--bin-dir DIR] [--systemd] [--yes] [--dry-run]"; exit 0 ;;
    *) die "unknown argument: $1" ;;
  esac
  shift
done

[ -n "$ROLE" ] || die "role required: client or server"

# Platform.
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

# Version: tag always has a leading v; artifact_version never does.
if [ -n "$VERSION" ]; then
  case "$VERSION" in
    v*) tag="$VERSION"; artifact_version="${VERSION#v}" ;;
    *)  tag="v$VERSION"; artifact_version="$VERSION" ;;
  esac
  resolved_version="$artifact_version"
else
  resolved_version="<latest, resolved at run time>"
  tag=""
  artifact_version=""
fi

rel() {
  # echo the release download URL for asset $1; requires resolved version.
  echo "https://github.com/${REPO}/releases/download/${tag}/$1"
}

print_plan() {
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
  echo "bin_dir:          ${BIN_DIR}"
  echo "systemd:          ${WANT_SYSTEMD}"
  echo "actions:          download+verify+install portunus-${ROLE} -> ${BIN_DIR}$( [ "$WANT_SYSTEMD" = yes ] && echo ' + install systemd unit' )"
}

# --dry-run short-circuits BEFORE any network call (incl. latest resolution).
if [ "$DRY_RUN" = "yes" ]; then
  print_plan
  exit 0
fi

need() { command -v "$1" >/dev/null 2>&1 || die "missing required tool: $1"; }
need curl
need tar
need uname

# Resolve latest if no explicit --version.
if [ -z "$tag" ]; then
  need sed
  tag="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
    | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n1)"
  [ -n "$tag" ] || die "could not resolve latest release tag"
  artifact_version="${tag#v}"
  resolved_version="$artifact_version"
fi

asset="portunus-${artifact_version}-${target}.tar.gz"
checksums="portunus-${artifact_version}-checksums.txt"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "→ downloading ${asset} (${tag})"
curl -fsSL "$(rel "$asset")" -o "$tmp/$asset" || die "download failed: $asset"
curl -fsSL "$(rel "$checksums")" -o "$tmp/$checksums" || die "download failed: $checksums"

echo "→ verifying sha256"
expected="$(grep -F " ${asset}" "$tmp/$checksums" | awk '{print $1}')"
[ -n "$expected" ] || die "no checksum entry for $asset"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmp/$asset" | awk '{print $1}')"
else
  actual="$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')"
fi
[ "$expected" = "$actual" ] || die "checksum mismatch for $asset"

tar -xzf "$tmp/$asset" -C "$tmp"
src="$tmp/portunus-${artifact_version}-${target}/portunus-${ROLE}"
[ -f "$src" ] || die "binary not found in archive: portunus-${ROLE}"

maybe_sudo() {
  if [ -w "$1" ] || [ "$(id -u)" = "0" ]; then sudo_cmd=""; else sudo_cmd="sudo"; fi
}
maybe_sudo "$BIN_DIR"
echo "→ installing portunus-${ROLE} to ${BIN_DIR}"
${sudo_cmd:-} install -m 0755 "$src" "${BIN_DIR}/portunus-${ROLE}"

if [ "$WANT_SYSTEMD" = "yes" ]; then
  if [ "$os" != "linux" ] || ! command -v systemctl >/dev/null 2>&1; then
    echo "warning: --systemd ignored (not Linux or systemctl missing)" >&2
  else
    unit="portunus-${ROLE}.service"
    echo "→ fetching hardened unit ${unit}"
    curl -fsSL "${RAW_BASE}/deploy/systemd/${unit}" -o "$tmp/$unit" || die "unit download failed"
    if [ "$ROLE" = "client" ]; then
      id portunus-client >/dev/null 2>&1 || sudo useradd --system --no-create-home --shell /usr/sbin/nologin portunus-client
      sudo install -d -o root -g portunus-client -m 0750 /etc/portunus
    else
      id portunus-server >/dev/null 2>&1 || sudo useradd --system --no-create-home --shell /usr/sbin/nologin portunus-server
      sudo install -d -o portunus-server -g portunus-server -m 0750 /var/lib/portunus
    fi
    sudo install -m 0644 "$tmp/$unit" "/etc/systemd/system/$unit"
    sudo systemctl daemon-reload
    echo "→ installed /etc/systemd/system/$unit"
  fi
fi

echo
echo "Done. Next steps:"
if [ "$ROLE" = "client" ]; then
  echo "  1. Get an enrollment command from the operator (Web UI Clients page)."
  echo "  2. portunus-client enroll 'portunus://...' --out ./client.bundle.json"
  if [ "$WANT_SYSTEMD" = "yes" ]; then
    echo "  3. sudo install -o root -g portunus-client -m 0640 ./client.bundle.json /etc/portunus/client.bundle.json"
    echo "  4. sudo systemctl enable --now portunus-client"
  else
    echo "  3. portunus-client"
  fi
else
  if [ "$WANT_SYSTEMD" = "yes" ]; then
    echo "  sudo systemctl enable --now portunus-server"
  else
    echo "  portunus-server --data-dir /var/lib/portunus serve"
  fi
fi

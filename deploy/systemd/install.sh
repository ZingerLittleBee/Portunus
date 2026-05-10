#!/usr/bin/env bash
# Install portunus-server and/or portunus-client systemd units.
#
# Usage (run as root):
#   ./install.sh server          # install just the server
#   ./install.sh client          # install just the client
#   ./install.sh server client   # install both
#
# Assumes the relevant binary is already at /usr/local/bin/portunus-{server,client}.
# Build it and copy yourself, e.g.:
#   cargo build --release -p portunus-server
#   sudo install -m 0755 target/release/portunus-server /usr/local/bin/portunus-server

set -euo pipefail

if [[ $EUID -ne 0 ]]; then
  echo "must run as root (try: sudo $0 $*)" >&2
  exit 1
fi

if [[ $# -eq 0 ]]; then
  echo "usage: $0 [server] [client]" >&2
  exit 2
fi

UNIT_DIR="$(cd "$(dirname "$0")" && pwd)"
TARGET_UNIT_DIR="/etc/systemd/system"

install_server() {
  if [[ ! -x /usr/local/bin/portunus-server ]]; then
    echo "ERROR: /usr/local/bin/portunus-server missing — install the binary first." >&2
    exit 3
  fi

  id portunus-server >/dev/null 2>&1 || \
    useradd --system --no-create-home --shell /usr/sbin/nologin portunus-server
  install -d -o portunus-server -g portunus-server -m 0750 /var/lib/portunus
  install -d -o root -g portunus-server -m 0750 /etc/portunus
  if [[ ! -f /etc/portunus/server.toml ]]; then
    if [[ -f "$UNIT_DIR/../server.toml.example" ]]; then
      install -o root -g portunus-server -m 0640 \
        "$UNIT_DIR/../server.toml.example" /etc/portunus/server.toml
      echo "→ wrote /etc/portunus/server.toml from server.toml.example (review before starting)"
    else
      echo "WARNING: /etc/portunus/server.toml not present and no example found." >&2
    fi
  fi

  install -m 0644 "$UNIT_DIR/portunus-server.service" "$TARGET_UNIT_DIR/"
  echo "→ installed portunus-server.service"
}

install_client() {
  if [[ ! -x /usr/local/bin/portunus-client ]]; then
    echo "ERROR: /usr/local/bin/portunus-client missing — install the binary first." >&2
    exit 3
  fi

  id portunus-client >/dev/null 2>&1 || \
    useradd --system --no-create-home --shell /usr/sbin/nologin portunus-client
  install -d -o root -g portunus-client -m 0750 /etc/portunus
  if [[ ! -f /etc/portunus/client.bundle.json ]]; then
    echo "→ /etc/portunus/client.bundle.json not present yet. Provision one on the server:"
    echo "    portunus-server --config-dir /var/lib/portunus provision-client <name> --out client.bundle.json"
    echo "  scp it here, then: install -o root -g portunus-client -m 0640 client.bundle.json /etc/portunus/"
  fi

  install -m 0644 "$UNIT_DIR/portunus-client.service" "$TARGET_UNIT_DIR/"
  echo "→ installed portunus-client.service"
}

for who in "$@"; do
  case "$who" in
    server) install_server ;;
    client) install_client ;;
    *) echo "unknown: $who (expected 'server' or 'client')" >&2; exit 2 ;;
  esac
done

systemctl daemon-reload
echo
echo "Done. Next steps:"
echo "  systemctl enable --now portunus-server   # if installed"
echo "  systemctl enable --now portunus-client   # if installed"
echo "  journalctl -u portunus-server -f         # tail logs"

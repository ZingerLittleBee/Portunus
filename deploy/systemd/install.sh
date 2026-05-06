#!/usr/bin/env bash
# Install forward-server and/or forward-client systemd units.
#
# Usage (run as root):
#   ./install.sh server          # install just the server
#   ./install.sh client          # install just the client
#   ./install.sh server client   # install both
#
# Assumes the relevant binary is already at /usr/local/bin/forward-{server,client}.
# Build it and copy yourself, e.g.:
#   cargo build --release -p forward-server
#   sudo install -m 0755 target/release/forward-server /usr/local/bin/forward-server

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
  if [[ ! -x /usr/local/bin/forward-server ]]; then
    echo "ERROR: /usr/local/bin/forward-server missing — install the binary first." >&2
    exit 3
  fi

  id forward-server >/dev/null 2>&1 || \
    useradd --system --no-create-home --shell /usr/sbin/nologin forward-server
  install -d -o forward-server -g forward-server -m 0750 /var/lib/forward
  install -d -o root -g forward-server -m 0750 /etc/forward
  if [[ ! -f /etc/forward/server.toml ]]; then
    if [[ -f "$UNIT_DIR/../server.toml.example" ]]; then
      install -o root -g forward-server -m 0640 \
        "$UNIT_DIR/../server.toml.example" /etc/forward/server.toml
      echo "→ wrote /etc/forward/server.toml from server.toml.example (review before starting)"
    else
      echo "WARNING: /etc/forward/server.toml not present and no example found." >&2
    fi
  fi

  install -m 0644 "$UNIT_DIR/forward-server.service" "$TARGET_UNIT_DIR/"
  echo "→ installed forward-server.service"
}

install_client() {
  if [[ ! -x /usr/local/bin/forward-client ]]; then
    echo "ERROR: /usr/local/bin/forward-client missing — install the binary first." >&2
    exit 3
  fi

  id forward-client >/dev/null 2>&1 || \
    useradd --system --no-create-home --shell /usr/sbin/nologin forward-client
  install -d -o root -g forward-client -m 0750 /etc/forward
  if [[ ! -f /etc/forward/client.bundle.json ]]; then
    echo "→ /etc/forward/client.bundle.json not present yet. Provision one on the server:"
    echo "    forward-server --config-dir /var/lib/forward provision-client <name> --out client.bundle.json"
    echo "  scp it here, then: install -o root -g forward-client -m 0640 client.bundle.json /etc/forward/"
  fi

  install -m 0644 "$UNIT_DIR/forward-client.service" "$TARGET_UNIT_DIR/"
  echo "→ installed forward-client.service"
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
echo "  systemctl enable --now forward-server   # if installed"
echo "  systemctl enable --now forward-client   # if installed"
echo "  journalctl -u forward-server -f         # tail logs"

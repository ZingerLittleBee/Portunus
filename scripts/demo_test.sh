#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

# shellcheck source=scripts/demo.sh
source scripts/demo.sh

assert_eq() {
  local want="$1" got="$2" msg="$3"
  if [[ "${got}" != "${want}" ]]; then
    printf 'not ok - %s: want %s, got %s\n' "${msg}" "${want}" "${got}" >&2
    exit 1
  fi
}

assert_ge() {
  local got="$1" min="$2" msg="$3"
  if (( got < min )); then
    printf 'not ok - %s: want >= %s, got %s\n' "${msg}" "${min}" "${got}" >&2
    exit 1
  fi
}

assert_eq download "$(traffic_profile_for_index 0)" "rule 0 profile"
assert_eq video "$(traffic_profile_for_index 1)" "rule 1 profile"
assert_eq intermittent "$(traffic_profile_for_index 2)" "rule 2 profile"
assert_eq download "$(traffic_profile_for_index 3)" "profile rotation"

tmp_dir="$(mktemp -d)"
trap 'rm -rf "${tmp_dir}"' EXIT

DATA_DIR="${tmp_dir}"
DEMO_TRAFFIC_SEED_MINUTES=9
RULE_LISTEN=(18001 18002 18003)
RULE_USER=(1 1 2)
RULE_EDGE=(edge-1 edge-1 edge-2)

python3 - "${DATA_DIR}/state.db" <<'PY'
import sqlite3
import sys

conn = sqlite3.connect(sys.argv[1])
conn.execute(
    "CREATE TABLE traffic_samples_1m ("
    "user_id TEXT NOT NULL,"
    "client_name TEXT NOT NULL,"
    "ts_minute INTEGER NOT NULL,"
    "bytes_in INTEGER NOT NULL,"
    "bytes_out INTEGER NOT NULL,"
    "PRIMARY KEY (user_id, client_name, ts_minute)"
    ")"
)
conn.commit()
conn.close()
PY

seed_demo_traffic_history >/dev/null

sample_minutes="$(
  python3 - "${DATA_DIR}/state.db" <<'PY'
import sqlite3
import sys

conn = sqlite3.connect(sys.argv[1])
print(conn.execute("SELECT COUNT(DISTINCT ts_minute) FROM traffic_samples_1m").fetchone()[0])
conn.close()
PY
)"
assert_ge "${sample_minutes}" 6 "seeded traffic history minutes"

port_file="${tmp_dir}/http-port"
python3 - "${port_file}" <<'PY' &
from http.server import BaseHTTPRequestHandler, HTTPServer
import sys

class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(401)
        self.end_headers()

    def log_message(self, format, *args):
        return

server = HTTPServer(("127.0.0.1", 0), Handler)
with open(sys.argv[1], "w", encoding="utf-8") as f:
    f.write(str(server.server_port))
server.serve_forever()
PY
http_pid="$!"

for _ in 1 2 3 4 5; do
  [[ -s "${port_file}" ]] && break
  sleep 0.1
done
HTTP_ENDPOINT="127.0.0.1:$(cat "${port_file}")"
: >"${DATA_DIR}/server.log"
if ! server_listening; then
  printf 'not ok - server readiness should use HTTP, not startup log text\n' >&2
  kill "${http_pid}" 2>/dev/null || true
  exit 1
fi
kill "${http_pid}" 2>/dev/null || true

printf 'ok - demo traffic profiles cover download, video, and intermittent flows\n'

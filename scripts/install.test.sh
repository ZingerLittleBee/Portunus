#!/bin/sh
# Network-free smoke test for scripts/install.sh --dry-run.
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
script="$here/install.sh"

fail() { echo "FAIL: $1" >&2; exit 1; }

out="$(sh "$script" client --version 1.4.1 --dry-run)" || fail "exit non-zero"

echo "$out" | grep -q '^role:[[:space:]]*client$' || fail "role line"
echo "$out" | grep -q '^tag:[[:space:]]*v1.4.1$' || fail "tag line"
echo "$out" | grep -q '^artifact_version:[[:space:]]*1.4.1$' || fail "artifact_version line"
echo "$out" | grep -q 'releases/download/v1.4.1/portunus-1.4.1-.*\.tar\.gz' || fail "download_url"
echo "$out" | grep -q 'portunus-1.4.1-checksums\.txt' || fail "checksums_url"

# Accepts a leading-v version identically.
out2="$(sh "$script" server --version v2.0.0 --dry-run)" || fail "v-prefixed exit"
echo "$out2" | grep -q '^role:[[:space:]]*server$' || fail "server role"
echo "$out2" | grep -q '^tag:[[:space:]]*v2.0.0$' || fail "v-normalised tag"
echo "$out2" | grep -q '^artifact_version:[[:space:]]*2.0.0$' || fail "v-normalised artifact"

# Unknown role is rejected non-zero.
if sh "$script" bogus --dry-run >/dev/null 2>&1; then fail "bogus role accepted"; fi

echo "PASS"

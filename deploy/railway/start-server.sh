#!/bin/sh
set -eu

operator_port="${PORT:-7080}"
data_dir="${PORTUNUS_DATA_DIR:-/var/lib/portunus}"
control_listen="${PORTUNUS_CONTROL_LISTEN:-0.0.0.0:7443}"
public_origin="${PORTUNUS_OPERATOR_HTTP_PUBLIC_ORIGIN:-}"
advertised_endpoint="${PORTUNUS_ADVERTISED_ENDPOINT:-}"

if [ -z "${public_origin}" ] && [ -n "${RAILWAY_PUBLIC_DOMAIN:-}" ]; then
  public_origin="https://${RAILWAY_PUBLIC_DOMAIN}"
fi

mkdir -p "${data_dir}"

cat > "${data_dir}/server.toml" <<EOF
control_listen = "${control_listen}"
operator_http_listen = "0.0.0.0:${operator_port}"
metrics_listen = "127.0.0.1:7081"
EOF

if [ -n "${public_origin}" ]; then
  printf 'operator_http_public_origin = "%s"\n' "${public_origin}" >> "${data_dir}/server.toml"
fi

set -- /usr/local/bin/portunus-server --data-dir "${data_dir}"

if [ -n "${advertised_endpoint}" ]; then
  set -- "$@" --advertised-endpoint "${advertised_endpoint}"
fi

exec "$@" \
  serve \
  --operator-http-listen "0.0.0.0:${operator_port}"

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

advertised_host="${advertised_endpoint%%:*}"
cert_path="${data_dir}/server.crt"
key_path="${data_dir}/server.key"

if [ -n "${advertised_host}" ]; then
  regenerate_cert=0
  if [ ! -s "${cert_path}" ] || [ ! -s "${key_path}" ]; then
    regenerate_cert=1
  elif ! openssl x509 -in "${cert_path}" -noout -checkhost "${advertised_host}" >/dev/null 2>&1; then
    regenerate_cert=1
  fi

  if [ "${regenerate_cert}" -eq 1 ]; then
    san="DNS:${advertised_host},DNS:localhost,IP:127.0.0.1,IP:::1"
    case "${advertised_host}" in
      *[!0-9.]*)
        ;;
      *)
        san="IP:${advertised_host},DNS:localhost,IP:127.0.0.1,IP:::1"
        ;;
    esac

    tmp_conf="$(mktemp)"
    cat > "${tmp_conf}" <<EOF
[req]
distinguished_name = dn
x509_extensions = v3_req
prompt = no

[dn]
CN = ${advertised_host}

[v3_req]
subjectAltName = ${san}
EOF
    openssl req \
      -x509 \
      -newkey ec \
      -pkeyopt ec_paramgen_curve:prime256v1 \
      -nodes \
      -days 825 \
      -keyout "${key_path}" \
      -out "${cert_path}" \
      -config "${tmp_conf}" \
      >/dev/null 2>&1
    rm -f "${tmp_conf}"
    chmod 0600 "${key_path}"
    chmod 0644 "${cert_path}"
  fi
fi

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

#!/usr/bin/env bash
# Build Linux binaries in the same Debian generation as the distroless runtime.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
arch="$(docker version --format '{{.Server.Arch}}')"
image="${PORTUNUS_DOCKER_BUILD_IMAGE:-rust:1.88-bookworm}"

case "${arch}" in
  amd64 | arm64) ;;
  *)
    echo "unsupported Docker server architecture: ${arch}" >&2
    exit 1
    ;;
esac

docker run --rm \
  -e CARGO_INCREMENTAL=0 \
  -e CARGO_TARGET_DIR=/tmp/portunus-target \
  -e CARGO_TERM_COLOR=always \
  -e HOST_GID="$(id -g)" \
  -e HOST_UID="$(id -u)" \
  -e PORTUNUS_DOCKER_ARCH="${arch}" \
  -v "${repo_root}:/work" \
  -w /work \
  "${image}" \
  sh -euxc '
    apt-get update
    apt-get install -y --no-install-recommends protobuf-compiler ca-certificates
    rm -rf /var/lib/apt/lists/*

    cargo build --release -p portunus-server -p portunus-client

    mkdir -p "/work/docker-bin/${PORTUNUS_DOCKER_ARCH}"
    cp /tmp/portunus-target/release/portunus-server "/work/docker-bin/${PORTUNUS_DOCKER_ARCH}/"
    cp /tmp/portunus-target/release/portunus-client "/work/docker-bin/${PORTUNUS_DOCKER_ARCH}/"
    chmod 0755 \
      "/work/docker-bin/${PORTUNUS_DOCKER_ARCH}/portunus-server" \
      "/work/docker-bin/${PORTUNUS_DOCKER_ARCH}/portunus-client"
    chown -R "${HOST_UID}:${HOST_GID}" "/work/docker-bin/${PORTUNUS_DOCKER_ARCH}"
  '

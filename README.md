# Portunus

[![CI](https://img.shields.io/github/actions/workflow/status/ZingerLittleBee/Portunus/ci.yml?branch=main&style=flat-square&logo=github&label=CI)](https://github.com/ZingerLittleBee/Portunus/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/ZingerLittleBee/Portunus?style=flat-square&logo=github&color=blue)](https://github.com/ZingerLittleBee/Portunus/releases)
[![Docker](https://img.shields.io/badge/GHCR-images-2496ED?style=flat-square&logo=docker&logoColor=white)](https://github.com/ZingerLittleBee/Portunus/pkgs/container/portunus-server)
[![License](https://img.shields.io/badge/license-AGPL--3.0-blue?style=flat-square)](#license)
[![Rust](https://img.shields.io/badge/rust-1.88%2B-orange?style=flat-square&logo=rust)](https://www.rust-lang.org)

**English** | [简体中文](README.zh-CN.md)

> **Fast TCP/UDP port forwarding in Rust.** One static binary, no runtime dependencies. Run it standalone from a single TOML file, or as a control plane that pushes rules to a fleet of edge nodes.

Forward a port with no server and no database — write a config, then install it as a service:

```sh
# 1. write your forwarding rules where the service can read them
sudo mkdir -p /etc/portunus
cat <<'EOF' | sudo tee /etc/portunus/portunus.toml >/dev/null
[[rule]]
name        = "ssh"
protocol    = "tcp"
listen_port = 2222
target      = "10.0.0.5:22"
EOF

# 2. install + start (detects systemd/OpenRC; reads the config above)
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sudo sh -s -- standalone --config /etc/portunus/portunus.toml
```

`:2222 → 10.0.0.5:22`, TCP and UDP, surviving reboots and SSH logout. Just trying it out? Drop `sudo` and the service and run it in the foreground: `portunus-standalone --config portunus.toml`.

## Why Portunus

- **Fast, and it stays fast.** Linux `splice(2)` zero-copy lifts single-stream TCP from 9.9 to 21.9 Gbps (2.2×). UDP batches syscalls with `recvmmsg`/`sendmmsg` — ~12× fewer than per-packet — and 1,000 concurrent UDP flows hold a fixed 64 KiB receive buffer, not 64 MiB. A CI benchmark gate fails any PR that regresses the data plane, so the numbers don't quietly rot.
- **Starts as one TOML, grows into a fleet.** Drop a config on a VPS and you have a forwarder. Point edge nodes at a `portunus-server` and the same tool becomes a control plane — central rule push, Web UI, RBAC, traffic quotas, audit log. One data-plane codebase backs both, so behavior never diverges.
- **One static binary, no dependencies.** Linux builds are static `musl` — one file runs on any distro (glibc, Alpine/musl, busybox). Docker images are `distroless/static`; install is a single POSIX-sh script (runs under dash/busybox ash) with checksum verification and a hardened systemd or OpenRC service.

## Features

- 🔀 **TCP & UDP forwarding** — TCP and UDP rules can even share the same port; the kernel demuxes by protocol.
- 📦 **Port ranges** — map a contiguous port window to a same-offset target window in one rule.
- 🌐 **DNS targets** — resolve target hostnames with TTL-aware caching and a fail-open grace window.
- 🔁 **Multi-target failover** — multiple A/AAAA records with passive and active health checks.
- 🔒 **TLS SNI routing** — route TCP connections by SNI hostname, wildcards supported.
- 🪪 **PROXY protocol** — preserve the original client address to the upstream (v1 and v2).
- 🚦 **Rate limiting & quotas** — per-rule and per-owner QoS plus monthly traffic caps.
- ⚡ **Zero-copy splice** — Linux `splice(2)` fast path for TCP, auto-enabled when no bandwidth limit applies.
- 👥 **Multi-user RBAC** — bearer-token auth with per-user grants scoped by client, port, and protocol.
- 📊 **Web UI + metrics** — embedded React dashboard, live per-rule stats, and a Prometheus `/metrics` endpoint.
- 📺 **Stats TUI** — standalone mode ships a terminal dashboard with sparklines, RTT, and a regex filter.

For UDP, port ranges, failover, PROXY protocol, and the stats TUI, see the [standalone guide](https://portunus.bybee.dev/en/docs/configuration/standalone). For a fleet of edge nodes with central rule push, a Web UI, and RBAC, see the [control-plane setup](https://portunus.bybee.dev/en/docs/getting-started/installation).

## Installation

The one-line script is POSIX `sh` (runs under `dash`/busybox `ash` — no `bash` required), detects OS/arch, and verifies release checksums. By default it installs **and starts** a service; pass `--no-service` to install the binary only. Add `--deploy docker` to any role to run it via Docker Compose instead.

**Standalone** — one host, one TOML file:

```sh
## Docker Compose
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sh -s -- standalone --deploy docker
## binary + service (systemd / OpenRC)
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sh -s -- standalone
```

**Control plane** — a central server plus any number of edge clients:

```sh
# control host
## Docker Compose
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sh -s -- server --deploy docker
## binary + service (systemd / OpenRC)
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sh -s -- server

# each edge host
## Docker Compose
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sh -s -- client --deploy docker
## binary + service (systemd / OpenRC)
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sh -s -- client
```

In binary mode the script installs a service via whichever init it detects — **systemd**, or **OpenRC** on Alpine; hosts with neither get the binary plus a seeded config and printed run instructions. Docker mode writes a `compose.yaml`. Either way the deploy is recorded so later `upgrade` / `status` / `uninstall` work too. Standalone installs accept `--config PATH` to point the service at a specific TOML file. Docker images live on GHCR as `portunus-{server,client,standalone}` — see the [Docker deployment guide](https://portunus.bybee.dev/en/docs/deployment/docker).

**From source** (Rust 1.88+ stable; `protoc` is vendored via `prost-build`):

```sh
cargo build --release -p portunus-server -p portunus-client -p portunus-standalone
```

Prebuilt binaries for Linux and macOS (x86_64 + aarch64) are on the [releases page](https://github.com/ZingerLittleBee/Portunus/releases).

More configuration — CLI flags, `server.toml` / `standalone.toml`, systemd hardening, advertised endpoints → [installation guide](https://portunus.bybee.dev/en/docs/getting-started/installation) and the [configuration reference](https://portunus.bybee.dev/en/docs/configuration/server).

## Documentation

- 📖 [Standalone configuration reference](https://portunus.bybee.dev/en/docs/configuration/standalone) — multi-target failover, PROXY protocol, rate limiting, systemd.
- 🐳 [Docker deployment](https://portunus.bybee.dev/en/docs/deployment/docker)
- 🛠️ [Operations & troubleshooting](https://portunus.bybee.dev/en/docs/operations/troubleshooting) — day-2 ops, backup/restore, upgrades.
- 🔌 [Operator HTTP API](https://portunus.bybee.dev/en/docs/api/operator-http) — operator endpoints and CLI reference.
- 📝 [CHANGELOG](CHANGELOG.md)

## Architecture

A Rust workspace (edition 2024, MSRV 1.88) of eight crates. The data plane is a shared library used by both the edge client and the standalone forwarder.

| Crate | Role |
|---|---|
| `portunus-server` | Control plane: gRPC + operator HTTP + Prometheus + embedded Web UI (SQLite-backed). |
| `portunus-client` | Edge node: authenticated gRPC stream + TCP/UDP forwarding. |
| `portunus-standalone` | TOML-driven forwarder, no control plane. |
| `portunus-forwarder` | Shared data-plane library (TCP/UDP, resolver, shutdown). |
| `portunus-proto` | gRPC schema (tonic-prost generated). |
| `portunus-core` | Shared IDs, errors, config, log redaction. |
| `portunus-auth` | Authenticator trait + token store. |
| `portunus-e2e` | Process-level integration tests. |

The auth model is **TLS + bearer token** (not mTLS). The operator HTTP and Prometheus listeners are loopback-pinned by default.

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

The Web UI lives in `webui/` (React + Vite + TypeScript) and is embedded into `portunus-server` at compile time — there is no runtime Node dependency on the host. See the [Makefile](Makefile) (`make help`) for dev workflows such as `make dev` and `make demo`.

## License

Licensed under the [GNU Affero General Public License v3.0](LICENSE) (`AGPL-3.0-only`).

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this work by you shall be licensed as above, without any additional terms or conditions.

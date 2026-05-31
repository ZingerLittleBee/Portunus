# Portunus

[![CI](https://img.shields.io/github/actions/workflow/status/ZingerLittleBee/Portunus/ci.yml?branch=main&style=flat-square&logo=github&label=CI)](https://github.com/ZingerLittleBee/Portunus/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/ZingerLittleBee/Portunus?style=flat-square&logo=github&color=blue)](https://github.com/ZingerLittleBee/Portunus/releases)
[![Docker](https://img.shields.io/badge/GHCR-images-2496ED?style=flat-square&logo=docker&logoColor=white)](https://github.com/ZingerLittleBee/Portunus/pkgs/container/portunus-server)
[![License](https://img.shields.io/badge/license-AGPL--3.0-blue?style=flat-square)](#license)
[![Rust](https://img.shields.io/badge/rust-1.88%2B-orange?style=flat-square&logo=rust)](https://www.rust-lang.org)

**English** | [简体中文](README.zh-CN.md)

> Fast TCP/UDP port forwarding in Rust — run it as a single-file standalone forwarder, or as a control plane that pushes rules to edge nodes.

Portunus forwards TCP and UDP traffic from a listen port to any `host:port` target. Use it two ways:

- **Standalone** — one binary driven by a single TOML file. No server, no database. Perfect for a VPS or a quick port forward.
- **Control plane** — a central `portunus-server` pushes rules to any number of `portunus-client` edge nodes over an authenticated gRPC stream, with a Web UI, RBAC, and Prometheus metrics.

## Features

- 🔀 **TCP & UDP forwarding** — TCP and UDP rules can even share the same port; the kernel demuxes by protocol.
- 📦 **Port ranges** — map a contiguous port window to a same-offset target window in one rule.
- 🌐 **DNS targets** — resolve target hostnames with TTL-aware caching and a fail-open grace window.
- 🔁 **Multi-target failover** — multiple A/AAAA records with automatic failover.
- 🔒 **TLS SNI routing** — route TCP connections by SNI hostname.
- 🪪 **PROXY protocol** — preserve the original client address to the upstream.
- 🚦 **Rate limiting & quotas** — per-rule and per-owner QoS and traffic caps.
- ⚡ **Zero-copy splice** — Linux `splice(2)` fast path for TCP.
- 👥 **Multi-user RBAC** — bearer-token auth with per-user grants scoped by client, port, and protocol.
- 📊 **Web UI + metrics** — embedded React dashboard, live per-rule stats, and a Prometheus `/metrics` endpoint.
- 📺 **Stats TUI** — standalone mode ships a terminal dashboard with sparklines, RTT, and a regex filter.

## Quick Start

Forward a port in three steps — no server, no database:

```sh
# 1. Install the standalone forwarder
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | bash -s -- standalone
```

```toml
# 2. Write portunus.toml
[[rule]]
name        = "ssh"
protocol    = "tcp"
listen_port = 2222
target      = "10.0.0.5:22"
```

```sh
# 3. Run it — TCP :2222 now forwards to 10.0.0.5:22
portunus-standalone --config portunus.toml
```

- UDP, port ranges, failover, PROXY protocol, the stats TUI → [standalone guide](https://portunus.bybee.dev/en/docs/configuration/standalone)
- A fleet of edge nodes with central rule push, a Web UI, and RBAC → [control-plane setup](https://portunus.bybee.dev/en/docs/getting-started/installation)

## Installation

The one-line script detects OS/arch and verifies release checksums (needs `bash` 4+). Add `--deploy docker` to any role to run it via Docker Compose instead of a system binary.

**Standalone** — one host, one TOML file:

```sh
## Docker Compose
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | bash -s -- standalone --deploy docker
## binary + systemd
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | bash -s -- standalone
```

**Control plane** — a central server plus any number of edge clients:

```sh
# control host
## Docker Compose
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | bash -s -- server --deploy docker
## binary + systemd
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | bash -s -- server

# each edge host
## Docker Compose
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | bash -s -- client --deploy docker
## binary + systemd
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | bash -s -- client
```

The script installs a systemd service (binary mode) or writes a `compose.yaml` (Docker mode), and records the deploy so later `upgrade` / `status` / `uninstall` work too. Docker images live on GHCR as `portunus-{server,client,standalone}` — see the [Docker deployment guide](https://portunus.bybee.dev/en/docs/deployment/docker).

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

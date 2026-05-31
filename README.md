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

### Standalone (simplest)

```sh
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | bash -s -- standalone
```

Write a `portunus.toml`:

```toml
[[rule]]
name        = "ssh"
protocol    = "tcp"
listen_port = 2222
target      = "10.0.0.5:22"
```

```sh
portunus-standalone --check --config portunus.toml   # validate (exits 0 if valid)
portunus-standalone --config portunus.toml           # run
portunus-standalone stats                            # live TUI dashboard
```

### Control plane (server + edge clients)

```sh
# On the control host — bootstrap an operator (prints a bearer token ONCE), then serve.
portunus-server --data-dir ./srv bootstrap-superadmin --name ops
portunus-server --data-dir ./srv serve               # Web UI + gRPC + metrics

# Enroll an edge node (one-time URI, with a TTL).
portunus-server --data-dir ./srv enroll-client edge-01 --ttl-secs 600
# → portunus-client enroll 'portunus://host:7443/enroll?...'

# On the edge host — redeem the URI, then run.
portunus-client enroll 'portunus://host:7443/enroll?...' --out ./client.bundle.json
portunus-client --bundle ./client.bundle.json

# Push a rule: port 8080 on edge-01 → example.com:80
export PORTUNUS_OPERATOR_TOKEN=<token-from-bootstrap>
portunus-server push-rule edge-01 8080 example.com:80
```

Open the Web UI at `http://127.0.0.1:7080` (loopback by default — SSH-tunnel or reverse-proxy for remote access).

## Installation

**Install script** (detects OS/arch, verifies release checksums). Requires `bash` 4+:

```sh
# role is one of: standalone | server | client
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | bash -s -- standalone
```

**Docker** (GHCR — pin a tag like `:1.7.0` for reproducible deploys):

```sh
docker pull ghcr.io/zingerlittlebee/portunus-server:latest
docker pull ghcr.io/zingerlittlebee/portunus-client:latest
docker pull ghcr.io/zingerlittlebee/portunus-standalone:latest
```

**From source** (Rust 1.88+ stable; `protoc` is vendored via `prost-build`):

```sh
cargo build --release -p portunus-server -p portunus-client -p portunus-standalone
```

Prebuilt binaries for Linux and macOS (x86_64 + aarch64) are on the [releases page](https://github.com/ZingerLittleBee/Portunus/releases).

## Documentation

- 📖 [Standalone configuration reference](docs/content/docs/configuration/standalone.mdx) — multi-target failover, PROXY protocol, rate limiting, systemd.
- 🐳 [Docker deployment](docs/content/docs/deployment/docker.mdx)
- 🛠️ [Operations runbook](docs/runbook.md) — day-1 install, day-2 ops, troubleshooting.
- 🔌 [Operator API](specs/001-tcp-forward-mvp/contracts/operator-api.md) — CLI subcommands + loopback HTTP API.
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

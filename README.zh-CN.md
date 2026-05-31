# Portunus

[![CI](https://img.shields.io/github/actions/workflow/status/ZingerLittleBee/Portunus/ci.yml?branch=main&style=flat-square&logo=github&label=CI)](https://github.com/ZingerLittleBee/Portunus/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/ZingerLittleBee/Portunus?style=flat-square&logo=github&color=blue)](https://github.com/ZingerLittleBee/Portunus/releases)
[![Docker](https://img.shields.io/badge/GHCR-images-2496ED?style=flat-square&logo=docker&logoColor=white)](https://github.com/ZingerLittleBee/Portunus/pkgs/container/portunus-server)
[![License](https://img.shields.io/badge/license-AGPL--3.0-blue?style=flat-square)](#许可证)
[![Rust](https://img.shields.io/badge/rust-1.88%2B-orange?style=flat-square&logo=rust)](https://www.rust-lang.org)

[English](README.md) | **简体中文**

> 用 Rust 写的高性能 TCP/UDP 端口转发 —— 既能作为单文件独立转发器运行，也能作为控制面向边缘节点下发规则。

Portunus 把 TCP / UDP 流量从监听端口转发到任意 `host:port` 目标。两种用法：

- **独立模式（Standalone）** —— 单个二进制，由一份 TOML 文件驱动。无需 Server，无需数据库。非常适合 VPS 或快速端口转发。
- **控制面模式** —— 中心化的 `portunus-server` 通过认证的 gRPC 流向任意数量的 `portunus-client` 边缘节点下发规则，并提供 Web UI、RBAC 与 Prometheus 指标。

## 特性

- 🔀 **TCP & UDP 转发** —— TCP 与 UDP 规则甚至可以共用同一端口，内核按协议解复用。
- 📦 **端口范围** —— 一条规则即可把一段连续端口窗口映射到同偏移的目标端口窗口。
- 🌐 **DNS 目标** —— 解析目标主机名，按 TTL 缓存，并带有 fail-open 宽限窗口。
- 🔁 **多目标 failover** —— 多条 A/AAAA 记录，自动故障切换。
- 🔒 **TLS SNI 路由** —— 按 SNI 主机名路由 TCP 连接。
- 🪪 **PROXY protocol** —— 向上游保留原始客户端地址。
- 🚦 **限速与配额** —— 按规则、按 owner 的 QoS 与流量上限。
- ⚡ **零拷贝 splice** —— Linux `splice(2)` TCP 快路径。
- 👥 **多用户 RBAC** —— bearer-token 认证，按 client / 端口 / 协议限定每用户的授权范围。
- 📊 **Web UI + 指标** —— 内嵌 React 面板、实时每规则统计，以及 Prometheus `/metrics` 端点。
- 📺 **统计 TUI** —— 独立模式自带终端面板，含 sparkline、RTT 与正则过滤。

## 快速开始

三步转发一个端口 —— 无需 Server，无需数据库：

```sh
# 1. 安装独立转发器
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | bash -s -- standalone
```

```toml
# 2. 写 portunus.toml
[[rule]]
name        = "ssh"
protocol    = "tcp"
listen_port = 2222
target      = "10.0.0.5:22"
```

```sh
# 3. 运行 —— TCP :2222 现在转发到 10.0.0.5:22
portunus-standalone --config portunus.toml
```

- UDP、端口范围、failover、PROXY protocol、统计 TUI → [独立模式指南](https://portunus.bybee.dev/zh/docs/configuration/standalone)
- 一批边缘节点、集中下发规则、Web UI 与 RBAC → [控制面部署](https://portunus.bybee.dev/zh/docs/getting-started/installation)

## 安装

一行脚本会自动检测 OS / 架构、校验 release 校验和（需要 `bash` 4+）。任意 role 加上 `--deploy docker` 即可改用 Docker Compose 部署（而非系统二进制）。

**独立模式** —— 单机，一份 TOML 文件：

```sh
## Docker Compose
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | bash -s -- standalone --deploy docker
## 二进制 + systemd
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | bash -s -- standalone
```

**控制面架构** —— 一个中心 Server 加任意数量的边缘 Client：

```sh
# 控制主机
## Docker Compose
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | bash -s -- server --deploy docker
## 二进制 + systemd
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | bash -s -- server

# 每台边缘主机
## Docker Compose
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | bash -s -- client --deploy docker
## 二进制 + systemd
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | bash -s -- client
```

脚本会安装 systemd 服务（二进制模式）或写出 `compose.yaml`（Docker 模式），并记录这套部署，后续 `upgrade` / `status` / `uninstall` 同样可用。Docker 镜像发布在 GHCR，名为 `portunus-{server,client,standalone}` —— 见 [Docker 部署指南](https://portunus.bybee.dev/zh/docs/deployment/docker)。

**从源码编译**（Rust 1.88+ 稳定版；`protoc` 已通过 `prost-build` 内联）：

```sh
cargo build --release -p portunus-server -p portunus-client -p portunus-standalone
```

Linux 与 macOS（x86_64 + aarch64）的预编译二进制见 [releases 页面](https://github.com/ZingerLittleBee/Portunus/releases)。

更多配置 —— 命令行标志、`server.toml` / `standalone.toml`、systemd 加固、对外端点 → 见 [安装文档](https://portunus.bybee.dev/zh/docs/getting-started/installation) 与 [配置参考](https://portunus.bybee.dev/zh/docs/configuration/server)。

## 文档

- 📖 [独立模式配置参考](https://portunus.bybee.dev/zh/docs/configuration/standalone) —— 多目标 failover、PROXY protocol、限速、systemd。
- 🐳 [Docker 部署](https://portunus.bybee.dev/zh/docs/deployment/docker)
- 🛠️ [运维与排障](https://portunus.bybee.dev/zh/docs/operations/troubleshooting) —— day-2 运维、备份恢复、升级。
- 🔌 [操作员 HTTP API](https://portunus.bybee.dev/zh/docs/api/operator-http) —— 操作员接口与 CLI 参考。
- 📝 [CHANGELOG](CHANGELOG.md)

## 架构

由八个 crate 组成的 Rust workspace（edition 2024，MSRV 1.88）。数据面是一个共享库，边缘 Client 与独立转发器都复用它。

| Crate | 职责 |
|---|---|
| `portunus-server` | 控制面：gRPC + 操作员 HTTP + Prometheus + 内嵌 Web UI（SQLite 持久化）。 |
| `portunus-client` | 边缘节点：认证的 gRPC 流 + TCP/UDP 转发。 |
| `portunus-standalone` | TOML 驱动的转发器，无控制面。 |
| `portunus-forwarder` | 共享数据面库（TCP/UDP、解析器、shutdown）。 |
| `portunus-proto` | gRPC schema（由 tonic-prost 生成）。 |
| `portunus-core` | 共享 ID、错误、配置、日志脱敏。 |
| `portunus-auth` | Authenticator trait + token store。 |
| `portunus-e2e` | 进程级集成测试。 |

认证模型为 **TLS + bearer token**（非 mTLS）。操作员 HTTP 与 Prometheus listener 默认固定到 loopback。

## 开发

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

Web UI 位于 `webui/`（React + Vite + TypeScript），在编译时被嵌入 `portunus-server` —— 部署主机上没有运行时 Node 依赖。开发流程（如 `make dev`、`make demo`）见 [Makefile](Makefile)（`make help`）。

## 许可证

采用 [GNU Affero 通用公共许可证 v3.0](LICENSE)（`AGPL-3.0-only`）授权。

除非你明确另行声明，否则你有意提交以纳入本作品的任何贡献，都将如上授权，无任何附加条款或条件。

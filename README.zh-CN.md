# Portunus

[![CI](https://img.shields.io/github/actions/workflow/status/ZingerLittleBee/Portunus/ci.yml?branch=main&style=flat-square&logo=github&label=CI)](https://github.com/ZingerLittleBee/Portunus/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/ZingerLittleBee/Portunus?style=flat-square&logo=github&color=blue)](https://github.com/ZingerLittleBee/Portunus/releases)
[![Docker](https://img.shields.io/badge/GHCR-images-2496ED?style=flat-square&logo=docker&logoColor=white)](https://github.com/ZingerLittleBee/Portunus/pkgs/container/portunus-server)
[![License](https://img.shields.io/badge/license-AGPL--3.0-blue?style=flat-square)](#许可证)
[![Rust](https://img.shields.io/badge/rust-1.88%2B-orange?style=flat-square&logo=rust)](https://www.rust-lang.org)

[![Deploy on Railway](https://railway.com/button.svg)](RAILWAY_TEMPLATE_URL)
<!-- TODO(railway-template): replace RAILWAY_TEMPLATE_URL after the template is published (Phase 2) -->

[English](README.md) | **简体中文**

> **用 Rust 写的高性能 TCP/UDP 端口转发。** 单个静态二进制，不依赖任何运行时。既可以一份 TOML 文件单机跑，也可以做控制面，把规则下发到一批边缘节点。

转发一个端口，不用 Server、不用数据库 —— 先写配置，再装成服务：

```sh
# 1. 把转发规则写到默认配置路径
sudo sh -c 'mkdir -p /etc/portunus && cat > /etc/portunus/standalone.toml' <<'EOF'
[[rule]]
name        = "ssh"
protocol    = "tcp"
listen_port = 2222
target      = "10.0.0.5:22"
EOF

# 2. 安装并启动（自动探测 systemd/OpenRC，读取 /etc/portunus/standalone.toml）
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sudo sh -s -- standalone
```

`:2222 → 10.0.0.5:22`，TCP 与 UDP，重启和退出 SSH 都不会停。想用别的文件就加 `--config /path/to/your.toml`。只是想试一下？去掉 `sudo` 和服务、前台运行即可：`portunus-standalone --config portunus.toml`。

## 为什么选 Portunus

- **快，而且不会越用越慢。** Linux `splice(2)` 零拷贝让单流 TCP 从 9.9 Gbps 跑到 21.9 Gbps（2.2 倍）。UDP 走 `recvmmsg`/`sendmmsg` 批量收发，系统调用比逐包少了约 12 倍；1000 条并发 UDP 流只占用固定的 64 KiB 接收缓冲，而不是 64 MiB。CI 里有一道性能基准关卡，谁的改动拖慢了数据面就会被挡回去，性能不会随着版本迭代悄悄退化。
- **单机起步，也能扩成机群。** 在一台 VPS 上放一份配置，它就是个转发器；把边缘节点接到 `portunus-server`，同一个程序又成了控制面：集中下发规则，还带 Web UI、RBAC、流量配额和审计日志。两种用法底层是同一套数据面代码，行为不会有出入。
- **一个静态二进制，没有依赖。** Linux 版本是静态 `musl` 二进制，一个文件就能在各种发行版上跑（glibc、Alpine/musl、busybox 都行）。Docker 镜像基于 `distroless/static`；安装只要跑一个 POSIX-sh 脚本（dash/busybox ash 均可），自带校验和核对，外加一份加固过的 systemd 或 OpenRC 服务。

## 特性

- 🔀 **TCP & UDP 转发** —— TCP 与 UDP 规则甚至可以共用同一端口，内核按协议解复用。
- 📦 **端口范围** —— 一条规则即可把一段连续端口窗口映射到同偏移的目标端口窗口。
- 🌐 **DNS 目标** —— 解析目标主机名，按 TTL 缓存，并带有 fail-open 宽限窗口。
- 🔁 **多目标 failover** —— 多条 A/AAAA 记录，被动与主动健康检查。
- 🔒 **TLS SNI 路由** —— 按 SNI 主机名路由 TCP 连接，支持通配符。
- 🪪 **PROXY protocol** —— 向上游保留原始客户端地址（v1 与 v2）。
- 🚦 **限速与配额** —— 按规则、按 owner 的 QoS，以及按月流量上限。
- ⚡ **零拷贝 splice** —— Linux `splice(2)` TCP 快路径，无带宽限制时自动启用。
- 👥 **多用户 RBAC** —— bearer-token 认证，按 client / 端口 / 协议限定每用户的授权范围。
- 📊 **Web UI + 指标** —— 内嵌 React 面板、实时每规则统计，以及 Prometheus `/metrics` 端点。
- 📺 **统计 TUI** —— 独立模式自带终端面板，含 sparkline、RTT 与正则过滤。

UDP、端口范围、failover、PROXY protocol、统计 TUI，见 [独立模式指南](https://portunus.bybee.dev/zh/docs/configuration/standalone)。一批边缘节点、集中下发规则、Web UI 与 RBAC，见 [控制面部署](https://portunus.bybee.dev/zh/docs/getting-started/installation)。

## 安装

一行脚本是纯 POSIX `sh`（可在 `dash` / busybox `ash` 下运行，无需 `bash`），会自动检测 OS / 架构并校验 release 校验和。默认会安装**并启动**服务；加 `--no-service` 则只装二进制不启动。任意 role 加上 `--deploy docker` 即可改用 Docker Compose 部署。

**独立模式** —— 单机，一份 TOML 文件。安装器不会替你写配置，所以先创建它；放在哪取决于安装方式：

```sh
# Docker Compose：配置是当前目录下的 ./portunus.toml（bind-mount 挂载）
cat > portunus.toml <<'EOF'
[[rule]]
name        = "ssh"
protocol    = "tcp"
listen_port = 2222
target      = "10.0.0.5:22"
EOF
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sh -s -- standalone --deploy docker
```

```sh
# 二进制 + 服务（systemd / OpenRC）：配置位于 /etc/portunus/standalone.toml
sudo sh -c 'mkdir -p /etc/portunus && cat > /etc/portunus/standalone.toml' <<'EOF'
[[rule]]
name        = "ssh"
protocol    = "tcp"
listen_port = 2222
target      = "10.0.0.5:22"
EOF
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sudo sh -s -- standalone
```

**控制面架构** —— 一个中心 Server 加任意数量的边缘 Client：

```sh
# 控制主机
## Docker Compose
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sh -s -- server --deploy docker
## 二进制 + 服务（systemd / OpenRC）
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sh -s -- server

# 每台边缘主机
## Docker Compose
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sh -s -- client --deploy docker
## 二进制 + 服务（systemd / OpenRC）
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sh -s -- client
```

二进制模式下，脚本会按检测到的 init 系统安装服务 —— **systemd**，或 Alpine 上的 **OpenRC**；两者都没有的主机则只装二进制并打印手动启动说明。Docker 模式则写出 `compose.yaml`。无论哪种方式都会记录这套部署，后续 `upgrade` / `status` / `uninstall` 同样可用。standalone 不会替你写配置 —— 由你自行创建（缺失时二进制直接退出）；`--config PATH` 可指定服务读取的 TOML 文件。Docker 镜像发布在 GHCR，名为 `portunus-{server,client,standalone}` —— 见 [Docker 部署指南](https://portunus.bybee.dev/zh/docs/deployment/docker)。

**从源码编译**（Rust 1.88+ 稳定版；`protoc` 已通过 `prost-build` 内联）：

```sh
cargo build --release -p portunus-server -p portunus-client -p portunus-standalone
```

Linux 与 macOS（x86_64 + aarch64）的预编译二进制见 [releases 页面](https://github.com/ZingerLittleBee/Portunus/releases)。

**在 Railway 部署控制平面** —— 直接拉取 GHCR 预构建镜像一键部署 `portunus-server`（Web UI + gRPC 控制平面），不在 Railway 上构建：

1. 点上方 **Deploy on Railway** 创建服务。
2. 打开服务的 **Deploy Logs**，复制 `Portunus onboarding setup token: <token>`。
3. 访问生成的 HTTPS 域名 → 引导页 → 粘贴 token，设置超管用户名 + 密码。
4. `provision-client` 生成一个 bundle，在任意公网主机上运行 `portunus-client --bundle <file>`，它会经 Railway TCP proxy 连接。

详见 [Railway 部署指南](https://portunus.bybee.dev/zh/docs/deployment/railway) 与 [`deploy/railway/README.md`](deploy/railway/README.md)。

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

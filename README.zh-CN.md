# Portunus

[English](README.md) | **简体中文**

基于端口的 TCP / UDP 转发服务，由控制面 Server、边缘 Client 与操作员
界面组成。

`portunus-server` 运行在控制主机上。边缘主机运行 `portunus-client`，
通过 TLS + bearer token 认证，并接受来自操作员的规则推送。每条规则在
Client 上绑定一个 listener（按规则的 `protocol` 决定是 TCP `accept`
循环还是 UDP `recv_from` 循环），并把流量转发到配置的 `host:port`
目标。每条规则的字节 / 连接 / 数据报指标每 5 秒回传给 Server，可通过
`rule-stats`（操作员 CLI / HTTP）以及 Prometheus（`/metrics`，仅
loopback）两种方式查看。

本仓库对应 v1.0.0 release。v1.0.0 是 Portunus 的首个稳定版本，保留了
v0.11 的 wire / REST / SQLite-schema 接口，并发布 release 二进制以及
GHCR Docker 镜像。release 说明与性能基线见
[`CHANGELOG.md`](CHANGELOG.md)。

## 状态

- 稳定 release（v1.0.0）—— 见 [CHANGELOG](CHANGELOG.md)。
- Rust 1.88，edition 2024，由六个 crate 组成的 workspace
  （`portunus-proto`、`portunus-core`、`portunus-auth`、
  `portunus-server`、`portunus-client`、`portunus-e2e`）。
- 认证模型：TLS + bearer token。基于证书的 Client 认证（mTLS）已在
  Constitution v2.0 中被刻意移除；见 `.specify/memory/constitution.md`。

## 安装

最快的安装方式是一行脚本（自动检测 OS / 架构，校验 release 校验和）：

```sh
# 边缘主机
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sh -s -- client
# 控制面主机
curl -fsSL https://raw.githubusercontent.com/ZingerLittleBee/Portunus/main/scripts/install.sh | sh -s -- server
```

也支持 Docker Compose；发布的镜像默认使用 `:latest`：

```sh
docker pull ghcr.io/zingerlittlebee/portunus-server:latest
docker pull ghcr.io/zingerlittlebee/portunus-client:latest
```

需要完全可复现的部署时，请固定到某个 release tag，例如 `:1.0.0`。
Docker Compose 指南见
[`docs/content/docs/deployment/docker.mdx`](docs/content/docs/deployment/docker.mdx)。

release 二进制发布于
[`github.com/ZingerLittleBee/Portunus/releases/tag/v1.0.0`](https://github.com/ZingerLittleBee/Portunus/releases/tag/v1.0.0)。

从源码编译需要 Rust 1.88+ 稳定版。`protoc` 已通过 `prost-build`
内联。

```sh
cargo build --release -p portunus-server -p portunus-client
# →  target/release/portunus-server
#    target/release/portunus-client
```

## 基本流程

```sh
# 操作员（主机 A）—— bootstrap 超级管理员操作员账号（v0.5.0+）。
# bearer token 只会打印一次 —— 请立即保存。
./target/release/portunus-server --data-dir ./srv bootstrap-superadmin --name ops
# →  superadmin user_id=_superadmin token=<paste-into-PORTUNUS_OPERATOR_TOKEN>

# 下面每个操作员子命令都从环境变量读取 PORTUNUS_OPERATOR_TOKEN。
export PORTUNUS_OPERATOR_TOKEN=<paste-token-here>

# 操作员 —— 为边缘主机创建一次性接入命令。
./target/release/portunus-server --data-dir ./srv \
  enroll-client edge-01 --ttl-secs 600
# → portunus-client enroll 'portunus://host:7443/enroll?...'

# 主机 A —— 启动 Server（state.db + TLS 材料自动生成）
./target/release/portunus-server --data-dir ./srv serve

# 主机 B —— 兑换接入 URI（写入 bundle），然后启动
./target/release/portunus-client enroll 'portunus://host:7443/enroll?...' --out ./client.bundle.json
./target/release/portunus-client --bundle ./client.bundle.json

# 操作员 —— 推送一条规则（edge-01 的 8080 → example.com:80）
./target/release/portunus-server push-rule edge-01 8080 example.com:80

# 操作员 —— 推送端口范围规则（30000-30050 → upstream.local:30000-30050）
./target/release/portunus-server push-rule edge-01 30000-30050 upstream.local:30000-30050

# 操作员 —— 推送 UDP 规则（v0.4.0+）
./target/release/portunus-server push-rule edge-01 6000 upstream.local:9999 --protocol udp

# UDP 和 TCP 规则可以共用同一端口 —— 内核按协议解复用
./target/release/portunus-server push-rule edge-01 6000 upstream.local:9999  # TCP:6000

# 操作员 —— 观察流量
./target/release/portunus-server rule-stats <rule_id>
./target/release/portunus-server rule-stats <rule_id> --per-port  # 仅范围规则
curl -s 127.0.0.1:7081/metrics | grep portunus_rule_bytes
```

范围规则（v0.2.0，
[`002-port-range-forward`](specs/002-port-range-forward/quickstart.md)）
用一次推送把一段连续的监听端口窗口映射到同偏移的目标端口窗口：
`30000-30050 → host:30000-30050` 原子地绑定 51 个端口，并把每个端口
转发到同偏移的目标。默认上限为每个范围 1024 个端口
（`server.toml` 中的 `range_rule_max_ports`）。每端口字节计数仅通过
`--per-port` 暴露，因此无论范围多大，Prometheus 基数预算都保持每条
规则一行。

DNS 名称目标（v0.3.0，
[`003-domain-name-forward`](specs/003-domain-name-forward/quickstart.md)）：
任意规则的目标 host 现在可以是 DNS 名称而非 IP 字面量。Client 在首次
连接时解析，按解析器报告的 TTL 缓存（钳制到 `[5 s, 5 min]`），刷新
失败时在最多 30 s 宽限期内继续提供上一次已知答案 —— 规则全程保持
Active，单个连接以分类原因快速失败。默认地址族为 IPv4 优先；按规则
传 `--prefer-ipv6` 翻转顺序。DNS 失败率按规则通过 `rule-stats` 以及
`/metrics` 中的 `portunus_rule_dns_failures_total{client,rule}` 暴露。

```sh
# DNS 目标 —— 首次连接时解析 api.example.com，按 TTL 缓存
./target/release/portunus-server push-rule edge-01 8443 api.example.com:443

# 同一目标，优先 IPv6（AAAA 优先；无 AAAA 时回退到 A）
./target/release/portunus-server push-rule edge-01 8444 api.example.com:443 --prefer-ipv6
```

v0.2.0 的 IP 目标规则保持其逐字节一致的热路径 —— 解析器层被完全
短路。

## 多用户 RBAC（v0.5.0，
[`005-multi-user-rbac`](specs/005-multi-user-rbac/quickstart.md)）

操作员 API 现在需要 bearer 认证（每个 `/v1/*` 请求都带
`Authorization: Bearer <token>`）。持久化状态存放在
`<data-dir>/state.db`，可选的配置覆盖文件是
`<data-dir>/server.toml`。两条 bootstrap 路径：

```sh
# 路径 A —— 交互式一次性 bootstrap（推荐）。
./target/release/portunus-server --data-dir ./srv bootstrap-superadmin --name ops
# 路径 B —— server.toml 快捷方式。加一次，重启，然后移除。
#   operator_token = "<43-char URL-safe-base64 token>"
./target/release/portunus-server gen-token  # ← 向 stdout 打印一个新 token
```

添加一个受限用户，给他一个凭证，限定他能推送的范围：

```sh
./target/release/portunus-server user-add alice --display-name Alice
./target/release/portunus-server credential-issue alice --label laptop
./target/release/portunus-server grant-add --user-id alice --client edge-01 \
  --listen-port-start 30000 --listen-port-end 30050 --protocols tcp,udp
```

现在 alice 只能在 `edge-01` 上 `push-rule`，仅限端口
`30000..=30050`，仅限 TCP 或 UDP。超出该范围她会收到 HTTP 403，原因
为 `client_not_granted`、`port_outside_grant` 或
`protocol_not_granted` 之一。`--client *` 的 grant 匹配任意 Client；
`--listen-port-end` 等于 `--listen-port-start` 的 grant 是单端口
grant。闭集匹配：单个 grant 必须覆盖整个请求的监听范围 —— 跨两个
grant 的规则会被拒绝。

`GET /v1/rules` 对非超级管理员用户只投影调用者自己拥有的规则；超级
管理员可用 `?owner=<user_id>` 按 owner 过滤。每个规则响应都带一个在
推送时打上的 `owner` 字段。审计日志：每个操作员请求都发出一条结构化
`event = "operator.allow"`（INFO）或 `"operator.deny"`（WARN）；原始
bearer token 永远不会进入审计代码路径（Constitution Principle IV）。

自助凭证轮换：alice 用她当前的 token 认证并自行轮换；响应携带一个新
token，旧 token 在后续请求中随即返回 401：

```sh
PORTUNUS_OPERATOR_TOKEN=<alice's old token> \
  ./target/release/portunus-server credential-rotate alice <credential_id>
```

数据面（gRPC client token、TCP/UDP 转发热路径、DNS 解析器、范围
规则）与 v0.4.0 **逐字节一致** —— 在测试 fixture 加上 bearer 头后，
每个现有的转发测试在 v0.5 router 下逐字通过。完整 HTTP 接口与
退出码表见
[`specs/005-multi-user-rbac/contracts/operator-api.md`](specs/005-multi-user-rbac/contracts/operator-api.md)。

完整的逐步演练 —— 包括密钥指纹固定、吊销以及 SC-001 五分钟目标 ——
见
[`specs/001-tcp-forward-mvp/quickstart.md`](specs/001-tcp-forward-mvp/quickstart.md)。

## 独立转发器（v1.4+）

`portunus-standalone` 是一个由 TOML 文件驱动的自包含 TCP/UDP
转发器 —— 不需要 `portunus-server`。它使用与 `portunus-client`
相同的数据面代码。

```sh
cargo build --release -p portunus-standalone
```

最小 `portunus.toml`：

```toml
[[rule]]
name        = "ssh"
protocol    = "tcp"
listen_port = 2222
target      = "10.0.0.5:22"
```

运行或校验：

```sh
./target/release/portunus-standalone --config portunus.toml
./target/release/portunus-standalone --check --config portunus.toml  # 合法则退出 0
```

完整配置参考、多目标 failover、PROXY protocol 以及 systemd unit
示例见
[`docs/content/docs/operations/standalone.mdx`](docs/content/docs/operations/standalone.mdx)。

## 部署

生产脚手架位于 [`deploy/`](deploy)：

- [`deploy/systemd/`](deploy/systemd) —— `portunus-server.service` 与
  `portunus-client.service`，带加固默认值（`User=` +
  `ProtectSystem=` + `CapabilityBoundingSet=` 等），外加一个创建服务
  用户并铺设 `/var/lib/portunus/` + `/etc/portunus/` 的
  `install.sh`。
- [`deploy/docker/`](deploy/docker) —— `Dockerfile.server` 与
  `Dockerfile.client` 运行时镜像，把预编译二进制拷入
  `distroless/cc:nonroot`，外加一个仅供本地试用的
  `docker-compose.yml`。
- [`deploy/server.toml.example`](deploy/server.toml.example) ——
  与 systemd unit 布局匹配的带注释样例配置。

day-1 安装、day-2 运维（开通、吊销、替换证书、备份）、可观测性以及
v0.1.0 局限的诚实清单见 [`docs/runbook.md`](docs/runbook.md)。

## 操作员 API

CLI 子命令与 loopback HTTP API（`http://127.0.0.1:7080/v1/...`）的
文档见
[`specs/001-tcp-forward-mvp/contracts/operator-api.md`](specs/001-tcp-forward-mvp/contracts/operator-api.md)。
退出码与 HTTP 状态映射在 v1 已冻结。

## Web UI

`portunus-server` 在操作员 HTTP listener（默认 loopback）上提供一个
单页 React UI。在现代浏览器（Chrome / Firefox / Safari / Edge ——
最新两个版本）中打开 listener 地址，在登录界面粘贴你的操作员
bearer token，即可获得：

- Dashboard、Users、Credentials、Grants、Rules、Clients、审计日志、
  Metrics、Settings。
- 通过 Server-Sent Events 的实时每规则统计（5 s 节奏；SSE 被阻断时
  回退到普通轮询）。
- English + 简体中文（在 Settings 中切换；跨刷新记住）。
- 浅色 / 深色 / `prefers-color-scheme` 主题。

UI 从不把 token 存入 `localStorage` 或 cookie —— bearer 仅存在于
`sessionStorage`，并在浏览器关闭时清除。每个 SPA 请求都流经 CLI
所用的同一个 `auth_layer` 中间件；租户只能看到自己的规则 / 凭证，
超级管理员看到全部。

远程访问仍是操作员的职责（listener 在启动时固定到 loopback）：从你
的工作站 SSH-tunnel `127.0.0.1:7080`，或把 listener 放在一个自带
认证的反向代理之后。

SPA 的构建说明见 [`webui/README.md`](webui/README.md)。release
流水线在 `cargo build --release -p portunus-server` 之前运行
`pnpm install --frozen-lockfile && pnpm build`，因此 bundle 在编译时
被嵌入二进制。部署主机上**没有**运行时 Node 依赖。

## 目录结构

```
crates/
  portunus-proto/    gRPC schema（控制面）—— 由 tonic-prost 生成
  portunus-core/     ID、错误、配置、结构化日志脱敏层
  portunus-auth/     Authenticator trait + FileTokenStore（模式 0600）
  portunus-server/   控制面二进制：gRPC + 操作员 HTTP + Prometheus
  portunus-client/   边缘二进制：双向 gRPC 流 + TCP 转发 listener
  portunus-e2e/      进程级集成测试
deploy/
  systemd/          portunus-{server,client}.service + install.sh
  docker/           Dockerfile.{server,client} + 本地 docker-compose.yml
  server.toml.example
docs/
  runbook.md        day-1 安装、day-2 运维、排障
specs/001-tcp-forward-mvp/
  spec.md           用户故事 + 验收标准
  plan.md           架构、依赖、技术背景
  data-model.md     实体与状态机
  contracts/        wire 格式（proto、operator-api、persistence）
  quickstart.md     双主机演练
  tasks.md          实现任务列表（驱动 /speckit-implement）
.specify/memory/constitution.md  项目原则（认证模型、性能门禁等）
```

## 开发

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo bench -p portunus-client --bench data_plane -- --save-baseline v0.1.0
```

criterion 基线位于
`crates/portunus-client/benches/baselines/v0.1.0.json`。不带
`--save-baseline` 重新运行 `cargo bench` 会与之比较。

CI 在触及数据面代码的 PR 上运行回归门禁
（`.github/workflows/bench.yml`）：若任一基准的中位数比已提交基线慢
超过 25%，`scripts/bench_regression_gate.py` 会失败。当有意的性能
变更落地时，重新采集并提交新数字：

```sh
cargo bench -p portunus-client --bench data_plane -- --save-baseline v0.1.0
# 重新生成 JSON 摘要（构建它的代码片段见 CHANGELOG）
```

### 本地多用户演示

`make demo` 在 loopback 上立起一个完整、自验证的多租户环境：它构建
二进制、启动 Server、在 http://localhost:5173 启动 Vite Web UI、
创建 N 个 RBAC 用户（每个都有自己的 grant + bearer token 和一个
独立的边缘 Client）、为每个用户向本地 echo 上游推送 K 条真实转发
规则、跑一次真实的端到端 TCP 往返加 RBAC / 跨租户检查、打印一份
操作员速查表（Web UI 登录、token、规则 id、监听端口、日志路径），
然后保持环境开启。

```sh
make demo                                         # 3 用户 × 2 规则，然后保持开启（Ctrl-C 停止 + 清理）
make demo DEMO_ARGS="--users 5 --rules-per-user 3" # 扩容
make demo DEMO_ARGS="--no-wait"                    # 运行 + 验证 + 退出（CI / 快速回归）
make demo DEMO_ARGS="--keep"                       # 复用 /tmp/portunus-demo，跳过清空 / bootstrap
make demo DEMO_ARGS="--dry-run"                    # 仅打印解析后的拓扑
```

标志（转发给 `scripts/demo.sh`）：`--users N`、
`--rules-per-user K`、`--base-listen P`（默认 18001）、`--keep`、
`--disable-splice`、`--no-wait`、`--dry-run`。一旦它打印
`demo ready`，用 `_superadmin` 与打印的 demo 密码
（默认 `portunus-demo-password`；用 `PORTUNUS_DEMO_PASSWORD=...`
覆盖）登录 http://localhost:5173，或手动操作：

```sh
# 数据面 —— 字节通过边缘 Client 转发到 echo 上游
printf 'hello\n' | nc 127.0.0.1 18001

# 监控 —— 每规则字节计数（token + 规则 id 来自速查表）
curl -s -H "Authorization: Bearer <user-token>" \
  http://127.0.0.1:7080/v1/rules/<rule_id>/stats | jq
```

状态存放在隔离的 `/tmp/portunus-demo`（绝不触碰 `make dev` 数据
目录）。统计按 Client 的上报间隔（约 5 s）刷新，因此刚发送的负载
需要片刻才会显示。

## 许可证

双重许可，任选其一：

- Apache License, Version 2.0（[LICENSE-APACHE](LICENSE-APACHE) 或
  <http://www.apache.org/licenses/LICENSE-2.0>）
- MIT license（[LICENSE-MIT](LICENSE-MIT) 或
  <http://opensource.org/licenses/MIT>）

由你选择。

### 贡献

除非你明确另行声明，否则你有意提交以纳入本作品的任何贡献（按
Apache-2.0 许可证定义），都将如上双重许可，无任何附加条款或条件。

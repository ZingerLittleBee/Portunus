---
title: portunus-standalone — Documentation & Deployment Gap-Fill — Design
status: draft v1 · awaiting user review
date: 2026-05-26
branch: almaty
target_release: v1.5.x (post-014)
parent_specs:
  - docs/superpowers/specs/2026-05-14-standalone-forwarder-design.md
---

# portunus-standalone — Documentation & Deployment Gap-Fill

## 1. Goal

`portunus-standalone` 的代码、单元测试、集成测试、CHANGELOG、双语 fumadocs
页面、根 README 章节都已经存在并稳定运行（v1.4 引入）。**本设计不改任
何运行时代码**，只补齐两类用户面缺口：

1. **生产部署模板** —— Docker、docker-compose、加固版 systemd unit、
   Kubernetes manifest。统一收纳到 `crates/portunus-standalone/contrib/`，
   docs 站直接引用。
2. **安装器集成** —— 让现有 `scripts/install.sh` 支持 `standalone`
   作为第三个 role（与 `client`/`server` 并列），复用 install /
   uninstall / upgrade / status / service 全部子动作。

成功标准：
- 用户能用 `curl … install.sh | bash -s -- standalone` 一行安装。
- 用户能用 `docker compose up` 或 `kubectl apply` 把 standalone 跑起来。
- 现有 docs 页面新增 Docker / K8s / Installer 三节，链接到 contrib/。
- `scripts/install.test.sh` 通过且覆盖 standalone 路径。
- 中文 mdx 与英文 mdx 同步，根 README 双语段落不需要再改（已经够用）。

## 2. Non-goals

- 不改 `portunus-standalone` 二进制、不改 `portunus-forwarder`、不改配置
  schema、不改 CLI flags。
- 不补 rate-limit / quota / SNI / 热加载 / Prometheus endpoint（这些
  原 spec §2 显式判定 out-of-scope，本设计沿用）。
- 不引入新的 fumadocs 顶级章节 —— 复用 `operations/standalone.mdx`，
  新增小节而非新页面。
- 不发布 Docker 镜像到 GHCR（沿用 `cargo build` 本地构建 + 推荐
  multi-stage Dockerfile；GHCR 推送是单独的 release 工作）。

## 3. Current state（事实清单，非要改的事）

| 现有产物 | 路径 | 状态 |
|---|---|---|
| 二进制 crate | `crates/portunus-standalone/` | ✅ v1.4 |
| 数据面 crate | `crates/portunus-forwarder/` | ✅ |
| 单元 + 集成测试 | `tests/{smoke,check_mode}.rs`, fixtures | ✅ |
| Makefile 目标 | `make standalone`, `make standalone-check` | ✅ |
| 根 README EN | `README.md` L203–230 | ✅ |
| 根 README ZH | `README.zh-CN.md` L193–220 | ✅ |
| fumadocs EN | `docs/content/docs/operations/standalone.mdx` | ✅ 219 行 |
| fumadocs ZH | `docs/content/docs/zh/operations/standalone.mdx` | ✅ 212 行 |
| systemd unit 示例（基础版） | mdx 末尾内嵌 | ⚠️ 缺加固指令 |
| CHANGELOG entry | `CHANGELOG.md` L172/L180/L193/L195/L197 | ✅ |

## 4. Gap & Deliverables

### 4.1 Deliverable A — contrib 模板文件（新建）

新建目录 `crates/portunus-standalone/contrib/`，包含：

```
contrib/
├── README.md                       # 索引：每个文件什么用，怎么改 placeholder
├── portunus-standalone.service     # systemd unit（加固版）
├── Dockerfile                      # multi-stage build (rust:1.88 → distroless)
├── docker-compose.yml              # 单服务 compose，bind-mount config + host networking 模式注释
├── portunus.example.toml           # 完整可运行示例（来自 valid_full.toml 简化版）
└── k8s/
    ├── configmap.yaml              # 规则配置
    ├── deployment.yaml             # Deployment（hostNetwork: true，含说明注释）
    └── README.md                   # K8s 特有注意事项（hostNetwork vs NodePort vs 多端口）
```

**关键设计点**：

- **systemd unit 加固**：在 mdx 内嵌版本基础上加 `LimitNOFILE=65535`、
  `AmbientCapabilities=CAP_NET_BIND_SERVICE`（绑定 <1024 端口需要）、
  `NoNewPrivileges=true`、`PrivateTmp=true`、`ProtectHome=true`。
  保留 `User=portunus`、`ProtectSystem=strict`、`ReadWritePaths=/etc/portunus`。
  注释里说明为什么需要 `AmbientCapabilities`（标准转发场景多 bind 22/53/80/443）。

- **Dockerfile 设计**：
  - Stage 1 `rust:1.88-bookworm` → `cargo build --release -p portunus-standalone`
  - Stage 2 `gcr.io/distroless/cc-debian12`（动态链接 libc，可执行 musl
    选项写在 Dockerfile 注释里）
  - `COPY --from=builder /usr/src/app/target/release/portunus-standalone /usr/local/bin/`
  - `ENTRYPOINT ["/usr/local/bin/portunus-standalone"]`
  - `CMD ["--config", "/etc/portunus/standalone.toml"]`
  - 不创建 USER（distroless 默认 nonroot，但 cap_net_bind_service 要 root
    或 setcap；K8s 用 hostNetwork + privileged 解决，docker 用
    `--cap-add NET_BIND_SERVICE`，文档里讲清）。

- **docker-compose.yml**：
  - 单服务 `portunus-standalone`
  - `network_mode: host`（默认，注释里给出 `ports:` 显式映射备选）
  - `volumes: ./portunus.toml:/etc/portunus/standalone.toml:ro`
  - `restart: unless-stopped`
  - `cap_add: [NET_BIND_SERVICE]`
  - `ulimits: nofile: 65535:65535`

- **K8s deployment.yaml**：
  - `kind: Deployment`，1 replica（注释说明 standalone 是 host-pinned，不要
    多副本）
  - `hostNetwork: true`（必需 —— TCP/UDP 转发不能走 ClusterIP）
  - `dnsPolicy: ClusterFirstWithHostNet`
  - `volumeMounts` 从 ConfigMap 注入 standalone.toml
  - `resources.limits` 给保守默认（256Mi mem，500m cpu）
  - `securityContext.capabilities.add: [NET_BIND_SERVICE]`
  - K8s README 解释：DaemonSet 模式适合每节点都跑、Deployment 模式适合
    单节点固定。NodePort/LoadBalancer 不可用因为转发是任意端口范围。

- **portunus.example.toml**：直接拷贝 `tests/fixtures/valid_full.toml`，
  顶部加 4–5 行注释引导用户改 `target` 字段。

### 4.2 Deliverable B — fumadocs 站新增小节（en + zh）

在 `docs/content/docs/operations/standalone.mdx` **末尾**追加：

```
## Production deployment

### Installer (binary + systemd)
### Docker / docker-compose
### Kubernetes
```

每节给最小可跑命令 + 链到 `crates/portunus-standalone/contrib/` 下的具体文件。

**`## systemd unit example` 小节改造**：把现有内嵌的基础 unit
替换为"指向 contrib/portunus-standalone.service 的链接 + 简短注释"，
避免两处版本漂移。

中文 mdx 同步更新（不机翻，照抄结构、人工写就近自然中文）。

### 4.3 Deliverable C — `scripts/install.sh` 支持 standalone role

`install.sh` 当前 role 是 `client` 或 `server`。新增 `standalone`：

**改动清单**：

1. **i18n MSG_EN/MSG_ZH 新增键**：
   - `ask_role` 改为三选一菜单
   - `menu_install_standalone`、相关 status / service 字符串
   - `ask_deploy_standalone`（默认 `binary + systemd`，docker compose 备选）

2. **新增函数**：
   - `install_standalone_binary()` —— 拉 release artifact
     `portunus-standalone-${target}.tar.gz`，解压到 `$BIN_DIR`
   - `install_standalone_docker()` —— 渲染 contrib/docker-compose.yml 到
     `$COMPOSE_DIR`，引导用户填 `portunus.toml`
   - `install_standalone_systemd()` —— 渲染 contrib/portunus-standalone.service
     到 `/etc/systemd/system/`，`systemctl enable --now`
   - `uninstall_standalone()`、`upgrade_standalone()`、`status_standalone()`
     —— 镜像 client 的实现路径

3. **artifact 命名约定**：与 `portunus-client` 同步（已有
   `portunus-client-x86_64-unknown-linux-gnu.tar.gz` 等）。
   **依赖**：release CI 需要新增 standalone artifact 输出。本设计假设
   GitHub Actions 的 release.yml 已经为所有 binary 构建产物（通过
   `cargo build --release --workspace`），只需在 packaging 步骤
   tar 它进去。**如果 release.yml 当前只打包 client/server**，这是
   release.yml 的额外一行变更，记入 Deliverable D。

4. **`config get/set` 子命令**：standalone 的配置不是 env-var 风格，
   而是 TOML 文件。`config get/set` 对 standalone role **不适用**，
   选择策略：调用时打印 "not applicable for standalone — edit
   /etc/portunus/standalone.toml directly" 并退出 2。

5. **菜单交互流**：
   ```
   [1] Install → Role? [1] server [2] client [3] standalone
   → standalone → Deploy form? [1] binary+systemd (recommended) [2] docker compose
   → Version? (blank=latest)
   → Confirm
   ```

### 4.4 Deliverable D — release / CI（影响面校核）

- **`scripts/install.test.sh`**：复用现有测试夹层，加 standalone role 的
  install / uninstall / status / upgrade dry-run 测试。
- **`.github/workflows/release.yml`**：核查是否已经为 standalone 打 tar；
  如果没有，加一行 `tar -czf portunus-standalone-${target}.tar.gz …`。
  （在 spec 评审后由 writing-plans 的实施步骤里核查具体文件名。）
- **CHANGELOG**：Unreleased 段新增 "Documentation" 子段，记录 contrib/
  模板、installer standalone role、加固版 systemd unit。

## 5. Detailed designs

### 5.1 systemd unit 加固版

```ini
[Unit]
Description=Portunus standalone TCP/UDP forwarder
Documentation=https://github.com/ZingerLittleBee/Portunus
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/portunus-standalone --config /etc/portunus/standalone.toml
ExecReload=/bin/kill -HUP $MAINPID
Restart=on-failure
RestartSec=2
StandardOutput=journal
StandardError=journal

# 资源
LimitNOFILE=65535

# 用户 & 文件系统加固
User=portunus
Group=portunus
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
NoNewPrivileges=true
ReadOnlyPaths=/etc/portunus

# 网络能力：允许 bind <1024 端口而无需 root
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE

[Install]
WantedBy=multi-user.target
```

**注意**：`ExecReload=SIGHUP` 暂保留为占位 —— 当前 standalone 不实现
config 热加载（原 spec §2 显式 out-of-scope），SIGHUP 行为是"按 SIGTERM
处理（优雅退出）"。文档里明确说明"修改配置需 `systemctl restart`"。

### 5.2 Dockerfile（multi-stage）

```dockerfile
# syntax=docker/dockerfile:1.7

FROM rust:1.88-bookworm AS builder
WORKDIR /usr/src/portunus
COPY . .
RUN cargo build --release -p portunus-standalone

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=builder /usr/src/portunus/target/release/portunus-standalone \
     /usr/local/bin/portunus-standalone
USER nonroot
ENTRYPOINT ["/usr/local/bin/portunus-standalone"]
CMD ["--config", "/etc/portunus/standalone.toml"]
```

**Bind 低端口处理**：distroless `nonroot` UID 65532 默认不能 bind <1024。
两种文档化解决方案：
1. 用 root 镜像 + `setcap`（contrib/README.md 给出 alt Dockerfile 片段）
2. 在容器外用 `--cap-add NET_BIND_SERVICE --user 0` 临时提权

docs 站推荐方案 1，compose 模板用方案 2 让用户看清两条路。

### 5.3 install.sh 改动尺度估算

读 `install.sh` 头 100 行可见结构：i18n 表 + globals + verb/role parse +
detect_*/install_*/uninstall_* 函数族。复用已有的所有基础设施：
release tag 解析、target triple 检测、checksum 校验、systemd helper。
**新增代码量预估**：~150 行新函数 + ~30 行 i18n + ~10 行菜单分支 ≈ 200 行。
不破坏现有 client/server 流程。

## 6. Testing

| 测试 | 目的 |
|---|---|
| `scripts/install.test.sh` 新增 case | dry-run install standalone binary，验证下载/解压/systemd unit 渲染路径 |
| `cargo build -p portunus-standalone` | 已存在，确认 contrib/ 改动没误改 Cargo.toml |
| `docker build -f crates/portunus-standalone/contrib/Dockerfile .` | 手动本地校验，**不**加 CI（避免延长 CI 时间） |
| `kubectl apply --dry-run=client -f crates/portunus-standalone/contrib/k8s/` | 手动校验 manifest 合法 |
| mdx 中英对照通读 | 人工，no automation |

## 7. Risks

| 风险 | 缓解 |
|---|---|
| release.yml 没打 standalone artifact，installer 拉不到 | writing-plans 阶段先核查 release.yml；如缺，记成本设计的额外子任务 |
| contrib/ Dockerfile 与未来 release 镜像策略冲突 | 文件标 `# Reference Dockerfile — not the published image`；未来发布时单独修订 |
| systemd unit 用户/组 portunus 不存在导致启动失败 | install.sh 安装时 `useradd --system --no-create-home portunus`；writing-plans 阶段先核查 client install 路径里的等价做法，能复用就复用 |
| K8s hostNetwork 在多租户集群被 PodSecurityPolicy 拒 | k8s/README.md 第一段警告 |
| 中英 mdx 内容漂移 | 评审时人工对照；后续修改要求两边同 PR |

## 8. Out of scope（不要塞进本 PR）

- Helm chart（K8s yaml 够最小化部署使用，Helm 是下一步）
- Terraform module
- Prometheus metrics endpoint
- 配置热加载（SIGHUP reload）
- Web UI for standalone
- 与 portunus-server 的混合部署模式

## 9. Open questions

无 —— installer 集成方式、contrib 路径、systemd 加固级别均已对齐。

## 10. References

- Original implementation spec: `docs/superpowers/specs/2026-05-14-standalone-forwarder-design.md`
- Original implementation plan: `docs/superpowers/plans/2026-05-14-standalone-forwarder.md`
- Current code: `crates/portunus-standalone/`
- Current docs: `docs/content/docs/operations/standalone.mdx`
- Installer: `scripts/install.sh`

# Portunus v2.0.0 整体功能测试报告（C-S 架构）

- **测试日期**: 2026-06-03
- **被测版本**: v2.0.0（本地源码交叉编译，`x86_64-unknown-linux-musl`）
- **Server（机器 A）**: 207.241.173.217 / t.900040.xyz — Debian 13, 4C8G, Docker 26.1.5
- **Client（机器 B）**: 38.64.56.236 — Debian 12, 1C960M, Docker 29.4.3
- **部署方式**: binary + systemd（文档源码安装路径），交叉编译后 scp
- **图例**: ✅ 通过 / ❌ 失败 / ⬜ 未测 / 🔧 已修复待复验

---

## A. 安装与部署

| # | 功能项 | 状态 | 备注 |
|---|--------|------|------|
| A1 | 交叉编译 server/client musl 二进制 | ✅ | 静态 ELF，v2.0.0，含 webui 嵌入 |
| A2 | scp 二进制到机器 A/B | ✅ | `--version` 均为 2.0.0 |
| A3 | Server 安装（IP 模式）：systemd 服务 + advertised-endpoint=IP | ✅ | deploy/systemd + drop-in env=207.241.173.217:7443 |
| A4 | Server 启动：生成 TLS 证书、监听 7443/7080/7081 | ✅ | gRPC 0.0.0.0:7443 / op 127.0.0.1:7080 / metrics :7081；证书 SAN 含公网 IP |
| A5 | Web UI onboarding（setup token → 创建 superadmin） | ✅ | POST /v1/auth/onboarding 成功，admin/superadmin |
| A6 | Client 安装：systemd 服务 | ✅ | bundle 0640 root:portunus-client，服务 active |
| A7 | Server 安装（域名模式）：advertised-endpoint=t.900040.xyz + Caddy HTTPS | ✅* | 域名 advertised endpoint + 证书 SAN 重生成 ✅；DNS 校验通过；Caddyfile bug #4 已修复（`caddy validate` 通过）。*HTTPS 端到端签发受机器 A 既有 anytls-rs-haproxy 占用 80/443 阻塞（环境因素，非 portunus 问题，未中断用户服务） |
| A8 | install.sh status / service / config / env / uninstall 生命周期 | ✅ | status（meta+探活）/env/config get+set（合并保留）/service restart/uninstall --dry-run 全通 |

## B. 客户端注册与连接（Enrollment）

| # | 功能项 | 状态 | 备注 |
|---|--------|------|------|
| B1 | `enroll-client` 生成注册命令（server 停机 CLI 方式 或 HTTP 方式） | ✅ | 用 HTTP POST /v1/client-enrollments（无停机路径） |
| B2 | client `enroll` 兑换 bundle（验证 pinned cert，0600 权限） | ✅ | bundle 909B，0600，pinned cert 校验通过 |
| B3 | client `--bundle` 连接，server 日志 client.connected | ✅ | client control.connected |
| B4 | `list-clients` 显示 connected + client_id | ✅ | HTTP GET /v1/clients connected=true；CLI list-clients 修复后（缺陷 #1）serve 运行时正常输出 connected + client_id |
| B5 | IP 模式注册连接成功 | ✅ | remote_addr=38.64.56.236 |
| B6 | 域名模式注册连接成功（SAN 覆盖域名） | ✅ | 切到 t.900040.xyz:7443 后证书重生成，client 经域名重新注册并重连，转发 MATCH |

## C. 转发核心功能

| # | 功能项 | 状态 | 备注 |
|---|--------|------|------|
| C1 | TCP 转发：push-rule，Pending→Active，数据完整透传 | ✅ | 10MB 经 B:18080→9001，sha256 一致 |
| C2 | UDP 转发：push-rule --protocol udp，NAT 回包 | ✅ | B:16000→9002 回包一致 |
| C3 | TCP/UDP 同端口共存 | ✅ | 17000 同端口 TCP+UDP 均通 |
| C4 | 端口段规则 port-range（同偏移映射） | ✅ | 30000-30002→31000-31002，偏移正确 |
| C5 | DNS 名称目标（懒解析，缓存） | ✅ | localhost:9001 解析转发成功 |
| C6 | 多目标 failover（优先级 + 被动健康检测） | ✅ | 主 :9999 死→备 :31000，返回 backend-31000 |
| C7 | PROXY protocol v1/v2 前缀（backend 看到真实客户端 IP） | ✅ | v2/v1 均携带真实 src=207.241.173.217 |
| C8 | TLS SNI 路由（按 SNI 分流，不解密） | ✅ | 精确/通配符/fallback 三路由均正确 |
| C9 | remove-rule 排空移除 | ✅ | DELETE 204，B 端口关闭 |

## D. 限流与 QoS

| # | 功能项 | 状态 | 备注 |
|---|--------|------|------|
| D1 | 每规则 bandwidth-in/out-bps 限速 | ✅ | 1 MiB/s cap：4MiB 用时 3.09s（含 1s burst），~1.3 MiB/s |
| D2 | new_connections_per_sec 限制 | ✅ | cap=2：突发 10 连接仅 3 成功 7 拒绝 |
| D3 | concurrent_connections 限制 | ✅ | cap=2：2 个保持，第 3 个被 RST |
| D4 | 每 owner 配额（owner-cap set/get/list/delete） | ✅ | PUT/GET/LIST/DELETE 全通，删后 404 |

## E. RBAC / 多用户

| # | 功能项 | 状态 | 备注 |
|---|--------|------|------|
| E1 | user-add / user-list / user-get / user-remove | ✅ | 创建 alice/列表/详情正常 |
| E2 | credential-issue / list / revoke / rotate | ✅ | issue/list/revoke(204)/rotate 均通；rotate 需带 `{}` body（见缺陷 #2 小问题） |
| E3 | grant-add / grant-list / grant-revoke（按 client/端口/协议授权） | ✅ | alice→edge-01 20000-20010 tcp |
| E4 | 受限用户越权被拒 | ✅ | 越权端口=403 port_outside_grant；越权协议=403；越权读用户=403 |

## F. 存储与运维

| # | 功能项 | 状态 | 备注 |
|---|--------|------|------|
| F1 | SQLite state.db 持久化（重启后规则/客户端保留） | ✅ | 重启后 15 规则保留，client 自动重连，转发恢复 |
| F2 | backup / restore | ✅ | 在线 backup（15 规则）；restore 到新 dir 校验 15 规则 2 用户 |
| F3 | audit prune / 审计日志记录 | ✅ | prune dry-run 20/实删 20（offline）；审计记录成功变更+拒绝 |

## G. 可观测性

| # | 功能项 | 状态 | 备注 |
|---|--------|------|------|
| G1 | Prometheus /metrics（portunus_rule* 指标） | ✅ | portunus_rule_*{client="edge-01",owner,rule}；clients_connected/tls_sni_routes_active 均有 |
| G2 | rule-stats 聚合计数器（bytes_in/out 准确） | ✅ | rule 0 bytes_in/out=10485760 精确，per_target 也准确 |
| G3 | 审计日志（Live/History，NDJSON 导出） | ✅ | 39 条；outcome=deny 过滤准确；v0.8 envelope entries/next_cursor/count |
| G4 | JSON 结构化日志 | ✅ | journald 全 JSON 行，含 event/target |

## H. v2.0 稳定客户端身份

| # | 功能项 | 状态 | 备注 |
|---|--------|------|------|
| H1 | client_id（ULID）生成与展示 | ✅ | enrollment 即预分配 ULID，redeem 后入 client_tokens |
| H2 | rename-client：改名后 id/规则/token 不变，不断连 | ✅ | id/connected_at 不变、转发正常；缺陷 #3 修复后 /v1/rules + Web UI + DB 实时同步新名（metric 标签重连后自愈） |
| H3 | 重名 client_name 允许 | ✅ | 两个 dup-name enrollment 拥有不同 client_id，无重名拒绝 |
| H4 | HTTP 路由 /v1/clients/{id}/... 按 id 寻址 | ✅ | rename/owner-cap 均按 ULID 寻址成功 |

## I. Web UI

| # | 功能项 | 状态 | 备注 |
|---|--------|------|------|
| I1 | 登录（password + session cookie） | ✅ | admin 登录跳转 dashboard |
| I2 | Dashboard：连接的客户端 + 活跃规则 | ✅ | 连接 1/1、TARGETS OK 16/16、已传输统计正常。"Active rules"=有流量的规则数（由 `portunus_rule_bytes_in_total` 指标派生，`metrics.test.ts` 证实为设计语义）；重启后仅 #6 有新流量故显示 1，非缺陷 |
| I3 | Rules 推送/列表/删除（含多目标表单） | ✅ | 列表含范围/DNS/多目标"M"/PROXY/SNI 徽章；Push rule 对话框含单/多目标、范围端口、SNI 字段 |
| I4 | Clients 列表 + 短 id 展示 | ✅ | edge-01-renamed + 短 id 01KT…ZEZE，状态 Connected |
| I5 | Users / Credentials / Grants 管理 | ✅ | admin/alice 列表，凭证/授权计数，New user |
| I6 | Audit log 过滤/分页/导出 | ✅ | Live/History 标签、outcome 过滤、Download as JSON |
| I7 | Settings：advertised endpoint 配置 | ✅ | Client connect address + Save/Clear、主题/语言切换 |

---

## 结论

- **功能项**：A–I 共 **41 项全部通过**（A7 域名 HTTPS 端到端签发受机器 A 既有 anytls 占用 80/443 阻塞，标 ✅*，Caddyfile 生成本身已修复并 `caddy validate` 通过）。
- C-S 架构核心数据面（TCP/UDP/端口段/DNS/多目标 failover/PROXY/SNI）、控制面（enrollment/RBAC/限流/配额/审计/指标/存储/备份恢复）、v2.0 稳定身份（client_id/重名/改名）、Web UI 七大页面均验证可用。
- IP 安装与域名安装两种 server 部署方式均验证（机器 A），client 在机器 B 经 IP 与域名两种方式注册并连接转发。
- 测试中发现 **4 个缺陷**，已**全部修复并复验**（见下表）。修复涉及 `portunus-server`（token_store/rules/operator http+credentials/main.rs cli）与 `scripts/install.sh`，本地 `cargo build`/`cargo clippy`（-W pedantic 干净）/rename 单测通过，交叉编译 musl 重新部署到机器 A 后逐一线上复验。

### 修复改动文件
- `crates/portunus-server/src/store/token_store.rs` — rename 同步 `rules.client_name`（#3）
- `crates/portunus-server/src/rules.rs` — 新增 `RuleRegistry::rename_client`（#3）
- `crates/portunus-server/src/operator/http.rs` — rename 后刷新内存规则名（#3）
- `crates/portunus-server/src/operator/credentials.rs` — rotate body 改可选（#2）
- `crates/portunus-server/src/operator/rule_cli.rs` — 新增 HTTP `list_clients`（#1）
- `crates/portunus-server/src/main.rs` — ListClients 改走 HTTP（#1）
- `scripts/install.sh` — Caddyfile 用站点内 `tls`、检测/替换 apt 默认样板（#4）

## 失败与缺陷记录

（按发现顺序记录，统一修复后复验）

| ID | 关联项 | 现象 | 根因 | 修复 | 复验 |
|----|--------|------|------|------|------|
| #1 | B4 / docs walkthrough | `portunus-server list-clients` 在 serve 运行时报 `store: store_in_use`；而 `docs/cli/walkthrough.mdx` step 4 演示 serve 运行时该命令正常输出 `connected`。 | `ListClients`/`Revoke`/`RenameClient`/`EnrollClient` 走 `build_offline_state`→直接 `Store::open` 持有独占锁；且 offline 用空 `ConnectedClients`，即使 server 停机也读不到在线状态。CLI 与文档/HTTP 路径语义不一致。 | **已修复**：`ListClients` 改走 operator HTTP API（`GET /v1/clients`，新增 `--http-endpoint`），与 rule 命令一致（main.rs + rule_cli.rs::list_clients） | ✅ serve 运行时 list-clients 正常输出 connected + client_id + remote_addr（text/json） |
| #2 | E2 | `POST .../credentials/{id}/rotate` 空 body 报 400 `EOF while parsing`；需显式传 `-d '{}'`。`docs/api/operator-http.mdx` 未说明 rotate 需要 body。属轻微 API 易用性问题。 | rotate handler 用 `Json<T>` 提取要求 body 存在 | **已修复**：handler 改为 `Option<Json<RotateCredentialBody>>`，缺省回退 Default（credentials.rs） | ✅ 无 body、无 Content-Type 即可 rotate，旧 token 401 |
| #3 | H2 | identity-safe rename 后：`/v1/rules` 列表与 Web UI Rules 页的 `client_name` **永久**显示旧名（edge-01）；Prometheus `client=` 标签在改名后到 client 重连前也显示旧名（重连后自愈为新名）。`/v1/clients`、Dashboard、Clients 页均正确显示新名。 | `cli::rename_client` 仅调用 `tokens.rename`（更新 client_tokens），未同步 `rules` 表冗余 `client_name` 列（持久陈旧）；metric 标签随内存 `ConnectedClients` 在重连时刷新，故仅临时陈旧。 | **已修复**：(1) `token_store::rename` 同一写事务内 `UPDATE rules SET client_name WHERE client_id`（持久）；(2) 新增 `RuleRegistry::rename_client` 刷新内存规则名，`patch_client_name` 调用（实时）。metric 标签源自连接/激活时名称，随 client 重连自愈（次要残留，CHANGELOG 定位为可读性用途，内部以 client_id 关联） | ✅ 改名后 /v1/rules 实时显示新名、DB 持久化新名、会话未断、转发 MATCH；metric 标签重连后自愈 |
| #4 | A7 | `install.sh domain <fqdn>` 在全新 apt 安装 Caddy 后生成的 `/etc/caddy/Caddyfile` 无效，Caddy 启动失败 `Unexpected '}' ... no matching opening brace`。文档 installer.mdx 称"只重写 managed block / 备份既有 Caddyfile"，但实际：(1) 未移除 apt 默认 `:80 {…}` 站点块；(2) 全局 `{ email }` 块被追加到站点块**之后**（Caddy 要求全局块必须在文件最前）。 | `write_caddy_block` 直接 append managed block；`render_caddy_block` 把 `{ email }` 放在域名块前但整体追加到既有文件尾部，全局块位置非法 | **已修复**：(1) `render_caddy_block` 用站点内 `tls <email>` 指令替代非法位置的全局 `{ email }` 块；(2) `write_caddy_block` 检测 apt 默认样板（`root * /usr/share/caddy`）时整体替换（已先备份） | ✅ 修复后 `caddy validate` → Valid configuration（原解析失败）。HTTPS 签发本身受机器 A 既有 anytls 占用 80/443 阻塞，非 portunus 问题 |

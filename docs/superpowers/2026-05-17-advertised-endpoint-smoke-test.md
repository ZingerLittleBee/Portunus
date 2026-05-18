# Advertised-Endpoint 运行时配置 — 冒烟测试流程

> 关联：`specs/2026-05-17-advertised-endpoint-runtime-config-design.md` ·
> `plans/2026-05-17-advertised-endpoint-runtime-config.md`
> 分支：`feat/advertised-endpoint-runtime-config`

手动端到端验证：四级解析、grammar/SAN 校验、enrollment 创建时解析 + redeem
复放、Web UI、错误脱敏。每步给出操作与预期。

要点：advertised endpoint 指的是 **gRPC 控制面** 的 `host:port`（默认端口
7443，`control_listen`），与 operator HTTP / Web UI 端口（7080）不是一回事。

---

## 0. 前置准备

```sh
git rev-parse --abbrev-ref HEAD          # 预期: feat/advertised-endpoint-runtime-config
PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-server   # 预期: 编译通过
```

准备一份 SAN 覆盖测试主机名的证书（或复用现有部署证书）。记下证书 SAN
覆盖的主机，例如 `public.example`、`localhost`、IP `127.0.0.1`。后续：

- `<COVERED_HOST>` = 证书 SAN 覆盖的主机
- `<UNCOVERED_HOST>` = 未覆盖的主机

---

## 1. Tier-4 兜底（无 override、无 seed → loopback）

```sh
make serve            # UI 在 http://127.0.0.1:7080
# 用首启 banner 里的 _superadmin / temporary_password 登录，拿到 bearer token
export TOK=<bearer-token>

curl -s -H "Authorization: Bearer $TOK" \
  http://127.0.0.1:7080/v1/settings/advertised-endpoint | jq
```

**预期**：HTTP 200，`override` 为 `null`，`source` 为 `"loopback"`，
`effective` 为 `127.0.0.1:7443`（端口为实际绑定的 control 端口），
`diagnostic` 为 `null`。

---

## 2. Tier-2 seed（环境变量 / CLI）

```sh
PORTUNUS_ADVERTISED_ENDPOINT=<COVERED_HOST>:7443 make serve
curl -s -H "Authorization: Bearer $TOK" \
  http://127.0.0.1:7080/v1/settings/advertised-endpoint | jq
```

**预期**：200，`override` 仍为 `null`，`source` 为 `"seed"`，`effective`
为 `<COVERED_HOST>:7443`。

**负例（seed 未被 SAN 覆盖，fail-closed）**：用
`PORTUNUS_ADVERTISED_ENDPOINT=<UNCOVERED_HOST>:7443` 启动并 GET。

**预期**：200，`effective` 为 `null`，`source` 为 `null`，`diagnostic`
含 tier=seed、未被 SAN 覆盖的说明（显式配置硬错误，不静默降级）。

---

## 3. Tier-1 operator override（PUT 持久化到 SQLite）

```sh
# bearer 鉴权下不需要 X-Portunus-CSRF；cookie 鉴权需要
curl -s -X PUT -H "Authorization: Bearer $TOK" -H "content-type: application/json" \
  -d '{"advertised_endpoint":"<COVERED_HOST>:7443"}' \
  http://127.0.0.1:7080/v1/settings/advertised-endpoint | jq

curl -s -H "Authorization: Bearer $TOK" \
  http://127.0.0.1:7080/v1/settings/advertised-endpoint | jq
```

**预期**：PUT 返回 200 且 body 即为刷新后的视图；GET 显示 `override` =
`<COVERED_HOST>:7443`，`source` = `"override"`，`effective` =
`<COVERED_HOST>:7443`。重启服务后再 GET，override 仍在（已落 SQLite，
优先级高于 seed）。

**清除 override**：PUT `{"advertised_endpoint":null}` 或 `""`，GET 应回落
到 seed/loopback 对应的 `source`。

---

## 4. PUT 校验负例（422）

```sh
# grammar 失败
curl -s -o /dev/null -w "%{http_code}\n" -X PUT -H "Authorization: Bearer $TOK" \
  -H "content-type: application/json" -d '{"advertised_endpoint":"https://x:7443"}' \
  http://127.0.0.1:7080/v1/settings/advertised-endpoint
# SAN 未覆盖
curl -s -X PUT -H "Authorization: Bearer $TOK" -H "content-type: application/json" \
  -d '{"advertised_endpoint":"<UNCOVERED_HOST>:7443"}' \
  http://127.0.0.1:7080/v1/settings/advertised-endpoint | jq
```

**预期**：第一条 HTTP `422`，body `error.code` = `endpoint_invalid`；
第二条 `422`，`error.code` = `endpoint_not_in_cert_san`。两种情况下
SQLite 里的 override 都**未**被写入（校验先于持久化）。

---

## 5. Tier-3 由请求 Host 推导（HTTP enrollment）

```sh
# 清除 override、且不带 seed 启动；用 Host 头驱动推导
curl -s -X POST -H "Authorization: Bearer $TOK" -H "content-type: application/json" \
  -H "Host: <COVERED_HOST>" \
  -d '{"name":"smoke-edge","address":"e.example.com","ttl_secs":300}' \
  http://127.0.0.1:7080/v1/client-enrollments | jq -r '.uri'
```

**预期**：返回的 `uri` 形如 `portunus://<COVERED_HOST>:7443/enroll?...`
（tier-3 用 Host 主机名 + control 端口）。若有更高优先级的
override/seed，则按优先级返回那个值。

---

## 6. resolve-once + redeem 复放

1. 用 override = `<COVERED_HOST>:7443` 创建一个 enrollment（步骤 3 + 步骤
   5 的 POST）。记下返回的 `uri`。
2. 在 redeem **之前**把 override 改成另一个同样被 SAN 覆盖的值（或清除）。
3. 用步骤 1 的凭据走客户端 enroll/redeem（`portunus-client` 或 gRPC
   redeem）。

**预期**：客户端拿到的 `WireCredentialBundle.server_endpoint` 是**创建时**
解析的那个 endpoint（持久化在 `client_enrollments.advertised_endpoint`），
不随后续 override 改变 —— 验证 “创建时解析一次、redeem 时复放”。

**fail-closed 原子性（legacy NULL 行）**：对一条 V010 之前的
NULL-endpoint enrollment，在解析失败配置下 redeem。

**预期**：redeem 失败（gRPC `failed_precondition`），且该 enrollment
**未被消费**、客户端 token **未轮换**（修好配置后可重试成功）。此路径
单元测试已覆盖。

---

## 7. Web UI

浏览器打开 `http://127.0.0.1:7080`，superadmin 登录 → Settings 页：

- 看到 “Client connect address / 客户端连接地址” 卡片（zh-CN / en 文案都在）。
- 输入框关联 `<Label>`（可点击聚焦 / 读屏可读）。
- 填一个被 SAN 覆盖的 `host:port` → Save → 下方 “Effective: … (override)”
  刷新；输入框反映服务器规范化后的值（不再显示陈旧输入）。
- Clear（auto）→ 回落到 seed/loopback，输入框置空。
- 填非法值 / 未覆盖主机 → 卡片内显示 422 的报错文案。

---

## 8. 错误脱敏（M1）

此项手工难以构造（需注入 StoreError），由单元测试覆盖：
`map_store_err_redacts_internal_message`、
`operator_error_store_from_redacts_message`。确认 HTTP 500 store 错误响应
体 `message` 为通用 `"store_error"`，原始 SQL/连接池细节只进服务端
`warn!(event="operator.store_error")` 日志。

---

## 9. 回归 & 收尾

```sh
PORTUNUS_SKIP_WEBUI=1 cargo test --workspace        # 全绿
cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check
# 停服，清理测试数据目录（如用 make dev：make clean）
```

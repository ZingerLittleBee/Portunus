# 月度流量配额与历史聚合（Traffic Quotas & History）— 设计

- **状态**：草案（待用户审阅）
- **日期**：2026-05-14
- **范围**：全栈（portunus-proto / portunus-server / portunus-client / webui）
- **依赖基线**：v0.4.0 UDP forwarding、v0.5.0 RBAC（users + grants）、
  v0.11.0 rate-limit/QoS（per-(user, client) cap）、v1.3.0 splice fast-path
- **预期版本**：v1.4.0

## 1. 背景与问题

当前流量统计仅覆盖 **per-rule** 维度：
- `rule_bytes_in/out_total`、`rule_udp_datagrams_in/out_total` 等 Prometheus
  指标都带 `{client, rule, owner}` 三个标签，技术上 PromQL 一句
  `sum by (owner)` 就能算"每个 user 用了多少"。
- 但 **没有 HTTP API、没有 UI 视图、没有任何持久化** 来呈现按 user / 按 client
  汇总的流量。
- 重启 client 后 in-memory 计数器归零；要看"昨天用了多少"必须自接 Prometheus
  + 长期存储。
- v0.11 的 rate-limit 是 **限速（bps）**，不是 **配额（本月最多 N GB）**。
  两件不同的事，今天没有月配额这个概念。

## 2. 目标

1. 在 **(user, client) pair** 维度引入月度流量配额（monthly quota）。
2. 配额耗尽后 **硬杀**：拒新连 + 立即断现有连接，零容忍超额。
3. 配额按 **billing anniversary**（开通时间作为锚点，逐月推进）周期重置。
4. 提供分钟级 + 小时级两层 SQLite 聚合表，分别保留 7 天 / 90 天。
5. 提供 HTTP API 让 Web UI 展示：
   - AccessEntry 表里每行的本期使用量进度条
   - UserDetail / ClientDetail 各自的 Traffic tab（带历史图表）
   - 超额状态 banner + 立即重置 / 加额按钮
6. 跟现有 wire protocol、SQLite schema、RBAC、AccessEntry 模型完全前向兼容。

### 非目标

- 不做按 rule 的月配额（v0.11 rate-limit 已经做 per-rule 限速，足够）。
- 不做软告警 / webhook 通知（首版只做硬杀 + UI banner）。
- 不做计费导出 / 发票 / 多币种（这是 SaaS 计费基础，不是计费本身）。
- 不做跨 server 集群同步（portunus 仍是单 server SQLite）。
- 不做 client-crash 期间字节追溯（process 内存丢失即丢失）。
- 不开 OTLP / 外部 metric backend 集成（Prometheus 标签足够）。

## 3. 数据模型

### 3.1 配额表 `traffic_quotas`（Server-authoritative）

```sql
CREATE TABLE traffic_quotas (
  user_id                       TEXT    NOT NULL,
  client_name                   TEXT    NOT NULL,
  monthly_bytes                 INTEGER NOT NULL,         -- 配额上限（bytes_in + bytes_out 总和）
  billing_anchor                INTEGER NOT NULL,         -- unix sec，开通时间，周期起点
  current_period_started_at     INTEGER NOT NULL,         -- 当前周期起点
  current_period_bytes_used     INTEGER NOT NULL DEFAULT 0,
  exhausted_at                  INTEGER,                  -- NULL = 未耗尽；非 NULL = 耗尽时刻
  created_at                    INTEGER NOT NULL,
  updated_at                    INTEGER NOT NULL,
  PRIMARY KEY (user_id, client_name)
);

CREATE INDEX traffic_quotas_by_client ON traffic_quotas(client_name);
```

**Period 推进规则**：按 **calendar 月** 锚定到 `billing_anchor` 的日期数。
例如 `billing_anchor=2026-01-15T10:00 UTC` → 周期 `[01-15, 02-15) → [02-15, 03-15) → ...`。
Server 在每次接到 StatsReport 或定时器 tick 时检查
`now >= current_period_started_at + 1 month` → 翻页：
```
current_period_started_at = next_anchor_date(current_period_started_at)
current_period_bytes_used = 0
exhausted_at = NULL
```
并立刻推 `TrafficQuotaState` 给 client。

### 3.2 历史聚合（分层 rollup）

```sql
-- 分钟级原始，保留 7 天
CREATE TABLE traffic_samples_1m (
  user_id      TEXT NOT NULL,
  client_name  TEXT NOT NULL,
  ts_minute    INTEGER NOT NULL,    -- unix sec，对齐到分钟
  bytes_in     INTEGER NOT NULL,
  bytes_out    INTEGER NOT NULL,
  PRIMARY KEY (user_id, client_name, ts_minute)
);

-- 小时级 rollup，保留 90 天
CREATE TABLE traffic_samples_1h (
  user_id      TEXT NOT NULL,
  client_name  TEXT NOT NULL,
  ts_hour      INTEGER NOT NULL,
  bytes_in     INTEGER NOT NULL,
  bytes_out    INTEGER NOT NULL,
  PRIMARY KEY (user_id, client_name, ts_hour)
);

-- Rollup 进度记录（单行表）
CREATE TABLE traffic_rollup_state (
  id                  INTEGER PRIMARY KEY CHECK (id = 1),
  last_rolled_up_hour INTEGER NOT NULL  -- 已完成 rollup 的最后一个小时（unix sec, 整点）
);
```

**容量估算**（100 个活跃 pair）：
- `1m`：100 × 1440 × 7 = 1.0 M 行 ≈ 几十 MB
- `1h`：100 × 24 × 90 = 216 K 行 ≈ 几 MB

**Rollup 任务**：后台 tokio task，每小时整点 +1min 触发：
1. 读 `traffic_rollup_state.last_rolled_up_hour`
2. 对 `[last+1h, now-1h)` 范围内每个小时，从 `1m` 表 SUM 聚合写入 `1h` 表
3. 更新 `last_rolled_up_hour`
4. 删除 `1m` 表中 `ts_minute < now - 7d` 的行
5. 删除 `1h` 表中 `ts_hour < now - 90d` 的行

### 3.3 与 AccessEntry 的关系

`traffic_quotas` 的 PK `(user_id, client_name)` 跟现有 `OwnerRateLimit` 表
**同维度**。UI 上把月配额作为 AccessEntry 的 **第三个属性**
（grant + rate-limit cap + monthly quota），复用 `quota-dev` 的表格 + 表单。

## 4. 上报 / 执行协议

### 4.1 拓扑

```
[Client]                                       [Server]
forwarder accept loop                          gRPC service
  │                                              │
  ├─ 每条 rule 累计 bytes_in/out                  ├─ traffic_quotas (SQLite)
  │  (现有，已生效)                                │
  │                                              │
  ├─ QuotaHandle: remaining (AtomicI64)          │
  │  挂在 (user_id, client_name) 上               │
  │  被该 pair 下所有 rule 共享                    │
  │                                              │
  ├─ remaining <= 0:                             │
  │   - accept 拒新连 / 丢新 UDP first-pkt        │
  │   - 已建立连接立即 RST / drop                  │
  │                                              │
  ├─ 每 5s StatsReport ────usage_delta_by_user───→ 累计 current_period_bytes_used
  │                                              │  - 写 1m 桶
  │                                              │  - 检查 >= monthly_bytes：
  │                                              │      - 写 exhausted_at
  │                                              │      - 推 QuotaState
  │                                              │
  ├─ 接 TrafficQuotaState ──←─────────────────── 推送（配额变更 / period 翻页）
  │   - 原子替换 remaining                        │
  │   - exhausted=false 时恢复 accept             │
  │                                              │
  └─ 重连（启动 / 网断恢复）：                       │
      Hello + 现有同步流程                          │
       ←──server 在初始 RuleUpdate 中─────────── 带回 (user, client) 的
           带上当前 period budget_remaining        period 剩余预算
```

### 4.2 Proto 扩展

```proto
// 新增：server → client 推送
message TrafficQuotaState {
  string user_id = 1;
  int64  budget_remaining_bytes = 2;   // monthly_bytes - current_period_bytes_used
  int64  period_started_at = 3;        // unix sec
  int64  period_ends_at = 4;
  bool   exhausted = 5;                // budget<=0 的显式信号
}

// 扩展现有：client → server StatsReport
message StatsReport {
  // ... 现有字段
  map<string, uint64> usage_delta_by_user = N;  // key = user_id
}
```

**wire 向后兼容**：
- proto3 optional + map 新字段
- 老 client 不发 `usage_delta_by_user` → server 上的 quota 始终 0 → 等于未启用
- 老 server 不认 `TrafficQuotaState` → client 收不到 → 本地 budget = ∞

### 4.3 关键决策

1. **Client 是 budget 的真理来源**。字节先在 client 计数到 0 立刻硬杀，
   不等 server 确认。Server 的 SQLite 副本是有延迟（≤5s）的镜像。
2. **Server 推 TrafficQuotaState 的两个时机**：
   - 配额配置变更（PUT / PATCH / DELETE quota）
   - Period 翻页（server 检测到 `now >= period_ends_at`）
   - **不**在每次 StatsReport 后都推
3. **Reporting cadence**：5 秒（复用现有 StatsReport 节拍）。
   最大泄漏 = 5s × 链路带宽（100 Mbps ≈ 62 MB，10 Gbps ≈ 6 GB）。
   超大单流场景在文档中标注"不适用"。
4. **Period 推进真理来源 = server**。Client 不自走，等 server 推。
   Server 推送如果 client 断开错过 → 重连 Hello 响应里立刻塞最新 state。
5. **没设 quota 的 (user, client) pair**：`traffic_quotas` 无行
   → server 不下发 → client `remaining = ∞`（`Option<i64>` 的 `None`）。
   完全旁路，零性能影响。
6. **硬杀的实现**：复用 v0.11 `RateLimitHandle` 模式。新增
   `QuotaHandle { remaining: AtomicI64, exhausted: AtomicBool }`，
   挂在 forwarder 的 per-rule 状态里。accept 钩子查 `exhausted` → 拒；
   copy loop 的 `record_in/out` 扣减后查 → ≤0 时关 socket。

## 5. HTTP API

### 5.1 配额 CRUD

挂在现有 `/v1/users/{user_id}` 路径下：

```
GET    /v1/users/{user_id}/quotas
PUT    /v1/users/{user_id}/quotas/{client_name}
PATCH  /v1/users/{user_id}/quotas/{client_name}
DELETE /v1/users/{user_id}/quotas/{client_name}
GET    /v1/users/{user_id}/quotas/{client_name}/status
```

**PUT body**：
```json
{ "monthly_bytes": 536870912000, "billing_anchor": 1704067200 }
```
`billing_anchor` 缺省 = 当前时间。`monthly_bytes = 0` 等价于"立即耗尽"。
要求 (user, client) 已存在 grant，否则返回 `422 quota_target_not_found`。

**PATCH body**：
```json
{ "monthly_bytes": 1073741824000, "reset_now": true }
```
`reset_now=true` 强制翻页：扣减 used=0、period_started_at=now、exhausted_at=NULL，
server 立刻推 TrafficQuotaState。

**GET status** 响应：
```json
{
  "monthly_bytes": 536870912000,
  "current_period_bytes_used": 312345678900,
  "current_period_started_at": 1714867200,
  "current_period_ends_at": 1717545600,
  "exhausted": false,
  "last_report_at": 1714953600
}
```

### 5.2 流量查询

```
GET /v1/users/{user_id}/traffic?
      client_name=...                 (可选，省略则跨 client 汇总)
      from=unix_sec&to=unix_sec        (必填)
      bucket=1m|1h                    (默认根据 to-from 自动)

GET /v1/clients/{client_name}/traffic?
      user_id=...                     (可选，省略则跨 user 汇总)
      from=&to=&bucket=
```

响应：
```json
{
  "bucket": "1m",
  "samples": [{"ts": 1714867200, "bytes_in": 1234567, "bytes_out": 7654321}, ...],
  "total_bytes_in": 1234567890,
  "total_bytes_out": 7654321098
}
```

**Bucket 边界**：
- `bucket=1m` 要求 `from >= now - 7d`，否则 `422 quota_bucket_out_of_retention`
- `bucket=1h` 要求 `from >= now - 90d`，否则同上
- 返回行数硬上限 10 000，超了 422 要求缩小时间窗

### 5.3 RBAC

| 操作 | superadmin | client owner | user 本人 |
|------|-----------|--------------|----------|
| PUT/DELETE quota | ✓ | ✓ | ✗ |
| PATCH reset_now / monthly_bytes | ✓ | ✓ | ✗ |
| GET status | ✓ | ✓ | ✓（仅自己） |
| GET traffic 历史 | ✓ | ✓（client 内） | ✓（仅自己） |

### 5.4 新增 Prometheus 指标

```
portunus_traffic_quota_bytes_used{user, client}             gauge
portunus_traffic_quota_bytes_limit{user, client}            gauge
portunus_traffic_quota_exhausted{user, client}              gauge (0/1)
portunus_traffic_quota_period_resets_total{user, client}    counter
portunus_traffic_quota_exhausted_total{user, client}        counter
```

Cardinality：`pairs × 5`，可控。

## 6. Web UI

### 6.1 AccessEntry 表扩展

在 `UserDetail` 的 `UserQuotaTable` 增加两列：

| Client | Ports / Proto | Cap (bps) | Monthly quota | This period |
|--------|--------------|-----------|---------------|-------------|
| edge-tokyo | 6000-6010 TCP | 100 Mbps | 500 GB / resets 06-08 | 312 GB / 62% ▓▓▓▓▓▓▓▓░░░░ |
| edge-osaka | 7000 UDP | — | —（无限）| 8 GB |
| edge-eu | 8000-8005 TCP | — | ⚠️ 100 GB（EXHAUSTED）/ resets in 14 d | ████████████ 100% |

**展开行（沿用现有 expand-to-edit）**：
- Monthly quota 编辑：数值 + 单位 (KB/MB/GB/TB)
- "Reset now" 按钮（带确认弹窗）
- 只读：`billing_anchor` + 下次翻页时间
- 数据新鲜度提示（"上次上报 3 秒前"）

**轮询**：`/quotas/{client}/status` 每 10 秒（React Query，仅可见行）。

### 6.2 Traffic Tab（UserDetail + ClientDetail 对称）

```
┌─ Tabs: [Quotas] [Traffic ★新] [Sessions] ──┐
│                                            │
│ Time range: [Last 24h ▾]  Bucket: [auto ▾] │
│ Client / User filter: [All ▾]              │
│                                            │
│ Total in: 1.24 TB  |  Total out: 856 GB    │
│                                            │
│ [折线/堆叠面积图]                            │
│                                            │
│ [CSV 导出] [深度链接]                        │
└────────────────────────────────────────────┘
```

Time range 预设：`Last 1h / 24h / 7d / This billing period / Custom`。
"This billing period" 要求选定单 client（period 起点各 client 不同）。

### 6.3 超额 Banner

`UserDetail` / `ClientDetail` 顶部，如有任一 pair `exhausted=true`，
显示红色 shadcn `Alert`：

> ⚠️ `edge-eu`: monthly quota exhausted. Forwarding paused until 2026-06-08.
> [Reset now] [Increase limit]

### 6.4 图表库

新增依赖：**`recharts`**（~95 KB gz）。理由：开发速度、shadcn token 兼容、社区大。
现有 webui size budget 500 KB gz，仍在范围内。如接近上限再考虑 `uPlot`（~45 KB）。

### 6.5 i18n

新命名空间 `traffic.*` + `userQuota.*` 扩展。中英对照：
- Monthly quota / 月度配额
- This period / 本期
- Resets on / 重置日期
- Exhausted / 已耗尽
- Reset now / 立即重置
- Billing anchor / 计费起点
- Traffic / 流量

### 6.6 实时刷新

- AccessEntry 表 "This period" 列：30 秒轮询 + quota 变更后立刻 invalidate
- Traffic 图表：用户主动触发拉取，不自动轮询

## 7. 边界场景

| # | 场景 | 处理 |
|---|------|------|
| 1 | Client 重启 | 重连 Hello 在初始 RuleUpdate 里带 budget；crash 期间字节丢失（文档说明） |
| 2 | Server 重启 | SQLite 持久化无损；rollup 任务启动时补跑欠的小时桶 |
| 3 | Client 长断 | 本地继续 forward + 扣减；恢复后批量上报，server 把 delta 归到"恢复时刻" |
| 4 | 重建 grant | quota 行保留（PK 不变），不重置 period；UI 可显式选择继承 |
| 5 | 时钟漂移 | period 用 unix sec 单调比较；跳大可一次性翻多 period |
| 6 | Migration | V008 只建表，无 backfill；现有 pair 默认无配额 = 无限 |
| 7 | Range/SNI/Multi-target | 数据面已挂 owner_id，统一归到 (user, client) 的 QuotaHandle |
| 8 | 删 user / client | 软删；quota 行不删（复活时复用） |
| 9 | 多 user / client | 一次 StatsReport 的 map 上千 key 仍 << 4MB gRPC 上限 |
| 10 | 保留边界查询 | from 超 90d 422；7d–90d 自动用 1h 桶 |
| 11 | UDP 计量 | 用 datagram payload 字节（不含头），跟 TCP 同单位 |
| 12 | PUT 同时跑 | client 原子替换 remaining；降配立即生效 |

## 8. 实现范围

| 层 | 变动 |
|----|------|
| portunus-proto | 新增 `TrafficQuotaState`、`StatsReport.usage_delta_by_user` |
| portunus-server | V008 migration、`traffic_quotas` CRUD、rollup task、quota state push、HTTP 5 endpoints、5 Prometheus metric、RBAC 接线 |
| portunus-client | `QuotaHandle` + 挂载到 forwarder accept/copy 路径、StatsReport 扩展、TrafficQuotaState 处理 |
| webui | AccessEntry 表两列、Traffic tab、超额 banner、recharts 依赖、i18n |
| portunus-e2e | 端到端测试：建 quota → 跑流量 → 验证硬杀 → 翻页恢复 |
| docs | runbook "启用月配额"、API 参考、troubleshooting 加 quota 触发日志 |

## 9. 风险 / 已知折衷

1. **5 秒上报延迟**：最大泄漏 ~5s × 带宽。文档说明，超大单流场景不推荐。
2. **Client crash 期间字节丢失**：内存态丢失，不可恢复。
3. **不支持月配额跨多 client 共享**：本版本 per-(user, client)，
   "user 全局总额"是潜在 v1.5 增强（schema 已经在 user 维度可加列）。
4. **不支持告警 webhook**：超额仅靠 UI banner + Prometheus exhausted_total
   counter。下版本可加 webhook subscriber。
5. **没有计费导出 / 发票**：本特性只到原始数据 + 实时硬杀。
6. **billing_anchor 不可变**：避免周期"重叠"的歧义。要换锚点必须 DELETE
   + PUT。
7. **rollup 任务的窗口语义**：rollup 在小时 +1min 触发，意味着 t=H+0..H+1min
   的查询会少看到刚过去那一小时的数据。可接受。

## 10. Out-of-scope（明确不做）

- 多 server 集群同步
- 跨 server 的全局配额
- 告警 webhook / 邮件
- 计费导出 / 发票 / 多币种
- 按流量类型分类计费（TCP 一价、UDP 一价）
- 跨 (user, client) pair 的共享配额池
- 软告警 / 80% 提醒（首版只硬杀）
- OTLP / 外部 metric backend
- 历史样本的图形化报表导出（仅 CSV）

## 11. 后续可能的扩展（不在本版本）

- v1.5: webhook 告警 + 80% soft alert
- v1.5: 全局月配额（per-user 跨 client）
- v1.6: 计费期可选（月 / 日 / 自定义）
- v1.7: 流量类型分价（TCP / UDP / 出 / 入）
- v2.0: 跨 server 集群配额

---

**审阅检查**：
- ✅ 无 TBD / TODO
- ✅ 4 节内部一致（数据模型 ↔ 协议 ↔ API ↔ UI 在 (user, client) 维度一致）
- ✅ scope 足以单一 plan 落地（一个 v1.4.0 release）
- ✅ 边界场景显式列出（§7）
- ✅ 与现有 wire / SQLite / RBAC 兼容性逐项说明

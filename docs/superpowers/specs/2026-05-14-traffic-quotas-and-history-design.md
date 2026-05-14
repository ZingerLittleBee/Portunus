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
2. 配额耗尽后 **bounded best-effort 硬杀**：拒新连 + 主动断现有连接。
   过量上限受 IO 计数粒度 + 上报延迟 + 重启窗口三者共同 bound（§4.3）。
3. 配额按 **billing anniversary**（开通时间作为锚点，逐月推进）周期重置，
   月底日期不存在时 clamp 到当月最后一天（§3.3）。
4. 提供分钟级 + 小时级两层 SQLite 聚合表，分别保留 7 天 / 90 天，
   **覆盖所有 (user, client) pair**（不限于设置了配额的 pair）。
5. 提供 HTTP API 让 Web UI 展示：
   - AccessEntry 表里每行的本期使用量进度条
   - UserDetail / ClientDetail 各自的 Traffic tab（带历史图表）
   - 超额状态 banner + 加额 / 清零本期用量按钮
6. 跟现有 wire protocol、SQLite schema、RBAC、AccessEntry 模型完全前向兼容；
   **wire 不新增 client→server 字段**，server 端从现有 `RuleStats` 按
   `owner_id` 聚合（§4.2）。

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
  monthly_bytes                 INTEGER NOT NULL
                                  CHECK (monthly_bytes >= 0
                                     AND monthly_bytes <= 9223372036854775807),  -- i64::MAX
  billing_anchor                INTEGER NOT NULL,         -- unix sec UTC，开通时间，永不变
  current_period_started_at     INTEGER NOT NULL,         -- 当前周期起点（≥ billing_anchor）
  current_period_bytes_used     INTEGER NOT NULL DEFAULT 0
                                  CHECK (current_period_bytes_used >= 0),
  exhausted_at                  INTEGER,                  -- NULL = 未耗尽；非 NULL = 耗尽时刻
  created_at                    INTEGER NOT NULL,
  updated_at                    INTEGER NOT NULL,
  PRIMARY KEY (user_id, client_name)
);

CREATE INDEX traffic_quotas_by_client ON traffic_quotas(client_name);
```

**整数边界**：
- `monthly_bytes`、`current_period_bytes_used`、proto `int64` 字段、Rust `AtomicI64`
  一致使用 **signed i64**，最大 `i64::MAX ≈ 8 EiB`（实质无限）。
- proto wire 选 `int64` 而非 `uint64`：让"超额"用负数表达 `remaining = monthly - used`
  在 client 上自然回环为负。
- API 拒绝 `monthly_bytes < 0` 或超出 i64 范围（`400 invalid_quota_size`）。

### 3.3 Period 推进规则（billing anniversary）

**核心不变量**：每个周期起点都从 **原始 `billing_anchor`** 偏移计算，
不从上一周期相对推进。这避免 Jan31 → Feb28 → Mar28 这种漂移。

```rust
/// 计算 `billing_anchor` 之后第 n 个周期起点（n = 0 即 anchor 本身）。
/// 全部使用 UTC。
fn period_start_at(billing_anchor: DateTime<Utc>, n: u32) -> DateTime<Utc> {
    let anchor_day = billing_anchor.day();   // 1..=31
    let anchor_time = (billing_anchor.hour(), billing_anchor.minute(), billing_anchor.second());

    let target_month_total = billing_anchor.year() as i32 * 12
                           + (billing_anchor.month() as i32 - 1)
                           + n as i32;
    let target_year  = target_month_total / 12;
    let target_month = (target_month_total % 12 + 1) as u32;

    // Clamp 到目标月的最后一天（Jan 31 → Feb 28/29 → Mar 31 → Apr 30 …）
    let max_day_of_month = days_in_month(target_year, target_month);
    let day = anchor_day.min(max_day_of_month);

    Utc.ymd(target_year, target_month, day)
       .and_hms(anchor_time.0, anchor_time.1, anchor_time.2)
}
```

**翻页触发**（在 StatsReport 处理或每 60s 定时器 tick）：

```rust
let now = Utc::now();
// 找到 now 所在的周期编号
let mut n = period_index_of(billing_anchor, current_period_started_at);
loop {
    let next_start = period_start_at(billing_anchor, n + 1);
    if next_start > now { break; }
    n += 1;                                  // 多月跳跃时一次性追上
}
let new_period_start = period_start_at(billing_anchor, n);
if new_period_start > current_period_started_at {
    // 翻页
    current_period_started_at = new_period_start;
    current_period_bytes_used = 0;
    exhausted_at = NULL;
    push TrafficQuotaUpdate { action: SET, state: ... } to client;
}
```

**边界场景**：
- `billing_anchor = 2026-01-31T00:00:00 UTC`：周期起点序列 = `01-31, 02-28, 03-31, 04-30, ..., 2027-01-31`（每月都从原始 anchor 计算 → 不漂移）
- 闰年:`billing_anchor=2024-02-29` → `2025-02-28`（clamp），`2026-02-28`，`2028-02-29`（闰）
- Server 时钟回拨 → `next_start > now` 立刻成立 → 不翻页（单调）
- Server 时钟向前跳大（如 NTP 校正几个月）→ while 循环一次性追到当前周期；
  中间周期的 `bytes_used` 快照不被记录（但 1m/1h 历史样本不丢）

### 3.4 历史聚合（分层 rollup）

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
  │  (现有, cumulative since process start)       │  traffic_samples_1m / 1h
  │                                              │
  ├─ QuotaHandle: remaining (AtomicI64)          │  ServerAggregator:
  │  挂在 (user_id, client_name) 上               │   - per (client, rule) 维护 RuleStats
  │  被该 pair 下所有 rule 共享                    │     的上一次 cumulative 快照
  │  (None / 缺席 = 无配额 = 完全旁路)              │   - 每次 StatsReport 计算 delta_in/out
  │                                              │   - 查 rule.owner_id → 累加到
  ├─ 每条 IO 操作 (read/write/splice 迭代):       │     (user_id, client_name) 桶
  │   per-rule bytes_in/out += n  (现有)          │   - 写 traffic_samples_1m (UPSERT 当前分钟)
  │   QuotaHandle.consume(n) → 若 < 0:           │   - 累加 current_period_bytes_used
  │     - 关闭 IO (return Err)                    │   - 若 >= monthly_bytes 且
  │     - 设 exhausted=true                       │     exhausted_at == NULL：
  │     accept 钩子查 exhausted → 拒新            │       写 exhausted_at = now
  │                                              │       推 TrafficQuotaUpdate{exhausted=true}
  ├─ 每 5s StatsReport (现有 RuleStats) ─────→  │
  │   cumulative bytes_in/out per rule           │
  │                                              │
  ├─ 接 TrafficQuotaUpdate ──←──────────────── 推送时机：
  │   action=SET: 原子替换 remaining             │   (a) 配额 PUT/PATCH/DELETE
  │   action=REMOVE: 摘掉 QuotaHandle = ∞       │   (b) period 翻页 (server tick)
  │   exhausted=false 时恢复 accept              │   (c) 已耗尽即时阻断
  │                                              │   (d) reconnect 阶段 replay
  └─ 重连：Hello → server replay 阶段，          │
     先所有 RuleUpdate，再所有 TrafficQuotaUpdate │
```

### 4.2 Proto 扩展

**Client → Server**:**不动**。沿用现有 `RuleStats { bytes_in, bytes_out, rule_id }`,
server 端从 rule store 查到 `(client_name, owner_id)` 后聚合到 (user, client) 桶。
好处:
- 复用 RuleStats 的 cumulative + previous-snapshot delta 算法(server 端已有,
  见 `RuleStatsCache::observe`),client 重启 → 计数器回 0 → server 检测到
  "新 cumulative < 旧 snapshot" → 重新 baseline,不会重算或漏算
- 无新 wire 字段,完全向后兼容

**Server → Client**:新增 `TrafficQuotaUpdate`,作为 `ServerMessage.payload` 的
新 oneof 变体(field 4)。结构模仿 v0.11 的 `OwnerRateLimitUpdate`(field 3):

```proto
message ServerMessage {
  oneof payload {
    Welcome welcome = 1;
    RuleUpdate rule_update = 2;
    OwnerRateLimitUpdate owner_rate_limit_update = 3;
    // 新增 v1.4 (013-traffic-quotas)。v1.3 及更早的 client 不认这个
    // 变体；server 端的 capability 门控以 Hello 里携带的 client_version
    // 为依据，对低版本 client 不推送此消息，并在创建 quota 时返回
    // 422 quota_unsupported_by_client。
    TrafficQuotaUpdate traffic_quota_update = 4;
  }
}

enum TrafficQuotaAction {
  TRAFFIC_QUOTA_ACTION_UNSPECIFIED = 0;
  SET    = 1;     // state 字段必填，表示新增或更新
  REMOVE = 2;     // state 字段必须为空，表示删除（client 摘掉 QuotaHandle）
}

message TrafficQuotaUpdate {
  string request_id = 1;                  // ULID, 日志关联
  string user_id    = 2;
  TrafficQuotaAction action = 3;
  optional TrafficQuotaState state = 4;   // SET 时携带；REMOVE 时空
}

message TrafficQuotaState {
  int64 monthly_bytes              = 1;   // 当前配额上限
  int64 budget_remaining_bytes     = 2;   // = monthly_bytes - current_period_bytes_used
                                          // 可以为负（耗尽场景）
  int64 period_started_at_unix_sec = 3;
  int64 period_ends_at_unix_sec    = 4;
  bool  exhausted                  = 5;   // budget_remaining <= 0 的显式信号
}
```

**Reconnect Replay 顺序**(server 端的 control-plane 同步流程):
1. 发 `Welcome`(现有)
2. **新增**:对该 client 上每个 (user, client) 配额发
   `TrafficQuotaUpdate { action: SET, state: current_state }` **(在 rule 之前)**
3. 对该 client 当前激活的每条 rule 发 `RuleUpdate { action: PUSH }`(现有)
4. 对该 client 当前每个 owner cap 发 `OwnerRateLimitUpdate { action: SET }`(现有)

**为什么 quota 在 rule 之前**(回应 Finding 5):rule 激活会立刻 bind listener
开始接受流量,如果 quota 尚未注册,该窗口内的字节会逃过 enforcement。把 quota
放到第 2 步,确保 rule 落地时其 (user, client) QuotaHandle 已经存在。

**QuotaScopeManager**(配套):仿 v0.11 的 `OwnerRateLimitScopeManager`
(crates/portunus-client/src/forwarder/rate_limit/scope.rs 模式),
对 (user_id, client_name) 维护 in-memory registry:
- `install(user_id, state)` / `remove(user_id)` 被 `TrafficQuotaUpdate` 处理器调用
- 转发器 accept 钩点 `lookup(rule.owner_user_id) -> Option<Arc<QuotaHandle>>`
- 这样后续动态 SET/REMOVE 在已建立的 rule 上立即生效,无需重启转发器

**No quota → No message**(对没设配额的 user-client pair,client 永远不知道,
`lookup()` 返回 `None`,跳过所有 quota 检查)。

### 4.3 关键决策

**1. Client 在 (user, client) pair 内本地执行,server 是 cumulative 真理来源。**
   一次连接的 bytes 先在 client 的 QuotaHandle.remaining 上扣减,本地到 0 触发硬杀
   (拒新 + 关现有)。Server 从 RuleStats 算 delta 入 SQLite,是会议记录而非实时
   gate。**故意有 5s 上报滞后:** server 的 used 数永远 ≤ client 实际已转发。

**2. 这是 "bounded best-effort hard kill",不是 "zero tolerance"。**
   完整的过量来源在 §9.1 给出量化表;此处只给量级直觉:
   - **稳态 over-quota**:仅受单次 IO 块大小 × 并发数限制(TCP userspace
     64 KiB,splice 1 MiB,UDP 单 datagram)
   - **重启 over-quota**:client 重启后,内存里的 remaining 丢失。Server
     当时的 SQLite `current_period_bytes_used` 落后实际转发约
     `StatsReport 周期 × 带宽`(5s × link Mbps),reconnect replay 阶段
     server 把这个滞后值下发 → 新 remaining 偏大 → 该窗口内允许过量。
     最大重启 over-quota ≈ `5s × bandwidth + reconnect RTT × bandwidth`。
     **10 Gbps 单 pair 约 6.25 GB,100 Mbps 单 pair 约 62 MB**。
   - 不适合按 bit 精确计费。§9.1 给出 buffer 推荐公式。

**3. Server 推 TrafficQuotaUpdate 的四个时机:**
   - (a) 操作员 PUT/PATCH/DELETE quota → 立即推 SET/REMOVE
   - (b) Period 翻页 → 推 SET(新 state, exhausted=false)
   - (c) Server 端聚合后**首次**观察到 `current_period_bytes_used >= monthly_bytes`
     → 推 SET(exhausted=true)(止血通知;**仅在 server 端发现耗尽,
     client 不主动报**,这样无需新增 client→server wire 字段)
   - (d) Reconnect replay 阶段 → 推所有 SET
   **不**在每次 StatsReport 都推 —— 推送只在状态切换 + 配置变更时。

   **关键:client 不主动通知 server 自己耗尽**(回应 Finding 2)。
   Client 本地 QuotaHandle 到 0 → 本地 enforcement 立刻生效(关连接 / 拒新),
   server 在下一个 StatsReport tick(≤ 5s)看到 cumulative ≥ monthly 时自然
   触发 (c)。这保持 client→server wire 0 改动。

**4. Period 推进真理在 server。** Client 不自走;只信任 server 推下来的
   period_started/period_ends。这样跨时区/时钟漂移由 server 单方处理。
   Client 断网期间若 server 翻页 → reconnect replay 阶段拿到最新 state。

**5. 没设 quota 的 pair 完全旁路。** `QuotaScopeManager.lookup()` 返回 `None`
   → forwarder 内的 `Option<Arc<QuotaHandle>>` 为 `None` → 一次外层
   `if let Some(h) = &quota_handle` 跳过所有 quota 操作。**只有 quota-enabled
   的连接才付额外开销**(一次 atomic 减 + check)。

**6. 硬杀的具体实现(覆盖 Finding 1, 修订自 Finding 3)。** 三条数据面路径:

   - **TCP userspace**:**不**包裹 `AsyncRead`/`AsyncWrite`(poll 中段返回 Err
     会让 caller 看到字节已传完但仍是错误,语义脏)。改为参考 v0.11
     `rate_limit::copy` 模式(rate_limit/copy.rs:213):写一个 quota-aware
     copy loop,`writer.write_all(&buf[..n]).await?` 成功后再 `total += n` +
     `quota.consume(n)`,若 `consume` 返回 exhausted → break 外层循环 → 关闭
     连接。**已交付字节先记账后判定**,无脏 IO 状态。当有 QuotaHandle 时,
     `copy_uncapped` 走这条 quota-aware loop;无 QuotaHandle 时直接走现有
     `copy_bidirectional_with_sizes`(splice 或纯 userspace 不变)。

   - **TCP splice (Linux)**:splice helper 已在每次 `bytes_out.fetch_add(n)`
     位置(splice.rs:524)推进字节计数。在该位置后插 quota hook:
     ```rust
     if let Some(h) = quota_handle.as_ref() {
         if h.consume_saturating(n as i64).is_exhausted() {
             return Err(SpliceError::QuotaExhausted);
         }
     }
     ```
     `try_join!` 的另一方向通过 `bytes_in` 同样处理。一次迭代上限 1 MiB
     (PipePair 容量),所以单次过量上限 = 1 MiB × 并发连接数。
     **不要叫"零开销"** —— quota-enabled flow 每个 chunk 多一次 atomic;
     旁路 flow 通过外层 `Option::None` 短路完全不进 hook,确实零开销
     (回应 reviewer 第 1 点)。

   - **UDP per-datagram**:UDP 已经是包级 IO,在 forwarder/udp/mod.rs
     datagram 转发函数里 payload 大小直接 consume。Exhausted → drop datagram
     + 关 upstream socket(flow 失效)。**精度最高:per-datagram。**

   **`QuotaHandle.consume(n)` 实现**(修订自 Finding 1 的 wrap 风险):
   ```rust
   /// 不会 underflow。已 exhausted 时 fast-path 返回,不再扣 remaining。
   pub fn consume(&self, n: i64) -> Result<(), QuotaExhausted> {
       // Fast path: 已耗尽,立即返回,不再 fetch_sub(防 wrap)
       if self.exhausted.load(Ordering::Acquire) {
           return Err(QuotaExhausted);
       }
       // CAS loop:把 remaining 减到 saturating 0,绝不允许变负越过 0
       let mut cur = self.remaining.load(Ordering::Relaxed);
       loop {
           if cur <= 0 {
               // 与并发 consume 竞争失败,且对方已扣到 0
               self.mark_exhausted();
               return Err(QuotaExhausted);
           }
           let new = cur.saturating_sub(n).max(0);  // 永不为负
           match self.remaining.compare_exchange_weak(
               cur, new, Ordering::AcqRel, Ordering::Relaxed
           ) {
               Ok(_) => {
                   if new == 0 {
                       self.mark_exhausted();
                       return Err(QuotaExhausted);
                   }
                   return Ok(());
               }
               Err(actual) => { cur = actual; continue; }
           }
       }
   }

   /// 只 set 一次 exhausted 位,无 wire 通知(server 自己会从 RuleStats
   /// cumulative 算出 >= monthly 时下发止血消息)
   fn mark_exhausted(&self) {
       let _ = self.exhausted.compare_exchange(
           false, true, Ordering::AcqRel, Ordering::Relaxed
       );
   }
   ```
   **关键不变量**:`remaining >= 0` 永远成立(CAS loop 保证),无溢出风险。
   已 exhausted 后所有 consume() 走 fast-path 直接返回,不动 remaining。

**7. 历史聚合覆盖所有 (user, client) pair,不只是配额 pair。**
   Server 的 RuleStats 处理器(`RuleStatsCache::observe`)每次收 cumulative,
   算 delta 后:
   - 写 `traffic_samples_1m`(UPSERT 当前分钟,delta 累加)
   - **若** (user, client) 存在 `traffic_quotas` 行 → 同步累加
     `current_period_bytes_used` + 检查耗尽
   这样 UI Traffic tab 能展示任何 pair 的历史,跟配额配置解耦。

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

**PATCH body**(两个独立 op,可同请求合并):
```json
{ "monthly_bytes": 1073741824000, "clear_period_usage": true }
```
- `monthly_bytes`(可选):修改配额上限,不动 period 边界。
- `clear_period_usage`(可选):**仅清零** `current_period_bytes_used` + 清
  `exhausted_at`。Period 起点 / 终点 / billing_anchor 都不动。
  适用于"管理员奖励本期额外字节"或"误算后修正"。
- 两个 op 一起时,先改 limit 再清 used,然后一次性推 TrafficQuotaUpdate。

**注意**:**没有** `reset_now`(原 design 的歧义概念已删)。
要完全换 billing_anchor:`DELETE` quota 再 `PUT` 新的(走两个 audit event,
明确审计 trail)。`billing_anchor` **不可变**。

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

### 5.2.5 资源层级说明(回应 Open Q B)

写路径(CRUD)挂在 user 维度 `/v1/users/{user_id}/quotas/{client_name}`,
与 `quota-dev` 分支的 AccessEntry 模型一致(操作员心智模型是"为这个 user
配置在哪些 client 上能用多少")。

读路径在两个维度都提供:
- `GET /v1/users/{u}/quotas`(user 视角)
- `GET /v1/clients/{c}/quotas`(client 视角,列出该 client 上所有 user 的 quota,
  含 `current_period_bytes_used`,**只读**)
- `GET /v1/users/{u}/traffic` 和 `GET /v1/clients/{c}/traffic`(对称的查询)

**有意分叉**:写路径不在 client 维度提供。理由:
- v0.11 的 owner cap `/v1/clients/{c}/owners/{o}/...` 是 cap 生命周期跟 rule 绑死
  的 ephemeral 资源(rule 全删 → cap GC),client 维度作为持久 resource 操作的入口
  合理。
- v1.4 traffic quota 是 billing 资源,跨越 rule 生命周期持久存在,且操作的核心
  对象是 user(不是 client),client 是限定符。
- 写路径单维度避免双更新源争夺(RBAC 一致性 + audit trail 一致性)。

### 5.3 错误码新增

- `422 quota_target_not_found`:PUT 时 (user, client) 没 grant
- `422 quota_bucket_out_of_retention`:from 超出 retention 边界
- `422 quota_unsupported_by_client`:目标 client 的 `Hello.client_version` < 1.4.0
  (与 v0.7/v0.11 现有 capability gate 一致,使用 `Hello.client_version` 而非
  `protocol_version`;参考 `crates/portunus-server/src/grpc/service.rs:99` 的
  `set_client_version` 模式)
- `400 invalid_quota_size`:monthly_bytes < 0 或超 i64::MAX
- `400 invalid_billing_anchor`:billing_anchor 不在合法 unix sec 范围
- 复用现有 `403 rbac_denied`、`404 user_not_found` / `client_not_found`

### 5.4 RBAC

| 操作 | superadmin | client owner | user 本人 |
|------|-----------|--------------|----------|
| PUT / DELETE quota | ✓ | ✓ | ✗ |
| PATCH monthly_bytes / clear_period_usage | ✓ | ✓ | ✗ |
| GET status | ✓ | ✓ | ✓(仅自己) |
| GET traffic 历史 | ✓ | ✓(client 内) | ✓(仅自己) |

### 5.5 新增 Prometheus 指标

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
- "Clear usage" 按钮(清零本期已用量,不改 period 边界;带确认弹窗,
  弹窗里点明"本期 billing anchor 不变,只清零计数")
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
> [Clear usage] [Increase limit]

### 6.4 图表库

新增依赖：**`recharts`**（~95 KB gz）。理由：开发速度、shadcn token 兼容、社区大。
现有 webui size budget 500 KB gz，仍在范围内。如接近上限再考虑 `uPlot`（~45 KB）。

### 6.5 i18n

新命名空间 `traffic.*` + `userQuota.*` 扩展。中英对照：
- Monthly quota / 月度配额
- This period / 本期
- Resets on / 重置日期
- Exhausted / 已耗尽
- Clear usage / 清零本期用量(刻意避开 "Reset" 一词,免得让操作员误以为
  重置 billing anchor 或 period 边界)
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
| 4 | 重建 grant | quota 行保留(PK 不变),不重置 period(**有意分叉 v0.11 owner-cap GC,见下文 §7.1**) |
| 5 | 时钟漂移 | period 用 unix sec 单调比较;跳大可一次性翻多 period(§3.3 while 循环) |
| 6 | Migration | V008 只建表,无 backfill;现有 pair 默认无配额 = 无限 |
| 7 | Range/SNI/Multi-target | 数据面已挂 owner_id,统一归到 (user, client) 的 QuotaHandle |
| 8 | 删 user / client | 软删;quota 行不删(复活时复用,§7.1) |
| 9 | 多 user / client | 不新增 wire 字段 → 无 gRPC 消息膨胀风险(只用现有 RuleStats) |
| 10 | 保留边界查询 | from 超 90d 422;7d–90d 自动用 1h 桶 |
| 11 | UDP 计量 | 用 datagram payload 字节(不含头),跟 TCP 同单位 |
| 12 | PUT 同时跑 | client 原子替换 remaining;降配立即生效 |

### 7.1 与 v0.11 owner-cap 的生命周期分叉(回应 Finding 5)

v0.11 的 `OwnerRateLimit`(per-(user, client) bandwidth/concurrent cap)在
`owner.rs:187 gc_after_rule_removed` 里实现 **ephemeral** 语义:当该 owner
在该 client 上 **最后一条 rule 被删** 时,cap envelope 被 GC 掉。

v1.4 traffic_quotas **不这么做**,**有意分叉**:

| 维度 | v0.11 owner cap | v1.4 traffic quota |
|------|-----------------|--------------------|
| 资源性质 | runtime control(限速 / 并发) | billing artifact(本期已用字节) |
| 生命周期 | 跟 rule 共存亡 | 跨 rule 持久 |
| 删最后一条 rule 后 | GC | 保留 |
| 删 user / client 后 | 级联删 | 软删保留(复活复用) |

**理由**:
- 若 quota 跟 rule 一起 GC,操作员/恶意用户可以在月底删光 rule、月初重建以
  "刷新"配额计数,绕过计费。
- billing anchor 一旦丢失就无法重建周期序列,使后续审计困难。
- 一行 `traffic_quotas` 几十字节,长期保留无空间成本。

**操作员清理 staled quota**:UI 提供 `DELETE` 显式入口;不自动 GC。

## 8. 实现范围

| 层 | 变动 |
|----|------|
| portunus-proto | **server→client only**:新增 `TrafficQuotaUpdate` + `TrafficQuotaState` 作为 `ServerMessage.payload` field 4。**Client→server 0 改动**。 |
| portunus-server | V008 migration、`traffic_quotas` CRUD、rollup task、per-(user, client) aggregator on RuleStats path、`TrafficQuotaUpdate` push + reconnect replay、HTTP 6 endpoints、5 Prometheus metric、RBAC 接线、period anniversary `next_period` 实现(§3.3) |
| portunus-client | `QuotaHandle { remaining: AtomicI64, exhausted: AtomicBool }` + saturating CAS `consume`、`QuotaScopeManager` (per-(user,client) registry,仿 `OwnerRateLimitScopeManager`)、TCP userspace quota-aware copy loop(仿 rate_limit/copy.rs:213,write_all 后扣减)、splice per-iteration consume hook、UDP per-datagram consume hook、`TrafficQuotaUpdate` SET/REMOVE 处理、reconnect replay 接收(quota 先于 rule) |
| webui | AccessEntry 表两列、Traffic tab、超额 banner、recharts 依赖、i18n、`clear_period_usage` 按钮 + 确认弹窗 |
| portunus-e2e | 端到端:建 quota → 跑流量 → 验证 1m/1h 历史 → 验证硬杀(三种路径) → 翻页恢复 → 重连 replay |
| docs | runbook "启用月配额"、API 参考、troubleshooting 加 quota 触发日志、明确"bounded best-effort"语义 |

## 9. 风险 / 已知折衷

### 9.1 Bounded best-effort 语义(回应 Finding 3 + 修订 Finding 4)

**这不是 zero-tolerance 系统**。过量上限分两类来源:

**A. 稳态 over-quota**(quota 工作正常的连接,触发 exhausted 后多传的字节)

| 数据面路径 | 单连接稳态过量上限 | 总稳态过量上限 |
|----------|------------------|--------------|
| TCP userspace quota-aware copy | 单次 `write_all` chunk(典型 64 KiB) | 64 KiB × 并发连接数 |
| TCP splice (Linux) | 单次 splice 迭代(≤ 1 MiB,PipePair 容量) | 1 MiB × 并发连接数 |
| UDP datagram | 单个 datagram payload(≤ 65 KiB) | 65 KiB × 并发 flow 数 |

稳态过量 **不依赖于带宽**,只跟 IO chunk × 并发数有关。100 个 TCP 连接同时
触发耗尽 = 最多 100 × 64 KiB = 6.4 MB 过量。可控,无论 100 Mbps 还是 10 Gbps。

**B. Recovery over-quota**(client 重启后丢失内存 counter 导致的允许多传)

| 来源 | 大小 | 100 Mbps pair | 10 Gbps pair |
|------|------|--------------|--------------|
| Client 重启间 server 上报滞后 | StatsReport 周期 × 链路带宽 | 5s × 12.5 MB/s ≈ **62 MB** | 5s × 1.25 GB/s ≈ **6.25 GB** |
| Reconnect replay RTT × 带宽 | 通常 < 100 ms × bw | < 1.25 MB | < 125 MB |
| **合计上限** | StatsReport + RTT 期间 | **~63 MB** | **~6.4 GB** |

(早期版本写"10 Gbps ≈ 10 MB"是把稳态 A 项的 max 当成总额,**错误**;
本表纠正:10 Gbps 的真实重启过量在 GB 级。)

**配额 buffer 推荐**:`max(1% × monthly_bytes, recovery_bound_bytes)`:
```
recommended_quota_threshold =
    monthly_bytes - max(monthly_bytes / 100,
                        StatsReport_period × link_bandwidth_bytes_per_sec)
```
- 1 TB 配额 × 100 Mbps:max(10 GB, 62 MB)= 10 GB buffer,告警阈值 990 GB
- 1 TB 配额 × 10 Gbps:max(10 GB, 6.25 GB)= 10 GB buffer,告警阈值 990 GB
- 10 GB 配额 × 100 Mbps:max(100 MB, 62 MB)= 100 MB buffer,告警阈值 9.9 GB
- 10 GB 配额 × 10 Gbps:max(100 MB, 6.25 GB)= **6.25 GB buffer**,告警阈值
  3.75 GB(此组合下,小配额在高速链路上 buffer 占比过大 → 建议下调
  StatsReport_period,或不要在高速链路上设极小配额)

**不是 bit-perfect billing**(回应 reviewer 第 3 点)。invoice 级别按 bit
计费的场景应增量加入:client 端 counter 持久化到 disk(crash 不丢);
server 端 cumulative ack。本版本明确不做。

### 9.2 其他风险

1. **Client crash 期间字节丢失**:in-memory counter 丢失,server 重连后从
   该 client 的下一次 RuleStats 重新 baseline。崩溃期间转发的字节不计费。
2. **不支持月配额跨多 client 共享**:per-(user, client) 维度。"user 全局
   总额"是潜在 v1.5 增强(schema 在 user 维度可加列)。
3. **不支持告警 webhook**:超额仅靠 UI banner + Prometheus
   `quota_exhausted_total` counter。
4. **没有计费导出 / 发票**:仅暴露原始数据 + CSV,不出账单。
5. **billing_anchor 不可变**:避免周期重叠歧义。换锚点 → DELETE + PUT。
6. **Rollup 窗口语义**:rollup 在小时 +1min 触发;`t=H+0..H+1min` 的查询
   看不到刚过去那个小时的 `1h` 行(但能从 `1m` 表实时算 sum)。
7. **历史样本会膨胀**:理论极端 100k pair × 1440min × 7d ≈ 1B 行 / 1m 表。
   需要监控 SQLite 大小,必要时下调 retention。文档给出"100 pair 推荐配置"
   作为参考起点。
8. **splice 路径丢失 quota check 风险**:若未来 splice helper 内部循环重构,
   per-iteration `consume()` hook 可能错过 → e2e 测试必须覆盖
   "linux splice + quota exhausted" 用例。

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

**审阅检查**:
- ✅ 无 TBD / TODO
- ✅ 4 节内部一致(数据模型 ↔ 协议 ↔ API ↔ UI 在 (user, client) 维度一致)
- ✅ scope 足以单一 plan 落地(一个 v1.4.0 release)
- ✅ 边界场景显式列出(§7)
- ✅ 与现有 wire / SQLite / RBAC 兼容性逐项说明

## 12. Review-driven 修订记录

Initial draft (commit 44fd6b6) 经一轮 code review 后修订:

1. **硬杀机制具体化**(Finding 1):三条数据面路径(TCP userspace / TCP splice /
   UDP)分别给出 per-IO instrumentation 方案,QuotaHandle.consume 在每次 IO
   操作后扣减(§4.3 决策 6)。
2. **删除 `usage_delta_by_user`**(Finding 2):Server 端从现有 `RuleStats`
   cumulative 算 delta 后按 owner_id 聚合,wire 端 client→server 0 改动(§4.2)。
3. **改为 bounded best-effort 语义**(Finding 3):新增 §9.1 量化过量上限,
   删除 "zero tolerance" 措辞,文档明确建议 1% buffer。
4. **完整定义 server→client wire**(Finding 4):`TrafficQuotaUpdate` 作为
   `ServerMessage.payload` field 4 oneof variant,带 SET/REMOVE action,
   `reconnect replay` 阶段显式发送(§4.2)。
5. **明确生命周期分叉**(Finding 5):新增 §7.1 把跟 v0.11 owner-cap GC 的
   分歧写清楚,给出"防止月底删 rule 刷新配额"等理由。
6. **完整定义 billing anniversary**(Finding 6):新增 §3.3 给出
   `period_start_at(anchor, n)` 伪代码 + Jan 31 / 闰年 / 多月跳跃边界。
7. **整数边界进 spec**(Finding 7):SQLite `CHECK monthly_bytes BETWEEN 0
   AND i64::MAX`,proto 改 `int64`(§3.1)。
8. **Open Q A**:历史聚合覆盖**所有** (user, client) pair,不限于设了配额的
   (§4.3 决策 7)。
9. **Open Q B**:写路径单维度 user-centric,读路径双维度对称;增加
   `GET /v1/clients/{c}/quotas`(§5.2.5)。
10. **Open Q C**:`reset_now` 改为 `clear_period_usage`(只清计数器,不动
    周期边界);完全重锚走 DELETE + PUT(§5.1 PATCH body)。

### Round 2(commit 2e3336c 之后)

1. **`QuotaHandle.consume` underflow 风险**(R2 Finding 1):伪代码改为
   "先 check exhausted fast-path + CAS loop saturating_sub",保证
   `remaining >= 0` 不变量,杜绝并发 fetch_sub 把计数器 wrap 到 i64::MIN
   的可能(§4.3 决策 6)。
2. **删 `report_exhausted_once()`**(R2 Finding 2):原伪代码里的 client
   主动通知与 "client→server 0 改动" 矛盾。改为 client 只 set 本地
   `exhausted` 位,server 端在下次 StatsReport(≤5s)看到 cumulative ≥
   monthly 时自然下发止血(§4.3 决策 3 + 决策 6 `mark_exhausted`)。
3. **TCP userspace 改用 quota-aware copy loop**(R2 Finding 3):放弃
   `QuotaInstrumentedStream` 包装方案(`poll_read`/`poll_write` 中段
   Err 语义脏)。改用类似 `rate_limit::copy` 的模式(rate_limit/copy.rs:213):
   `write_all().await?` 成功之后 `total += n; quota.consume(n)?`,
   出错则 break 外层循环关闭连接,无脏 IO 状态(§4.3 决策 6 TCP userspace)。
4. **重启 over-quota 数字纠错**(R2 Finding 4):原 §9.1 写
   "10 Gbps ≈ 10 MB" 是把稳态 A 项当总额,**错误**。重启场景的真实上限
   是 StatsReport 周期 × 带宽,10 Gbps 真实 ≈ 6.25 GB。§9.1 重写两表
   分别量化稳态 + recovery,并把 buffer 推荐改成
   `max(1%, recovery_bound)` 公式,避免小配额在高速链路上 buffer 失效。
5. **Replay 顺序改成 quota 在 rule 之前**(R2 Finding 5):quota 在第 2 步,
   rule 在第 3 步,确保 rule 落地激活 listener 时 QuotaHandle 已 registry
   就位。配套定义 `QuotaScopeManager`(仿 v0.11 `OwnerRateLimitScopeManager`)
   让后续动态 SET/REMOVE 在已建立的 rule 上立即生效(§4.2)。
6. **Capability gate 改用 `Hello.client_version`**(R2 Finding 6):跟现有
   v0.7/v0.11 capability gate 一致,而非新造 `protocol_version` 分支
   (§5.3 错误码)。
7. **UI 文案 "Reset now" → "Clear usage"**(R2 Finding 7):API 已改名
   `clear_period_usage`,UI 同步;i18n key 注明"刻意避开 Reset"避免操作员
   误以为重置 billing anchor(§6.1 / §6.3 / §6.5)。

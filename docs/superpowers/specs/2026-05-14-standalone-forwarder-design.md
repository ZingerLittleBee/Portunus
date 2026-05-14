# Standalone Forwarder — Design

**Status:** draft · awaiting user review
**Branch:** `feat/standalone-forwarder`
**Target release:** v1.5.0
**Date:** 2026-05-14

## 1. Goal

把当前 `portunus-client` 内的数据面(`forwarder/`, `resolver/`, `port_groups`,
`shutdown`)抽到独立的 `portunus-forwarder` lib crate,在此之上交付一个新的
`portunus-standalone` 二进制——一份不依赖 portunus-server 控制平面、用 TOML
驱动的"小型服务器重量级转发器"(类比 frpc-stcp / gost 的本地子集)。

成功标准:
1. `portunus-client` 行为对调用者零变化(同样的 bundle、同样的重连、同样的 quota
   语义、同样的字节计数 / Prometheus 暴露)。
2. `portunus-standalone` 单一 binary 可用 TOML 启动多规则转发,二进制不携带
   gRPC / tonic / portunus-proto / portunus-auth 依赖。
3. 数据面代码 100% 共享——同一段 `proxy.rs / splice.rs / udp/` 既被 client
   消费、也被 standalone 消费,后续 v1.5+ 数据面演进自动两边受益。
4. 性能门:`portunus-forwarder` benches 相对 v1.4.0 黄金基线无 >5% 回退;
   splice 微基准字节级一致。

## 2. Audience & Use Case

目标用户:在单台 VPS / 容器内运行的轻量 TCP/UDP 转发器使用者。典型场景:
- SSH/DB 端口出墙转发
- 域名后端的反向 TCP 代理(域名解析 + 主动失败转移)
- UDP 游戏服务 / DNS 转发
- 后端是 nginx/haproxy,前置加 PROXY protocol 让后端拿到真实 client IP

明确**不在**单机版范围内:
- 集中控制(server 端 RBAC / Web UI / 审计)
- TLS SNI 路由(高级特性,留给 client 控制面)
- 每用户字节配额 / QoS 限速(需要多租户 + 状态管理)
- 热重载(运行时改 TOML 必须重启进程)
- HTTP 管理面 / Prometheus 端点(只 stderr 结构化日志)

## 3. Architecture

### 3.1 Crate 布局

```
crates/
├── portunus-proto/        (unchanged)
├── portunus-core/         + Protocol 类型从 proto 上提到这里
├── portunus-auth/         (unchanged)
├── portunus-forwarder/    新增 lib crate · 抽出的数据面
├── portunus-server/       (unchanged)
├── portunus-client/       slimmer · 改为依赖 portunus-forwarder
├── portunus-standalone/   新增 binary crate · 仅依赖 portunus-forwarder
└── portunus-e2e/          (unchanged)
```

### 3.2 `portunus-forwarder` 内部布局

```
src/
├── lib.rs                  // re-exports
├── forwarder/              // 从 portunus-client/src/forwarder/ 整体平移
│   ├── mod.rs              //   ClientRule, RuleStatusEvent, run_forwarder
│   ├── proxy.rs            //   TCP 转发核心
│   ├── splice.rs           //   Linux splice(2) 数据面
│   ├── udp/                //   UDP 流表 + 转发
│   ├── range.rs            //   端口 range 原子绑定
│   ├── failover.rs         //   多目标
│   ├── failover_path.rs    //
│   ├── probe.rs            //   主动探测
│   ├── proxy_protocol.rs   //   PROXY v1/v2 前缀
│   ├── sni/                //   SNI 路由(client 用,standalone TOML 不暴露)
│   ├── quota/              //   QuotaHandle 原子计数(client 用)
│   ├── rate_limit/         //   per-rule 令牌桶(client 用)
│   └── stats.rs            //   RuleStats 原子计数器 + StatsSink trait
├── resolver/               // hickory-resolver 包装
├── port_groups.rs          // 监听端口聚合
└── shutdown.rs             // CancellationToken + signal handler
```

### 3.3 公共 API 表面 (`pub use`)

```rust
// portunus-forwarder/src/lib.rs
pub use forwarder::{ClientRule, MultiTarget, RuleStatusEvent, run_forwarder};
pub use forwarder::quota::QuotaHandle;
pub use forwarder::stats::{
    StatsSink, StatsSnapshot, RuleStatsSnapshot,
    RejectReason, ThrottleKind,
    spawn_stats_reporter,
};
pub use forwarder::sni::{SniRoute, SniRouteSpec};
pub use forwarder::rate_limit::RateLimitSpec;
pub use forwarder::proxy_protocol::ProxyProtocolVersion;
pub use resolver::{Resolve, LiveResolver};
pub use shutdown::Shutdown;
pub use port_groups::{rebuild_watches, GroupMember};
```

### 3.4 控制面 ↔ 数据面接缝

数据面 lib 通过 4 个接缝与控制面通信:

| 接缝 | 类型 | client 实现 | standalone 实现 |
|------|------|-------------|-----------------|
| 规则输入 | `ClientRule` 值 | 从 gRPC `Rule` 翻译 | 从 TOML 反序列化 |
| 生命周期 | `run_forwarder(...)` async fn | 同 | 同 |
| Stats 输出 | `Arc<dyn StatsSink>` | `GrpcStatsSink` → StatsReport → bidi stream | `LoggingStatsSink` → tracing::info! 每 60s |
| 配额 | `ClientRule.quota: Option<Arc<QuotaHandle>>` | `QuotaScopeManager` 构造并替换 | 始终 `None`(短路) |

`StatsSink` trait 详见 §6.2。

## 4. `portunus-standalone` 详细设计

### 4.1 CLI

```
portunus-standalone --config <PATH> [--check] [--log-level LVL] [--log-format FMT]

  -c, --config <PATH>    TOML 配置文件
                         默认搜索: ./standalone.toml → /etc/portunus/standalone.toml
      --check            仅校验配置后退出,不绑定端口(exit 0 / 2)
      --log-level <LVL>  覆盖 [global].log_level (off|error|warn|info|debug|trace)
      --log-format <FMT> 覆盖 [global].log_format (json|pretty)
  -V, --version
  -h, --help
```

无 subcommand。

### 4.2 TOML schema

```toml
[global]
log_level            = "info"     # 默认
log_format           = "json"     # 默认 (容器/systemd 友好)
shutdown_drain_secs  = 30         # SIGINT/SIGTERM 后 drain 时长

[defaults]
udp_max_flows        = 1024
udp_flow_idle_secs   = 60
prefer_ipv6          = false

[[rule]]
name        = "ssh-tunnel"        # 必填 · workspace 内唯一 · 派生 RuleId
protocol    = "tcp"               # "tcp" | "udp"
listen      = "0.0.0.0:2222"      # "<addr>:<port>" 或 "<addr>:<lo>-<hi>"
target      = "10.0.0.5:22"       # 单目标; 与 targets 互斥

# 端口 range
[[rule]]
name     = "web-range"
protocol = "tcp"
listen   = "0.0.0.0:8000-8009"
target   = "10.0.0.10:8000-8009"  # range size 必须等于 listen range size

# 域名后端
[[rule]]
name        = "https-by-name"
protocol    = "tcp"
listen      = "0.0.0.0:443"
target      = "backend.internal:443"
prefer_ipv6 = true

# 多目标 failover + PROXY protocol
[[rule]]
name                = "ha-https"
protocol            = "tcp"
listen              = "0.0.0.0:8443"
targets             = [
  { host = "primary.internal",   port = 443, priority = 0  },
  { host = "secondary.internal", port = 443, priority = 10 },
]
probe_interval_secs = 5            # 可选 · 主动探测; 缺省=被动检测
proxy_protocol      = "v2"         # "off" | "v1" | "v2" (默认 "off")

# UDP
[[rule]]
name               = "game-udp"
protocol           = "udp"
listen             = "0.0.0.0:27015"
target             = "10.0.0.20:27015"
udp_max_flows      = 4096          # 覆盖 [defaults]
udp_flow_idle_secs = 120
```

#### 4.2.1 解析与校验
- `#[serde(deny_unknown_fields)]` 在所有 sections 上;拼错字段或非平衡套餐字段
  (sni_routes / rate_limit / quota / bandwidth_*)立即报错并提示。
- `protocol` 字符串枚举,大小写不敏感。
- `target` 与 `targets` 互斥(`oneof` 语义);
  `targets` + 端口 range 不能并存(沿用 v0.7 现状)。
- `listen` / `target` range 大小必须相等。
- `udp_max_flows / udp_flow_idle_secs` 仅 UDP 生效,TCP 时设置则 warn-log 一次。
- `proxy_protocol` 仅 TCP 生效。
- `name` 必填、workspace 内唯一;`RuleId` = `format!("standalone:{name}")`。

#### 4.2.2 启动期错误格式

```
error: rule "web-range": listen range 8000-8009 (10 ports) does not match
       target range 8000-8019 (20 ports)
  at /etc/portunus/standalone.toml line 14
```

非零 exit code:
- `2` 配置错误(包括 `--check` 检测出的所有问题)
- `1` 启动期 bind 失败 / 运行期 rule failure 级联
- `0` 正常关停

### 4.3 启动门 + 失败语义

```rust
async fn run(cfg: Config) -> ExitCode {
    let shutdown = Shutdown::new();
    tokio::spawn(shutdown.clone().signal_handler());

    let resolver = Arc::new(LiveResolver::from_env());
    let stats_sink = Arc::new(LoggingStatsSink::new(Duration::from_secs(60)));

    let (status_tx, mut status_rx) = mpsc::channel(64);
    let mut joinset = JoinSet::new();
    let rule_ids: HashSet<RuleId> = cfg.rule_ids();

    for rule in cfg.into_client_rules() {
        joinset.spawn(run_forwarder(
            rule,
            shutdown.token(),
            Duration::from_secs(cfg.global.shutdown_drain_secs),
            status_tx.clone(),
            stats_sink.clone(),
            resolver.clone(),
        ));
    }
    drop(status_tx);

    // 启动门: 收集每个 rule_id 的首个 Activated/Failed
    let mut pending = rule_ids.clone();
    let mut failures: Vec<(RuleId, String)> = Vec::new();
    while !pending.is_empty() {
        match status_rx.recv().await {
            Some(Activated { rule_id }) => { pending.remove(&rule_id); }
            Some(Failed { rule_id, reason }) => {
                pending.remove(&rule_id);
                failures.push((rule_id, reason));
            }
            Some(Removed { .. }) => {} // 启动期不应发生
            None => break,
        }
    }
    if !failures.is_empty() {
        eprintln!("error: {} rule(s) failed to bind:", failures.len());
        for (id, why) in &failures { eprintln!("  - {id}: {why}"); }
        shutdown.trigger();
        while joinset.join_next().await.is_some() {}
        return ExitCode::from(1);
    }

    // 运行态: 任意 Failed 触发级联
    tokio::spawn(async move {
        while let Some(ev) = status_rx.recv().await {
            if let Failed { rule_id, reason } = &ev {
                error!(event = "rule.failed", %rule_id, %reason);
                // 级联 (未来若加 rebind 路径才会进这里)
            }
        }
    });

    while joinset.join_next().await.is_some() {}
    info!(event = "standalone.stopped");
    ExitCode::SUCCESS
}
```

### 4.4 `LoggingStatsSink`

```rust
pub struct LoggingStatsSink { /* 内部聚合状态 */ }

impl StatsSink for LoggingStatsSink {
    fn drain(&self, snap: StatsSnapshot) {
        for (rule_id, rs) in snap.per_rule {
            info!(
                event = "standalone.stats",
                rule = %rule_id,
                in_bytes = rs.bytes_in,
                out_bytes = rs.bytes_out,
                active_conns = rs.active_conns,
            );
        }
    }
    fn record_reject(&self, rule: RuleId, reason: RejectReason) {
        warn!(event = "standalone.reject", %rule, ?reason);
    }
    fn record_throttle(&self, rule: RuleId, kind: ThrottleKind) {
        warn!(event = "standalone.throttle", %rule, ?kind);
    }
}
```

无 `_total` 后缀,无 monotonic counter 语义。

### 4.5 信号处理

- `SIGINT` / `SIGTERM` → `Shutdown::trigger()` → cancel token → 各 forwarder
  停止 accept → 在 `shutdown_drain_secs` 内 drain → exit 0。
- `SIGHUP` → 静默忽略(标准库 `tokio::signal::unix::SignalKind::hangup()`
  注册一个空 handler,避免默认 terminate 行为)。

### 4.6 资源限制
- 启动时 log `getrlimit(RLIMIT_NOFILE)`,如果 < 4096 加 warn。
- 不主动 setrlimit;依赖 systemd unit / docker --ulimit。

## 5. `portunus-client` 侧改动

行为零变化。改动按代价分:

### 5.1 机械化(脚本可改)
- `git mv` forwarder/ resolver/ shutdown.rs port_groups.rs → portunus-forwarder/src/
- 全局替换 `use crate::forwarder::` → `use portunus_forwarder::`(等)
- `portunus-client/Cargo.toml`:删 forwarder 专属 deps、加 `portunus-forwarder`
- benches 跟源迁到 portunus-forwarder

### 5.2 StatsSink 抽象(主要设计工作)

当前 `stats.rs` 同时承担:
1. 每规则原子计数(数据面 hot path)
2. Aggregator + ticker(把 RuleStats 快照成 `StatsReport` → mpsc → gRPC)

拆分:**(1) 留在 forwarder,(2) 拆成 trait + binary 各自实现**。

```rust
// portunus-forwarder/src/forwarder/stats.rs
pub struct RuleStats { /* atomic counters · unchanged */ }

#[derive(Clone, Debug)]
pub struct RuleStatsSnapshot {
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub active_conns: u64,
    pub conns_total: u64,
    pub conns_rejected: u64,
    pub conns_throttled: u64,
}

pub struct StatsSnapshot {
    pub interval: Duration,
    pub per_rule: Vec<(RuleId, RuleStatsSnapshot)>,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum RejectReason {
    TargetUnavailable,
    ConcurrencyCap,
    RateLimit,
    QuotaExhausted,
    AuthFailure,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ThrottleKind {
    Bandwidth,
    NewConnRate,
}

pub trait StatsSink: Send + Sync {
    fn drain(&self, snapshot: StatsSnapshot);
    fn record_reject(&self, rule: RuleId, reason: RejectReason);
    fn record_throttle(&self, rule: RuleId, kind: ThrottleKind);
}

pub fn spawn_stats_reporter(
    sink: Arc<dyn StatsSink>,
    rules: Arc<RwLock<HashMap<RuleId, Arc<RuleStats>>>>,
    interval: Duration,
    cancel: CancellationToken,
) -> JoinHandle<()>;
```

**关键不变量**:数据面 hot path 仍只动原子计数器;trait 调用只在 ticker
任务内发生,与 v1.4.0 数据路径字节级一致。

**`GrpcStatsSink`** (`portunus-client/src/control.rs` 新增):把 `drain()`
的 snapshot 翻译成 `StatsReport`,通过 gRPC bidi stream 上行。封装现有
ticker → mpsc 路径,接口换成 trait。

### 5.3 QuotaScopeManager 不动
`QuotaScopeManager` 留在 portunus-client(消费 server `Welcome.user_quotas`)。
继续构造 `Arc<QuotaHandle>`(类型 re-exported from forwarder)注入
`ClientRule.quota`。重连重放顺序(quotas-before-rules)不变。

### 5.4 `Protocol` 类型上提
`Protocol`(TCP / UDP)从 `portunus-proto::v1::Protocol` 上提到
`portunus-core::Protocol`。proto + server + client 各加一处
`From<core::Protocol> for proto::v1::Protocol` 转换。wire 不变。

**Fallback**:若上提引发意外耦合,退路是 portunus-forwarder 直接依赖
portunus-proto(简单但 forwarder 多一个 gRPC schema 依赖)。Plan 阶段定。

### 5.5 测试归位
- forwarder 模块的 `#[cfg(test)] mod tests` 跟模块搬到 portunus-forwarder
- `portunus-client/tests/` 集成测试改用 `MockStatsSink`(测试帮手)
- `portunus-e2e/tests/traffic_quotas.rs` 等 e2e 不动

## 6. 测试策略

| 测试层 | 位置 | 覆盖 |
|--------|------|------|
| 数据面单元测试 | `portunus-forwarder/src/**/tests` | 跟模块搬入新 crate |
| client 单元测试 | `portunus-client/tests/*` | reconcile / 重连重放 / Quota / GrpcStatsSink ↔ MockStatsSink |
| standalone 单元测试 | `portunus-standalone/src/config.rs::tests` | TOML 解析 happy + error |
| standalone 集成测试 | `portunus-standalone/tests/smoke.rs` | 临时 echo 服务器 + TOML + assert_cmd |
| E2E | `portunus-e2e/tests/standalone_*.rs` | 多规则 / failover / PROXY v2 |
| `--check` CI | `portunus-standalone/tests/check_mode.rs` | 6 fixtures, exit 0/2 |
| bench 回归 | `portunus-forwarder/benches/*` | criterion baseline 沿用 v0.1.0 |

性能门:Phase 3 后做一次 splice 微基准对比 v1.4.0,确认 StatsSink trait
未引入开销(预期 0 ns,因为 hot path 仍是原子计数器)。

## 7. 迁移分阶段

```
Phase 1 — 公共 crate 框架
  ├─ T1.1 创建 crates/portunus-forwarder 空壳 + workspace 注册
  ├─ T1.2 Protocol 类型上提到 portunus-core
  └─ T1.3 workspace build + test 通过

Phase 2 — 平移数据面
  ├─ T2.1 git mv forwarder/ resolver/ shutdown.rs port_groups.rs
  ├─ T2.2 lib.rs 写 pub use re-exports
  ├─ T2.3 portunus-client use 路径全局替换
  ├─ T2.4 Cargo.toml 调整
  ├─ T2.5 benches 跟源迁
  └─ 不变量: portunus-client 行为零变化

Phase 3 — StatsSink trait 抽象
  ├─ T3.1 引入 StatsSink trait + StatsSnapshot
  ├─ T3.2 把 stats.rs 的 ticker 拆出 spawn_stats_reporter
  ├─ T3.3 GrpcStatsSink 实现 (client crate)
  ├─ T3.4 MockStatsSink 测试帮手
  └─ 验证: e2e 全绿 + splice bench 零回退

Phase 4 — portunus-standalone binary
  ├─ T4.1 crate 骨架 + Cargo.toml + bin target
  ├─ T4.2 config.rs · TOML 解析 + 校验
  ├─ T4.3 main.rs · clap CLI
  ├─ T4.4 runtime.rs · 启动门 + 信号 + 关停
  ├─ T4.5 stats_sink.rs · LoggingStatsSink
  └─ T4.6 单元 + 集成测试

Phase 5 — E2E + 文档
  ├─ T5.1 standalone E2E 3 场景
  ├─ T5.2 docs/operations/standalone.mdx (中英)
  ├─ T5.3 README.md 单机版段落
  ├─ T5.4 CHANGELOG.md v1.5.0
  └─ T5.5 Makefile 加 standalone / standalone-check 目标
```

每个 Phase 是独立可 merge 的 PR。

## 8. 风险登记

| 风险 | 缓解 |
|------|------|
| Phase 2 平移漏 `use crate::...` | `cargo build --workspace` 立即报错;无运行时风险 |
| Phase 3 StatsSink trait 引入数据面抖动 | bench 黄金对比 v1.4.0;必要时 `#[inline]` |
| Protocol 上提到 core 牵连 wire 兼容 | proto wire 不变;仅 Rust 类型重命名 + 双向 From |
| 用户 TOML 字段拼错 → 静默失活 | `#[serde(deny_unknown_fields)]` + `--check` |
| 单机版误用 SNI/quota 字段 | 解析器显式拒收,错误信息引导用户使用 portunus-client |

## 9. Out of Scope (v1.5.0)

- TLS 终止(数据面只做裸字节转发,不做 TLS handshake)
- HTTP 管理面 / Prometheus / `/metrics`
- 热重载(SIGHUP / 文件 watch)
- 多租户配额 / 字节预算
- SNI 路由暴露
- 限速 / QoS 暴露
- Web UI

这些保留给 portunus-client 控制面;未来如有独立诉求,以 minor 版本演进。

## 10. Open Questions

无。所有决策已在 brainstorming 阶段对齐。

---

**Next step:** 经用户复核后,移交 `superpowers:writing-plans` skill 生成
`docs/superpowers/plans/2026-05-14-standalone-forwarder.md`(每个 Phase 拆成
bite-sized TDD 任务)。

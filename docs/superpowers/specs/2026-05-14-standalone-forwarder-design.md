# Standalone Forwarder — Design

**Status:** draft v2 · awaiting user review (v2 addresses 10 spec-review findings)
**Branch:** `feat/standalone-forwarder`
**Target release:** v1.5.0
**Date:** 2026-05-14

## 1. Goal

把当前 `portunus-client` 内的数据面(`forwarder/`, `resolver/`, `shutdown`)
抽到独立的 `portunus-forwarder` lib crate,在此之上交付一个新的
`portunus-standalone` 二进制——一份**不依赖** portunus-server 控制平面、
用 TOML 驱动的"小型服务器重量级转发器"(对标 frpc-stcp / gost 的本地子集)。

成功标准:
1. `portunus-client` 行为对调用者零变化(同 bundle、同重连、同 quota、同 Prometheus
   暴露、同 `StatsReport` 字节;现有 `*_wire_compat` 测试全绿)。
2. `portunus-standalone` 单一 binary,**Cargo 依赖图不含** tonic / tonic-prost /
   prost / portunus-proto / portunus-auth。
3. 数据面代码 100% 共享 —— 同一段 `proxy.rs / splice.rs / udp/` 被两个 binary 消费。
4. 性能门:`portunus-forwarder` 的两个 criterion baseline(`data_plane` → v0.1.0、
   `splice_throughput` → v1.2.0)不回退 >5%。

## 2. Audience & Use Case

目标用户:在单台 VPS / 容器内运行的轻量 TCP/UDP 转发器使用者。典型:
- SSH/DB 端口出墙转发
- 域名后端的反向 TCP 代理(DNS 解析 + 主动失败转移)
- UDP 游戏服务 / DNS 转发
- 后端是 nginx/haproxy,前置加 PROXY protocol 让后端拿到真实 client IP

**不在** v1.5 范围内(未来 minor 可议):
- TLS 终止
- SNI 路由(留给 portunus-client)
- 多用户配额 / QoS 限速
- 热重载
- 显式 bind 地址(永远 wildcard,见 §4.2.2)
- HTTP 管理面 / Prometheus 端点

## 3. Architecture

### 3.1 Crate 布局

```
crates/
├── portunus-proto/        (unchanged)
├── portunus-core/         + Protocol 类型新增(见 §3.5)
├── portunus-auth/         (unchanged)
├── portunus-forwarder/    新增 lib crate · 抽出的数据面 · proto-free
├── portunus-server/       内部 Protocol 改用 From<core::Protocol>(见 §3.5)
├── portunus-client/       slimmer · 依赖 portunus-forwarder + 保留 wire 翻译层
├── portunus-standalone/   新增 binary crate · 仅依赖 portunus-forwarder
└── portunus-e2e/          (unchanged)
```

### 3.2 `portunus-forwarder` 内部布局

```
src/
├── lib.rs                  // pub use re-exports
├── forwarder/              // 从 portunus-client/src/forwarder/ 整体平移
│   ├── mod.rs              //   ClientRule, RuleStatusEvent, run_forwarder
│   ├── proxy.rs            //   TCP 转发核心
│   ├── splice.rs           //   Linux splice(2)
│   ├── udp/                //   UDP 流表 + 转发
│   ├── range.rs            //   端口 range 原子绑定
│   ├── failover.rs         //   多目标
│   ├── failover_path.rs    //
│   ├── probe.rs            //   主动探测
│   ├── proxy_protocol.rs   //   PROXY v1/v2 prelude
│   ├── sni/                //   SNI listener(lib 内可用,standalone TOML 不暴露)
│   ├── quota/              //   QuotaHandle 原子计数(client 用)
│   ├── rate_limit/         //   per-rule 令牌桶 + 内部 snapshot 类型
│   │                       //   (drain_to_proto 移到 client,见 §3.4)
│   └── stats.rs            //   RuleStats 原子计数器 + StatsSink trait + snapshots
├── port_groups.rs          // SNI 端口聚合(client 用;snapshot_listener_stats
│                           // 返回 wire-neutral 类型,见 §3.4)
├── resolver/               // hickory-resolver 包装
└── shutdown.rs             // CancellationToken;signal_handler 留在 client/
                            // standalone 各自(见 §4.5、§5.6)
```

### 3.3 公共 API 表面 (`pub use` from lib.rs)

```rust
// 规则与生命周期
pub use forwarder::{
    ClientRule, MultiTarget, RuleStatusEvent, run_forwarder,
};

// 配额(client 实例化,standalone 永远不构造)
pub use forwarder::quota::QuotaHandle;

// Stats(wire-neutral 接缝)
pub use forwarder::stats::{
    StatsSink, StatsSnapshot, RuleStatsSnapshot,
    PerPortStatsSnapshot, PerTargetStatsSnapshot,
    RateLimitStatsSnapshot, OwnerRateLimitStatsSnapshot,
    SniListenerStatsSnapshot,
    RejectReason, ThrottleKind, TargetHealth,
    spawn_stats_reporter,
};

// SNI / RateLimit / PROXY 数据面入口(client 用; standalone TOML 不暴露)
pub use forwarder::sni::SniRouteResolver;
pub use forwarder::rate_limit::RateLimitScopeManager;
pub use forwarder::proxy_protocol::ProxyProtocolMode;

// Resolver
pub use resolver::{Resolve, LiveResolver};

// Shutdown 原语(signal handling 不放在 lib;见 §4.5)
pub use shutdown::Shutdown;

// PortGroupManager 是 client 的内部,不公开;
// 如果 client 仍需 import,改成 `portunus_forwarder::port_groups::PortGroupManager`
// (从 pub(crate) 升 pub)。若 standalone 不需要(平衡套餐不含 SNI),
// 也可以保留 pub(crate) 让 PortGroupManager 留在 client。Plan 阶段定。
```

**收窄说明**(对应 finding 8):
- v1 spec 列的 `SniRoute / SniRouteSpec / RateLimitSpec` 类型不存在,删掉。
- `GroupMember` / `rebuild_watches` 当前是 `pub(crate)`,v1 错误地列为 public — 删掉。
- 公共表面只保留**真实需要稳定**的入口:`ClientRule`、`run_forwarder`、wire-neutral
  snapshot 集、`Shutdown`、`Resolve` trait。其余按需在 plan 阶段调整。

### 3.4 控制面 ↔ 数据面接缝(forwarder 必须 proto-free)

**finding 2** 实测发现 forwarder 多处直接引用 `portunus_proto::v1::*`:
- `forwarder/mod.rs:37` `splice.rs:22` `proxy.rs:350` — 单纯用 `Protocol` 枚举
- `forwarder/rate_limit/stats.rs:19` `scope.rs:790/795/1516` — `drain_to_proto()`
  返回 `Vec<proto::v1::OwnerRateLimitStats>` 等 wire 构造函数
- `port_groups.rs:359-368` — `snapshot_listener_stats()` 返回
  `Vec<proto::v1::SniListenerStats>`

**整改方案**:forwarder crate 的 Cargo.toml **不依赖** `portunus-proto`。所有
wire 翻译函数(`drain_to_proto`、`snapshot_listener_stats` 等)从 forwarder
**删除**,在 forwarder 内改成返回 wire-neutral snapshot:

```rust
// portunus-forwarder/src/forwarder/rate_limit/stats.rs (重写)
impl RateLimitStatsAccumulator {
    pub fn drain(&self) -> Option<RateLimitStatsSnapshot> { /* ... */ }
}

// portunus-forwarder/src/forwarder/stats.rs (新增类型)
#[derive(Clone, Debug, Default)]
pub struct RateLimitStatsSnapshot {
    pub active_connections: u32,
    pub conns_rejected_concurrent: u64,
    pub conns_rejected_rate: u64,
    pub bytes_throttled: u64,
    pub bytes_throttled_in: u64,
    pub bytes_throttled_out: u64,
    pub reject_reasons: Vec<(RejectReason, u64)>,
    // 字段集合与现有 proto::RuleRateLimitStats 一对一;Plan 阶段对齐
}
```

```rust
// portunus-client/src/control.rs (新增 wire 翻译层)
impl From<RateLimitStatsSnapshot> for proto::v1::RuleRateLimitStats { ... }
impl From<SniListenerStatsSnapshot> for proto::v1::SniListenerStats { ... }
impl From<PerTargetStatsSnapshot> for proto::v1::PerTargetStats { ... }
// ...
```

接缝总览:

| 接缝 | 类型 | client | standalone |
|------|------|--------|------------|
| 规则输入 | `ClientRule` 值 | gRPC `Rule` → ClientRule | TOML → ClientRule |
| 生命周期 | `run_forwarder(...)` | 同 | 同 |
| Stats 输出 | `Arc<dyn StatsSink>` | `GrpcStatsSink`(wire-neutral snapshot → `From` → `StatsReport` → bidi) | `LoggingStatsSink`(snapshot → `tracing::info!`) |
| 配额 | `ClientRule.quota: Option<Arc<QuotaHandle>>` | `QuotaScopeManager` 构造并替换 | 永远 `None`(短路) |

### 3.5 `Protocol` 类型上提(finding 9)

当前 3 个独立 `Protocol` 枚举:
- `portunus_proto::v1::Protocol`(gRPC wire,proto3 i32)
- `portunus_server::rules::Protocol`(server 内部 + JSON persist,serde lowercase)
- 各 forwarder 模块直接消费上面的 proto enum

**整改方案**:在 `portunus-core` 新增**权威** `Protocol` 枚举(serde lowercase
+ Display + FromStr),其他三处都改成 `From<core::Protocol> for X` /
`From<X> for core::Protocol` 双向转换:

```rust
// portunus-core/src/protocol.rs (新)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Protocol { Tcp, Udp }

// portunus-proto: From + Into;wire i32 不变
// portunus-server/src/rules.rs: 删本地 Protocol;use portunus_core::Protocol
// portunus-forwarder: use portunus_core::Protocol(替换所有 portunus_proto 引用)
// portunus-client: gRPC 边界用 portunus_proto 的 i32,内部转 core::Protocol
```

**工作量评估**(从 v1 spec 校准):比 v1 估的"proto + 双向 From"多一层:
还要改 server 的 `rules.rs` + `rules_v0.4_*.rs`(JSON 持久化)+ server 内部
凡是 match Protocol 的地方。仍属于 Phase 1 范围,但不再是"双向 From 一行字"
那么轻量。**预期 ~20 个文件 touch,纯类型重定向,运行时行为不变,wire 不变**。

## 4. `portunus-standalone` 详细设计

### 4.1 CLI

```
portunus-standalone [--config <PATH>] [--check] [--log-level LVL] [--log-format FMT]

  -c, --config <PATH>    TOML 配置文件路径(可选)
                         默认搜索顺序:
                           1. $PORTUNUS_STANDALONE_CONFIG
                           2. ./standalone.toml
                           3. /etc/portunus/standalone.toml
                         全部不存在 → exit 2
      --check            仅校验配置后退出,不绑定端口(exit 0=合法 / 2=非法)
      --log-level <LVL>  覆盖 [global].log_level (off|error|warn|info|debug|trace)
      --log-format <FMT> 覆盖 [global].log_format (json|pretty)
  -V, --version
  -h, --help
```

**finding 10 修订**:`--config` 是**可选**字段(短选项 `-c`),有默认搜索路径,
不冲突。同时如全部搜索失败给出明确提示("looked at: A, B, C")。

### 4.2 TOML schema

```toml
[global]
log_level            = "info"
log_format           = "json"
shutdown_drain_secs  = 30

[defaults]
udp_max_flows        = 1024
udp_flow_idle_secs   = 60
prefer_ipv6          = false

# 最小 TCP 转发(单目标 · 单端口)
[[rule]]
name         = "ssh-tunnel"
protocol     = "tcp"
listen_port  = 2222               # u16
target       = "10.0.0.5:22"      # 单目标 desugar 成 1-element targets

# 端口 range
[[rule]]
name         = "web-range"
protocol     = "tcp"
listen_ports = "8000-8009"        # 字符串 "lo-hi"
target       = "10.0.0.10:8000-8009"

# 域名后端
[[rule]]
name         = "https-by-name"
protocol     = "tcp"
listen_port  = 443
target       = "backend.internal:443"
prefer_ipv6  = true

# 多目标 failover + PROXY protocol(rule-level,所有 targets 同值)
[[rule]]
name           = "ha-https"
protocol       = "tcp"
listen_port    = 8443
targets        = [
  { host = "primary.internal",   port = 443, priority = 0  },
  { host = "secondary.internal", port = 443, priority = 10 },
]
probe_interval_secs = 5
proxy_protocol      = "v2"        # "off" | "v1" | "v2";rule-level,应用到全部 targets

# UDP
[[rule]]
name               = "game-udp"
protocol           = "udp"
listen_port        = 27015
target             = "10.0.0.20:27015"
udp_max_flows      = 4096
udp_flow_idle_secs = 120
```

#### 4.2.1 字段语义(每一条都对照 ClientRule 字段)

| TOML 字段 | 类型 | → ClientRule | 校验 |
|-----------|------|--------------|------|
| `name` | string,workspace 内唯一 | 派生 `RuleId`(见 §4.2.3) | 必填,非空 |
| `protocol` | "tcp" \| "udp" | `portunus_core::Protocol` | 必填 |
| `listen_port` | u16 | `listen_range = PortRange::single(p)` | 与 `listen_ports` 互斥 |
| `listen_ports` | "lo-hi" | `listen_range = PortRange::new(lo,hi)` | lo<=hi;与 `listen_port` 互斥 |
| `target` | "host:port" 或 "host:lo-hi" | desugar 成 1-element `targets`(见 §4.2.4) | 与 `targets` 互斥 |
| `targets` | TOML array of `{host,port,priority}` | `rule.targets` 直填 | 与 `target` 互斥;`targets` + range 不允许 |
| `prefer_ipv6` | bool,默认 [defaults] | `rule.prefer_ipv6` | 仅 TCP 生效 |
| `probe_interval_secs` | u32?,默认 None | `rule.probe_interval_secs` | 仅 TCP 多目标 |
| `proxy_protocol` | "off"\|"v1"\|"v2",默认 "off" | 每个 target 的 `proxy_protocol` 字段都拷贝此值(rule-level apply-all,见 §4.2.5) | 仅 TCP |
| `udp_max_flows` | u32,默认 [defaults] | `rule.udp_max_flows` | 仅 UDP |
| `udp_flow_idle_secs` | u32,默认 [defaults] | `rule.udp_flow_idle_secs` | 仅 UDP |

`#[serde(deny_unknown_fields)]` 在所有 sections 上 —— 拼错字段或非平衡套餐
字段(sni_routes / rate_limit / quota / bandwidth_* / listen_addr / bind_addr)
立即报错,错误信息提示"该特性需 portunus-client 控制平面 v1.5"。

#### 4.2.2 listen 地址绑定(finding 4)

**v1.5 结论**:`listen_port` / `listen_ports` 是**纯端口**,**永远 wildcard 双栈
bind**(IPv4 + IPv6,与现状 `range.rs::bind_port` 一致)。**不**接受 `listen_addr`
或 `bind_addr` 字段;deny_unknown_fields 会拒绝。

**未来路径**(v1.6+ 议程,**不**进 v1.5):新增 `ListenEndpoint { addr: IpAddr, range: PortRange }`,改 TCP `bind_port`、UDP `run_listener`、SNI listener 三处。
client 默认仍 wildcard(对调用者零变化),standalone TOML 通过 `listen_addr` 字段
opt-in。这是真实数据面改造,scope creep,不在 v1.5。

#### 4.2.3 `RuleId` 派生(finding 1)

`RuleId(pub u64)` 是 newtype。standalone 派生方式:

```rust
// portunus-standalone/src/config.rs
use std::collections::HashMap;
use twox_hash::xxh3::Hash64;        // 新增 workspace dep: twox-hash

fn derive_rule_id(name: &str) -> RuleId {
    RuleId(Hash64::hash(name.as_bytes()))
}

/// 启动期冲突检测;若 hash 撞了(理论 2^-32 概率,但用户起两条相同 name
/// 会必撞)直接 exit 2。
fn build_registry(rules: &[ParsedRule]) -> Result<HashMap<RuleId, String>, ConfigError> {
    let mut reg = HashMap::new();
    for r in rules {
        let id = derive_rule_id(&r.name);
        if let Some(prev) = reg.insert(id, r.name.clone()) {
            return Err(ConfigError::RuleIdCollision { prev, current: r.name.clone(), id });
        }
    }
    Ok(reg)
}
```

`name` 唯一性已经在 TOML 层显式检测(发现重名直接拒);xxh3 冲突检测是 belt &
suspenders。Registry 同时驱动**日志字段 `rule_name=`**——所有 standalone-emitted
日志(stats / reject / failure)在 RuleId 之外**强制带 `rule_name`**,人类可读。

```rust
// LoggingStatsSink.drain():
for (rule_id, rs) in snap.per_rule {
    let name = registry.get(&rule_id).map(String::as_str).unwrap_or("?");
    info!(event="standalone.stats", rule=%rule_id, rule_name=name,
          in_bytes=rs.bytes_in, ...);
}
```

#### 4.2.4 单目标 desugar(finding 5)

当前数据面只有 `RuleTarget.proxy_protocol`(per-target)。standalone 单目标
case 解析时**统一构造 1-element `targets` 数组**:

```rust
// Config → Vec<ClientRule> 转换
match (parsed.target, parsed.targets) {
    (Some(s), None) => {
        let (host, port) = parse_host_port(&s)?;
        vec![MultiTarget {
            spec: TargetSpec { host, port, priority: 0,
                               proxy_protocol: parsed.proxy_protocol },
            target: classify_target(&host)?,
        }]
    }
    (None, Some(ts)) => {
        ts.into_iter().map(|t| MultiTarget {
            spec: TargetSpec { host: t.host, port: t.port, priority: t.priority,
                               proxy_protocol: parsed.proxy_protocol /* apply-all */ },
            target: classify_target(&t.host)?,
        }).collect()
    }
    _ => return Err(ConfigError::TargetExclusivity),
}
```

#### 4.2.5 PROXY protocol 语义(finding 5)

- **v1.5 schema 仅 rule-level** `proxy_protocol`:解析时拷贝到每个 target 的
  `proxy_protocol` 字段(apply-all)。
- **不支持 per-target 异构 PROXY 模式**(简化 schema;若用户需要请用
  portunus-client 控制面)。文档明示。
- 内部数据面仍是 per-target(`failover_path.rs:554` 不变),只是 standalone
  解析层永远写成"全部相同"。

#### 4.2.6 启动期错误格式(finding 10)

```
error: rule "web-range": listen range size (10) does not match target range size (20)
  at /etc/portunus/standalone.toml [rule]#1
```

**finding 10 修订**:不再承诺精确 line number。`toml::de::Error::span()` 在
`toml = "0.8"` 起有 `Span { start, end }`,但不可靠(`#[serde(flatten)]` 等
情况下丢)。措辞改成**"best-effort 定位"**:有 span 就给文件:行:列,没有
就给 `[rule]#N` 索引 + 字段路径。**永远不 panic**;永远给用户足够定位线索。

非零 exit code:
- `2` 配置错误(语法 / 语义 / RuleId 冲突 / 搜索路径全失败)
- `1` 启动期 bind 失败 / 运行期 fatal
- `0` 正常关停

### 4.3 启动门 + 运行期失败级联(finding 6)

```rust
async fn run(cfg: Config) -> ExitCode {
    let shutdown = Shutdown::new();
    let registry = cfg.rule_registry();                       // HashMap<RuleId, String>

    // 信号 handler 留在 standalone(SIGHUP no-op,见 §4.5)
    tokio::spawn(standalone_signal_handler(shutdown.clone()));

    let resolver = Arc::new(LiveResolver::from_env());
    let stats_sink: Arc<dyn StatsSink> = Arc::new(LoggingStatsSink::new(
        Duration::from_secs(60),
        registry.clone(),
    ));
    let (status_tx, mut status_rx) = mpsc::channel(64);
    let (fatal_tx, mut fatal_rx) = mpsc::channel::<()>(1);     // 运行期 fatal 通道

    let mut joinset = JoinSet::new();
    let expected_activations: HashSet<RuleId> = registry.keys().copied().collect();
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

    // --- 启动门 ---
    let mut pending = expected_activations.clone();
    let mut startup_failures: Vec<(RuleId, String)> = Vec::new();
    while !pending.is_empty() {
        match status_rx.recv().await {
            Some(RuleStatusEvent::Activated { rule_id }) => { pending.remove(&rule_id); }
            Some(RuleStatusEvent::Failed { rule_id, reason }) => {
                pending.remove(&rule_id);
                startup_failures.push((rule_id, reason));
            }
            Some(RuleStatusEvent::Removed { rule_id }) => {
                // 不应在启动期出现,记录后继续等
                warn!(event="standalone.unexpected_removed", %rule_id);
                pending.remove(&rule_id);
            }
            None => break,
        }
    }
    if !startup_failures.is_empty() {
        eprintln!("error: {} rule(s) failed to bind:", startup_failures.len());
        for (id, why) in &startup_failures {
            let name = registry.get(id).map(String::as_str).unwrap_or("?");
            eprintln!("  - {name} ({id}): {why}");
        }
        shutdown.trigger();
        while joinset.join_next().await.is_some() {}
        return ExitCode::from(1);
    }

    // --- 运行态 status 转发(运行期 Failed → fatal_tx) ---
    let reg_clone = registry.clone();
    let fatal_tx_clone = fatal_tx.clone();
    tokio::spawn(async move {
        while let Some(ev) = status_rx.recv().await {
            match ev {
                RuleStatusEvent::Failed { rule_id, reason } => {
                    let name = reg_clone.get(&rule_id).map(String::as_str).unwrap_or("?");
                    error!(event="rule.failed", %rule_id, rule_name=name, %reason);
                    let _ = fatal_tx_clone.try_send(());
                }
                RuleStatusEvent::Removed { rule_id } => {
                    let name = reg_clone.get(&rule_id).map(String::as_str).unwrap_or("?");
                    info!(event="rule.removed", %rule_id, rule_name=name);
                }
                RuleStatusEvent::Activated { rule_id } => {
                    let name = reg_clone.get(&rule_id).map(String::as_str).unwrap_or("?");
                    info!(event="rule.reactivated", %rule_id, rule_name=name);
                }
            }
        }
    });
    drop(fatal_tx);                                          // 让 fatal_rx 在 senders 全 drop 后 close

    // --- 主循环:select { fatal | joinset } ---
    let exit_code;
    loop {
        tokio::select! {
            _ = fatal_rx.recv() => {
                error!(event="standalone.fatal_shutdown");
                shutdown.trigger();
                while let Some(res) = joinset.join_next().await {
                    if let Err(e) = res {
                        error!(event="standalone.task_panic", error=%e);
                    }
                }
                exit_code = ExitCode::from(1);
                break;
            }
            join = joinset.join_next() => {
                match join {
                    Some(Err(e)) => {
                        error!(event="standalone.task_panic", error=%e);
                        shutdown.trigger();
                        // 继续 drain 其他任务
                    }
                    Some(Ok(_)) | None if joinset.is_empty() => {
                        // 所有 forwarder 自然退出(因为 shutdown.trigger())
                        exit_code = if shutdown.token().is_cancelled() { ExitCode::SUCCESS } else { ExitCode::from(1) };
                        break;
                    }
                    Some(Ok(_)) => continue,
                    None => { exit_code = ExitCode::SUCCESS; break; }
                }
            }
        }
    }

    info!(event = "standalone.stopped");
    exit_code
}
```

**finding 6 修订要点**:
- 加 `(fatal_tx, fatal_rx)` 通道。运行期 `Failed` 事件触发 `fatal_tx.try_send()`,
  主 loop `tokio::select!` 捕获后 `shutdown.trigger()` + drain。
- 处理 `JoinError`(任务 panic)— 也算 fatal。
- 退出码:正常信号 → 0;fatal / panic → 1。

### 4.4 `LoggingStatsSink` 行为

每 60s 调用 `drain(snapshot)`,把每条规则的核心计数器打 `info!`,reject /
throttle 即时 `warn!`。**所有日志强制带 `rule_name`**(见 §4.2.3)。

```jsonl
{"ts":"...","level":"INFO","event":"standalone.stats","rule":"12345...","rule_name":"ssh-tunnel","in_bytes":12345678,"out_bytes":98765432,"active_conns":3,"datagrams_in":0,"datagrams_out":0,"active_flows":0}
{"ts":"...","level":"WARN","event":"standalone.reject","rule":"67890...","rule_name":"ha-https","reason":"target_unavailable"}
```

字段未覆盖 `StatsReport` 全部字段(SNI / per-target / per-port / rate-limit /
quota — 平衡套餐根本不会启用)。如未来 v1.6+ 启用某项,LoggingStatsSink 加字段即可,
trait 不动。

### 4.5 信号处理(finding 7)

**不复用** `portunus_forwarder::shutdown::Shutdown::signal_handler`(该方法
仅处理 SIGINT/SIGTERM,改它会动 client 行为)。standalone 自实现:

```rust
// portunus-standalone/src/signal.rs
#[cfg(unix)]
pub async fn standalone_signal_handler(shutdown: Shutdown) {
    use tokio::signal::unix::{SignalKind, signal};
    let mut sigint  = signal(SignalKind::interrupt()).expect("install SIGINT handler");
    let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    let mut sighup  = signal(SignalKind::hangup()).expect("install SIGHUP handler");
    // SIGHUP 永远只 recv 但不 trigger(否则进程会被默认 terminate)
    loop {
        tokio::select! {
            _ = sigint.recv()  => { tracing::info!(event="shutdown.signal", signal="SIGINT");  shutdown.trigger(); return; }
            _ = sigterm.recv() => { tracing::info!(event="shutdown.signal", signal="SIGTERM"); shutdown.trigger(); return; }
            _ = sighup.recv()  => { tracing::info!(event="standalone.sighup_ignored"); /* loop continues */ }
        }
    }
}
```

client 侧的 `Shutdown::signal_handler` 不动。

### 4.6 资源限制
- 启动 log `getrlimit(RLIMIT_NOFILE)`,如果 < 4096 加 warn。
- 不主动 setrlimit;依赖 systemd unit / docker --ulimit。

## 5. `portunus-client` 侧改动

行为零变化。分四档:

### 5.1 机械化(脚本可改)
- `git mv` forwarder/ resolver/ port_groups.rs shutdown.rs → portunus-forwarder/src/
- 全局替换 `use crate::forwarder::` → `use portunus_forwarder::`(等)
- `portunus-client/Cargo.toml`:删 `nix / hickory-resolver / async-trait / tokio-rustls / rustls / tokio-stream`(若仍直接用,保留),加 `portunus-forwarder = { workspace = true }`
- benches `data_plane / range_install / dns_resolver / udp_data_plane / sni_route` 跟源迁到 `portunus-forwarder/benches/`

### 5.2 StatsSink trait + wire-neutral snapshot(finding 3)

**完整字段集**(对照现行 `proto::v1::RuleStats` + `StatsReport`):

```rust
// portunus-forwarder/src/forwarder/stats.rs

#[derive(Clone, Debug, Default)]
pub struct RuleStatsSnapshot {
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub active_connections: u64,

    // per-port (range rules)
    pub per_port: Vec<PerPortStatsSnapshot>,

    // DNS (FR-008)
    pub dns_failures: u64,

    // UDP (v0.4)
    pub datagrams_in: u64,
    pub datagrams_out: u64,
    pub active_flows: u64,
    pub flows_dropped_overflow: u64,

    // multi-target (v0.7)
    pub target_failovers_total: u64,
    pub per_target: Vec<PerTargetStatsSnapshot>,

    // SNI (v0.9) - per-rule counters
    pub sni_route_exact_total: u64,
    pub sni_route_wildcard_total: u64,
    pub sni_route_fallback_total: u64,

    // rate limit (v0.11) - per-rule
    pub rate_limit: Option<RateLimitStatsSnapshot>,
}

#[derive(Clone, Debug, Default)]
pub struct PerPortStatsSnapshot {
    pub listen_port: u16,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub active_connections: u64,
    pub datagrams_in: u64,
    pub datagrams_out: u64,
}

#[derive(Clone, Debug, Default)]
pub struct PerTargetStatsSnapshot {
    pub index: u16,
    pub host: String,
    pub port: u16,
    pub priority: u32,
    pub health: TargetHealth,
    pub consecutive_failures: u32,
    pub last_failure_at_unix_ms: u64,
    pub last_success_at_unix_ms: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub connections_accepted: u64,
}

#[derive(Clone, Debug, Default)]
pub struct RateLimitStatsSnapshot {
    pub active_connections: u32,
    pub conns_rejected_concurrent: u64,
    pub conns_rejected_rate: u64,
    pub bytes_throttled_in: u64,
    pub bytes_throttled_out: u64,
    pub reject_breakdown: Vec<(RejectReason, u64)>,
    // 字段与 proto::v1::RuleRateLimitStats 一对一;Plan 阶段对齐
}

#[derive(Clone, Debug, Default)]
pub struct OwnerRateLimitStatsSnapshot {
    pub owner: String,
    pub active_connections: u32,
    pub conns_rejected: u64,
    pub bytes_throttled: u64,
    // 字段与 proto::v1::OwnerRateLimitStats 一对一
}

#[derive(Clone, Debug, Default)]
pub struct SniListenerStatsSnapshot {
    pub listen_port: u16,
    pub sni_miss_total: u64,
    pub sni_parse_failures_total: u64,
    // 与 proto::v1::SniListenerStats 一对一
}

#[derive(Clone, Debug)]
pub struct StatsSnapshot {
    pub interval: Duration,
    pub sent_at_unix_ms: u64,
    pub per_rule: Vec<(RuleId, RuleStatsSnapshot)>,
    pub sni_listeners: Vec<SniListenerStatsSnapshot>,
    pub owner_rate_limits: Vec<OwnerRateLimitStatsSnapshot>,
}

#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub enum TargetHealth { #[default] Unknown, Healthy, Unhealthy }

#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum RejectReason {
    OwnerConcurrent,
    OwnerRate,
    RuleConcurrent,
    RuleRate,
    QuotaExhausted,
    TargetUnavailable,
}

#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum ThrottleKind { Bandwidth, NewConnRate }

pub trait StatsSink: Send + Sync {
    fn drain(&self, snapshot: StatsSnapshot);
    fn record_reject(&self, rule: RuleId, reason: RejectReason);
    fn record_throttle(&self, rule: RuleId, kind: ThrottleKind);
}

pub fn spawn_stats_reporter(
    sink: Arc<dyn StatsSink>,
    rules: Arc<RwLock<HashMap<RuleId, Arc<RuleStats>>>>,
    sni_port_groups: Option<Arc<PortGroupManager>>,      // None for standalone
    owner_rate_limits: Option<Arc<OwnerRateLimitStatsRegistry>>,
    interval: Duration,
    cancel: CancellationToken,
) -> JoinHandle<()>;
```

**关键不变量(对应 finding 3 + 6.x wire compat)**:
- 数据面 hot path 仍只动原子计数器;trait 调用只在 ticker 任务内发生。
- forwarder 不知道 proto 类型;snapshot 字段集与 proto 一一对应,
  client 实现 `From<RuleStatsSnapshot> for proto::v1::RuleStats` 等。
- 现有 `*_wire_compat` 测试(`dns_wire_compat`、`udp_wire_compat`、
  `sni_wire_compat`、`rate_limit_wire_compat`、`007-multi-target` 类)
  全部继续跑,**StatsReport 字节相同**才允许合并。

### 5.3 `Protocol` 上提

见 §3.5。Phase 1 触动 ~20 个文件(proto + server/rules.rs + 各 JSON 持久化层
+ forwarder 内 use 替换 + client gRPC 边界 From),纯类型重定向。

### 5.4 QuotaScopeManager 不动
`QuotaScopeManager` 留在 `portunus-client/src/control.rs`,继续构造
`Arc<QuotaHandle>` 注入 `ClientRule.quota`。重连重放顺序不变。

### 5.5 测试归位
- forwarder 模块的 `#[cfg(test)] mod tests` 跟模块搬到 portunus-forwarder
- `portunus-client/tests/*` 集成测试改用 `MockStatsSink` 帮手
- 关键的 `*_wire_compat` 测试**留在 client**(它们验 proto wire 字节)
- `portunus-e2e/tests/traffic_quotas.rs` 等 e2e 不动

### 5.6 Shutdown 不被 standalone 污染
`portunus-forwarder::shutdown::Shutdown` 保持当前 SIGINT/SIGTERM 行为。
standalone 不调用其 `signal_handler`,而是自实现(§4.5)。

## 6. 测试策略

| 测试层 | 位置 | 覆盖 |
|--------|------|------|
| 数据面单元测试 | `portunus-forwarder/src/**/tests` | 跟模块迁入 |
| **wire 字节级回归** | `portunus-client/tests/*_wire_compat.rs` | 现有套件全留,验 `RuleStatsSnapshot → From → proto::RuleStats` 字节相同 |
| client 单元测试 | `portunus-client/tests/*` | reconcile / 重连重放 / Quota / GrpcStatsSink ↔ MockStatsSink |
| standalone config | `portunus-standalone/src/config.rs::tests` | TOML 解析 / 校验 / RuleId 冲突 / deny_unknown_fields |
| standalone 集成 | `portunus-standalone/tests/smoke.rs` | echo + assert_cmd loopback |
| E2E | `portunus-e2e/tests/standalone_*.rs` | 多规则 / failover / PROXY v2 |
| `--check` CI | `portunus-standalone/tests/check_mode.rs` | 至少 6 fixtures(3 合法 3 非法),exit 0/2 |
| bench 回归 | `portunus-forwarder/benches/*` | `data_plane` → v0.1.0 baseline;`splice_throughput` → v1.2.0 baseline |

**finding 10 修订**:bench baseline 口径明确:
- `data_plane` bench(v0.1.0 起):baseline = v0.1.0(CLAUDE.md 已定)
- `splice_throughput` microbench(v1.3 起):baseline = v1.2.0(CLAUDE.md 已定)
- "对比 v1.4.0"指 wire 字节回归测试的 git tag,**不是** bench baseline

## 7. 迁移分阶段

```
Phase 1 — 公共 crate 框架 + Protocol 上提 (PR 1)
  ├─ T1.1 创建 crates/portunus-forwarder 空壳 + workspace 注册
  ├─ T1.2 portunus-core 新增 Protocol 类型(serde lowercase)
  ├─ T1.3 proto 加 From/Into core::Protocol
  ├─ T1.4 server/rules.rs 删本地 Protocol,改用 core
  ├─ T1.5 server 各 match 处类型重定向
  ├─ T1.6 workspace build + test 通过(行为零变化)

Phase 2 — 平移数据面 + proto-free 净化 (PR 2)
  ├─ T2.1 git mv forwarder/ resolver/ port_groups.rs shutdown.rs
  ├─ T2.2 forwarder 内 portunus_proto 引用全部改成 core::Protocol
  ├─ T2.3 rate_limit/stats.rs 的 drain_to_proto 拆成 drain()→Snapshot(proto-free)
  ├─ T2.4 port_groups::snapshot_listener_stats 同上改成 snapshot()→Vec<SniListenerStatsSnapshot>
  ├─ T2.5 lib.rs 写 pub use re-exports(收窄到真实需要的)
  ├─ T2.6 portunus-client 加 From<Snapshot> for proto::* 翻译层
  ├─ T2.7 client use 路径全局替换 + Cargo.toml 调整
  ├─ T2.8 benches 跟源迁
  └─ 验证: workspace test + *_wire_compat 全绿(字节级回归)

Phase 3 — StatsSink trait 抽象 (PR 3)
  ├─ T3.1 引入 StatsSink trait + StatsSnapshot 完整字段集
  ├─ T3.2 从 control.rs:357 拆出 spawn_stats_reporter(进 forwarder lib)
  ├─ T3.3 control.rs 实现 GrpcStatsSink(snapshot → proto::StatsReport → mpsc)
  ├─ T3.4 MockStatsSink 测试帮手
  └─ 验证: e2e + 所有 wire compat 全绿 + splice bench 零回退

Phase 4 — portunus-standalone binary (PR 4)
  ├─ T4.1 crate 骨架 + Cargo.toml(无 proto/tonic/auth)+ bin target
  ├─ T4.2 config.rs · TOML schema + deny_unknown_fields + RuleId xxhash + 冲突检测
  ├─ T4.3 main.rs · clap CLI + 默认搜索路径
  ├─ T4.4 runtime.rs · 启动门 + fatal channel + signal handler(SIGHUP no-op)
  ├─ T4.5 stats_sink.rs · LoggingStatsSink(强制 rule_name 字段)
  ├─ T4.6 单元 + 集成测试(--check fixtures 至少 6 个)
  └─ 验证: `cargo run -p portunus-standalone -- --check tests/fixtures/full.toml`

Phase 5 — E2E + 文档 (PR 5)
  ├─ T5.1 portunus-e2e/tests/standalone_*.rs 3 场景
  ├─ T5.2 docs/operations/standalone.mdx(中英)
  ├─ T5.3 README.md 单机版段落 + 示例 TOML
  ├─ T5.4 CHANGELOG.md v1.5.0 草稿
  └─ T5.5 Makefile 加 standalone / standalone-check 目标
```

每 Phase 独立可 merge 的 PR;Phase 1-2 是行为零变化迁移,Phase 3 是行为敏感
(StatsSink trait),Phase 4-5 是纯新增。

## 8. 风险登记

| 风险 | 缓解 |
|------|------|
| Phase 2 forwarder 遗漏 `portunus_proto` 引用 | Cargo.toml 不含 portunus-proto,任何漏改直接编译失败 |
| Phase 3 StatsSink trait 引入数据面抖动 | bench 对比 v1.4.0;hot path 仍是原子计数器 |
| Phase 2 snapshot 字段集偏离 proto | client `From` 翻译 + `*_wire_compat` 测试字节比对 |
| Protocol 上提牵连 server JSON 持久化兼容 | rule JSON serde 仍 lowercase;wire bytes 不变 |
| RuleId xxhash 冲突 | 启动期 registry 冲突检测 + 友好错误 |
| standalone TOML 字段拼错 → 静默失活 | `deny_unknown_fields` + `--check` |
| 用户在 standalone TOML 写 SNI/quota 字段 | 解析器拒收并提示用 portunus-client |
| 不同信号策略干扰 client 测试 | standalone 自实现 signal handler;`Shutdown` 不动 |

## 9. Out of Scope (v1.5.0)

- TLS 终止 / mTLS
- 显式 bind 地址(永远 wildcard;v1.6+ 议程)
- HTTP 管理面 / Prometheus / `/metrics`
- 热重载(SIGHUP / 文件 watch)
- 多租户配额 / 字节预算
- SNI 路由暴露
- 限速 / QoS 暴露
- per-target 异构 PROXY 模式(用 portunus-client 控制面)
- Web UI

## 10. Resolved Questions (was Open in v1)

1. **RuleId 派生**:`xxh3_64(name)` + 启动期 registry 冲突检测 +
   日志强制带 `rule_name` 字段。详见 §4.2.3。
2. **forwarder proto-free 边界**:彻底搬离 —— forwarder Cargo.toml 不含
   portunus-proto;wire 翻译层(`drain_to_proto`、`snapshot_listener_stats`
   等)全部移到 client 的 `From<Snapshot> for proto::*` impl。详见 §3.4 / §5.2。
3. **listen 地址**:v1.5 schema 只接 port / port range,永远 wildcard 双栈
   bind。显式 bind 地址留给 v1.6+。详见 §4.2.2。

无未决问题。

---

**Next step:** 经用户复核后,移交 `superpowers:writing-plans` skill 生成
`docs/superpowers/plans/2026-05-14-standalone-forwarder.md`(每个 Phase 拆成
bite-sized TDD 任务)。

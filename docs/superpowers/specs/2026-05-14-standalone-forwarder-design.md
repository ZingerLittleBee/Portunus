# Standalone Forwarder — Design

**Status:** draft v6 · awaiting user review (v6 addresses 3 v5-review findings + 3 small fixes)
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
│   ├── sni/                //   SNI listener(client 通过 port_groups 调用)
│   ├── quota/              //   QuotaHandle 原子计数(client 用)
│   ├── rate_limit/         //   per-rule 令牌桶 + RateLimitStatsAccumulator
│   │                       //   drain() 返回 wire-neutral snapshot(见 §3.4)
│   └── stats.rs            //   RuleStats 原子计数器 + snapshot getters
├── resolver/               // hickory-resolver 包装
│                           // 新增 LiveResolver::with_system_defaults() (见 §5.6)
└── shutdown.rs             // CancellationToken;signal_handler 是 SIGINT/SIGTERM only
                            // standalone 自实现 SIGHUP-aware handler(见 §4.5)
```

**`port_groups.rs` 留在 portunus-client**(决议见 §10):它是控制面的 reconcile
辅助,把多条同端口 SNI 规则聚合成共享 listener。standalone 平衡套餐不暴露 SNI,
不需要这个聚合层。client 侧 import `portunus_forwarder::sni::{SniListener, ...}`
继续工作。

### 3.3 公共 API 表面 (`pub use` from lib.rs)

```rust
// 规则与生命周期
pub use forwarder::{
    ClientRule, MultiTarget, RuleStatusEvent, MultiTargetObservability,
    run_forwarder,
};

// 配额(client 实例化,standalone 永远不构造)
pub use forwarder::quota::QuotaHandle;

// Stats — wire-neutral snapshot 类型 + getter
// (无 StatsSink trait;每个 binary 拥有自己的 reporter,见 §5.2)
pub use forwarder::stats::{
    RuleStats,
    RuleStatsSnapshot, RuleStatsSnapshotBasic,                // v6 finding 3
    PerPortStatsSnapshot, PerTargetStatsSnapshot,
    RateLimitStatsSnapshot, OwnerRateLimitStatsSnapshot,
    SniListenerStatsSnapshot,
    RateLimitRejectReason, TargetHealth,
};

// SNI 数据面入口(client 的 port_groups 直接调用,standalone 不用)
//
// 当前真实路径:forwarder/sni/listener.rs 内 pub struct。
// Phase 2 将给 forwarder/sni/mod.rs 加 re-export(`pub use listener::*;`)
// 让 lib.rs 这条 re-export 编译通过 — 否则要写精确子路径
// `forwarder::sni::listener::SniListener` 等。
pub use forwarder::sni::{
    SniListener, SniListenerCounters, SniRouteResolver, SniRuleSlot,
};

// Rate limit 控制面对象(client 用)
//
// 当前真实路径:`scope::RateLimitScopeManager`、
// `scope::{OwnerRateLimitHandle, OwnerRateLimitStatsRegistry, RuleRateLimitHandle}`、
// `stats::RateLimitStatsAccumulator`。
// Phase 2 将给 rate_limit/mod.rs 加 `pub use scope::*; pub use stats::*;`
// 让顶层导出编译通过。
pub use forwarder::rate_limit::{
    RateLimitScopeManager,
    OwnerRateLimitHandle, OwnerRateLimitStatsRegistry,
    RateLimitStatsAccumulator,
    RuleRateLimitHandle,
};

// PROXY protocol 类型(真实名字 — finding 5)
pub use forwarder::proxy_protocol::ProxyProtocolPrelude;
// ProxyProtocolVersion 已经在 portunus-core(rule_target.rs),不重新导出

// Resolver
pub use resolver::{Resolve, LiveResolver, HickoryResolver};
// ResolverConfig 透传 (hickory_resolver crate)

// Shutdown 原语 — signal handling 不放在 lib(见 §4.5)
pub use shutdown::Shutdown;
```

**finding 8 整改要点**(v2 错的地方):
- ❌ `ProxyProtocolMode` → ✅ `ProxyProtocolPrelude` + `ProxyProtocolVersion`(后者已在 core)
- ❌ `SniRoute / SniRouteSpec` → ✅ `SniRouteResolver / SniRuleSlot / SniListener / SniListenerCounters`
- ❌ `RateLimitSpec` → ✅ `RateLimitScopeManager / RateLimitStatsAccumulator / RuleRateLimitHandle / OwnerRateLimitHandle`
- ❌ `PortGroupManager` 留待 plan → ✅ **不**进 lib;留在 client(决议见 §10)
- ❌ `GroupMember / rebuild_watches` → 不 pub(`pub(crate)` 在 client 内)

### 3.4 控制面 ↔ 数据面接缝(forwarder 必须 proto-free)

**finding 2** 实测:forwarder 多处直接引用 `portunus_proto::v1::*`:
- `forwarder/mod.rs:37` `splice.rs:22` `proxy.rs:350` — 单纯用 `Protocol` 枚举
- `forwarder/rate_limit/stats.rs:19` `scope.rs:790/795/1516` — `drain_to_proto()`
- `port_groups.rs:359-368` — `snapshot_listener_stats()`(port_groups 留在 client,
  但其调用的 `SniListenerCounters` 在 forwarder 内,counter 类型必须 proto-free)

**整改方案**:forwarder crate 的 Cargo.toml **不依赖** `portunus-proto`。所有
wire 构造函数(`drain_to_proto` 等)从 forwarder **删除**,改成返回 wire-neutral
snapshot:

```rust
// portunus-forwarder/src/forwarder/rate_limit/stats.rs (重写)
impl RateLimitStatsAccumulator {
    /// proto-free snapshot. Returns None for "empty" accumulators
    /// (所有字段都还是初始默认值)— wire-compat 保留 proto3
    /// default-stripping 语义。
    pub fn drain(&self) -> Option<RateLimitStatsSnapshot>;
}
```

```rust
// portunus-client/src/control.rs (新增 wire 翻译层)
impl From<RateLimitStatsSnapshot> for proto::v1::RateLimitStats { ... }
impl From<OwnerRateLimitStatsSnapshot> for proto::v1::OwnerRateLimitStats { ... }
impl From<RuleStatsSnapshot> for proto::v1::RuleStats { ... }
impl From<SniListenerStatsSnapshot> for proto::v1::SniListenerStats { ... }
impl From<PerTargetStatsSnapshot> for proto::v1::PerTargetStats { ... }
impl From<PerPortStatsSnapshot> for proto::v1::PerPortStats { ... }
```

接缝总览(**finding 2 修订:取消 StatsSink trait**):

| 接缝 | 类型 | client | standalone |
|------|------|--------|------------|
| 规则输入 | `ClientRule` 值 | gRPC `Rule` → ClientRule | TOML → ClientRule |
| 生命周期 | `run_forwarder(...)` | 同 | 同 |
| Stats 暴露 | snapshot getter (`RuleStats::snapshot_basic()` + `MultiTargetObservability::snapshot_per_target()` + `RateLimitStatsAccumulator::drain()`) | client `build_rule_stats_snapshot(rule_id, &slot)` 装配完整 snapshot,通过 `From<Snapshot>` 转 proto | standalone reporter 仅消费 `snapshot_basic()`,直接 `tracing::info!` |
| 配额 | `ClientRule.quota: Option<Arc<QuotaHandle>>` | `QuotaScopeManager` 构造并替换 | 永远 `None`(短路) |

每个 binary 自己写 reporter(无共享 `spawn_stats_reporter` API)—— 因为 client
的 reporter 需要 RuleSlot 周边状态(`is_range`、`multi_target_obs`、`targets_view`、
`rate_limit_limiter`、`rate_limit_stats` accumulator),不是单靠 `Arc<RuleStats>`
就能生成的(finding 2)。把这部分逻辑硬塞进 lib 会引入"forwarder 必须知道 RuleSlot"
的反向依赖,反而比保留在 client 更糟。

### 3.5 `Protocol` 类型上提(finding 9 — v2 已修)

当前 3 个独立 `Protocol` 枚举:
- `portunus_proto::v1::Protocol`(gRPC wire,proto3 i32)
- `portunus_server::rules::Protocol`(server 内部 + JSON persist,serde lowercase)
- 各 forwarder 模块直接消费上面的 proto enum

**整改方案**:在 `portunus-core` 新增**权威** `Protocol` 枚举(serde lowercase
+ Display + FromStr),其他三处都改成 `From<core::Protocol> for X` /
`From<X> for core::Protocol` 双向转换。

**工作量评估**:涉及 ~20 个文件 touch(proto + server/rules.rs + JSON 持久化
+ forwarder 内 use 替换 + client gRPC 边界 From),纯类型重定向,运行时行为
不变,wire 不变。

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

# 最小 TCP 转发(单目标 · 单端口 · 无 PROXY → fast path)
[[rule]]
name         = "ssh-tunnel"
protocol     = "tcp"
listen_port  = 2222
target       = "10.0.0.5:22"

# 端口 range(单目标)
[[rule]]
name         = "web-range"
protocol     = "tcp"
listen_ports = "8000-8009"
target       = "10.0.0.10:8000-8009"

# 域名后端
[[rule]]
name         = "https-by-name"
protocol     = "tcp"
listen_port  = 443
target       = "backend.internal:443"
prefer_ipv6  = true

# 多目标 failover + PROXY protocol
[[rule]]
name           = "ha-https"
protocol       = "tcp"
listen_port    = 8443
targets        = [
  { host = "primary.internal",   port = 443, priority = 0  },
  { host = "secondary.internal", port = 443, priority = 10 },
]
health_check_interval_secs = 5
proxy_protocol      = "v2"        # rule-level apply-all

# UDP
[[rule]]
name               = "game-udp"
protocol           = "udp"
listen_port        = 27015
target             = "10.0.0.20:27015"
udp_max_flows      = 4096
udp_flow_idle_secs = 120
```

#### 4.2.1 字段语义

| TOML 字段 | 类型 | → ClientRule | 校验 |
|-----------|------|--------------|------|
| `name` | string,workspace 内唯一 | 派生 `RuleId`(§4.2.3) | 必填,非空 |
| `protocol` | "tcp" \| "udp" | `portunus_core::Protocol` | 必填 |
| `listen_port` | u16 | `listen_range = PortRange::single(p)` | 与 `listen_ports` 互斥 |
| `listen_ports` | "lo-hi" | `listen_range = PortRange::new(lo,hi)` | lo<=hi;与 `listen_port` 互斥 |
| `target` | "host:port" 或 "host:lo-hi" | 见 §4.2.4 条件 desugar | 与 `targets` 互斥 |
| `targets` | array of `{host,port,priority}` | `rule.targets` 直填 | 与 `target` 互斥;targets + range 不允许 |
| `prefer_ipv6` | bool | `rule.prefer_ipv6` | 仅 TCP 生效 |
| `health_check_interval_secs` | u32? | `rule.health_check_interval_secs` | 仅 TCP 多目标 |
| `proxy_protocol` | "off"\|"v1"\|"v2",默认 "off" | 每个 target 的 `proxy_protocol`(apply-all) | 仅 TCP;§4.2.5 |
| `udp_max_flows` | u32 | `rule.udp_max_flows` | 仅 UDP |
| `udp_flow_idle_secs` | u32 | `rule.udp_flow_idle_secs` | 仅 UDP |

`#[serde(deny_unknown_fields)]` 在所有 sections。拼错字段或非平衡套餐字段
(sni_routes / rate_limit / quota / bandwidth_* / listen_addr / bind_addr)
立即报错。

**v6 finding 2:`[[rule]]` 至少 1 条**。空 TOML(或 0 条 rule)在 `Config::load`
校验阶段直接 `exit 2`,错误信息 `error: configuration must define at least
one [[rule]]`。这是规则:不允许"启动了一个什么都不转发的进程"。

#### 4.2.2 listen 地址绑定

v1.5 结论:`listen_port` / `listen_ports` 是**纯端口**,**永远 wildcard 双栈
bind**(IPv4 + IPv6,与现状 `range.rs::bind_port` 一致)。不接受 `listen_addr`
或 `bind_addr` 字段。显式 bind 地址留给 v1.6+(`ListenEndpoint { addr, range }`
+ 改 TCP/UDP/SNI bind 路径)。

#### 4.2.3 `RuleId` 派生(finding 1 + 7)

`RuleId(pub u64)` 是 u64 newtype。standalone 派生方式 — **用 workspace 已有
`blake3`**(在 `portunus-core/src/fingerprint.rs` 已经使用,无需新增依赖):

```rust
// portunus-standalone/src/config.rs
fn derive_rule_id(name: &str) -> RuleId {
    // blake3::hash 返回 [u8; 32] —— 用 copy_from_slice 静态保证前 8 字节存在,
    // 无 panic 路径(v6 small fix:不再用 try_into().expect(...))
    let h = blake3::hash(name.as_bytes());
    let mut arr = [0u8; 8];
    arr.copy_from_slice(&h.as_bytes()[..8]);
    RuleId(u64::from_le_bytes(arr))
}

/// 启动期 registry 冲突检测。Hash 冲突理论上极罕见
/// (任意一对 64-bit hash 撞概率 = 2^-64;生日界:N 条规则发生任意一对碰撞
/// 概率 ≈ N² / 2^65 — 对 N=10^6 仍然小于 10^-8),但用户起两条相同
/// name 必定冲突,所以同时充当**重名检测器**。
fn build_registry(rules: &[ParsedRule]) -> Result<HashMap<RuleId, String>, ConfigError> {
    let mut reg = HashMap::new();
    let mut by_name = HashSet::new();
    for r in rules {
        if !by_name.insert(r.name.clone()) {
            return Err(ConfigError::DuplicateName(r.name.clone()));
        }
        let id = derive_rule_id(&r.name);
        if let Some(prev) = reg.insert(id, r.name.clone()) {
            return Err(ConfigError::RuleIdCollision { prev, current: r.name.clone(), id });
        }
    }
    Ok(reg)
}
```

`name` 重复显式拒收(`DuplicateName`);hash 撞库(几乎不可能)用 `RuleIdCollision`
错误提示用户改名。Registry 同时驱动**日志字段 `rule_name=`**——所有 standalone
日志在 RuleId 之外**强制带 `rule_name`**:

```rust
// standalone reporter 内(v6 finding small fix:snapshot → snapshot_basic):
for (rule_id, rs) in registry.iter() {
    let snap = rs.snapshot_basic();
    info!(event="standalone.stats", rule=%rule_id, rule_name=%name, ...);
}
```

#### 4.2.4 单目标条件 desugar(finding 3)

当前 `forwarder/mod.rs:202` 用 `rule.targets.is_empty()` 作为 fast-path 判
据 —— 无脑 desugar 会把单目标也送进 `failover_path::run_tcp`,**改变字节
路径,失去 v0.6 单目标 fast path**(虽然 splice 在 failover_path 内仍然可用,
但 dial 状态机和重连计数器会被启用)。

**条件 desugar**:

| 输入 | `proxy_protocol` | desugar | 数据面路径 |
|------|-----------------|---------|------------|
| `target = "h:p"` | `"off"` 或缺省 | `targets = []` | v0.6 fast path(不变) |
| `target = "h:p"` | `"v1"` 或 `"v2"` | `targets = [{h,p,priority:0,proxy_protocol:V}]` | failover_path(带 1 元素) |
| `targets = [...]` | 任意 | targets 原样 + apply-all | failover_path |

文档明示:**对 `target = "h:p"` 单目标规则启用 PROXY protocol,
等同于把它升级为 1 元素 `targets` 列表,该规则会走 failover 数据路径
(行为与多目标一致;吞吐和延迟有轻微开销)。如不能接受,改写为多目标 TOML。**

#### 4.2.5 PROXY protocol 语义

- **v1.5 schema 仅 rule-level** `proxy_protocol`:解析时拷贝到每个 target 的
  `proxy_protocol` 字段(apply-all)。
- **不支持 per-target 异构 PROXY 模式**(简化 schema)。
- 数据面 (`failover_path.rs:554`) 仍 per-target,只是解析层永远写成"全部相同"。

#### 4.2.6 启动期错误格式

```
error: rule "web-range": listen range size (10) does not match target range size (20)
  at /etc/portunus/standalone.toml [rule]#1
```

`toml::de::Error::span()` 在 `toml = "0.8+"` 起有 `Span { start, end }`,但
`#[serde(flatten)]` 等情况下会丢。措辞 = **best-effort 定位**:有 span 就给
file:line:col,没有就给 `[rule]#N` 索引 + 字段路径。永远不 panic。

非零 exit code:
- `2` 配置错误(语法 / 语义 / RuleId 冲突 / 重名 / 搜索路径全失败)
- `1` 启动期 bind 失败 / 运行期 fatal
- `0` 正常关停

### 4.3 启动门 + 运行期失败级联(finding 4)

```rust
async fn run(cfg: Config) -> ExitCode {
    let shutdown = Shutdown::new();
    let registry = cfg.rule_registry();                       // Arc<HashMap<RuleId, String>>

    // SIGHUP-aware signal handler — install 失败直接 exit 1(不 panic)
    let signal_task = match install_standalone_signal_handler(shutdown.clone()) {
        Ok(j) => j,
        Err(e) => {
            error!(event = "standalone.signal_install_failed", error = %e);
            return ExitCode::from(1);
        }
    };

    let resolver = match LiveResolver::with_system_defaults() {        // §5.6
        Ok(r) => Arc::new(r),
        Err(e) => {
            error!(event = "standalone.resolver_init_failed", error = %e);
            return ExitCode::from(1);
        }
    };

    let (status_tx, mut status_rx) = mpsc::channel(64);
    let (fatal_tx, mut fatal_rx) = mpsc::channel::<()>(1);

    // finding 3 修订(v3):不动 run_forwarder 签名(stats: Arc<RuleStats> 必填)。
    // 在循环里就地构造 Arc<RuleStats>,登记到 registry,再传给 forwarder。
    let rule_stats_handles: Arc<RwLock<HashMap<RuleId, Arc<RuleStats>>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let reporter = spawn_standalone_reporter(
        Arc::clone(&rule_stats_handles),
        Arc::clone(&registry),
        Duration::from_secs(60),
        shutdown.token(),
    );

    let mut joinset = JoinSet::new();
    let expected: HashSet<RuleId> = registry.keys().copied().collect();
    for parsed in cfg.into_iter_rules() {                              // ParsedRule
        let rule_id = parsed.rule_id;
        let rule: ClientRule = parsed.into_client_rule();              // 现有 schema

        // v5 finding 3 修订:生产构造是 RuleStats::for_range(range) -> Arc<Self>,
        // 不是 RuleStats::new()(后者 #[cfg(test)] 且无参)。已经返回 Arc,不再包一层。
        let stats: Arc<RuleStats> = RuleStats::for_range(rule.listen_range);

        // v5 small fix:统一错误处理 — 锁中毒不 expect,记录后跳过该规则。
        // 实际上 stats_handles 是 standalone 独有,只有 reporter 读、main 写,
        // 中毒只可能因 reporter spawn 内 panic — 这种情况下跳过本条规则比 panic 友好。
        match rule_stats_handles.write() {
            Ok(mut guard) => { guard.insert(rule_id, Arc::clone(&stats)); }
            Err(e) => {
                error!(event = "standalone.stats_registry_poisoned",
                       %rule_id, error = %e,
                       "skipping rule registration; reporter will miss its stats");
                // 继续 spawn forwarder — 规则仍工作,只是不出 stats 日志
            }
        }

        joinset.spawn(run_forwarder(
            rule,
            resolver.clone(),
            status_tx.clone(),
            shutdown.token(),
            Duration::from_secs(cfg.global.shutdown_drain_secs),
            stats,                                                     // 现签名要求
        ));
    }
    drop(status_tx);

    // --- 启动门 ---
    let mut pending = expected.clone();
    let mut startup_failures: Vec<(RuleId, String)> = Vec::new();
    while !pending.is_empty() {
        match status_rx.recv().await {
            Some(RuleStatusEvent::Activated { rule_id }) => { pending.remove(&rule_id); }
            Some(RuleStatusEvent::Failed { rule_id, reason }) => {
                pending.remove(&rule_id);
                startup_failures.push((rule_id, reason));
            }
            Some(RuleStatusEvent::Removed { rule_id }) => {
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
        let _ = reporter.await;
        return ExitCode::from(1);
    }

    // --- 运行态 status 转发 ---
    let reg_clone = Arc::clone(&registry);
    let fatal_tx_clone = fatal_tx.clone();
    tokio::spawn(async move {
        while let Some(ev) = status_rx.recv().await {
            match ev {
                RuleStatusEvent::Failed { rule_id, reason } => {
                    let name = reg_clone.get(&rule_id).map(String::as_str).unwrap_or("?");
                    error!(event="rule.failed", %rule_id, rule_name=%name, %reason);
                    let _ = fatal_tx_clone.try_send(());
                }
                RuleStatusEvent::Removed { rule_id } => {
                    let name = reg_clone.get(&rule_id).map(String::as_str).unwrap_or("?");
                    info!(event="rule.removed", %rule_id, rule_name=%name);
                }
                RuleStatusEvent::Activated { rule_id } => {
                    let name = reg_clone.get(&rule_id).map(String::as_str).unwrap_or("?");
                    info!(event="rule.reactivated", %rule_id, rule_name=%name);
                }
            }
        }
    });
    drop(fatal_tx);

    // --- 主循环:select { fatal | joinset } ---
    let mut fatal_flag = false;                              // finding 4 修订
    loop {
        tokio::select! {
            biased;
            // 1. 仅 Some(()) 进入 fatal 分支;sender 全部 drop 后 None 不触发
            Some(()) = fatal_rx.recv() => {
                error!(event="standalone.fatal_shutdown");
                fatal_flag = true;
                shutdown.trigger();
            }
            join = joinset.join_next() => {
                match join {
                    Some(Err(e)) => {
                        // 任务 panic — 也算 fatal,无论后续 drain 是否清干净
                        error!(event="standalone.task_panic", error=%e);
                        fatal_flag = true;
                        shutdown.trigger();
                    }
                    Some(Ok(_)) => continue,
                    None => break,                            // 所有 task 都退出
                }
            }
        }
    }
    // v6 finding 2:无论以何路径退出主 loop(fatal / 所有任务自然结束 /
    // joinset 空),都先 trigger shutdown,确保 reporter 和 signal task
    // 一定能收到 cancel 信号后退出 await。不依赖 fatal 路径自身的 trigger。
    if !shutdown.token().is_cancelled() {
        shutdown.trigger();
    }
    let _ = reporter.await;
    let _ = signal_task.await;
    info!(event = "standalone.stopped");
    if fatal_flag { ExitCode::from(1) } else { ExitCode::SUCCESS }
}
```

**finding 4 修订要点**:
- `Some(()) = fatal_rx.recv()` 用模式匹配只对真实 `Some(())` 触发;
  `None`(所有 sender drop 后)不再误进 fatal 分支。
- `fatal_flag` 在循环外持有,任何**真实** fatal(运行期 Failed 或 JoinError)
  都会把它置 true。最终 exit code 由 `fatal_flag` 决定,跟 `shutdown.is_cancelled()`
  解耦(即使 fatal 之后 shutdown.trigger 了,exit code 仍然是 1)。
- `biased;` 让 select 优先检查 fatal 通道(纯保险,不影响正确性)。

### 4.4 Standalone reporter(finding 2 修订:不复用 client 的复杂 reporter)

```rust
// portunus-standalone/src/reporter.rs
pub fn spawn_standalone_reporter(
    rule_stats: Arc<RwLock<HashMap<RuleId, Arc<RuleStats>>>>,
    registry: Arc<HashMap<RuleId, String>>,
    interval: Duration,
    cancel: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tick.tick() => {
                    // 锁中毒(任一 writer panic 后)→ 跳过本 tick,
                    // 主 select 仍然继续(reporter 不应该把进程拖死)
                    let map = match rule_stats.read() {
                        Ok(g) => g,
                        Err(e) => {
                            warn!(event = "standalone.reporter_lock_poisoned", error = %e);
                            continue;
                        }
                    };
                    for (rule_id, rs) in map.iter() {
                        // v5 finding 2:用 snapshot_basic(无 multi-target / rate-limit
                        // 上下文),standalone 平衡套餐够用
                        let snap = rs.snapshot_basic();
                        let name = registry.get(rule_id).map(String::as_str).unwrap_or("?");
                        info!(event="standalone.stats",
                              rule=%rule_id, rule_name=%name,
                              in_bytes=snap.bytes_in,
                              out_bytes=snap.bytes_out,
                              active_conns=snap.active_connections,
                              datagrams_in=snap.datagrams_in,
                              datagrams_out=snap.datagrams_out,
                              active_flows=snap.active_flows);
                    }
                }
            }
        }
    })
}
```

**关键**:
- 仅消费 `RuleStats::snapshot_basic() -> RuleStatsSnapshotBasic`(forwarder
  提供的基础 getter,v5 finding 2)。不需要 RuleSlot 周边状态。
- per_target / SNI / rate-limit / per_port snapshot 字段在 standalone reporter
  里**不展开**(平衡套餐不启用 SNI / rate-limit;per_target / per_port 信息
  在数据面发生事件时由 forwarder 自己 `tracing::warn!` 出来,reporter 无需周期 dump)。
- reject / throttle 事件由 forwarder 内部直接 `tracing::warn!`,**不**通过
  reporter 中转(标准 tracing 路径,跟 v1.4 既有 `proxy.*` 事件命名风格一致)。

### 4.5 信号处理

standalone 自实现 signal handler(不污染 client 共享的 `Shutdown::signal_handler`):

```rust
// portunus-standalone/src/signal.rs
#[cfg(unix)]
pub fn install_standalone_signal_handler(shutdown: Shutdown) -> io::Result<JoinHandle<()>> {
    use tokio::signal::unix::{SignalKind, signal};
    // 三个订阅都在 spawn 前同步 install — 失败立即返回 io::Error,
    // 让 run() 走 error! + ExitCode::from(1) 路径。不 panic。
    let mut sigint  = signal(SignalKind::interrupt())?;
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sighup  = signal(SignalKind::hangup())?;
    let cancel = shutdown.token();
    Ok(tokio::spawn(async move {
        loop {
            tokio::select! {
                // 关键(v5 finding 1):同时监听 shutdown token,这样运行期
                // fatal 触发 shutdown.trigger() 后 signal task 也会自然退出,
                // 避免 main 的 signal_task.await 死锁。
                _ = cancel.cancelled() => { tracing::debug!(event="standalone.signal_handler_exit", reason="shutdown_triggered_externally"); return; }
                _ = sigint.recv()  => { tracing::info!(event="shutdown.signal", signal="SIGINT");  shutdown.trigger(); return; }
                _ = sigterm.recv() => { tracing::info!(event="shutdown.signal", signal="SIGTERM"); shutdown.trigger(); return; }
                _ = sighup.recv()  => { tracing::info!(event="standalone.sighup_ignored"); /* loop continues */ }
            }
        }
    }))
}
```

client 侧 `Shutdown::signal_handler` 不动。

### 4.6 资源限制
- 启动 log `getrlimit(RLIMIT_NOFILE)`,如果 < 4096 加 warn。
- 不主动 setrlimit;依赖 systemd unit / docker --ulimit。

## 5. `portunus-client` 侧改动

行为零变化。

### 5.1 机械化迁移
- `git mv` `forwarder/` `resolver/` `shutdown.rs` → `portunus-forwarder/src/`
- **`port_groups.rs` 留在 portunus-client**(决议见 §10)
- 全局替换 `use crate::forwarder::` → `use portunus_forwarder::`(等)
- `portunus-client/Cargo.toml`:删 `nix / hickory-resolver / async-trait /
  tokio-rustls / rustls / tokio-stream`(凡通过 forwarder 间接来的);
  加 `portunus-forwarder = { workspace = true }`
- benches `data_plane / range_install / dns_resolver / udp_data_plane / sni_route`
  跟源迁到 `portunus-forwarder/benches/`

### 5.2 Wire-neutral snapshot 类型(finding 1 + 3 修订)

```rust
// portunus-forwarder/src/forwarder/stats.rs

// ─── per-rule(对照 proto::v1::RuleStats 字段)───
#[derive(Clone, Debug)]
pub struct RuleStatsSnapshot {
    pub rule_id: RuleId,                                      // proto field 1 — v6 finding 1
    pub bytes_in: u64,                                        // field 2
    pub bytes_out: u64,                                       // field 3
    pub active_connections: u32,                              // field 4
    pub per_port: Vec<PerPortStatsSnapshot>,                  // field 5 (range rule)
    pub dns_failures: u64,                                    // field 6 (v0.3)
    pub datagrams_in: u64,                                    // field 7+ (v0.4 UDP)
    pub datagrams_out: u64,
    pub active_flows: u32,
    pub flows_dropped_overflow: u64,
    pub target_failovers_total: u64,                          // field 11 (v0.7 multi-target)
    pub per_target: Vec<PerTargetStatsSnapshot>,              // field 12
    pub sni_route_exact_total: u64,                           // (v0.9 SNI)
    pub sni_route_wildcard_total: u64,
    pub sni_route_fallback_total: u64,
    pub rate_limit: Option<RateLimitStatsSnapshot>,           // (v0.11)
}

#[derive(Clone, Debug, Default)]
pub struct PerPortStatsSnapshot {
    pub listen_port: u16,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub active_connections: u32,
    pub datagrams_in: u64,
    pub datagrams_out: u64,
}

#[derive(Clone, Debug, Default)]
pub struct PerTargetStatsSnapshot {
    pub index: u32,
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

// ─── rate-limit(对照 proto::v1::RateLimitStats — finding 1 修订)───
#[derive(Clone, Debug, Default)]
pub struct RateLimitStatsSnapshot {
    /// Cumulative reject totals by reason. Sparse — only present reasons.
    pub reject_total: Vec<(RateLimitRejectReason, u64)>,
    /// 累计被带宽限流阻塞的微秒(读/写两个方向各自累计)
    pub throttle_micros_in: u64,
    pub throttle_micros_out: u64,
    /// Live count of connections (TCP) or flows (UDP) under this scope
    pub active_connections: u32,
}

#[derive(Clone, Debug, Default)]
pub struct OwnerRateLimitStatsSnapshot {
    pub owner_id: String,
    pub stats: RateLimitStatsSnapshot,                        // 嵌套(对齐 proto)
}

#[derive(Clone, Debug, Default)]
pub struct SniListenerStatsSnapshot {
    // 对照 proto::v1::SniListenerStats:
    pub listen_port: u16,                                     // 1
    pub sni_route_miss_total: u64,                            // 2
    pub client_hello_parse_failures_total: u64,               // 3
    /// v1.6 (010-proxy-protocol-and-peek-histogram):
    /// 每个固定 bucket 的累计计数,顺序与
    /// `portunus_core::PEEK_HISTOGRAM_BUCKETS_SECS` 一致
    pub client_hello_peek_bucket_counts: Vec<u64>,            // 4
    pub client_hello_peek_sum_micros: u64,                    // 5
    pub client_hello_peek_count: u64,                         // 6
}

/// 真实 wire 是 `PerTargetStats.health: uint32`(不是 enum),
/// 当前实现见 `failover.rs::Health` → `as_wire(): Healthy=0, Failed=1`。
/// 这里镜像该约定,默认 Healthy。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TargetHealth {
    #[default]
    Healthy,
    Failed,
}

impl TargetHealth {
    /// 与 forwarder/failover.rs::Health::as_wire() 1:1 — 写到
    /// `PerTargetStats.health: uint32`。
    pub fn as_wire(self) -> u32 {
        match self {
            Self::Healthy => 0,
            Self::Failed => 1,
        }
    }
}

/// 与 proto::v1::RateLimitRejectReason 1:1
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RateLimitRejectReason {
    Unspecified,
    ConnConcurrent,
    ConnRate,
    UdpFlowRate,
    OwnerConcurrent,
    OwnerConnRate,
    OwnerUdpFlowRate,
}
```

**Snapshot getter**(v5 finding 2 修订)。`RuleStats` 内部只持有
基础计数器(bytes_in/out、active_connections、per_port、DNS/UDP 计数、SNI
trio);多目标/限速/per_target 等字段都不在 `RuleStats` 内,而是分散在
`MultiTargetObservability`、`RateLimitStatsAccumulator`、`RateLimitScopeManager`
之类的并行结构里。因此:

```rust
// portunus-forwarder/src/forwarder/stats.rs
impl RuleStats {
    /// 仅基础计数器 — 不含 per_target / rate_limit。
    /// 字段集对应当前 RuleStats 内部 atomics 的子集:
    /// bytes_in/out、active_connections、per_port、dns_failures、
    /// datagrams_in/out、active_flows、flows_dropped_overflow、
    /// sni_route_{exact,wildcard,fallback}_total。
    /// per_target / rate_limit / multi_target failover counters
    /// 需要外部 caller 自行从 MultiTargetObservability /
    /// RateLimitStatsAccumulator 取后合并 — 见下方 client snapshot 装配。
    pub fn snapshot_basic(&self) -> RuleStatsSnapshotBasic { /* atomic loads */ }
}

impl RateLimitStatsAccumulator {
    pub fn drain(&self) -> Option<RateLimitStatsSnapshot> { /* ... */ }
}

impl OwnerRateLimitStatsRegistry {
    pub fn drain(&self) -> Vec<OwnerRateLimitStatsSnapshot> { /* ... */ }
}

impl SniListenerCounters {
    pub fn snapshot(&self, listen_port: u16) -> SniListenerStatsSnapshot { /* ... */ }
}

impl MultiTargetObservability {
    /// per-target snapshot 组装 — 现 control.rs::build_per_target 的等价物。
    /// 入参 `targets` 是 ClientRule.targets 的引用(host/port/priority 元数据)。
    /// 数据面通过 try_lock 取 HealthState,失败则跳过该 target 本 tick。
    pub fn snapshot_per_target(&self, targets: &[MultiTarget]) -> (u64, Vec<PerTargetStatsSnapshot>);
    //   ^ target_failovers_total                              ^ per_target 子表
}
```

```rust
// portunus-forwarder/src/forwarder/stats.rs(snapshot 类型)
#[derive(Clone, Debug, Default)]
pub struct RuleStatsSnapshotBasic {
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub active_connections: u32,
    pub per_port: Vec<PerPortStatsSnapshot>,                 // 注意见下方 §5.2-build
    pub dns_failures: u64,
    pub datagrams_in: u64,
    pub datagrams_out: u64,
    pub active_flows: u32,
    pub flows_dropped_overflow: u64,
    pub sni_route_exact_total: u64,
    pub sni_route_wildcard_total: u64,
    pub sni_route_fallback_total: u64,
}
```

**client 装配**(`portunus-client/src/control.rs`,替代当前的 inline 构造):

```rust
/// 把当前 RuleSlot 的所有上下文聚合成完整的 RuleStatsSnapshot。
/// 与现有 send_stats_report 内 inline 构造 1:1 等价 — wire compat
/// 由此函数保证(per_port 仅 is_range==true 时填、rate_limit None=无 cap、
/// per_target empty=单目标 ……)。
fn build_rule_stats_snapshot(
    rule_id: RuleId,
    slot: &RuleSlot,
) -> RuleStatsSnapshot {
    let basic = slot.stats.snapshot_basic();

    // is_range 决定 per_port 是否上线 — 单端口 wire 字节稳定。
    let per_port = if slot.is_range { basic.per_port } else { Vec::new() };

    // multi-target 才填 per_target + target_failovers_total
    let (target_failovers_total, per_target) = match slot.multi_target_obs.as_ref() {
        Some(obs) => obs.snapshot_per_target(&slot.targets_view),
        None => (0, Vec::new()),
    };

    // 限速 accumulator: drain() 返 None 表示空帐户(proto3 default-stripping)
    let rate_limit = slot.rate_limit_stats.as_ref().and_then(|acc| {
        if let Some(limiter) = slot.rate_limit_limiter.as_ref() {
            acc.set_active_connections(limiter.active_connections());
        }
        acc.drain()
    });

    RuleStatsSnapshot {
        rule_id,                                              // v6 finding 1 — proto field 1
        bytes_in: basic.bytes_in,
        bytes_out: basic.bytes_out,
        active_connections: basic.active_connections,
        per_port,
        dns_failures: basic.dns_failures,
        datagrams_in: basic.datagrams_in,
        datagrams_out: basic.datagrams_out,
        active_flows: basic.active_flows,
        flows_dropped_overflow: basic.flows_dropped_overflow,
        target_failovers_total,
        per_target,
        sni_route_exact_total: basic.sni_route_exact_total,
        sni_route_wildcard_total: basic.sni_route_wildcard_total,
        sni_route_fallback_total: basic.sni_route_fallback_total,
        rate_limit,
    }
}
```

**standalone 装配**:它只需基础计数器(平衡套餐无 SNI / rate-limit;multi-target
信息由 forwarder 数据面 `proxy.*` 事件直接日志)。reporter 调
`rs.snapshot_basic()` 就够,不构造完整 `RuleStatsSnapshot`。

**client 翻译层(新)** — 在 `portunus-client/src/control.rs` 加:
```rust
impl From<RuleStatsSnapshot> for proto::v1::RuleStats { ... }
impl From<RateLimitStatsSnapshot> for proto::v1::RateLimitStats { ... }
impl From<OwnerRateLimitStatsSnapshot> for proto::v1::OwnerRateLimitStats { ... }
impl From<SniListenerStatsSnapshot> for proto::v1::SniListenerStats { ... }
impl From<PerTargetStatsSnapshot> for proto::v1::PerTargetStats { ... }
impl From<PerPortStatsSnapshot> for proto::v1::PerPortStats { ... }
// TargetHealth → u32(proto 字段是 uint32,不是 enum;见 finding 2)
impl From<RateLimitRejectReason> for proto::v1::RateLimitRejectReason { ... }
// PerTargetStats.health 的赋值是 snapshot.health.as_wire() (u32),
// 在 `From<PerTargetStatsSnapshot> for proto::v1::PerTargetStats` 内直接调用。
```

**client 的 reporter 不动**(`control.rs:357 stats_tick + send_stats_report` 留在
client):它本来就需要消费 RuleSlot 周边状态(`is_range / multi_target_obs /
targets_view / rate_limit_limiter / rate_limit_stats`),那些字段是 client 私有
的。只是把"构造 proto 字段"改成"`build_rule_stats_snapshot(rule_id, &slot)
.into()`"(装配函数内部调 `snapshot_basic()` + `snapshot_per_target()` +
`acc.drain()`)。

### 5.3 `Protocol` 上提
见 §3.5。Phase 1 触动 ~20 个文件,纯类型重定向。

### 5.4 QuotaScopeManager 不动
`QuotaScopeManager` 留在 `portunus-client/src/control.rs`,继续构造
`Arc<QuotaHandle>` 注入 `ClientRule.quota`。重连重放顺序不变。

### 5.5 测试归位
- forwarder 模块的 `#[cfg(test)] mod tests` 跟模块搬到 portunus-forwarder
- `portunus-client/tests/*` 集成测试 — 集成路径不变;`From<Snapshot>` 翻译层
  的单元测试新建在 `portunus-client/tests/snapshot_from_proto.rs`
- 关键的 `*_wire_compat` 测试**留在 client**(验证 `Snapshot → From → proto`
  仍然字节级与 v1.4.0 一致)
- `portunus-e2e/tests/traffic_quotas.rs` 等 e2e 不动

### 5.6 LiveResolver 便利构造函数(finding 6)

forwarder lib 新增 helper(当前 `LiveResolver::new(inner, config)` 太冗长):

```rust
// portunus-forwarder/src/resolver/mod.rs (新增)
impl LiveResolver<HickoryResolver> {
    /// Build a resolver wired to the system /etc/resolv.conf with default
    /// transport options. Replaces inline boilerplate at call sites.
    pub fn with_system_defaults() -> io::Result<Self> {
        let config = ResolverConfig::default();
        let inner = Arc::new(HickoryResolver::from_system(&config)?);
        Ok(Self::new(inner, config))
    }
}
```

client 的 `control.rs:212` 调用 `LiveResolver::new(Arc::new(HickoryResolver::from_system(...)?), ...)`
也可以同步改用 `with_system_defaults()` 收尾(但**不在 v1.5 范围内强制**;
等到下一次 control.rs 重构再换)。

### 5.7 Shutdown 不被 standalone 污染
`portunus-forwarder::shutdown::Shutdown` 保持当前 SIGINT/SIGTERM 行为。
standalone 不调用其 `signal_handler`,自实现(§4.5)。

## 6. 测试策略

| 测试层 | 位置 | 覆盖 |
|--------|------|------|
| 数据面单元测试 | `portunus-forwarder/src/**/tests` | 跟模块迁入 |
| Snapshot getter 单元测试 | `portunus-forwarder/tests/snapshot.rs` | 验证 atomic load → snapshot 字段正确性 |
| **wire 字节级回归** | `portunus-client/tests/*_wire_compat.rs` | 现有套件全留 + 新增 `From<Snapshot>` 翻译层用例 |
| client 单元测试 | `portunus-client/tests/*` | reconcile / 重连重放 / Quota |
| standalone config | `portunus-standalone/src/config.rs::tests` | TOML 解析 / 校验 / RuleId 冲突 / 重名 / 条件 desugar / deny_unknown_fields |
| standalone reporter | `portunus-standalone/src/reporter.rs::tests` | snapshot 周期日志 / cancel 即停 |
| standalone 集成 | `portunus-standalone/tests/smoke.rs` | echo + assert_cmd loopback / SIGHUP 忽略 / fatal exit 1 |
| E2E | `portunus-e2e/tests/standalone_*.rs` | 多规则 / failover / PROXY v2 |
| `--check` CI | `portunus-standalone/tests/check_mode.rs` | 至少 6 fixtures(3 合法 3 非法),exit 0/2 |
| bench 回归 | `portunus-forwarder/benches/*` | `data_plane` → v0.1.0 baseline;`splice_throughput` → v1.2.0 baseline |

## 7. 迁移分阶段

```
Phase 1 — 公共 crate 框架 + Protocol 上提 (PR 1)
  ├─ T1.1 创建 crates/portunus-forwarder 空壳 + workspace 注册
  ├─ T1.2 portunus-core 新增 Protocol 类型(serde lowercase)
  ├─ T1.3 proto 加 From/Into core::Protocol
  ├─ T1.4 server/rules.rs 删本地 Protocol,改用 core
  ├─ T1.5 server 各 match 处类型重定向
  ├─ T1.6 workspace build + test 通过

Phase 2 — 平移数据面 + proto-free 净化 + snapshot getter (PR 2)
  ├─ T2.1 git mv forwarder/ resolver/ shutdown.rs → portunus-forwarder/src/
  ├─ T2.2 forwarder 内 portunus_proto 引用改成 core::Protocol
  ├─ T2.3 RateLimitStatsAccumulator::drain_to_proto → drain() 返 Snapshot
  ├─ T2.4 OwnerRateLimitStatsRegistry::drain_to_proto → drain() 返 Vec<Snapshot>
  ├─ T2.5 SniListenerCounters proto-free
  ├─ T2.6 RuleStats::snapshot_basic() getter + RuleStatsSnapshotBasic
  ├─ T2.7 MultiTargetObservability::snapshot_per_target() 提取(从 control.rs::build_per_target 平移)
  ├─ T2.8 lib.rs pub use re-exports(对照 §3.3);
  │       rate_limit/mod.rs + sni/mod.rs 加 `pub use scope::*; pub use stats::*; pub use listener::*;`
  ├─ T2.9 portunus-client 加 From<Snapshot> for proto::* 翻译层(注意:无 TargetHealth From,直接调 as_wire())
  ├─ T2.10 client/control.rs 新增 build_rule_stats_snapshot(rule_id, slot),
  │        send_stats_report 改用 build_*().into() — wire compat 由 *_wire_compat 套件守护
  ├─ T2.11 LiveResolver::with_system_defaults() 新增
  ├─ T2.12 client use 路径全局替换 + Cargo.toml 调整
  ├─ T2.13 benches 跟源迁
  └─ 验证: workspace test + *_wire_compat 字节级全绿

Phase 3 — portunus-standalone binary (PR 3)
  ├─ T3.1 crate 骨架 + Cargo.toml(无 proto/tonic/auth)+ bin target
  ├─ T3.2 config.rs · TOML schema + deny_unknown_fields
  ├─ T3.3 RuleId blake3 派生 + 冲突 + 重名检测
  ├─ T3.4 单目标条件 desugar
  ├─ T3.5 main.rs · clap CLI + 默认搜索路径
  ├─ T3.6 signal.rs · standalone_signal_handler(SIGHUP no-op)
  ├─ T3.7 reporter.rs · spawn_standalone_reporter
  ├─ T3.8 runtime.rs · 启动门 + fatal channel + fatal_flag + exit code
  ├─ T3.9 单元 + 集成测试(--check fixtures 至少 6 个)
  └─ 验证: cargo run -p portunus-standalone -- --check tests/fixtures/full.toml

Phase 4 — E2E + 文档 (PR 4)
  ├─ T4.1 portunus-e2e/tests/standalone_*.rs 3 场景
  ├─ T4.2 docs/operations/standalone.mdx(中英)
  ├─ T4.3 README.md 单机版段落 + 示例 TOML
  ├─ T4.4 CHANGELOG.md v1.5.0 草稿
  └─ T4.5 Makefile 加 standalone / standalone-check 目标
```

**v3 修订:Phase 3 合并 v2 的 Phase 3+4**(因为 StatsSink trait 取消,
不再需要单独 PR 抽 trait)。

## 8. 风险登记

| 风险 | 缓解 |
|------|------|
| Phase 2 forwarder 遗漏 `portunus_proto` 引用 | Cargo.toml 不含 portunus-proto,任何漏改直接编译失败 |
| Phase 2 snapshot 字段集偏离 proto | client `From` 翻译 + 现有 `*_wire_compat` 测试字节级比对 |
| Phase 2 stats hot path 引入抖动 | 数据面仍是原子 fetch_add;`snapshot_basic()` 等 getter 只在 ticker 内调用;bench 对比现有 baseline(`data_plane` → v0.1.0、`splice_throughput` → v1.2.0)零回退 |
| Protocol 上提牵连 server JSON 兼容 | rule JSON serde 仍 lowercase;wire bytes 不变 |
| RuleId blake3 冲突 | registry 检测;blake3 64-bit prefix 任意一对碰撞概率 2^-64 |
| 单目标 + PROXY 走 failover_path 的开销 | 文档明示 opt-in 代价;数据面仍是 splice;影响仅限有 PROXY 需求的单目标 |
| standalone TOML 字段拼错 → 静默失活 | `deny_unknown_fields` + `--check` |
| 不同信号策略干扰 client 测试 | standalone 自实现 signal handler;`Shutdown` 不动 |
| `LiveResolver::with_system_defaults` 引入 hickory 默认变化 | 单元测试锁定 ResolverConfig 默认值,future hickory bump 必须显式 |

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
- `PortGroupManager` 公开化(留在 client 内私有)

## 10. Resolved Questions (cumulative)

v1 → v2:
1. **RuleId 派生**:blake3(name).prefix_u64 + 启动期 registry 重名 / 冲突检测
   + 日志强制带 `rule_name` 字段。详见 §4.2.3。**v3 修正**:从 xxh3_64 改为
   blake3(workspace 已用,免新增依赖)。
2. **forwarder proto-free 边界**:彻底搬离 —— forwarder Cargo.toml 不含
   portunus-proto;wire 翻译层全部移到 client 的 `From<Snapshot> for proto::*` impl。
3. **listen 地址**:v1.5 schema 只接 port / port range,永远 wildcard 双栈 bind。

v2 → v3:
4. **StatsSink trait 取消**(finding 2):forwarder 仅暴露 wire-neutral
   snapshot getter,reporter 由各 binary 自己拥有。Client 的 reporter 仍在
   `control.rs:357`,只把"直接构造 proto"改成"`build_rule_stats_snapshot(...)
   .into()`"。
5. **单目标 PROXY desugar 策略**(finding 3):无 PROXY 单目标保留 `targets=[]`
   fast path;带 PROXY 单目标 desugar 成 1-element targets。文档明示 opt-in 代价。
6. **PROXY 类型名**(finding 5):用真实 `ProxyProtocolVersion`(已在 core)+
   `ProxyProtocolPrelude`(forwarder);v2 写的 `ProxyProtocolMode` 删除。
7. **`PortGroupManager` 归属**(finding 5):**留在 portunus-client**,不进 lib。
   它是控制面 reconcile 辅助;standalone 平衡套餐不需要。
8. **fatal channel + JoinError exit code**(finding 4):`Some(()) = fatal_rx.recv()`
   排除 None 误进 fatal;独立 `fatal_flag: bool` 跟踪,JoinError 也置 flag,
   最终 exit code 由 flag 决定(与 `shutdown.is_cancelled()` 解耦)。
9. **LiveResolver 便利构造**(finding 6):forwarder 加
   `LiveResolver::with_system_defaults() -> io::Result<Self>`。
10. **hash 库**(finding 7):不引入 twox-hash;用 workspace 已有 blake3
    取前 8 字节。碰撞数学:单对 2^-64;N 条规则生日界 ≈ N²/2^65。

v3 → v4:
11. **SniListenerStatsSnapshot peek histogram**(v4 finding 1):snapshot 字段
    与 proto::v1::SniListenerStats 1-6 字段完全对齐 —— 加
    `client_hello_peek_bucket_counts: Vec<u64>` + `client_hello_peek_sum_micros: u64`
    + `client_hello_peek_count: u64`(v1.6 添加的 010 peek histogram)。
12. **TargetHealth wire 类型**(v4 finding 2):proto 实际是 `uint32`,不是 enum。
    `TargetHealth` 重定义为 `{ Healthy, Failed }`,默认 Healthy,提供
    `as_wire() -> u32`(对齐 forwarder/failover.rs::Health::as_wire)。
    删 `From<TargetHealth> for proto::v1::TargetHealth`。
13. **RuleStats 句柄传递**(v4 finding 3):保留现有
    `run_forwarder(rule, resolver, status_tx, cancel, drain, stats: Arc<RuleStats>)`
    签名,**不改 ClientRule**。standalone runtime 在 spawn 前就地构造
    `Arc<RuleStats>` 并登记到 `rule_stats_handles` registry。
14. **re-export 子路径**(v4 finding 4):`§3.3` 用顶层短路径表示
    *最终意图*。Phase 2 任务把 `rate_limit/mod.rs` 和 `sni/mod.rs` 加
    `pub use scope::*; pub use stats::*; pub use listener::*;` 让短路径
    成立;否则 lib.rs 的 re-export 不通过编译。
15. **expect/unwrap 清退**(v4 finding 5):
    - `LiveResolver::with_system_defaults()` → 失败 `error!` + `ExitCode::from(1)`,不 panic
    - signal handler install → 同上,`install_standalone_signal_handler() -> io::Result<JoinHandle<()>>`
    - reporter 锁中毒 → `warn!` + `continue`(本 tick 跳过,不杀进程)
16. **字段名修正**:`probe_interval_secs` → `health_check_interval_secs`(对齐
    `ClientRule::health_check_interval_secs`)。

v4 → v5:
17. **signal task 死锁修复**(v5 finding 1):signal handler 内 select 增加
    `cancel.cancelled()` 分支 —— 运行期 fatal 触发 `shutdown.trigger()` 后
    signal task 也会自然退出,`signal_task.await` 不再卡死。
18. **RuleStats::snapshot() 边界**(v5 finding 2):snapshot getter 重命名为
    `snapshot_basic()`,**只返回基础计数器**(bytes / per_port / DNS / UDP /
    SNI trio)。`per_target / target_failovers_total` 由
    `MultiTargetObservability::snapshot_per_target()` 提供;`rate_limit` 由
    `RateLimitStatsAccumulator::drain()` 提供。完整 `RuleStatsSnapshot` 由
    **client 侧** `build_rule_stats_snapshot(rule_id, slot)` 装配(替代 inline
    构造),保证 wire compat。standalone 平衡套餐只用 `snapshot_basic()`。
19. **RuleStats 生产构造**(v5 finding 3):用
    `RuleStats::for_range(rule.listen_range) -> Arc<Self>`(生产 API);
    `RuleStats::new()` 是 `#[cfg(test)]` 不可用。`for_range` 已返回 Arc,
    standalone 不再 `Arc::new()` 包一层。
20. **stats registry 锁中毒处理**:`rule_stats_handles.write()` 失败时
    `error!` 后跳过该规则注册(forwarder 仍 spawn,只是 reporter 拿不到 stats),
    不 panic、不 expect。
21. **去除 LoggingStatsSink 残留称呼**:文档统一叫"standalone reporter",
    没有"sink"类型(无 trait,无独立结构)。

v5 → v6:
22. **RuleStatsSnapshot 加 rule_id**(v6 finding 1):proto `RuleStats.rule_id = 1`
    是必填字段。snapshot 新增 `pub rule_id: RuleId`,`build_rule_stats_snapshot
    (rule_id, &slot)` 把入参写进 snapshot;`From<RuleStatsSnapshot> for
    proto::v1::RuleStats` 自然可填 field 1。
23. **配置至少 1 条 rule**(v6 finding 2):空 TOML 或 0 条 [[rule]] 在 config
    load 阶段直接 exit 2。同时 runtime tail 加 `if !shutdown.is_cancelled()
    { shutdown.trigger() }` 兜底,确保任何路径退出主 loop 都能让 reporter /
    signal task 收到 cancel 后退出 await,不依赖 fatal 路径自身的 trigger。
24. **`RuleStatsSnapshotBasic` 进 public 表面**(v6 finding 3):§3.3 的
    `pub use forwarder::stats::{ ..., RuleStatsSnapshotBasic, ... };`
    与 `RuleStats::snapshot_basic()` 返回类型保持一致。
25. **去除 expect/unwrap 的最后一处**(v6 small fix):blake3 hash 取前 8 字节
    从 `try_into().expect("blake3 hash >= 32 bytes")` 改成
    `copy_from_slice(&h.as_bytes()[..8])`,静态尺寸保证、无 panic 路径。
26. **所有 `RuleStats::snapshot()` 残留 → `snapshot_basic()`**(v6 small fix):
    §3.4 接缝表、§4.2.3 reporter 样例、§4.4 描述、§5.2 client 段、风险表、
    §10 finding 4 旧描述,全部口径统一。

无未决问题。

---

**Next step:** 经用户复核后,移交 `superpowers:writing-plans` skill 生成
`docs/superpowers/plans/2026-05-14-standalone-forwarder.md`(每个 Phase 拆成
bite-sized TDD 任务)。

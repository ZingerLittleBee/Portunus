# Owner Client Connection Limit Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ensure the `(client_name, owner_id)`-scoped `concurrent_connections` cap shipped in v0.11 is end-to-end correct on every data-plane path, fix the SNI dispatcher path that today silently drops owner / rule limiters, and prove v1.3.0 splice does not regress the gate.

**Architecture:** Reuse the existing v0.11 owner envelope keyed by `(client_name, owner_id)`. Control plane persists and pushes `OwnerRateLimitUpdate`. portunus-client applies the owner-scope admission gate before the per-rule gate via `try_acquire_layered`. Lowering the cap rejects only new work; existing connections drain naturally (`OwnerRateLimitScopeManager::update`, not `install`). The SNI dispatcher needs its `GroupMember` / `SniRuleSlot` structs extended to carry the four limiter handles end-to-end.

**Tech Stack:** Rust 2024 (MSRV 1.88), Tokio, tonic/prost, axum, SQLite/rusqlite, existing `portunus_core::RateLimit` and `forwarder::rate_limit` modules.

---

## Up-front invariants (READ FIRST)

Three things must be true at the end. Two are already true today; one is the actual code change.

1. **Capped SNI rules currently lose their limiter.** Verified trace:
   - `crates/portunus-client/src/control.rs:758-761` — every TCP single-port rule with `sni_pattern.is_some()` is routed through `port_groups::apply_push`. No "capped → legacy" diversion.
   - `crates/portunus-client/src/port_groups.rs:79-91` — the `GroupMember` struct does not have `rate_limit` / `owner_rate_limit` fields.
   - `crates/portunus-client/src/port_groups.rs:159-174` — `apply_push` constructs `GroupMember` without copying limiter handles from the incoming `ClientRule`.
   - `crates/portunus-client/src/forwarder/sni/listener.rs:404-407` — the proxy call site passes four explicit `None` slots.

   The comment at `sni/listener.rs:397-407` describing a "legacy accept_loop" diversion is historical. Task 5 fixes this by threading four `Option<Arc<...>>` fields through `GroupMember` → `SniRuleSlot` → `proxy_with_preread_and_prelude`.

2. **v1.3.0 splice ignores `concurrent_connections`.** Verified:
   - `crates/portunus-client/src/forwarder/splice.rs:93-94` computes `has_bandwidth_cap` from `{rule, owner}` bandwidth-cap presence only.
   - `crates/portunus-client/src/forwarder/splice.rs:818, 882` already prove rule- and owner-side `concurrent_connections` does not disable splice.

   Task 7 adds a `new_connections_per_sec`-only test for symmetry and a guard-lifetime integration test (the genuinely missing piece).

3. **Lowering a cap drains gracefully.** Verified: `crates/portunus-client/src/control.rs:887-909`'s `apply_owner_rate_limit_update` calls `OwnerRateLimitScopeManager::update` (not `install`), preserving the active gauge so in-flight connections survive a hot lower. Task 6 adds the test that locks this in.

## File Map

**Files to create (1):**
- `crates/portunus-e2e/tests/owner_connection_limit.rs` — server↔client integration smoke (Task 10).

**Files to modify (9):**
- `crates/portunus-server/tests/rate_limit_owner_contract.rs` — add two HTTP contract tests (Task 1).
- `crates/portunus-client/src/forwarder/udp/mod.rs` — add UDP concurrent-only test (Task 3).
- `crates/portunus-client/src/forwarder/failover_path.rs` — add failover concurrent test if missing (Task 4).
- `crates/portunus-client/src/port_groups.rs` — extend `GroupMember`; copy limiter fields in `apply_push`; thread through `rebuild_watches`; add reproducer + enforcement tests (Task 5).
- `crates/portunus-client/src/forwarder/sni/listener.rs` — extend `SniRuleSlot`; replace four `None`s at line 404; rewrite stale comment (Task 5).
- `crates/portunus-client/src/forwarder/rate_limit/scope.rs` — add lower-cap drain test if missing (Task 6).
- `crates/portunus-client/src/forwarder/splice.rs` — add `new_connections_per_sec`-only invariant test (Task 7).
- `crates/portunus-client/src/forwarder/mod.rs` — add splice guard-lifetime integration test (Task 7).
- `crates/portunus-server/src/operator/owner_cap_cli.rs` — add CLI smoke test if missing (Task 8).

**Files to verify, no edits expected:**
- `crates/portunus-core/src/rate_limit.rs`
- `crates/portunus-server/src/operator/owner_cap.rs`
- `crates/portunus-server/src/owner.rs`
- `crates/portunus-server/src/store/owner_cap_store.rs`
- `crates/portunus-server/src/grpc/service.rs`
- `crates/portunus-client/src/control.rs`
- `crates/portunus-client/src/forwarder/mod.rs` (Task 2 verification only)
- `webui/` (Task 9 manual smoke only)

---

## Chunk 1: Contract Pinning

### Task 1: Pin Owner Concurrent API Semantics

**Files:**
- Modify: `crates/portunus-server/tests/rate_limit_owner_contract.rs`
- Read: `crates/portunus-server/src/operator/owner_cap.rs:128-138` (the `version_at_least_0_11` gate)

- [ ] **Step 1: Read existing contract test patterns**

```bash
rg -n "fn |register_fake_client|client_version|0\.11\.0|concurrent" crates/portunus-server/tests/rate_limit_owner_contract.rs | head -30
```

Identify the existing helper for registering a fake client at a chosen version. We will reuse it. The two new tests live in the same file as siblings of existing concurrent / bandwidth tests.

- [ ] **Step 2: Write the failing test for the 0.11.0 concurrent-only PUT/GET round-trip**

Add to `crates/portunus-server/tests/rate_limit_owner_contract.rs`:

```rust
#[tokio::test]
async fn t070_concurrent_only_put_get_round_trip_v011_client() {
    let env = TestEnv::start().await;
    let client_name = "edge-01";
    let owner_id = "alice";

    env.register_fake_client(client_name, "0.11.0").await;

    // PUT — set ONLY concurrent_connections; all other rate-limit
    // fields must remain absent in the response (no defaulting).
    let body = serde_json::json!({ "concurrent_connections": 100 });
    let put = env
        .http_put(&format!("/v1/clients/{client_name}/owners/{owner_id}/rate-limit"), &body)
        .await;
    assert_eq!(put.status(), 200, "PUT body={body}");
    let put_json: serde_json::Value = put.json().await;
    assert_eq!(put_json["rate_limit"]["concurrent_connections"], 100);
    for field in [
        "bandwidth_in_bps",
        "bandwidth_out_bps",
        "new_connections_per_sec",
        "bandwidth_in_burst",
        "bandwidth_out_burst",
        "new_connections_burst",
    ] {
        assert!(
            put_json["rate_limit"][field].is_null(),
            "field {field} should be null after concurrent-only PUT, got {:?}",
            put_json["rate_limit"][field]
        );
    }

    // GET round-trip — same shape.
    let get = env
        .http_get(&format!("/v1/clients/{client_name}/owners/{owner_id}/rate-limit"))
        .await;
    assert_eq!(get.status(), 200);
    let get_json: serde_json::Value = get.json().await;
    assert_eq!(get_json["rate_limit"]["concurrent_connections"], 100);
    assert!(get_json["rate_limit"]["bandwidth_in_bps"].is_null());

    // gRPC push capture — the server-to-client envelope must mirror the API.
    let push = env.next_owner_rate_limit_push(client_name).await;
    assert_eq!(push.action, OwnerRateLimitAction::Set as i32);
    assert_eq!(push.owner_id, owner_id);
    let rl = push.rate_limit.as_ref().expect("rate_limit present");
    assert_eq!(rl.concurrent_connections, Some(100));
    assert_eq!(rl.bandwidth_in_bps, None);
    assert_eq!(rl.bandwidth_out_bps, None);
    assert_eq!(rl.new_connections_per_sec, None);
}
```

If `register_fake_client` / `http_put` / `http_get` / `next_owner_rate_limit_push` aren't in the existing helper API under the name used above, rename to whatever the file already uses — DO NOT invent a new harness.

- [ ] **Step 3: Run the test to verify it fails or passes**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --test rate_limit_owner_contract t070_concurrent_only_put_get_round_trip_v011_client -- --nocapture
```

Expected: PASS (the API already supports this — this is a regression-coverage test). If it fails, the failure tells you exactly where the contract drifted (PUT body, GET body, or push envelope); fix inside `operator/owner_cap.rs` / `owner_cap_store.rs` only.

- [ ] **Step 4: Write the failing test for the < 0.11.0 capability gate**

Add to the same file:

```rust
#[tokio::test]
async fn t071_concurrent_put_returns_422_for_pre_011_client() {
    let env = TestEnv::start().await;
    let client_name = "edge-old";
    let owner_id = "alice";

    env.register_fake_client(client_name, "0.10.0").await;

    let body = serde_json::json!({ "concurrent_connections": 100 });
    let put = env
        .http_put(&format!("/v1/clients/{client_name}/owners/{owner_id}/rate-limit"), &body)
        .await;
    assert_eq!(put.status(), 422, "expected capability gate to reject");
    let put_json: serde_json::Value = put.json().await;
    assert_eq!(put_json["error"], "rate_limit_unsupported_by_client");

    // No push must have been emitted for this client.
    assert!(env.try_recv_owner_rate_limit_push(client_name).is_none());

    // No persisted row.
    let get = env
        .http_get(&format!("/v1/clients/{client_name}/owners/{owner_id}/rate-limit"))
        .await;
    assert_eq!(get.status(), 404);
}
```

- [ ] **Step 5: Run the gate test**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --test rate_limit_owner_contract t071_concurrent_put_returns_422_for_pre_011_client -- --nocapture
```

Expected: PASS. The gate at `operator/owner_cap.rs:138` (`version_at_least_0_11`) renders `422 rate_limit_unsupported_by_client`.

- [ ] **Step 6: Commit**

```bash
git add crates/portunus-server/tests/rate_limit_owner_contract.rs
git commit -m "test(owner-cap): pin concurrent PUT/GET round-trip + 0.11 capability gate"
```

---

## Chunk 2: Client-Side Enforcement Coverage

### Task 2: Confirm Plain-TCP Owner Cap Enforcement

**Files:**
- Read: `crates/portunus-client/src/forwarder/mod.rs:2117` (test `t030_owner_cap_binds_before_rule_cap_on_tcp_accept`)
- Read: `crates/portunus-client/src/forwarder/rate_limit/scope.rs:351` (`try_acquire_layered`)

- [ ] **Step 1: Read the existing test and confirm it covers all four invariants**

```bash
rg -n "t030_owner_cap_binds_before_rule_cap_on_tcp_accept" crates/portunus-client/src/forwarder/mod.rs
```

Open the test body. Confirm it asserts:
1. Owner gate runs before rule gate (a connection that exceeds the owner cap but is within the rule cap is rejected by owner, not by rule).
2. Surplus accept increments the `OwnerConcurrent` reject counter (not the rule reject counter).
3. The active owner gauge does NOT include the rejected connection.
4. The active rule gauge does NOT include the rejected connection either (the rejection happens before the rule guard is acquired).

- [ ] **Step 2: Run**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client forwarder::tests::t030_owner_cap_binds_before_rule_cap_on_tcp_accept -- --nocapture
```

Expected: PASS. If any of the four invariants from Step 1 is NOT asserted, add the missing assertion and commit; otherwise no change.

- [ ] **Step 3: Commit (only if Step 2 added an assertion)**

```bash
git add crates/portunus-client/src/forwarder/mod.rs
git commit -m "test(owner-cap): tighten plain-TCP owner-first test invariants"
```

### Task 3: Add UDP Owner `concurrent_connections` Test

**Files:**
- Modify: `crates/portunus-client/src/forwarder/udp/mod.rs`
- Read: `crates/portunus-client/src/forwarder/udp/mod.rs:191, 271, 553, 616, 711, 772` (the `owner_admit_guard` plumbing)
- Read: `crates/portunus-client/src/forwarder/udp/mod.rs:1123` (existing test `t030_owner_cap_binds_before_rule_cap_on_udp_first_packet`)

- [ ] **Step 1: Confirm the production code already plumbs the guard**

```bash
rg -n "owner_admit_guard|OwnerConcurrent" crates/portunus-client/src/forwarder/udp/mod.rs
```

Expected hits at `udp/mod.rs:191, 271, 553, 616, 711, 772`. The implementation exists; this task adds a test that specifically exercises `concurrent_connections` (the existing test covers `new_connections_per_sec`).

- [ ] **Step 2: Write the failing test**

Append to the `#[cfg(test)] mod tests` block in `crates/portunus-client/src/forwarder/udp/mod.rs`:

```rust
#[tokio::test]
async fn t072_owner_concurrent_cap_drops_second_source_keeps_first_flow_alive() {
    use crate::forwarder::rate_limit::scope::{
        OwnerId, OwnerRateLimitHandle, OwnerRateLimitScopeManager,
    };
    use crate::forwarder::rate_limit::stats::RateLimitStatsAccumulator;
    use portunus_core::{RateLimit, RejectReason};

    // ─ Setup (mirrors t030 at udp/mod.rs:1123) ───────────────────
    let owner_mgr = Arc::new(OwnerRateLimitScopeManager::new());
    let owner_id = OwnerId::new("alice");
    owner_mgr.install(
        &owner_id,
        Some(&RateLimit {
            concurrent_connections: Some(1),
            // NO new_connections_per_sec — concurrent is the only gate.
            ..Default::default()
        }),
    );
    let owner_limiter = Arc::new(OwnerRateLimitHandle::new(owner_id, Arc::clone(&owner_mgr)));
    let owner_stats = Arc::new(RateLimitStatsAccumulator::new());

    // Reuse the harness in t030 (lines 1130-1186): spawn_udp_echo,
    // run_listener with (rule_limiter=None, rule_stats=None, owner=owner_limiter,
    // owner_stats=owner_stats). Adapt the exact helper names to whatever
    // t030 already uses — DO NOT introduce a parallel harness.
    let echo = spawn_udp_echo().await;
    let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await.unwrap();
    let listen_port = probe.local_addr().unwrap().port();
    drop(probe);

    let stats = RuleStats::new();
    let cancel = CancellationToken::new();
    let cancel_run = cancel.clone();
    let stats_run = Arc::clone(&stats);
    let task_owner = Arc::clone(&owner_limiter);
    let task_owner_stats = Arc::clone(&owner_stats);
    let task = tokio::spawn(async move {
        run_listener(
            RuleId(901),
            listen_port,
            Target::Ip(echo.ip()),
            echo.port(),
            false,
            1024,
            Duration::ZERO,
            stats_run,
            test_resolver(),
            cancel_run,
            None,                       // rule_limiter
            None,                       // rule_stats
            Some(task_owner),
            Some(task_owner_stats),
        )
        .await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // ─ Source A: first packet admits, flow stays alive ──────────
    let a = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    a.send_to(b"hello-a", (Ipv4Addr::LOCALHOST, listen_port))
        .await
        .unwrap();
    let mut buf = [0u8; 64];
    let (n, _) = tokio::time::timeout(Duration::from_secs(2), a.recv_from(&mut buf))
        .await
        .expect("first source must round-trip")
        .unwrap();
    assert_eq!(&buf[..n], b"hello-a");

    assert_eq!(owner_limiter.active_connections(), 1, "first flow occupies the slot");
    assert_eq!(owner_stats.reject_total(RejectReason::OwnerConcurrent), 0);

    // ─ Source B: first packet must drop on owner gate ──────────
    let b = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    b.send_to(b"hello-b", (Ipv4Addr::LOCALHOST, listen_port))
        .await
        .unwrap();
    let dropped = tokio::time::timeout(Duration::from_millis(300), b.recv_from(&mut buf)).await;
    assert!(dropped.is_err(), "source B must be dropped, not echoed");

    assert_eq!(owner_limiter.active_connections(), 1, "still only source A active");
    assert_eq!(
        owner_stats.reject_total(RejectReason::OwnerConcurrent),
        1,
        "source B increments OwnerConcurrent reject counter"
    );

    cancel.cancel();
    task.await.unwrap();
}
```

If `force_drop_idle_flows` or any of the spawn / bind helpers don't exist verbatim, locate the existing UDP test's helpers (around `udp/mod.rs:1123`) and rename. **Do not** invent a parallel harness.

- [ ] **Step 3: Run the test to verify it fails or passes**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client forwarder::udp::tests::t072_owner_concurrent_cap_drops_second_source_keeps_first_flow_alive -- --nocapture
```

Expected: PASS. The UDP enforcement already exists; the test confirms it.

If FAIL: read the panic to identify whether it's a harness mismatch (rename helpers) or a real regression in the `owner_admit_guard` flow. Fix the real regression by reading `udp/mod.rs:191-280` and `udp/mod.rs:540-720` — do not work around with `concurrent_connections > 1`.

- [ ] **Step 4: Commit**

```bash
git add crates/portunus-client/src/forwarder/udp/mod.rs
git commit -m "test(owner-cap): UDP concurrent-only drop / hold / re-admit"
```

### Task 4: Add Multi-Target TCP Owner Cap Test if Missing

**Files:**
- Modify: `crates/portunus-client/src/forwarder/failover_path.rs`

- [ ] **Step 1: Check for existing coverage**

```bash
rg -n "OwnerConcurrent|owner_cap|owner_rate_limit|try_acquire_layered" crates/portunus-client/src/forwarder/failover_path.rs
```

Expected: plumbing hits at lines 162-163, 227-228, 232, 246-247, 296-297, 359-360, 740-741, 757-758. If a test named `*owner*concurrent*` exists, skip to Step 4.

- [ ] **Step 2: Write the failing test** (paste at the end of `failover_path.rs` `#[cfg(test)] mod tests`)

```rust
#[tokio::test]
async fn t073_owner_concurrent_cap_rejects_second_failover_accept() {
    use crate::forwarder::rate_limit::scope::{
        OwnerId, OwnerRateLimitHandle, OwnerRateLimitScopeManager,
    };
    use crate::forwarder::rate_limit::stats::RateLimitStatsAccumulator;
    use portunus_core::{RateLimit, RejectReason};

    let owner_mgr = Arc::new(OwnerRateLimitScopeManager::new());
    let owner_id = OwnerId::new("alice");
    owner_mgr.install(
        &owner_id,
        Some(&RateLimit {
            concurrent_connections: Some(1),
            ..Default::default()
        }),
    );
    let owner_limiter = Arc::new(OwnerRateLimitHandle::new(owner_id, Arc::clone(&owner_mgr)));
    let owner_stats = Arc::new(RateLimitStatsAccumulator::new());

    // Reuse the existing failover harness in this file. Find a test
    // above this block (look near line 200-400 of failover_path.rs)
    // that builds a multi-target rule; copy its plumbing. The four
    // limiter slots on the failover spawn are:
    //   rule_limiter, rule_stats, owner_limiter, owner_stats.
    let (target, target_addr) = spawn_tcp_echo_server().await;
    let (listener, listen_addr) = bind_ephemeral_tcp_listener().await;
    let cancel = CancellationToken::new();
    let task = tokio::spawn(spawn_failover_with_limiters(
        listener,
        target_addr,
        cancel.clone(),
        None,                       // rule_limiter
        None,                       // rule_stats
        Some(Arc::clone(&owner_limiter)),
        Some(Arc::clone(&owner_stats)),
    ));

    // First connection — admitted, kept open.
    let conn_a = tokio::net::TcpStream::connect(listen_addr).await.unwrap();
    // Give the accept guard a moment to land.
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(owner_limiter.active_connections(), 1);
    assert_eq!(owner_stats.reject_total(RejectReason::OwnerConcurrent), 0);

    // Second connection — accepted by kernel then RST'd by owner gate.
    let conn_b = tokio::net::TcpStream::connect(listen_addr).await.unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(owner_limiter.active_connections(), 1, "rejected conn must not bump active");
    assert_eq!(
        owner_stats.reject_total(RejectReason::OwnerConcurrent),
        1,
        "OwnerConcurrent must tick"
    );

    drop(conn_a);
    drop(conn_b);
    cancel.cancel();
    task.await.unwrap();
    drop(target);
}
```

If `build_failover_rule` / `run_failover_forwarder` / `spawn_tcp_echo_server` don't exist verbatim, locate the existing failover tests above this block and rename.

- [ ] **Step 3: Run**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client forwarder::failover_path::tests::t073_owner_concurrent_cap_rejects_second_failover_accept -- --nocapture
```

Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add crates/portunus-client/src/forwarder/failover_path.rs
git commit -m "test(owner-cap): multi-target TCP owner-concurrent rejection"
```

---

## Chunk 3: SNI Dispatcher — Thread Limiters

This is the actual code change. Tasks 1-4 are coverage; Task 5 is the only one that modifies production behavior.

### Task 5: Thread `(rule_rate_limit, owner_rate_limit)` Through The SNI Dispatcher

**Files:**
- Modify: `crates/portunus-client/src/port_groups.rs` (extend `GroupMember`, update `apply_push`, update `rebuild_watches`, add tests)
- Modify: `crates/portunus-client/src/forwarder/sni/listener.rs` (extend `SniRuleSlot`, replace four `None`s, rewrite comment)

- [ ] **Step 1: Write the failing reproducer test in `port_groups.rs`**

Append to the `#[cfg(test)] mod tests` block in `crates/portunus-client/src/port_groups.rs`:

```rust
#[tokio::test]
async fn t074_apply_push_carries_owner_rate_limit_into_group_member() {
    use crate::forwarder::rate_limit::scope::{
        OwnerId, OwnerRateLimitHandle, OwnerRateLimitScopeManager,
    };
    use portunus_core::RateLimit;

    // Build a capped SNI rule.
    for port in 50_200..50_300 {
        let mut mgr = PortGroupManager::new();
        let mut r = rule(1, port, 9001, Some("api.example.com"));

        let owner_mgr = Arc::new(OwnerRateLimitScopeManager::new());
        let owner_id = OwnerId::new("alice");
        owner_mgr.install(&owner_id, Some(&RateLimit {
            concurrent_connections: Some(1),
            ..Default::default()
        }));
        let owner_handle = Arc::new(OwnerRateLimitHandle::new(owner_id, Arc::clone(&owner_mgr)));
        r.owner_rate_limit = Some(Arc::clone(&owner_handle));
        // Note: `owner_rate_limit_stats` is not set here — Step 1 only
        // asserts the handle field travels into GroupMember.

        if mgr.apply_push(r, live_resolver()).is_ok() {
            let member = mgr
                .group_member_for_test(/*listen_port=*/ port, /*rule_id=*/ RuleId(1))
                .expect("member registered");
            assert!(
                member.owner_rate_limit.is_some(),
                "GroupMember must carry the owner_rate_limit handle"
            );
            mgr.shutdown();
            return;
        }
    }
    panic!("could not bind a free port in 50200..50300");
}
```

This requires a `group_member_for_test` accessor and a `RuleId` constructor — see Step 3.

- [ ] **Step 2: Run the test to confirm it FAILS**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client port_groups::tests::t074_apply_push_carries_owner_rate_limit_into_group_member -- --nocapture
```

Expected: **FAIL** — either with `error[E0609]: no field 'owner_rate_limit' on type 'GroupMember'` (the struct lacks the field) OR `error[E0599]: no method 'group_member_for_test'` (the accessor doesn't exist yet). Both are expected. Read the compile error; do not edit other files yet.

- [ ] **Step 3: Extend `GroupMember` with the four limiter fields**

Edit `crates/portunus-client/src/port_groups.rs:79-91` from:

```rust
#[derive(Clone)]
struct GroupMember {
    rule_id: RuleId,
    sni_pattern: Option<String>,
    target: Target,
    target_port: u16,
    proxy_protocol: Option<portunus_core::ProxyProtocolVersion>,
    prefer_ipv6: bool,
    listen_port: u16,
    stats: Arc<RuleStats>,
    sni_route_exact_total: Arc<AtomicU64>,
    sni_route_wildcard_total: Arc<AtomicU64>,
    sni_route_fallback_total: Arc<AtomicU64>,
}
```

to:

```rust
#[derive(Clone)]
struct GroupMember {
    rule_id: RuleId,
    sni_pattern: Option<String>,
    target: Target,
    target_port: u16,
    proxy_protocol: Option<portunus_core::ProxyProtocolVersion>,
    prefer_ipv6: bool,
    listen_port: u16,
    stats: Arc<RuleStats>,
    sni_route_exact_total: Arc<AtomicU64>,
    sni_route_wildcard_total: Arc<AtomicU64>,
    sni_route_fallback_total: Arc<AtomicU64>,
    // Limiter handles for the rule and its owner. `None` means uncapped
    // on that scope. Plumbed end-to-end through `SniRuleSlot` so the
    // SNI dispatcher's `proxy_with_preread_and_prelude` call applies
    // the v0.11 admission gate identically to the legacy accept path.
    rate_limit: Option<Arc<crate::forwarder::rate_limit::scope::RuleRateLimitHandle>>,
    rate_limit_stats: Option<Arc<crate::forwarder::rate_limit::stats::RateLimitStatsAccumulator>>,
    owner_rate_limit: Option<Arc<crate::forwarder::rate_limit::scope::OwnerRateLimitHandle>>,
    owner_rate_limit_stats: Option<Arc<crate::forwarder::rate_limit::stats::RateLimitStatsAccumulator>>,
}
```

- [ ] **Step 4: Have `apply_push` copy the four fields from `ClientRule`**

Edit `crates/portunus-client/src/port_groups.rs:159-174` from:

```rust
let member = GroupMember {
    rule_id: rule.rule_id,
    sni_pattern: rule.sni_pattern.clone(),
    target: rule.target.clone(),
    target_port: rule.target_range.start(),
    proxy_protocol: rule
        .targets
        .first()
        .and_then(|target| target.spec.proxy_protocol),
    prefer_ipv6: rule.prefer_ipv6,
    listen_port,
    stats: Arc::clone(&stats),
    sni_route_exact_total: Arc::clone(&stats.sni_route_exact_total),
    sni_route_wildcard_total: Arc::clone(&stats.sni_route_wildcard_total),
    sni_route_fallback_total: Arc::clone(&stats.sni_route_fallback_total),
};
```

to:

```rust
let member = GroupMember {
    rule_id: rule.rule_id,
    sni_pattern: rule.sni_pattern.clone(),
    target: rule.target.clone(),
    target_port: rule.target_range.start(),
    proxy_protocol: rule
        .targets
        .first()
        .and_then(|target| target.spec.proxy_protocol),
    prefer_ipv6: rule.prefer_ipv6,
    listen_port,
    stats: Arc::clone(&stats),
    sni_route_exact_total: Arc::clone(&stats.sni_route_exact_total),
    sni_route_wildcard_total: Arc::clone(&stats.sni_route_wildcard_total),
    sni_route_fallback_total: Arc::clone(&stats.sni_route_fallback_total),
    rate_limit: rule.rate_limit.clone(),
    rate_limit_stats: rule.rate_limit_stats.clone(),
    owner_rate_limit: rule.owner_rate_limit.clone(),
    owner_rate_limit_stats: rule.owner_rate_limit_stats.clone(),
};
```

- [ ] **Step 5: Add the `group_member_for_test` accessor**

Inside `impl PortGroupManager` in `crates/portunus-client/src/port_groups.rs`, add:

```rust
#[cfg(test)]
pub(crate) fn group_member_for_test(&self, listen_port: u16, rule_id: RuleId) -> Option<&GroupMember> {
    self.groups.get(&listen_port).and_then(|g| g.members.get(&rule_id))
}
```

- [ ] **Step 6: Update the test fixtures in `port_groups.rs` so they still type-check**

Both `#[cfg(test)] mod tests` `rule(...)` builders (around lines 401 and 539 — find them with `rg -n "owner_rate_limit: None" crates/portunus-client/src/port_groups.rs`) already set `owner_rate_limit: None` on `ClientRule`, which is fine — they're populating the `ClientRule` struct, not `GroupMember`. No edit needed here unless rustc errors point at them.

- [ ] **Step 7: Re-run the reproducer**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client port_groups::tests::t074_apply_push_carries_owner_rate_limit_into_group_member -- --nocapture
```

Expected: **PASS**. `GroupMember` now carries the handle.

- [ ] **Step 8: Extend `SniRuleSlot` with the same four fields**

Edit `crates/portunus-client/src/forwarder/sni/listener.rs:124-138` from:

```rust
#[derive(Clone)]
pub struct SniRuleSlot {
    pub rule_id: RuleId,
    pub target: Target,
    pub target_port: u16,
    pub proxy_protocol: Option<portunus_core::ProxyProtocolVersion>,
    pub prefer_ipv6: bool,
    pub listen_port: u16,
    pub stats: Arc<RuleStats>,
    pub sni_route_exact_total: Arc<AtomicU64>,
    pub sni_route_wildcard_total: Arc<AtomicU64>,
    pub sni_route_fallback_total: Arc<AtomicU64>,
}
```

to:

```rust
#[derive(Clone)]
pub struct SniRuleSlot {
    pub rule_id: RuleId,
    pub target: Target,
    pub target_port: u16,
    pub proxy_protocol: Option<portunus_core::ProxyProtocolVersion>,
    pub prefer_ipv6: bool,
    pub listen_port: u16,
    pub stats: Arc<RuleStats>,
    pub sni_route_exact_total: Arc<AtomicU64>,
    pub sni_route_wildcard_total: Arc<AtomicU64>,
    pub sni_route_fallback_total: Arc<AtomicU64>,
    pub rate_limit: Option<Arc<crate::forwarder::rate_limit::scope::RuleRateLimitHandle>>,
    pub rate_limit_stats: Option<Arc<crate::forwarder::rate_limit::stats::RateLimitStatsAccumulator>>,
    pub owner_rate_limit: Option<Arc<crate::forwarder::rate_limit::scope::OwnerRateLimitHandle>>,
    pub owner_rate_limit_stats: Option<Arc<crate::forwarder::rate_limit::stats::RateLimitStatsAccumulator>>,
}
```

- [ ] **Step 9: Have `rebuild_watches` copy the four fields**

Edit `crates/portunus-client/src/port_groups.rs:374-388` from:

```rust
SniRuleSlot {
    rule_id: m.rule_id,
    target: m.target.clone(),
    target_port: m.target_port,
    proxy_protocol: m.proxy_protocol,
    prefer_ipv6: m.prefer_ipv6,
    listen_port: m.listen_port,
    stats: Arc::clone(&m.stats),
    sni_route_exact_total: Arc::clone(&m.sni_route_exact_total),
    sni_route_wildcard_total: Arc::clone(&m.sni_route_wildcard_total),
    sni_route_fallback_total: Arc::clone(&m.sni_route_fallback_total),
},
```

to:

```rust
SniRuleSlot {
    rule_id: m.rule_id,
    target: m.target.clone(),
    target_port: m.target_port,
    proxy_protocol: m.proxy_protocol,
    prefer_ipv6: m.prefer_ipv6,
    listen_port: m.listen_port,
    stats: Arc::clone(&m.stats),
    sni_route_exact_total: Arc::clone(&m.sni_route_exact_total),
    sni_route_wildcard_total: Arc::clone(&m.sni_route_wildcard_total),
    sni_route_fallback_total: Arc::clone(&m.sni_route_fallback_total),
    rate_limit: m.rate_limit.clone(),
    rate_limit_stats: m.rate_limit_stats.clone(),
    owner_rate_limit: m.owner_rate_limit.clone(),
    owner_rate_limit_stats: m.owner_rate_limit_stats.clone(),
},
```

- [ ] **Step 10: Replace the four `None`s at the proxy call site**

Edit `crates/portunus-client/src/forwarder/sni/listener.rs:385-408` from:

```rust
    let res = proxy_with_preread_and_prelude(
        stream,
        Some(preread),
        &resolver,
        slot.rule_id,
        &slot.target,
        slot.target_port,
        slot.prefer_ipv6,
        proxy_prelude,
        cancel,
        Some(Arc::clone(&slot.stats)),
        slot.listen_port,
        // 011-rate-limiting-qos T020/T030: the SNI dispatcher does
        // not currently carry per-rule or per-owner limiters through
        // PortGroupSlot (it pre-dates the cap envelope). Pre-0.11
        // SNI rules stay uncapped; capped SNI rules are routed
        // through the legacy accept_loop instead because rate_limit +
        // sni_pattern together aren't yet plumbed through the port-
        // group cache. Future work will revisit this end-to-end.
        None,
        None,
        None,
        None,
    )
    .await;
```

to:

```rust
    let res = proxy_with_preread_and_prelude(
        stream,
        Some(preread),
        &resolver,
        slot.rule_id,
        &slot.target,
        slot.target_port,
        slot.prefer_ipv6,
        proxy_prelude,
        cancel,
        Some(Arc::clone(&slot.stats)),
        slot.listen_port,
        // v0.11 limiters carried end-to-end through `PortGroupManager`
        // (`GroupMember`) and the per-port watch into `SniRuleSlot`.
        // `try_acquire_layered` inside the proxy call applies the
        // owner-then-rule gate identically to the legacy accept path.
        slot.rate_limit.clone(),
        slot.rate_limit_stats.clone(),
        slot.owner_rate_limit.clone(),
        slot.owner_rate_limit_stats.clone(),
    )
    .await;
```

- [ ] **Step 11: Build to verify the crate compiles**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-client
```

Expected: clean build. If a type mismatch points at `RuleRateLimitHandle` vs `Arc<RuleRateLimitHandle>` or similar, check the field's declared type in `forwarder/mod.rs:130-137` (`pub rate_limit: Option<Arc<...>>`); align imports.

- [ ] **Step 12: Run the existing port_groups tests**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client port_groups -- --nocapture
```

Expected: PASS, including `t074_…` from Step 1. The pre-existing tests (`first_push_binds_listener_second_share_it` etc.) still pass — they construct `ClientRule` with `owner_rate_limit: None` and that path remains valid.

- [ ] **Step 13: Write the SNI enforcement integration test**

Append to the `#[cfg(test)] mod tests` block in `crates/portunus-client/src/port_groups.rs`:

```rust
#[tokio::test]
async fn t075_sni_dispatcher_enforces_owner_concurrent_cap_end_to_end() {
    use crate::forwarder::rate_limit::scope::{
        OwnerId, OwnerRateLimitHandle, OwnerRateLimitScopeManager,
    };
    use crate::forwarder::rate_limit::stats::RateLimitStatsAccumulator;
    use portunus_core::{RateLimit, RejectReason};
    use tokio::io::AsyncWriteExt;

    let owner_mgr = Arc::new(OwnerRateLimitScopeManager::new());
    let owner_id = OwnerId::new("alice");
    owner_mgr.install(&owner_id, Some(&RateLimit {
        concurrent_connections: Some(1),
        ..Default::default()
    }));
    let owner_limiter = Arc::new(OwnerRateLimitHandle::new(owner_id, Arc::clone(&owner_mgr)));
    let owner_stats = Arc::new(RateLimitStatsAccumulator::new());

    let (server, target_addr) = spawn_tcp_echo_server().await;
    for listen_port in 50_300..50_400 {
        let mut mgr = PortGroupManager::new();
        let mut r = rule(1, listen_port, target_addr.port(), Some("api.example.com"));
        r.owner_rate_limit = Some(Arc::clone(&owner_limiter));
        r.owner_rate_limit_stats = Some(Arc::clone(&owner_stats));
        if mgr.apply_push(r, live_resolver()).is_err() {
            continue; // port busy — try the next one
        }

        // Drive a real TLS ClientHello for SNI = "api.example.com" via the
        // existing test helper (see `forwarder::sni::client_hello::build_client_hello`).
        let listen_addr = std::net::SocketAddr::from(([127, 0, 0, 1], listen_port));
        let mut conn_a = tokio::net::TcpStream::connect(listen_addr).await.unwrap();
        conn_a.write_all(
            &crate::forwarder::sni::client_hello::build_client_hello(Some("api.example.com")),
        ).await.unwrap();

        // Give the dispatcher a moment to admit + acquire the owner guard.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(owner_limiter.active_connections(), 1);

        // Second connection — same SNI, should be RST'd by owner gate.
        let mut conn_b = tokio::net::TcpStream::connect(listen_addr).await.unwrap();
        conn_b.write_all(
            &crate::forwarder::sni::client_hello::build_client_hello(Some("api.example.com")),
        ).await.unwrap();

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(owner_limiter.active_connections(), 1, "rejected conn must not bump active");
        assert_eq!(
            owner_stats.reject_total(RejectReason::OwnerConcurrent),
            1,
            "OwnerConcurrent must tick once"
        );

        drop(conn_a);
        drop(conn_b);
        mgr.shutdown();
        drop(server);
        return;
    }
    panic!("could not bind a free port in 50300..50400");
}
```

- [ ] **Step 14: Run the SNI enforcement test**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client port_groups::tests::t075_sni_dispatcher_enforces_owner_concurrent_cap_end_to_end -- --nocapture
```

Expected: PASS. The owner gate fires inside `proxy_with_preread_and_prelude` → `try_acquire_layered`, and the second connection is RST.

If FAIL with `active != 1` after the first connection: the guard isn't being captured. Check that the changes at Step 10 actually compile and aren't shadowed by a stale build artifact — rerun `cargo build` and look at the call site.

- [ ] **Step 15: Run the full client suite to catch regressions**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client -- --nocapture 2>&1 | tail -40
```

Expected: every test passes. In particular `forwarder::sni::*`, `forwarder::tests::*`, `port_groups::tests::*`.

- [ ] **Step 16: Commit**

```bash
git add crates/portunus-client/src/port_groups.rs crates/portunus-client/src/forwarder/sni/listener.rs
git commit -m "feat(sni): thread owner/rule limiters through PortGroupManager into the SNI dispatcher"
```

---

## Chunk 4: Reconnect and Hot-Update Behavior

### Task 6: Confirm Owner Cap Push and Reconnect Replay

**Files:**
- Read: `crates/portunus-server/src/grpc/service.rs` (welcome replay)
- Read: `crates/portunus-client/src/control.rs:887-909` (`apply_owner_rate_limit_update`, calls `update` not `install`)
- Modify (if test missing): `crates/portunus-client/src/forwarder/rate_limit/scope.rs`

- [ ] **Step 1: Confirm `apply_owner_rate_limit_update` uses `update`, not `install`**

```bash
rg -nA 4 "fn apply_owner_rate_limit_update" crates/portunus-client/src/control.rs
```

Expected: at line 909, the call is `owner_rate_limit_scope.update(&owner_id, envelope.as_ref());`. If it ever changes to `install`, drained semantics regress — Step 4's test will catch it.

- [ ] **Step 2: Check whether the lower-cap drain test already exists**

```bash
rg -n "fn.*lower.*cap.*drain|fn.*update_cap.*does_not|fn.*shared.*active_gauge" crates/portunus-client/src/forwarder/rate_limit/scope.rs
```

If a test already covers "cap 5 → 2 guards held → cap 1 → next acquire rejects → drop one → still rejects → drop two → admits", skip to Step 4. Otherwise continue.

- [ ] **Step 3: Add the missing lower-cap drain test**

Append to the `#[cfg(test)] mod tests` block in `crates/portunus-client/src/forwarder/rate_limit/scope.rs`:

```rust
#[tokio::test]
async fn t076_lowering_owner_concurrent_cap_drains_gracefully() {
    use portunus_core::{RateLimit, RejectReason};

    let scope = Arc::new(OwnerRateLimitScopeManager::new());
    let owner_id = OwnerId::new("alice");
    let mk_cap = |n: u32| RateLimit {
        concurrent_connections: Some(n),
        ..Default::default()
    };

    // Initial cap = 5.
    scope.install(&owner_id, Some(&mk_cap(5)));
    let limiter = Arc::new(OwnerRateLimitHandle::new(owner_id, Arc::clone(&scope)));
    let stats = Arc::new(RateLimitStatsAccumulator::new());

    // Helper: drive try_acquire_layered for owner-only (no rule limiter).
    // Mirror the call sites in failover_path.rs:246-247 and udp/mod.rs:376.
    let acquire = || try_acquire_layered(Some(&limiter), None, false);

    let g1 = match acquire() {
        LayeredAcquire::Admitted { owner_guard, .. } => owner_guard.expect("owner guard 1"),
        other => panic!("expected admit 1, got {other:?}"),
    };
    let g2 = match acquire() {
        LayeredAcquire::Admitted { owner_guard, .. } => owner_guard.expect("owner guard 2"),
        other => panic!("expected admit 2, got {other:?}"),
    };
    assert_eq!(limiter.active_connections(), 2);

    // Hot lower to 1 — active guards survive (no kill).
    scope.update(&limiter.owner_id_for_test(), Some(&mk_cap(1)));
    assert_eq!(limiter.active_connections(), 2, "lower cap must not kill live guards");

    // New acquire rejects.
    match acquire() {
        LayeredAcquire::Rejected { reason, .. } => {
            assert_eq!(reason, RejectReason::OwnerConcurrent);
            stats.record_reject(reason);
        }
        other => panic!("expected reject after lower-to-1, got {other:?}"),
    }
    assert_eq!(stats.reject_total(RejectReason::OwnerConcurrent), 1);

    // Drop one guard — still over cap.
    drop(g1);
    assert_eq!(limiter.active_connections(), 1);
    match acquire() {
        LayeredAcquire::Rejected { reason, .. } => {
            assert_eq!(reason, RejectReason::OwnerConcurrent);
            stats.record_reject(reason);
        }
        other => panic!("expected reject at active==cap, got {other:?}"),
    }
    assert_eq!(stats.reject_total(RejectReason::OwnerConcurrent), 2);

    // Drop the second — under cap.
    drop(g2);
    assert_eq!(limiter.active_connections(), 0);
    let _g3 = match acquire() {
        LayeredAcquire::Admitted { owner_guard, .. } => owner_guard.expect("admit after drain"),
        other => panic!("expected admit after drain, got {other:?}"),
    };
    assert_eq!(limiter.active_connections(), 1);
}
```

The test introduces a small accessor `owner_id_for_test(&self) -> &OwnerId` on `OwnerRateLimitHandle` — add it under `#[cfg(test)]` near the existing handle methods. If `try_acquire_layered`'s `LayeredAcquire` shape doesn't match exactly (variant names / fields), use whatever shape the existing failover or udp call sites use; the names above mirror the call site at `failover_path.rs:246-247`. The point is: admit returns a guard, reject returns a reason; the test only needs to distinguish those two outcomes.

- [ ] **Step 4: Run the focused tests**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client rate_limit::scope -- --nocapture
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client control::tests::owner -- --nocapture
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib grpc::service -- --nocapture
```

Expected: PASS, including any new test from Step 3.

- [ ] **Step 5: Commit**

```bash
git add crates/portunus-client/src/forwarder/rate_limit/scope.rs
git commit -m "test(owner-cap): lower-cap drains existing guards gracefully"
```

---

## Chunk 5: v1.3.0 splice Interaction

### Task 7: Confirm `concurrent_connections` Does Not Disable Splice

**Files:**
- Read: `crates/portunus-client/src/forwarder/splice.rs:42` (`CopyCtx` — note: **NOT** `SpliceCtx`)
- Read: `crates/portunus-client/src/forwarder/splice.rs:818, 882` (existing concurrent-only tests)
- Modify: `crates/portunus-client/src/forwarder/splice.rs` (add `new_connections_per_sec` test)
- Modify: `crates/portunus-client/src/forwarder/mod.rs` (add Linux-only guard-lifetime test)

- [ ] **Step 1: Confirm the two existing concurrent-only tests pass**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client forwarder::splice::tests::rule_with_concurrent_only_does_not_force_userspace -- --nocapture
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client forwarder::splice::tests::owner_concurrent_only_does_not_force_userspace -- --nocapture
```

Expected: PASS. Read each test (at lines 818 and 882) to learn the `CopyCtx`-building helper they use — Step 2 needs it.

- [ ] **Step 2: Add the `new_connections_per_sec`-only invariant test**

Append next to the existing tests in `crates/portunus-client/src/forwarder/splice.rs` `#[cfg(test)] mod tests`:

```rust
#[test]
fn t077_owner_new_connections_per_sec_only_does_not_force_userspace() {
    use portunus_core::RateLimit;
    // Reuse the same fixture pattern as
    // owner_concurrent_only_does_not_force_userspace at splice.rs:882.
    let ctx = make_ctx_with_owner_rl(&RateLimit {
        new_connections_per_sec: Some(50),
        concurrent_connections: None,
        bandwidth_in_bps: None,
        bandwidth_out_bps: None,
        bandwidth_in_burst: None,
        bandwidth_out_burst: None,
        new_connections_burst: None,
    });
    assert!(eligible(&ctx), "owner new_connections_per_sec must not disable splice");
}

#[test]
fn t078_rule_new_connections_per_sec_only_does_not_force_userspace() {
    use portunus_core::RateLimit;
    let ctx = make_ctx_with_rule_rl(&RateLimit {
        new_connections_per_sec: Some(50),
        concurrent_connections: None,
        bandwidth_in_bps: None,
        bandwidth_out_bps: None,
        bandwidth_in_burst: None,
        bandwidth_out_burst: None,
        new_connections_burst: None,
    });
    assert!(eligible(&ctx), "rule new_connections_per_sec must not disable splice");
}
```

`make_ctx_with_owner_rl` / `make_ctx_with_rule_rl` are the helper names that the existing tests at lines 818 and 882 use. If their names differ, rename to match.

- [ ] **Step 3: Run the two new tests**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client forwarder::splice::tests::t077_owner_new_connections_per_sec_only_does_not_force_userspace -- --nocapture
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client forwarder::splice::tests::t078_rule_new_connections_per_sec_only_does_not_force_userspace -- --nocapture
```

Expected: PASS.

- [ ] **Step 4: Add the splice guard-lifetime integration test (Linux-only)**

Append to the `#[cfg(test)] mod tests` block in `crates/portunus-client/src/forwarder/mod.rs`:

```rust
#[cfg(target_os = "linux")]
#[tokio::test]
async fn t079_owner_concurrent_guard_spans_splice_connection_lifetime() {
    use crate::forwarder::rate_limit::scope::{
        OwnerId, OwnerRateLimitHandle, OwnerRateLimitScopeManager,
    };
    use portunus_core::RateLimit;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let owner_mgr = Arc::new(OwnerRateLimitScopeManager::new());
    let owner_id = OwnerId::new("alice");
    owner_mgr.install(&owner_id, Some(&RateLimit {
        concurrent_connections: Some(1),
        ..Default::default()
    }));
    let owner_handle = Arc::new(OwnerRateLimitHandle::new(
        owner_id.clone(),
        Arc::clone(&owner_mgr),
    ));

    let (server, target_addr) = spawn_tcp_echo_server().await; // existing helper
    let (listener, listen_addr) = bind_ephemeral_tcp().await;
    let rule = build_plain_tcp_rule(
        /*id=*/ 1,
        listen_addr.port(),
        target_addr,
        owner_handle.clone(),
        /*rule_rl=*/ None,
    );

    let cancel = CancellationToken::new();
    let task = tokio::spawn(run_plain_tcp_forwarder(rule, listener, cancel.clone()));

    let mut conn = tokio::net::TcpStream::connect(listen_addr).await.unwrap();
    conn.write_all(b"ping").await.unwrap();
    let mut buf = [0u8; 4];
    conn.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"ping");

    // Splice connection is live — owner active gauge must read 1.
    assert_eq!(
        owner_handle.active_connections(),
        1,
        "guard must persist for the duration of the splice connection"
    );

    // Close upstream — guard must drop.
    drop(conn);
    // Wait briefly for the forwarder task to observe FIN and drop the guard.
    for _ in 0..50 {
        if owner_handle.active_connections() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        owner_handle.active_connections(),
        0,
        "guard must drop when connection closes"
    );

    cancel.cancel();
    task.await.unwrap();
    drop(server);
}
```

If `build_plain_tcp_rule` / `run_plain_tcp_forwarder` / `spawn_tcp_echo_server` / `bind_ephemeral_tcp` aren't the exact helper names already in `forwarder/mod.rs` `#[cfg(test)]`, rename to whatever is already there — every existing `t0XX_*` integration test in that block already uses such helpers, copy the pattern from the closest neighbour.

- [ ] **Step 5: Run the Linux-only integration test**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client forwarder::tests::t079_owner_concurrent_guard_spans_splice_connection_lifetime -- --nocapture
```

Expected on Linux: PASS. On macOS: the test is `#[cfg(target_os = "linux")]`-gated and is skipped — the runner reports `0 passed; 0 failed; 1 ignored`. If you're on macOS for development, run on the Linux bench host or in CI.

- [ ] **Step 6: Commit**

```bash
git add crates/portunus-client/src/forwarder/splice.rs crates/portunus-client/src/forwarder/mod.rs
git commit -m "test(splice): new_connections_per_sec eligibility + owner-guard spans splice lifetime"
```

---

## Chunk 6: Operator Experience

### Task 8: CLI Smoke For `owner-cap set --concurrent-connections`

**Files:**
- Read: `crates/portunus-server/src/operator/owner_cap_cli.rs` (CLI handlers at lines 8, 259, 282, 303)
- Modify (if missing): `crates/portunus-server/src/operator/owner_cap_cli.rs` (add test)

The CLI shape from `owner_cap_cli.rs:6-9`:

```text
portunus-server owner-cap list <client>
portunus-server owner-cap get  <client> <owner>
portunus-server owner-cap set  <client> <owner> [--bandwidth-in-bps N | --bandwidth-out-bps N | --new-connections-per-sec N | --concurrent-connections N | --bandwidth-in-burst N | --bandwidth-out-burst N | --new-connections-burst N]
portunus-server owner-cap delete <client> <owner>
```

- [ ] **Step 1: Confirm `set` accepts a lone `--concurrent-connections`**

```bash
rg -n "concurrent.connections|rate_limit_no_caps_provided" crates/portunus-server/src/operator/owner_cap_cli.rs
```

Confirm `owner_cap_cli.rs:259` lists `--concurrent-connections` in its validation message:
`"error: validation.rate_limit_no_caps_provided (set at least one --bandwidth-in-bps, --bandwidth-out-bps, --new-connections-per-sec, or --concurrent-connections)"`

That means `--concurrent-connections N` alone is valid.

- [ ] **Step 2: Check for existing CLI test coverage**

```bash
rg -n "owner_cap.*concurrent|cli.*concurrent|owner-cap set" crates/portunus-server/src/operator/owner_cap_cli.rs crates/portunus-server/tests
```

If a CLI test already exercises `set ... --concurrent-connections N`, skip to Step 4.

- [ ] **Step 3: Add a focused CLI test if missing**

Append to the test module in `crates/portunus-server/src/operator/owner_cap_cli.rs` (or its sibling test file if the existing CLI tests are external):

```rust
#[tokio::test]
async fn t080_owner_cap_set_with_concurrent_only_round_trips() {
    // Use the same HTTP-backed CLI harness as the existing tests in
    // this file. Look near `fn set` for the test fixture pattern.
    let env = TestCliEnv::start().await;
    env.register_fake_client("edge-01", "0.11.0").await;

    let out = env
        .run_owner_cap(&["set", "edge-01", "alice", "--concurrent-connections", "100"])
        .await
        .expect("cli ok");

    // Text mode output shape from owner_cap_cli.rs:282.
    assert!(out.stdout.contains("owner-cap set"));
    assert!(out.stdout.contains("client=edge-01"));
    assert!(out.stdout.contains("owner=alice"));

    // JSON-mode round-trip.
    let out_json = env
        .run_owner_cap(&["get", "edge-01", "alice", "--format", "json"])
        .await
        .expect("cli ok");
    let parsed: serde_json::Value = serde_json::from_str(&out_json.stdout).unwrap();
    assert_eq!(parsed["rate_limit"]["concurrent_connections"], 100);
    assert!(parsed["rate_limit"]["bandwidth_in_bps"].is_null());
    assert!(parsed["rate_limit"]["new_connections_per_sec"].is_null());
}
```

If `TestCliEnv` / `run_owner_cap` don't exist verbatim, locate the existing CLI test helper above this block and rename — typically `cli_runner` / `cmd!` / similar in `crates/portunus-server/tests`.

- [ ] **Step 4: Run**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server owner_cap -- --nocapture
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/portunus-server/src/operator/owner_cap_cli.rs
# also stage any external test file under crates/portunus-server/tests/ that was touched
git commit -m "test(cli): owner-cap set --concurrent-connections round-trip"
```

### Task 9: Web UI Owner Quotas Manual Smoke

**Files:** none (manual smoke after the server-side changes land).

- [ ] **Step 1: Start the dev stack**

```bash
make dev
```

Wait for the `temporary_password=…` banner if this is a first-run on this data dir, then open <http://localhost:5173> and log in as `_superadmin`.

- [ ] **Step 2: Provision a fake client**

Use the CLI in a second terminal:

```bash
portunus-server client register edge-01 --version 0.11.0
```

If the dev stack uses a different helper, follow the existing webui dev docs. The point is to have one client at version `>= 0.11.0` visible in the UI.

- [ ] **Step 3: Add a concurrent-only owner cap via the UI**

In the Web UI:
1. Navigate to `Clients → edge-01 → Owner quotas` tab.
2. Click `Add owner quota`. Enter `owner_id = alice`. Set only `concurrent_connections = 100`. Leave bandwidth and new-conn fields empty.
3. Save.

Expected: the row appears; the value `100` is shown.

- [ ] **Step 4: Round-trip the value**

1. Reload the page (`Cmd/Ctrl + R`).
2. Verify the row still shows `concurrent_connections = 100`.
3. Edit the row to `concurrent_connections = 50`. Save.
4. Reload. Verify it shows `50`.
5. In a terminal: `curl -s -H "Authorization: Bearer $TOKEN" http://localhost:7080/v1/clients/edge-01/owners/alice/rate-limit | jq` — confirm `rate_limit.concurrent_connections == 50` and other rate-limit fields are `null`.

- [ ] **Step 5: Delete and verify 404**

1. In the UI, delete the row.
2. `curl` again — expect `404`.

If any step fails, fix in `webui/src/...` and re-test. Commit with a `fix(webui)` prefix.

---

## Chunk 7: End-to-End Confidence

### Task 10: Server-Client Integration Smoke (Committed, Not Optional)

**Files:**
- Create: `crates/portunus-e2e/tests/owner_connection_limit.rs`
- Read: `crates/portunus-e2e/src/lib.rs` and existing tests under `crates/portunus-e2e/tests/` for the helper style.

- [ ] **Step 1: Read the existing e2e harness shape**

```bash
ls crates/portunus-e2e/tests
rg -n "fn .*fixture|start_server|provision_client|push_rule" crates/portunus-e2e/src/lib.rs | head -20
```

Identify the existing fixture pattern. Reuse it verbatim — do not invent a parallel helper layer.

- [ ] **Step 2: Write the failing test (held-connection + reject)**

Create `crates/portunus-e2e/tests/owner_connection_limit.rs`:

```rust
//! E2E: owner concurrent_connections cap enforced on a real
//! portunus-client over a real gRPC push, surviving server restart.

use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test(flavor = "multi_thread")]
async fn t081_owner_concurrent_cap_one_allowed_second_rst() {
    let mut env = portunus_e2e::Env::start_with_client("edge-01", "0.11.0").await;
    let echo = env.spawn_tcp_echo_target().await;
    let listen_port = env.allocate_listen_port();

    // Push a TCP rule owned by alice.
    env.http_post_rule(serde_json::json!({
        "rule_id": "r-1",
        "owner_id": "alice",
        "client_name": "edge-01",
        "protocol": "Tcp",
        "listen_range": { "start": listen_port, "end": listen_port },
        "target": { "host": echo.host(), "port_start": echo.port(), "port_end": echo.port() },
    })).await.expect_status(200);

    // Set the cap.
    env.http_put(
        "/v1/clients/edge-01/owners/alice/rate-limit",
        &serde_json::json!({ "concurrent_connections": 1 }),
    ).await.expect_status(200);

    // Wait for the push to land on the client.
    env.wait_for_owner_cap_applied("edge-01", "alice").await;

    // First connection — admitted.
    let mut conn_a = tokio::net::TcpStream::connect(("127.0.0.1", listen_port)).await.unwrap();
    conn_a.write_all(b"hello").await.unwrap();
    let mut buf = [0u8; 5];
    conn_a.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"hello");

    // Second connection — accepted then closed before any echo.
    let conn_b = tokio::net::TcpStream::connect(("127.0.0.1", listen_port)).await.unwrap();
    let mut buf_b = [0u8; 1];
    let res = tokio::time::timeout(Duration::from_millis(500), {
        let mut conn_b = conn_b;
        async move { conn_b.read(&mut buf_b).await }
    }).await;
    // FIN / RST encodes as Ok(0) or an error; either is acceptable.
    match res {
        Ok(Ok(0)) | Ok(Err(_)) => {}
        Ok(Ok(n)) => panic!("rejected connection should not deliver bytes, got {n}"),
        Err(_) => panic!("rejected connection should close, not hang"),
    }

    // Stats: server pulls owner stats via the existing stats-report path.
    let stats = env.fetch_owner_stats("edge-01", "alice").await;
    assert!(stats.rejects_concurrent >= 1, "OwnerConcurrent must tick");

    drop(conn_a);
}
```

If `Env::start_with_client` / `http_post_rule` / `wait_for_owner_cap_applied` / `fetch_owner_stats` aren't on the existing harness verbatim, **rename** to whatever the harness exposes. The semantic shape is what matters.

- [ ] **Step 3: Add the persistence round-trip test**

Append to the same file:

```rust
#[tokio::test(flavor = "multi_thread")]
async fn t082_owner_cap_survives_server_and_client_restart() {
    let mut env = portunus_e2e::Env::start_with_client("edge-01", "0.11.0").await;
    let echo = env.spawn_tcp_echo_target().await;
    let listen_port = env.allocate_listen_port();
    env.http_post_rule(serde_json::json!({
        "rule_id": "r-1",
        "owner_id": "alice",
        "client_name": "edge-01",
        "protocol": "Tcp",
        "listen_range": { "start": listen_port, "end": listen_port },
        "target": { "host": echo.host(), "port_start": echo.port(), "port_end": echo.port() },
    })).await.expect_status(200);
    env.http_put(
        "/v1/clients/edge-01/owners/alice/rate-limit",
        &serde_json::json!({ "concurrent_connections": 1 }),
    ).await.expect_status(200);
    env.wait_for_owner_cap_applied("edge-01", "alice").await;

    // Stop the CLIENT first (drops in-memory cap state).
    env.stop_client("edge-01").await;
    // Then stop the SERVER.
    env.stop_server().await;

    // Restart both with the same data dir / client bundle.
    env.restart_server_with_same_data_dir().await;
    env.restart_client("edge-01", "0.11.0").await;

    // Confirm the persisted row is still in SQLite, and that the welcome
    // replay pushed it back to the client.
    let get = env.http_get("/v1/clients/edge-01/owners/alice/rate-limit").await;
    get.expect_status(200);
    assert_eq!(get.json()["rate_limit"]["concurrent_connections"], 1);

    env.wait_for_owner_cap_applied("edge-01", "alice").await;

    // Cap is enforced as before.
    let mut conn_a = tokio::net::TcpStream::connect(("127.0.0.1", listen_port)).await.unwrap();
    conn_a.write_all(b"after-restart").await.unwrap();
    let mut buf = [0u8; 13];
    conn_a.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"after-restart");

    let conn_b = tokio::net::TcpStream::connect(("127.0.0.1", listen_port)).await.unwrap();
    let mut buf_b = [0u8; 1];
    let res = tokio::time::timeout(Duration::from_millis(500), {
        let mut conn_b = conn_b;
        async move { conn_b.read(&mut buf_b).await }
    }).await;
    match res {
        Ok(Ok(0)) | Ok(Err(_)) => {}
        other => panic!("post-restart cap broken: {other:?}"),
    }

    drop(conn_a);
}
```

If `stop_client` / `stop_server` / `restart_*` helpers don't exist, add them to `crates/portunus-e2e/src/lib.rs` — but read the existing process-management helpers first; one of them likely covers this with a different name (e.g. `kill_and_restart`).

- [ ] **Step 4: Run both e2e tests**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-e2e --test owner_connection_limit -- --nocapture
```

Expected: PASS for both.

- [ ] **Step 5: Commit**

```bash
git add crates/portunus-e2e/tests/owner_connection_limit.rs
# Also stage any helper additions to crates/portunus-e2e/src/lib.rs.
git commit -m "test(e2e): owner concurrent cap enforced + survives server/client restart"
```

---

## Chunk 8: Final Verification

### Task 11: Format, Clippy, Workspace Tests, Handoff

**Files:** any modified files.

- [ ] **Step 1: Format only the crates this plan touched**

```bash
cd crates/portunus-server && cargo fmt && cd -
cd crates/portunus-client && cargo fmt && cd -
cd crates/portunus-e2e && cargo fmt && cd -
```

This avoids drive-by reformatting of unrelated crates. CI runs `cargo fmt --all --check` as the gate; any miss surfaces upstream.

- [ ] **Step 2: Clippy (matches CI gate)**

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: zero warnings. The workspace sets `clippy::pedantic = warn`; any new `allow` must be accompanied by a comment justifying why.

- [ ] **Step 3: Re-run all focused tests in order**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --test rate_limit_owner_contract t070 -- --nocapture
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --test rate_limit_owner_contract t071 -- --nocapture
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client forwarder::tests::t030_owner_cap_binds_before_rule_cap_on_tcp_accept -- --nocapture
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client forwarder::udp::tests::t072 -- --nocapture
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client forwarder::failover_path::tests::t073 -- --nocapture
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client port_groups::tests::t074 -- --nocapture
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client port_groups::tests::t075 -- --nocapture
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client rate_limit::scope::tests::t076 -- --nocapture
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client forwarder::splice::tests::t077 -- --nocapture
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client forwarder::splice::tests::t078 -- --nocapture
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client forwarder::tests::t079 -- --nocapture
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server owner_cap -- --nocapture
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-e2e --test owner_connection_limit -- --nocapture
```

Expected: each PASS.

- [ ] **Step 4: Workspace test pass (catches accidental regressions)**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test --workspace
```

Expected: PASS.

- [ ] **Step 5: Write the PR description**

In the PR description, state these facts explicitly:

- **Scope:** `(client_name, owner_id)`-keyed `concurrent_connections` cap.
- **Pathways covered:** plain TCP accept (Task 2), UDP first-packet (Task 3), multi-target TCP failover (Task 4), SNI dispatcher (Task 5 — production code change), reconnect / hot-reload (Task 6).
- **v1.3.0 splice:** `concurrent_connections`-only owner caps do NOT disable the splice fast path (Task 7); the `ActiveGuard` spans the entire splice connection (Task 7 Step 4).
- **Lower-cap drain:** existing connections survive; new acquisitions reject (Task 6 Step 3).
- **0.11 capability gate:** PUT against a `< 0.11.0` client returns `422 rate_limit_unsupported_by_client`; no row is persisted; no push emitted (Task 1 Step 4).
- **Persistence:** the cap survives a server restart even when the client also restarts (Task 10 Step 3).

- [ ] **Step 6: Final commit if any test names or helper names were renamed during execution**

```bash
git status
# If anything is dirty, stage and commit with a clear message.
git add -p
git commit -m "chore: align test helper names with existing fixtures"
```

---

## Notes

- **No new dependencies.** All four new fields on `GroupMember` / `SniRuleSlot` use types already imported in `forwarder/rate_limit/scope.rs` and `stats.rs`.
- **No new endpoint or table.** The v0.11 `OwnerRateLimitUpdate` protobuf and SQLite row schema already carry `concurrent_connections`.
- **No new wire-format changes.** The SNI threading is internal client-side plumbing.
- **Delete stale code cleanly.** Replace the `011-rate-limiting-qos T020/T030: ... future work will revisit` comment at `sni/listener.rs:397-407` with the one-liner at Step 10 — do not leave a `// kept for context` marker.
- **CLAUDE.md / AGENTS.md** describe the workspace-level invariants. If any new lint warning appears under `clippy::pedantic`, prefer adjusting the code over adding `#[allow(...)]`; only add `allow` with a comment explaining why.

# Owner Client Connection Limit — Verification & SNI Gap Closure

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prove that the existing `(client_name, owner_id)`-scoped `concurrent_connections` cap shipped in v0.11 is fully exercised, fix the SNI dispatcher path that today silently drops owner / rule limiters when constructing `GroupMember`, and confirm v1.3.0 splice does not regress the gate.

**This is not a new feature.** v0.11 already shipped the data model, persistence, server push, client enforcement (`try_acquire_layered`), and Web UI for owner caps including `concurrent_connections`. This plan locks down verification, documents one intentional v0.11 gap (SNI), and adds the v1.3.0 interaction tests that didn't exist when this work was first sketched.

**Architecture:** Reuse the existing v0.11 owner envelope keyed by `(client_name, owner_id)`. Control plane persists and pushes `OwnerRateLimitUpdate`. portunus-client applies the owner-scope admission gate before the per-rule gate. Lowering the cap rejects only new work; existing connections drain naturally (`OwnerRateLimitScopeManager::update`, not `install`).

**Tech Stack:** Rust 2024, Tokio, tonic/prost, axum, SQLite/rusqlite, existing `portunus_core::RateLimit` and `forwarder::rate_limit` modules.

---

## Up-front risk callouts

These three things need to be true at the end. They are NOT all confirmed today; read them before starting Chunk 2.

1. **Capped SNI rules currently lose their limiter.** Tracing the code path:
   - `control.rs:758-761` routes EVERY TCP single-port rule with `sni_pattern.is_some()` into `port_groups.apply_push` — there is **no** "capped → legacy" diversion.
   - `port_groups.rs:159-174` builds `GroupMember` from the incoming `ClientRule` and explicitly does NOT capture `rate_limit` / `owner_rate_limit` / their stats. Those fields are dropped here.
   - `forwarder/sni/listener.rs:404` then passes four explicit `None` slots into the proxy call.

   Net effect: a rule with `sni_pattern = Some(_)` AND an owner `concurrent_connections` cap is admitted unconditionally — the cap is silently lost. The comment at `sni/listener.rs:397-407` describing a "legacy accept_loop" diversion is historical context from when the routing was different; it does not match current code. **Task 5 defaults to "upgrade the dispatcher to thread limiters through `GroupMember`+`PortGroupSlot`"** unless step 1 finds an upstream gate I missed.

2. **v1.3.0 splice eligibility ignores `concurrent_connections`.** `forwarder/splice.rs:93-94` computes `has_bandwidth_cap` from `{rule, owner}` bandwidth caps only. A rule with only an owner `concurrent_connections` cap **still goes through splice**. We need an explicit invariant test (Chunk 4) so this can't silently regress.

3. **The OwnerConcurrent guard must live for the full connection, splice or not.** The accept-time gate returns an `ActiveGuard`; if a splice connection task drops it before the byte stream ends, the `concurrent_connections` gauge under-counts. Verify the guard is captured into the forwarder task, not the accept frame.

## Scope

In scope:
- `concurrent_connections` as an owner-level cap for all rules owned by `owner_id` on one `client_name`.
- TCP active connections and UDP active NAT flows.
- Splice-on / splice-off byte-stability for the owner gate.
- Server HTTP API, CLI, persistence, server-to-client push, reconnect hydration, client enforcement, stats/metrics, Web UI smoke.
- SNI routing path (decide between "legacy enforcement is sufficient" and "upgrade dispatcher" — Task 5).

Out of scope:
- Cluster-wide user caps across multiple portunus-client agents.
- Putting quota fields on RBAC grants.
- Killing existing connections when a cap is lowered.
- Per-bandwidth owner caps (already covered by v0.11 `bandwidth_in_bps` / `bandwidth_out_bps`).

## File Map

- `crates/portunus-core/src/rate_limit.rs` — cap envelope + validation.
- `crates/portunus-server/src/operator/owner_cap.rs` — REST handlers.
- `crates/portunus-server/src/operator/owner_cap_cli.rs` — CLI wrapper.
- `crates/portunus-server/src/owner.rs` — service, cache, validation, lifecycle.
- `crates/portunus-server/src/store/owner_cap_store.rs` — SQLite persistence.
- `crates/portunus-server/src/grpc/service.rs` — Welcome / reconnect replay.
- `crates/portunus-client/src/control.rs` — applies `OwnerRateLimitUpdate`; wires owner handles into rules.
- `crates/portunus-client/src/forwarder/rate_limit/scope.rs` — `try_acquire_layered`, `OwnerRateLimitScopeManager::{install,update}`.
- `crates/portunus-client/src/forwarder/mod.rs` — plain TCP accept path (test `t030_owner_cap_binds_before_rule_cap_on_tcp_accept` already present).
- `crates/portunus-client/src/forwarder/failover_path.rs` — multi-target TCP accept path.
- `crates/portunus-client/src/forwarder/udp/mod.rs` — UDP first-packet path (test `t030_owner_cap_binds_before_rule_cap_on_udp_first_packet` already present; UDP `owner_admit_guard` plumbed at `udp/mod.rs:191,271,553,616,711,772`).
- `crates/portunus-client/src/forwarder/proxy.rs` + `splice.rs` — eligibility check (`has_bandwidth_cap` from `{rule, owner}` bandwidth caps; concurrent does NOT block splice).
- `crates/portunus-client/src/port_groups.rs` — SNI port-group manager.
- `crates/portunus-client/src/forwarder/sni/listener.rs` — SNI dispatcher; the four `None` slots at line 404 are the real evidence of the SNI gap.
- `crates/portunus-server/tests/rate_limit_owner_contract.rs` — HTTP contract.
- `crates/portunus-e2e/tests/` — add one server-client smoke (Task 8, committed not optional).
- `webui/` — Owner quotas tab (v0.11 already shipped; verification only).

## Chunk 1: Contract Pinning

### Task 1: Pin Owner Concurrent API Semantics

**Files:**
- Modify: `crates/portunus-server/tests/rate_limit_owner_contract.rs`
- Read: `crates/portunus-server/src/operator/owner_cap.rs`
- Read: `crates/portunus-server/src/operator/owner_cap_cli.rs`

- [ ] **Step 1: Focused HTTP contract test — concurrent-only cap on a 0.11.0 client**

Register a fake `0.11.0` client; send `PUT /v1/clients/{client}/owners/alice/rate-limit`:

```json
{ "concurrent_connections": 100 }
```

Assert:
- 200 response; body contains `rate_limit.concurrent_connections = 100` and the other rate-limit fields are `null` / unset (not overwritten to defaults).
- `GET` returns the same value.
- Captured server push is `OwnerRateLimitUpdate { action = SET, owner_id = "alice", rate_limit.concurrent_connections = 100 }`.

- [ ] **Step 2: Focused HTTP contract test — capability gate on < 0.11.0 client**

Register a fake `0.10.0` client; send the same PUT. Assert:
- Response is `422 rate_limit_unsupported_by_client` (the v0.11 gate at `operator/owner_cap.rs:138` via `version_at_least_0_11`).
- No `OwnerRateLimitUpdate` is emitted on the gRPC push channel for that client.
- A subsequent `GET` on the same path returns 404 / not-set (no row was persisted).

- [ ] **Step 3: Run the focused contract tests**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --test rate_limit_owner_contract concurrent -- --nocapture
```

Expected: PASS for both tests. Fix only contract drift inside `owner_cap.rs` / `owner_cap_cli.rs` / `owner_cap_store.rs`. Do not create parallel endpoints.

## Chunk 2: Client-Side Enforcement Coverage

### Task 2: Verify Plain TCP Owner Cap Path

**Files:** `crates/portunus-client/src/forwarder/mod.rs`, `crates/portunus-client/src/forwarder/rate_limit/scope.rs`

- [ ] **Step 1: Confirm `t030_owner_cap_binds_before_rule_cap_on_tcp_accept` proves**

  - owner gate runs before rule gate,
  - surplus accept increments `OwnerConcurrent`,
  - rule reject counter remains unchanged,
  - active owner and rule gauges do not over-count rejected accepts.

- [ ] **Step 2: Run**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client forwarder::tests::t030_owner_cap_binds_before_rule_cap_on_tcp_accept -- --nocapture
```

Expected: PASS.

### Task 3: Verify UDP Owner `concurrent_connections` Enforcement

**Files:** `crates/portunus-client/src/forwarder/udp/mod.rs`, `crates/portunus-client/src/forwarder/rate_limit/scope.rs`

**Pre-step — confirm implementation exists, not just `new_connections_per_sec`:**

```bash
rg -n "owner_admit_guard|OwnerConcurrent" crates/portunus-client/src/forwarder/udp/mod.rs
```

Expect hits at `udp/mod.rs:191, 271, 553, 616, 711, 772`. If `owner_admit_guard` is plumbed through `acquire_first_packet` → guard-holder task, the gauge is correctly maintained and this task is verification only.

- [ ] **Step 1: Confirm existing `t030_owner_cap_binds_before_rule_cap_on_udp_first_packet`** covers owner-first ordering for at least `new_connections_per_sec`.

- [ ] **Step 2: Add a focused UDP `concurrent_connections` test** (one new test, not a rewrite):

  - install owner cap `concurrent_connections = 1`, no `new_connections_per_sec`,
  - open one UDP flow and keep its guard alive (do not drop the per-flow task),
  - send a first packet from a second source address,
  - assert: second source is dropped at the owner gate, `OwnerConcurrent` reject counter increments, rule-level counter is untouched, the original flow's bytes continue,
  - drop the first flow's guard; send again from the second source; assert it now admits.

- [ ] **Step 3: Run**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client forwarder::udp -- --nocapture
```

Expected: PASS for both the existing test and the new concurrent test.

### Task 4: Verify Multi-Target TCP Owner Cap Path

**Files:** `crates/portunus-client/src/forwarder/failover_path.rs`, `crates/portunus-client/src/forwarder/failover.rs`

- [ ] **Step 1: Confirm existing coverage**

```bash
rg -n "OwnerConcurrent|owner_cap|owner_rate_limit|try_acquire_layered" crates/portunus-client/src/forwarder/failover_path.rs
```

Expect hits at `failover_path.rs:162-163, 227-228, 232, 246-247, 296-297, 359-360, 740-741, 757-758`.

- [ ] **Step 2: Add a focused multi-target TCP test if missing** — owner cap = 1, rule cap higher / absent, first connection held, second connection rejected by owner gate, `OwnerConcurrent` increments, no per-rule reject increment.

- [ ] **Step 3: Run**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client failover_path -- --nocapture
```

Expected: PASS.

### Task 5: SNI Path — Thread Limiters Through The Dispatcher

**Files:** `crates/portunus-client/src/port_groups.rs`, `crates/portunus-client/src/forwarder/sni/listener.rs`, `crates/portunus-client/src/control.rs`

Default disposition is **upgrade the dispatcher**, per up-front callout #1. Step 1 is the only out — if it finds an upstream filter that prevents capped SNI rules from reaching `apply_push` in the first place, the rest of the task collapses to a verify-only routing tripwire. Otherwise, do the threading work.

- [ ] **Step 1: Search for an upstream filter (cheap, ~5 min)**

```bash
rg -n "sni_pattern|routes_via_sni|legacy_accept|fall(back|_to)_legacy" crates/portunus-client/src/control.rs crates/portunus-client/src/port_groups.rs
```

Read `control.rs:751-761` (the `routes_via_sni` decision). Confirm there is no branch like "if rule has rate_limit AND sni_pattern, force legacy path". If you find one, jump to Step 5 (verify-only). The expected outcome is: no such filter exists.

- [ ] **Step 2: Add a failing reproducer test in `port_groups.rs`**

Build a `ClientRule` with `sni_pattern = Some("api.example.com")`, `owner_id = "alice"`, `owner_rate_limit = Some(<concurrent_connections = 1>)`, plus a stats handle. Drive `apply_push` (use the existing `live_resolver` fixture). Through the test fixture, expose a way to read whatever `GroupMember` ends up storing for this rule. Assert that the stored member carries the `owner_rate_limit` handle. **This test will fail today** because `GroupMember` at `port_groups.rs:159-174` drops it.

- [ ] **Step 3: Thread the four limiter handles through**

  - Extend `GroupMember` with `rate_limit`, `rate_limit_stats`, `owner_rate_limit`, `owner_rate_limit_stats` (all `Option<...>`).
  - Have `apply_push` clone them from the incoming `ClientRule`.
  - Pass them through to the SNI dispatcher call at `forwarder/sni/listener.rs:404` instead of the four `None`s.
  - Update the comment at `sni/listener.rs:397-407` — it currently describes a routing that no longer exists; replace it with a one-liner explaining the new pass-through.
  - Remove `owner_rate_limit: None` from the test fixtures in `port_groups.rs` if they get in the way; pass actual `None`-wrapped handles instead so they continue to type-check.

- [ ] **Step 4: Add the enforcement test (was Task 5 Step 3)**

End-to-end inside `port_groups.rs` `#[cfg(test)]` (or a focused `sni/listener.rs` test if easier with the existing harness): owner cap = 1; open one TLS-style connection through `api.example.com`; assert second connection is RST'd by the owner gate; `OwnerConcurrent` reject counter increments; rule reject counter unchanged.

- [ ] **Step 5: Verify-only fallback (only if Step 1 found an upstream filter)**

Add a routing-invariant assertion (e.g., a test that pushes a capped SNI rule and checks it landed in the legacy path, not in `port_groups`' SNI port table). Skip steps 2-4.

- [ ] **Step 6: Run**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client port_groups -- --nocapture
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client forwarder::sni -- --nocapture
```

Expected: PASS. **Estimate: +1-2 days vs the rest of the plan**; flag back if Step 3 reveals deeper plumbing (e.g., the SNI dispatcher proxy call signature needs more args).

## Chunk 3: Reconnect and Hot-Update Behavior

### Task 6: Verify Owner Cap Push and Reconnect Replay

**Files:** `crates/portunus-server/src/grpc/service.rs`, `crates/portunus-client/src/control.rs`

- [ ] **Step 1: Confirm server replay** — Welcome / post-connect sends persisted owner cap rows only to clients with version `>= 0.11.0`.

- [ ] **Step 2: Confirm client update semantics** — `apply_owner_rate_limit_update` at `control.rs:887` uses `OwnerRateLimitScopeManager::update` (not `install`), preserving the active gauge so lowering shares carryover and drains naturally.

- [ ] **Step 3: Add / confirm test for lower-cap graceful drain**

  - start with owner concurrent cap `5`,
  - admit two active connections (two `ActiveGuard`s held),
  - update to cap `1`,
  - new acquisition rejects as `OwnerConcurrent`,
  - drop one guard — still no admit (live count `1` ≥ cap `1`),
  - drop the second guard — new acquisition admits.

- [ ] **Step 4: Run**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client rate_limit::scope -- --nocapture
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client control::tests::owner -- --nocapture
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib grpc::service -- --nocapture
```

Expected: PASS.

## Chunk 4: v1.3.0 splice Interaction

### Task 7: Confirm owner `concurrent_connections` Does Not Disable Splice

**Files:** `crates/portunus-client/src/forwarder/splice.rs`, `crates/portunus-client/src/forwarder/proxy.rs`, `crates/portunus-client/src/forwarder/mod.rs`

Eligibility predicate at `splice.rs:121-122` is `matches!(ctx.protocol, Protocol::Tcp) && !ctx.disable_splice && !ctx.has_bandwidth_cap`, with `has_bandwidth_cap` computed at `splice.rs:93-94` from `{rule, owner}` bandwidth caps only. `concurrent_connections` and `new_connections_per_sec` do not appear here. **The context struct is named `CopyCtx`, not `SpliceCtx`.**

Two of the three invariant tests already exist; this task adds the missing pieces.

- [ ] **Step 1: Confirm the existing tests still pass**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client forwarder::splice::tests::rule_with_concurrent_only_does_not_force_userspace -- --nocapture
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client forwarder::splice::tests::owner_concurrent_only_does_not_force_userspace -- --nocapture
```

Locations: `splice.rs:818` and `splice.rs:882`. Both assert `eligible(&CopyCtx { has_bandwidth_cap: false, ... }) == true` when only `concurrent_connections` is set.

- [ ] **Step 2: Add `new_connections_per_sec`-only invariant test if missing**

```bash
rg -n "new_connections_per_sec" crates/portunus-client/src/forwarder/splice.rs
```

If no test covers an owner / rule cap with only `new_connections_per_sec` set, add one paralleling the two existing tests: build a `CopyCtx` whose handle has `new_connections_per_sec = Some(50)` and all bandwidth caps `None`. Assert `eligible(&ctx) == true`. Cover both rule-side and owner-side variants.

- [ ] **Step 3: Add guard-lifetime integration test (new, the genuinely missing piece)**

In `forwarder/mod.rs` `#[cfg(all(test, target_os = "linux"))]`: install an owner cap `concurrent_connections = 1`; accept one connection that goes through splice; while bytes flow through `splice::copy_bidirectional`, assert the owner scope's `active_count() == 1`. Close the upstream side; await task shutdown; assert `active_count()` returns to `0`. This proves the `ActiveGuard` is captured by the forwarder task, not dropped at accept frame.

- [ ] **Step 4: Run**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client forwarder::splice -- --nocapture
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-client forwarder::tests -- --nocapture
```

Expected: PASS. macOS skips the Linux-only test via `cfg`.

## Chunk 5: Operator Experience

### Task 8: Confirm CLI Shape for Owner Connection Limit

**Files:** `crates/portunus-server/src/main.rs`, `crates/portunus-server/src/operator/owner_cap_cli.rs`

CLI shape (confirmed in code today):

```text
portunus-server owner-cap list <client>
portunus-server owner-cap get <client> <owner>
portunus-server owner-cap set <client> <owner>
                                  [--bandwidth-in-bps N]
                                  [--bandwidth-out-bps N]
                                  [--new-connections-per-sec N]
                                  [--concurrent-connections N]
                                  [--bandwidth-in-burst N]
                                  [--bandwidth-out-burst N]
                                  [--new-connections-burst N]
portunus-server owner-cap delete <client> <owner>
```

- [ ] **Step 1: Confirm `set` accepts a lone `--concurrent-connections`**

```bash
rg -n "concurrent-connections|concurrent_connections|RateLimitArgs" crates/portunus-server/src/main.rs crates/portunus-server/src/operator/owner_cap_cli.rs
```

Confirm that `set` with **only** `--concurrent-connections 100` (no other flags) does not hit the `rate_limit_no_caps_provided` validation. The validation message at `owner_cap_cli.rs:259` lists `--concurrent-connections` as one of the valid options, so this should be the case.

- [ ] **Step 2: Add a CLI integration test if missing**

Test that:

```text
portunus-server owner-cap set edge-01 alice --concurrent-connections 100
```

- Text mode prints `owner-cap set client=edge-01 owner=alice updated_at_unix_ms=…` (format from `owner_cap_cli.rs:282`).
- JSON mode (`--format json`) emits `rate_limit.concurrent_connections = 100` with other cap fields `null`.

- [ ] **Step 3: Run**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server owner_cap -- --nocapture
```

Expected: PASS.

### Task 9: Web UI Owner Quotas Smoke

**Files:** `webui/src/...` (no code change expected; manual smoke).

v0.11 shipped the Owner quotas tab (screenshot referenced as `webui-owner-quotas-en.png` in `docs/content/docs/features/rate-limiting.mdx`). Do a 1-minute manual smoke after server changes land:

- [ ] **Step 1: Start the dev stack** (`make dev`).

- [ ] **Step 2: Open Client detail → Owner quotas** tab. Add an owner cap with **only** `concurrent_connections = 100` for owner `alice`. Save.

- [ ] **Step 3: Reload the page** — value persists. Edit to `concurrent_connections = 50` — save round-trips.

- [ ] **Step 4: Delete the cap** — row disappears; subsequent `GET /v1/clients/.../owners/alice/rate-limit` returns 404.

If any step fails, fix in `webui/` and re-test. UI bugs are in scope here because the v0.11 cap envelope explicitly supports `concurrent_connections` standalone.

## Chunk 6: End-To-End Confidence

### Task 10: Server-Client Integration Test (committed, not optional)

**Files:** Create `crates/portunus-e2e/tests/owner_connection_limit.rs`. Read `crates/portunus-e2e/src/lib.rs` and adjacent tests for the e2e helper style.

This test crosses gRPC push, SQLite persistence, and client hydration in a single real-process flow. The unit tests in Tasks 2-7 cover layered admission inside one process; this one closes the loop.

- [ ] **Step 1: Build the scenario**

  - start a real `portunus-server`,
  - provision a real `portunus-client` (`0.11.0` or newer),
  - create owner `alice`; push one TCP rule owned by `alice` (e.g. `127.0.0.1:0 → 127.0.0.1:<echo>`),
  - set owner cap `concurrent_connections = 1` via `PUT /v1/clients/{id}/owners/alice/rate-limit`,
  - hold one TCP connection open through the rule,
  - open a second connection; assert it is RST'd / closed before any forwarded bytes,
  - assert stats report (or Prometheus scrape if simpler with existing helpers) contains an `OwnerConcurrent` reject increment for `(edge-01, alice)`.

- [ ] **Step 2: Persistence round-trip (drop client memory first)**

The naive "restart server only, third connection still rejected" check does NOT prove SQLite replay — the client's in-memory `OwnerRateLimitScopeManager` would have kept the cap alive across the server outage. To actually exercise `welcome → persisted row → push → client hydration`:

  1. Stop the client process. Then stop the server. The client's in-memory cap dies with it.
  2. Restart the server **with the same data dir** (the SQLite file persists).
  3. Restart the client **with the same client id / bundle**, completing a fresh handshake.
  4. Through the (now hydrated) rule, open one connection and hold it.
  5. Open a second connection. Assert: RST'd by the owner gate, `OwnerConcurrent` reject increment.
  6. Assert via the operator HTTP API (`GET /v1/clients/{id}/owners/alice/rate-limit`) that the persisted row still reads `concurrent_connections = 1`.

This sequence is the only one that proves all three things: SQLite persisted the row, the welcome replay sent it on reconnect, and the client applied it fresh from memory zero.

- [ ] **Step 3: Run**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-e2e --test owner_connection_limit -- --nocapture
```

Expected: PASS.

## Chunk 7: Final Verification

### Task 11: Formatting, Lints, Workspace Tests

**Files:** any modified Rust files.

- [ ] **Step 1: Format (only touched packages — avoid drive-by reformatting)**

```bash
cargo fmt
```

Run from each crate root that this plan modified (portunus-server, portunus-client, portunus-core if touched, portunus-e2e). CI runs `cargo fmt --all --check` as the gate, so any miss is caught upstream — but the plan executor should NOT reformat unrelated crates. If you genuinely changed every crate, `cargo fmt --all` is fine; otherwise scoped `cargo fmt` keeps the diff focused.

- [ ] **Step 2: Clippy (matches CI gate)**

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: zero warnings. The workspace sets `clippy::pedantic = warn`; any new `allow` must come with a comment justifying why.

- [ ] **Step 3: Re-run every focused test from Tasks 1-10**

Each task block lists the exact command. Run them all once more in sequence as the green-light gate.

- [ ] **Step 4: Workspace test pass**

```bash
PORTUNUS_SKIP_WEBUI=1 cargo test --workspace
```

Expected: PASS. Catches accidental regressions in adjacent code.

- [ ] **Step 5: Summarize final behavior in the PR description**

Explicitly state:
- owner limit scope: `(client_name, owner_id)`,
- `concurrent_connections` applies across all rules owned by that user on that client (TCP accepts, UDP first packets, multi-target failover, and SNI via legacy accept_loop routing — see Task 5),
- v1.3.0 splice fast path is unaffected by `concurrent_connections`-only owner caps (Task 7),
- lowering the cap drains gracefully; existing connections survive,
- < 0.11.0 clients get `422 rate_limit_unsupported_by_client` (Task 1 Step 2),
- tests run: list the commands.

## Notes

- **No new dependencies.**
- **No new endpoint or table** unless Task 5 Step 3 reveals that the SNI dispatcher needs the upgrade — in which case flag back before doing it (it's a +1-2 day deviation).
- **Delete stale code cleanly.** No `// moved to X` comments; just remove.
- **The plan does NOT change wire format.** Existing `OwnerRateLimitUpdate` protobuf is sufficient.

# Quickstart: TCP Zero-Copy Fast Path

**Phase**: 1
**Audience**: bench-host operators and reviewers validating SC-001 / SC-002.

This is **not** an operator-facing quickstart in the v0.x sense (the
feature has no operator surface — FR-003). It is the procedure a
reviewer uses to verify the optimization actually delivers ≥ 1.4×
throughput before approving the merge.

---

## §1. Host requirements

- Linux kernel ≥ 5.10 (4.x works but lacks some splice perf
  improvements; out-of-baseline).
- **Dedicated bench host** — not a shared CI runner, not a laptop on
  battery. Shared CPUs invalidate the comparison (SC-001 explicitly
  excludes generic CI per the spec).
- `cpupower` set to `performance` governor (or kernel-level equivalent).
- `taskset` / `cpuset` to pin the bench to specific cores.
- `/proc/sys/fs/pipe-max-size` ≥ 1048576 (the default on modern
  kernels). If lower, the optimization still functions but the bench
  numbers will not match the spec gate; raise it temporarily for the
  measurement.

---

## §2. Capture baseline (BEFORE any splice code lands)

This step **must** run on the bench host on the commit immediately
preceding the splice implementation — typically v1.2.0's tip:

```sh
cd crates/portunus-client
git checkout v1.2.0          # or the commit before splice work began
cargo bench --bench data_plane -- --save-baseline v1.2.0
```

The baseline JSON lands at
`crates/portunus-client/benches/baselines/v1.2.0.json`. Commit it.

---

## §3. Build the optimized binary

```sh
cd <repo-root>
cargo build --release -p portunus-client
```

Verify the splice module is compiled in (Linux only):

```sh
nm target/release/portunus-client 2>/dev/null | grep -i splice
# Expect: matches involving 'portunus_client::forwarder::splice'
```

On macOS / Windows the symbol must be absent — confirming the
`#[cfg(target_os = "linux")]` gate worked.

---

## §4. Run the new bench

```sh
cd crates/portunus-client
cargo bench --bench splice_throughput
```

The bench produces 4 groups:

1. `plain_tcp_1mib_chunks/splice_on`  — fast path
2. `plain_tcp_1mib_chunks/splice_off` — userspace (sets
   `PORTUNUS_DISABLE_SPLICE=1` via `Criterion`'s env mechanism)
3. `sni_routed_1mib_chunks/splice_on` — SNI rule, fast path after peek
4. `sni_routed_1mib_chunks/splice_off`

Compare against the saved baseline:

```sh
cargo bench --bench splice_throughput -- --baseline v1.2.0
```

**Gate**:

- `plain_tcp_1mib_chunks/splice_on` throughput ≥ baseline × 1.4 — SC-001.
- p99 setup latency (`p99_setup_latency_us` histogram in the bench
  output) within ±5 % of baseline — SC-002.
- `*_splice_off` numbers must match baseline within Criterion's noise
  floor — confirms the env kill switch is honoured.

---

## §5. Byte-stability sweep (SC-003)

The full integration suite is run twice — once normally, once with
`PORTUNUS_DISABLE_SPLICE=1`. Both must pass identically.

```sh
cd <repo-root>
# Run 1 — optimization enabled (default on Linux)
cargo test --workspace --no-fail-fast

# Run 2 — optimization disabled
PORTUNUS_DISABLE_SPLICE=1 cargo test --workspace --no-fail-fast
```

Test selectors that must pass identically in both runs:

- `portunus-client::forwarder::*` (all forwarder tests).
- `portunus-e2e::tests::*` (end-to-end real-socket integration).
- `portunus-server::tests::rate_limit_*` (v0.11 invariants).
- `portunus-server::tests::sni_*` (v0.9 invariants).
- `portunus-server::tests::proxy_protocol_*` (v0.10 invariants).

**Acceptance**: zero diff in pass/fail set between the two runs.
Spec gate SC-005 ("no v0.11 rate-limit SC regression") and SC-003
("byte stability") fall out of this sweep.

---

## §6. Fallback resilience (SC-007)

A small chaos test verifies the `Unsupported` fallback path in
isolation, by injecting the errno at the syscall boundary:

```sh
cd crates/portunus-client
cargo test --release \
    --test splice_unsupported \
    -- --nocapture
```

This test (in `src/forwarder/splice.rs` `#[cfg(test)] mod unsupported`)
overrides the `splice_raw` wrapper via a `mockall`-style hook to
return `ENOSYS` on the first call. The test asserts:

- The connection completes via the userspace path.
- `bytes_in` / `bytes_out` advance.
- Exactly one `proxy.splice_unsupported_fallback` event is emitted
  with `errno_name = "ENOSYS"`.

---

## §7. Smoke run on a real workload (optional, recommended before tag)

If the team has access to the v0.11 production bench setup (HTTP/2
TLS proxy via SNI routing, ~10 long-lived connections, 1 Gbps target),
run an A/B for 5 minutes each:

```sh
# A: PORTUNUS_DISABLE_SPLICE=1 ./portunus-client --bundle ./edge.bundle.json
# B: ./portunus-client --bundle ./edge.bundle.json
```

Collect via Prometheus:

- `rate(portunus_rule_bytes_out_total[1m])` — per-rule throughput
- `process_cpu_seconds_total` — `portunus-client` CPU
- Compute `throughput / cpu` (bytes per CPU-second).

Acceptance: B/A ≥ 1.4 on bytes-per-CPU-second.

---

## §8. After acceptance: CHANGELOG entry

Add to `CHANGELOG.md` under the next version section:

```markdown
### Added

- **Linux TCP zero-copy fast path** — on Linux hosts the `portunus-client`
  data plane now uses `splice(2)` for TCP forwarding when a rule has no
  bandwidth caps and no per-owner bandwidth cap. The optimization is
  applied automatically with no rule-level configuration; behaviour and
  byte stream are identical to the previous userspace path. macOS and
  Windows builds are unchanged. Set `PORTUNUS_DISABLE_SPLICE=1` on the
  `portunus-client` environment to force the userspace path for
  diagnostic purposes; see `docs/runbook.md` for triage guidance.
```

---

## §9. Troubleshooting (`docs/runbook.md` addendum)

Symptoms and remediation that the runbook addendum covers:

| Symptom | Probable cause | Action |
|---|---|---|
| Single `proxy.splice_unsupported_fallback` per connection at start, then steady state | seccomp policy or container LSM denies `splice` | If acceptable, ignore — fallback is functional. To suppress the log, set `PORTUNUS_DISABLE_SPLICE=1`. |
| `proxy.splice_pipe_size_failed` recurring on every connection | `/proc/sys/fs/pipe-max-size` is below the requested 1 MiB | Either raise the sysctl, or accept reduced peak throughput. No correctness impact. |
| Throughput on Linux unexpectedly equal to baseline | Optimization not active. Check: env var set? rule has bandwidth caps? owner has bandwidth caps? | Inspect `proxy.splice_selected` event — its absence indicates the rule was ineligible. |
| Mismatch between `bytes_in` and bytes the upstream actually received under RST | Behaviour is identical to the userspace path: counters count delivered bytes only. Connection-reset semantics unchanged. | Not a bug; document for users who notice. |

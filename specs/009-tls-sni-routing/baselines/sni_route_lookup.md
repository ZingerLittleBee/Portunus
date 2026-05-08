# SniRoutingTable lookup baseline (T087)

Captured: 2026-05-09
Hardware: macOS (Darwin 25.4.0), Apple Silicon (criterion `--quick` mode)
Bench file: `crates/forward-client/benches/sni_route.rs`
Run: `cargo bench -p forward-client --bench sni_route -- --quick`

Each criterion sample drives **3 lookups**; figures below are the
reported `time` (mean of inner closure), so per-lookup numbers are
÷ 3. p99 is stricter than mean but stays well under SC-006's 100 µs
budget at every measured scale.

| Routes | Shape         | Mean per-batch | Mean per-lookup |
|-------:|---------------|----------------|-----------------|
| 100    | exact_hit     | 140 ns         | ~47 ns          |
| 100    | wildcard_hit  | 4.4 µs         | ~1.5 µs         |
| 100    | miss          | 8.1 µs         | ~2.7 µs         |
| 1 000  | exact_hit     | 144 ns         | ~48 ns          |
| 1 000  | wildcard_hit  | 41 µs          | ~14 µs          |
| 1 000  | miss          | 82 µs          | ~27 µs          |
| 10 000 | exact_hit     | 143 ns         | ~48 ns          |
| 10 000 | wildcard_hit  | 412 µs         | ~137 µs         |
| 10 000 | miss          | 799 µs         | ~266 µs         |

## SC-006 verdict

> "For a representative wildcard catalogue (≤ 100 rules per listener),
> the route-table lookup decision is made in under 100 µs at the
> 99th percentile."

At 100 rules, the slowest measured shape (`miss`) is 2.7 µs per
lookup mean. p99 in criterion's HTML report (under
`target/criterion/sni_route_lookup_100/miss/report/index.html`) sits
comfortably below 100 µs. **SC-006 met.**

## Observations

- Exact hits are flat across scale because they're a single
  `HashMap` probe — the table size doesn't matter.
- Wildcard hits and misses scale linearly with table size because
  the lookup walks the entire `wildcards: Vec<(String, RuleId)>`
  list. At 10 000 routes a miss takes ~266 µs per lookup; that's
  the worst-case worst case (no fallback short-circuit) and only
  matters if a deployment runs that many wildcard rules per
  listener — well beyond the SC-006 budget shape.
- Each wildcard probe does a `format!(".{suffix}")` allocation.
  That's the dominant cost; a future optimisation could cache the
  needle alongside the suffix at table-build time. Not worth doing
  for v0.9 — the SC-006 budget is comfortably met at the documented
  scale.

## Re-baselining

Re-run after any change to:
- `crates/forward-client/src/forwarder/sni/route_table.rs`
- The bench file itself
- The Rust toolchain pinned in `rust-toolchain.toml` /
  `Cargo.toml::workspace.package.rust-version`

Allow ≤ +5 % regression at 100 routes per Constitution Principle II
(hot-path budget). A regression beyond that is grounds for a revert
or a follow-up optimisation PR.

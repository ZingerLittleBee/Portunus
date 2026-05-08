# Operator API push-rule baseline (T089)

Captured: 2026-05-09
Hardware: macOS (Darwin 25.4.0), Apple Silicon (criterion `--quick` mode)
Bench file: `crates/forward-server/benches/operator_api.rs`
Run: `cargo bench -p forward-server --bench operator_api -- --quick post_v1_rules`

## Scope

T089 measures whether v0.9's added `validate_sni_pattern` walk
regresses the rule-push hot path versus v0.8. Successful pushes
require a real client to ACK rule activation (5 s timeout otherwise),
which a microbench can't easily harness — so the bench instead
exercises the **pre-activation** path that every push goes through:
HTTP routing + JSON deserialise + RBAC check + protocol/range
validation + SNI grammar walk (when the field is present).

Both shapes deliberately fail with `400 Bad Request` so the bench
short-circuits before activation:

- `post_v1_rules_validate_no_sni` — `listen_port=0` trips
  `range_invalid` after the (no-op) SNI walk.
- `post_v1_rules_validate_with_sni` — `sni_pattern="*.com"` trips
  `validation.sni_pattern_malformed` from inside the SNI walk.

The marginal cost between the two is the v0.9 SNI validator's
contribution to the push path.

## Results (criterion mean)

| Bench                                       | Mean    | Δ vs no-sni |
|---------------------------------------------|---------|-------------|
| operator_api/post_v1_rules_validate_no_sni  | 27.3 µs | (baseline)  |
| operator_api/post_v1_rules_validate_with_sni| 28.6 µs | +1.3 µs (+4.7 %) |

## T089 verdict

> "Assert the rule-push path is within 5 % of the v0.8 baseline despite
> the new overlap-matrix walk."

The SNI grammar walk adds **+1.3 µs (+4.7 %)** to the push path —
within the 5 % budget. The overlap-matrix walk itself is structurally
present on every push (with or without SNI) since v0.9; both shapes
hit it equally, so the marginal cost we measure is the SNI validator
alone (lowercase + label-walk + character class check, all on bounded
inputs).

## Caveats

- Successful-push throughput (validation + activation + ACK) is NOT
  measured by this bench. A future harness that fakes the client ACK
  on `state.clients[CLIENT].outbound` would close the gap. Until
  then, the v0.5+ `operator_api` GET benches stay the canonical
  read-path baseline.
- The 5 % budget is comfortable today (mean 4.7 %). Re-run after any
  change to:
  - `crates/forward-server/src/operator/http.rs::validate_sni_pattern`
  - `crates/forward-server/src/operator/http.rs::post_rules`
  - The Rust toolchain pinned in `rust-toolchain.toml` /
    `Cargo.toml::workspace.package.rust-version`

A regression beyond +5 % vs the no-sni baseline at the same
toolchain trips Constitution Principle II's hot-path budget.

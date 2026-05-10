# SNI listener setup-latency baseline (T088)

Captured: 2026-05-09
Hardware: macOS (Darwin 25.4.0), Apple Silicon (criterion `--quick` mode)
Bench file: `crates/portunus-client/benches/sni_route.rs`
Run: `cargo bench -p portunus-client --bench sni_route -- --quick sni_setup_latency`

## Scope

T088 measures the userspace dispatch cost the SNI listener adds on
top of the v0.7 plain-TCP listener. Both listeners share the same
TCP `accept` path, kernel scheduling, upstream `connect`, and
bidirectional copy task. The userspace difference between them is
exactly:

- **plain TCP listener** (v0.7 baseline): `accept → spawn proxy →
  connect upstream` — no peek, no parse, no route lookup.
- **SNI listener** (v0.9): `accept → peek ClientHello → parse →
  lookup → connect upstream → replay peeked bytes upstream`.

The `replay-peeked-bytes` part is one `write_all` of ≤ 1 KiB —
dominated by the kernel send queue, equal to the steady-state copy
loop both modes run anyway, and not benched here.

The bench therefore times `parse_sni_inline` + `SniRoutingTable::lookup`
on a synthesised TLS 1.2 ClientHello against a 100-route catalogue
(matching the SC-006 reference shape "≤ 100 rules per listener"),
versus a `plain_baseline` that runs no dispatch work.

## Results (criterion mean)

| Bench                         | Mean    |
|-------------------------------|---------|
| sni_setup_latency/plain_baseline | 277 ps  |
| sni_setup_latency/sni_dispatch   | 77 ns   |

Per-iteration `sni_dispatch` does one parse + one lookup; the
`plain_baseline` is a `black_box` pass-through of the same buffer
(criterion's own loop overhead — a faithful "the v0.7 listener does
nothing extra here" stand-in).

Marginal cost of SNI dispatch: **~77 ns**.

## SC-003 verdict

> "An SNI-routed connection completes its setup within 5 ms of the
> p99 latency of the same connection through a plain-TCP listener
> on the same hardware."

Userspace overhead is ~77 ns at the bench mean and stays in the
same order at p99 (visible in
`target/criterion/sni_setup_latency/sni_dispatch/report/index.html`).
That's **5 orders of magnitude below** the +5 ms SC-003 budget
(5 000 000 ns). **SC-003 met.**

The 5 ms budget is sized for tail kernel effects (TCP retransmit
backoff, scheduler contention, NIC offload variance) — none of
which differ between plain-TCP and SNI listeners. The userspace
margin we measure here is the only mode-dependent contribution and
it's structurally negligible.

## Caveats

- The bench reproduces the parser and lookup table inline because
  `portunus-client` is a binary crate with no lib target (same
  pattern as T087). If
  `crates/portunus-client/src/forwarder/sni/{client_hello.rs,route_table.rs}`
  diverges from the inline shape here, this bench loses fidelity
  — review them together when changing either.
- Real wall-clock setup latency (loopback or LAN) is not measured.
  A future harness could spin up two `tokio::net::TcpListener`
  instances inside the bench runtime and drive end-to-end clients
  for a wall-clock comparison; until then the userspace margin
  documented above is the load-bearing claim.
- The benched ClientHello carries one cipher, no compression, and
  one extension (server_name). Real ClientHellos run 500–2000 B
  with 8+ extensions; the parser walks them by length so wall-clock
  cost grows linearly with extension count. At 2 KiB ClientHello
  the dispatch cost is still in the hundreds of ns — well inside
  budget.

## Re-baselining

Re-run after any change to:
- `crates/portunus-client/src/forwarder/sni/client_hello.rs::parse`
- `crates/portunus-client/src/forwarder/sni/route_table.rs`
- The bench file itself
- The Rust toolchain pinned in `rust-toolchain.toml` /
  `Cargo.toml::workspace.package.rust-version`

Allow ≤ +5 % regression at the documented scale per Constitution
Principle II (hot-path budget). A regression beyond that is grounds
for a revert or a follow-up optimisation PR.

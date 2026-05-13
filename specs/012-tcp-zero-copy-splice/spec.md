# Feature Specification: TCP Zero-Copy Fast Path (Linux splice)

**Feature Branch**: `012-tcp-zero-copy-splice`
**Created**: 2026-05-13
**Status**: Draft
**Input**: Brainstorming session 2026-05-13 — direction "deepen L4 → performance / kernel acceleration → low-conn-count / high-throughput workloads"

## Clarifications

### Session 2026-05-13

- Q: Driving workload shape? → A: Few connections, high throughput (large transfers — VPN tunnels, mirror proxy, intra-DC interconnect). p99 setup latency budget is not the bottleneck; CPU spent on user↔kernel copy is.
- Q: Should this be operator-visible (rule field) or invisible? → A: Invisible. No wire / config / Web UI changes. Implementation detail.
- Q: Which exact incompatibility forces the userspace path? → A: Any **bandwidth** cap (per-rule `bandwidth_in_bps` / `bandwidth_out_bps`, or per-owner equivalents). Per-rule `concurrent_connections` and `new_connections_per_sec` gate at accept time and remain compatible.
- Q: SNI peek and PROXY-protocol prelude — do they disqualify a rule? → A: No. Both are *prefix-only* operations. After the prelude / peek+replay finishes, the remaining bytes are pure bidirectional pump and may use the zero-copy path.
- Q: Cross-platform expectations? → A: Linux-only fast path. macOS / Windows continue on the existing userspace path with byte-identical behaviour.
- Q: Bench acceptance gate? → A: On Linux, single-connection 1 MiB-chunk TCP forwarding throughput ≥ baseline × 1.4; p99 connection-setup latency must stay within 5 % of baseline.

## User Scenarios & Testing *(mandatory)*

### User Story 1 — Large-transfer TCP traffic gets kernel zero-copy (Priority: P1) 🎯 MVP

An operator runs Portunus in front of a mirror cache / intra-DC interconnect /
VPN tunnel where individual TCP connections sustain 100 MB/s – 1 GB/s and a
single edge host serves a handful of concurrent flows. Today the
`portunus-client` data plane is pure-userspace
(`tokio::io::copy_bidirectional_with_sizes` with 64 KiB per-direction
buffers — `PROXY_COPY_BUF_SIZE` in `forwarder/proxy.rs`); at line rate
the CPU is dominated by
`read(2) → memcpy → write(2)` syscall pairs, capping single-core throughput
well below NIC speed. The operator wants the same rules to forward the same
bytes at substantially higher per-CPU throughput, **without changing any rule
configuration, wire field, or Web UI**.

**Why this priority**: This is the single largest predicted per-CPU
throughput win available without changing the runtime model (Tokio) or the
wire surface. The exact multiplier on production hardware is unknown until
SC-001 is measured; the spec commits to ≥ 1.4× on the in-tree bench as the
ship gate. The optimization turns CPU bottlenecks into NIC / kernel
bottlenecks for the workload class this product is most often pointed at
(raw L4 transparent forwarding) and raises the headroom under which
existing v0.11 rate-limit and v0.9 SNI features run.

**Independent Test**: On a Linux host, push a plain TCP rule with no caps and
no SNI, drive 1 GiB of payload through it from a loopback client to a
loopback echo upstream, measure throughput. Compare against the same rule on
the same host with the optimization disabled (`PORTUNUS_DISABLE_SPLICE=1`).
The optimized path delivers ≥ 1.4× the throughput; both paths deliver
byte-identical content; both report identical `bytes_in` / `bytes_out` to
Prometheus and per-rule stats.

**Acceptance Scenarios**:

1. **Given** a plain TCP rule on `portunus-client` running on Linux 5.10+,
   no `rate_limit`, no SNI pattern, target without `proxy_protocol`,
   **When** an operator pushes 1 GiB through the rule from a single TCP
   connection, **Then** the bytes arrive at the upstream byte-identical to
   the client's send buffer, and per-rule `bytes_in` / `bytes_out` counters
   advance by exactly 1 GiB each.

2. **Given** the same rule but with `PORTUNUS_DISABLE_SPLICE=1` set in the
   `portunus-client` environment, **When** the same 1 GiB transfer happens,
   **Then** the bytes still arrive byte-identical and the counters are
   identical, **and** the throughput is the v1.2.0 baseline (not the
   optimized path) — the variable acts as a kill switch for triage.

3. **Given** a `portunus-client` running on **macOS or Windows** with the
   same rule shape, **When** the same 1 GiB transfer happens, **Then** the
   bytes still arrive byte-identical and behaviour is unchanged from v1.2.0
   (the optimization is silently absent on non-Linux platforms).

4. **Given** the kernel rejects the fast-path syscall before the first byte
   has moved (e.g. seccomp policy denies `splice`, or an unusual socket
   type), **When** the data plane attempts the fast path, **Then** it
   transparently falls back to the v1.2.0 userspace path for that connection
   and emits exactly one structured warning per connection identifying the
   errno; subsequent connections retry the fast path independently.

5. **Given** a long-lived flow that has been transferring bytes via the
   fast path, **When** the upstream sends RST mid-transfer, **Then** the
   connection terminates with the same error classification and counter
   semantics as the userspace path (in-flight not-yet-delivered bytes are
   dropped, both paths agree on `bytes_delivered`).

---

### User Story 2 — Existing v0.9 SNI / v0.10 PROXY rules also benefit (Priority: P2)

An operator runs SNI-routed HTTPS termination (009) and / or PROXY-protocol
v1/v2 prelude (010) on rules that forward heavy steady-state TLS traffic
(streaming, image / object delivery). The prelude phase — ClientHello peek +
replay, or PROXY header write — is short and userspace-bound; once it
finishes, the remaining lifetime of the connection is just byte-pumping. The
operator wants the post-prelude segment to also benefit from the zero-copy
path without losing SNI routing or PROXY semantics.

**Why this priority**: SNI and PROXY rules are exactly the rules carrying the
"heavy TLS" traffic that benefits most from zero-copy. Restricting the
optimization to "plain TCP only" would exclude a large fraction of the very
traffic this product is deployed for.

**Independent Test**: On a Linux host, push (a) an SNI rule that routes
`api.example.com` to upstream, and (b) a plain TCP rule with
`target.proxy_protocol = 2`. For each, run a 1 GiB transfer after the
prelude completes. Compare against `PORTUNUS_DISABLE_SPLICE=1`. The optimized
path delivers ≥ 1.4× throughput on the post-prelude segment, the prelude
itself is byte-identical between both paths, and the upstream sees the same
PROXY v2 header (resp. the same SNI-routed dispatch) in both cases.

**Acceptance Scenarios**:

1. **Given** an SNI rule on a Linux client, **When** a TLS ClientHello peeks
   successfully and the connection is dispatched to an upstream, **Then**
   the peeked bytes are replayed to the upstream byte-identical and the
   subsequent steady-state stream uses the fast path.

2. **Given** a rule with `target.proxy_protocol = 1` or `2`, **When** the
   prelude has been fully written to the upstream socket, **Then** the
   subsequent bidirectional stream uses the fast path; the prelude itself
   is byte-identical to v1.2.0.

3. **Given** an SNI rule whose ClientHello peek **times out** or **fails to
   parse**, **When** the connection is closed per existing v0.9 semantics,
   **Then** the fast-path code never executes for that connection and v0.9
   error counters / tracing remain unchanged.

---

### User Story 3 — Rate-limit and concurrent-cap rules keep correct semantics (Priority: P1)

An operator runs a mix of capped rules: some with `bandwidth_*_bps`, some
with only `concurrent_connections`, some with only `new_connections_per_sec`,
some with per-owner caps. The operator must be confident that turning on the
optimization does **not** weaken any cap or alter any reject reason.

**Why this priority**: Silent rate-limit weakening would break tenant
isolation invariants the product has committed to since v0.11. This story is
P1 because it is the credibility gate for shipping the feature at all.

**Independent Test**: On a Linux host, run the existing v0.11 rate-limit
integration test suite **twice**: once with the optimization enabled, once
with `PORTUNUS_DISABLE_SPLICE=1`. SC-001 (bandwidth ±10 %), SC-002
(concurrent N+1 RST), SC-003 (new-conn rate ±10 %), SC-006 (tenant
isolation) must pass identically in both runs. The reject reasons reported
on the wire and in metrics are bit-identical.

**Acceptance Scenarios**:

1. **Given** a rule with `bandwidth_in_bps = 1_000_000`, **When** a transfer
   runs through it, **Then** the data plane uses the userspace path (the
   fast path is not selected) and v0.11's SC-001 holds (throughput within
   ±10 % of the cap).

2. **Given** a rule with `bandwidth_*_bps = None` and `concurrent_connections =
   100`, **When** 101 concurrent connections attempt to traverse it, **Then**
   the 101st is rejected with the existing `conn_concurrent` reason **and**
   the first 100 connections use the fast path (concurrent cap gates at
   accept time, not in the data path).

3. **Given** an owner with `bandwidth_in_bps` set at the owner level on a
   rule that has no per-rule bandwidth cap, **When** a transfer runs through
   it, **Then** the data plane uses the userspace path (owner bandwidth cap
   forces userspace, mirroring per-rule semantics).

4. **Given** the same rule has its per-rule `bandwidth_in_bps` **removed**
   via a hot-update PUT, **When** subsequent new connections traverse it,
   **Then** they may use the fast path; existing in-flight connections
   continue on whichever path they started on.

---

### Edge Cases

- **`pipe2` fails with `ENFILE` / `EMFILE`**: connection cannot install its
  pipe pair; the connection fails with the same error class as any other
  ENFILE/EMFILE on this host. The fast path does not silently degrade to
  userspace in this case because the resource pressure is host-wide and
  retrying on the userspace path is unlikely to help.
- **`F_SETPIPE_SZ` request exceeds `/proc/sys/fs/pipe-max-size`**: the
  request is best-effort. Failure produces a single `tracing::debug` event
  for the connection and the pipe uses the kernel default (typically 64 KiB).
  Behaviour is correct; only peak throughput is reduced.
- **Kernel returns `ENOSYS` for `splice` (seccomp, very old kernels, unusual
  filesystems)**: the very first syscall returns the unsupported errno
  before any byte has moved. The connection falls back to the userspace
  path. A single warn-level event is emitted; subsequent connections retry
  independently (do not cache "unsupported" host-wide — different rules /
  socket types may behave differently).
- **One side EOFs while the other is mid-write**: the fast path performs
  the same half-close behaviour as `copy_bidirectional` — shutdown the
  write half of the still-open side, continue draining the reverse
  direction, exit once both directions have EOF'd.
- **Connection RST mid-transfer with bytes still queued in the pipe**: the
  in-pipe bytes are released when the connection's pipe pair drops. The
  `bytes_in` / `bytes_out` counters only advance when a byte has been
  written to the destination socket — never when it is only in the pipe.
- **PROXY-out prelude write fails (upstream RST before header complete)**:
  this is a pre-fast-path error. The existing v0.10 multi-target failover
  classification applies; the fast path never executes for that connection.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The `portunus-client` data plane MUST use the kernel zero-copy
  fast path on Linux for any TCP rule that has no per-rule bandwidth cap,
  whose owner has no bandwidth cap, and whose target does not require
  per-byte accounting.
- **FR-002**: On non-Linux platforms (`target_os != "linux"`), no
  fast-path code MUST be compiled into the `portunus-client` binary; the
  call site uses `#[cfg(target_os = "linux")]` to gate the entire branch.
  On Linux, runtime eligibility checks (bandwidth-cap, disable-splice) are
  permitted at the call site.
- **FR-003**: The optimization MUST NOT introduce any operator-visible
  surface: no new rule field, no new operator-API field, no new wire field,
  no new Web UI control, no new server-issued capability gate. The
  optimization MUST NOT appear in any spec / plan / docs *as a thing
  operators configure*.
- **FR-004**: The environment variable `PORTUNUS_DISABLE_SPLICE=1`, when set
  on `portunus-client` at process start, MUST force every connection to use
  the userspace path. The variable is **internal / undocumented to
  operators**, intended for triage and bench A/B; it MUST NOT be advertised
  in `--help`, configuration docs, or release notes beyond a brief
  troubleshooting reference.
- **FR-005**: Selection MUST be per-connection, based on the rule and owner
  state at connection-acceptance time. A subsequent hot-update that adds
  a bandwidth cap MUST NOT migrate already-in-flight fast-path connections
  to the userspace path (mirrors v0.11 hot-reload semantics for in-flight
  flows).
- **FR-006**: When the kernel reports that the fast-path syscall is
  unsupported — explicitly `ENOSYS`, `EINVAL`, `EPERM`, or
  `EOPNOTSUPP` / `ENOTSUP` (Linux treats them as the same value but
  `nix` may surface either constant) — **and zero bytes have moved**,
  the connection MUST fall back to the userspace path in a
  functionally-transparent way: byte stream and counters identical to a
  native userspace run, with a single `proxy.splice_unsupported_fallback`
  tracing event identifying the errno (per FR-011). After any byte has
  moved into the kernel pipe, **no** fallback is permitted — subsequent
  errors are connection-level and propagate as such. `EAGAIN` and
  `EINTR` are **not** fallback conditions: `EAGAIN` is the readiness
  signal (handled by `TcpStream::try_io`'s WouldBlock contract);
  `EINTR` is a retry condition. Neither alters the connection's path.
- **FR-007**: The fast path MUST preserve `copy_bidirectional`'s
  half-close semantics: when one direction EOFs, the write half of the
  reverse direction is shut down and the reverse direction continues
  draining; the connection completes when both directions have EOF'd.
- **FR-008**: The fast path MUST update the same per-rule `bytes_in` /
  `bytes_out` counters and the same Prometheus metrics as the userspace
  path, with identical semantics: counters advance for bytes delivered to
  the destination socket, never for bytes that only traversed the pipe.
- **FR-009**: When `pipe2` fails (`ENFILE` / `EMFILE` or other), the
  connection MUST fail with that error class; the data plane MUST NOT
  silently degrade to the userspace path on resource-pressure errors.
- **FR-010**: `F_SETPIPE_SZ` requests MUST be best-effort: a failure to
  enlarge the pipe MUST NOT fail the connection. The pipe size request
  MUST be at least the default (64 KiB) and SHOULD be 1 MiB where the host
  allows it.
- **FR-011**: Tracing events emitted by the fast path MUST use the
  existing `proxy.*` event-name convention. New event names introduced:
  `proxy.splice_selected`, `proxy.splice_unsupported_fallback`,
  `proxy.splice_pipe_size_failed`. No existing event renamed or removed.
- **FR-012**: All existing v0.1–v0.11 contract / integration / wire-compat
  tests MUST pass unchanged with the optimization enabled (i.e., default)
  and again with `PORTUNUS_DISABLE_SPLICE=1`. Byte-stability is the gate.

### Key Entities

- **CopyCtx** (internal, per-connection): the runtime context the
  forwarder builds at connection acceptance time. Carries the eligibility
  inputs (`has_bandwidth_cap`, `disable_splice`, protocol, OS) used by the
  fast-path selector. Not exposed.
- **PipePair** (internal, per-connection on Linux): a `pipe2`-allocated
  read/write fd pair used as the kernel intermediate buffer between
  source and destination sockets. RAII; closed on connection completion.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001** *(throughput, P1)*: On a dedicated Linux perf/bench host
  (NOT generic CI — CI runners' shared-CPU jitter would render the
  comparison meaningless), with a single TCP connection pumping 1 GiB
  in 1 MiB chunks through a plain rule, the fast path delivers ≥ 1.4×
  the throughput of `PORTUNUS_DISABLE_SPLICE=1` on the same host.
  Measured via a criterion bench reproducing the shape of
  `forwarder::proxy::proxy` (see also v0.1.0 `data_plane.rs` bench).
- **SC-002** *(setup latency, P1)*: p99 connection-setup latency (from
  `accept` to first byte forwarded) on the same bench is within ±5 % of
  the baseline. The optimization MUST NOT regress short-connection
  workloads.
- **SC-003** *(byte stability, P1)*: For every existing rule shape
  (plain TCP, SNI, PROXY-out, rate-limited, owner-capped), the byte
  stream observed at the upstream is bit-identical with the optimization
  enabled vs. disabled. Verified by running the full integration suite
  twice (once with `PORTUNUS_DISABLE_SPLICE=1`).
- **SC-004** *(metric continuity, P1)*: `bytes_in` / `bytes_out` and all
  derived Prometheus metrics are identical between the two runs of the
  integration suite. Per-rule stats reports are identical.
- **SC-005** *(no operator regression, P1)*: All v0.11 rate-limit Success
  Criteria (SC-001..SC-007 from spec 011) continue to hold with the
  optimization enabled.
- **SC-006** *(no cross-platform regression, P1)*: macOS and Windows
  builds compile with no fast-path code in the binary, all existing tests
  pass, no behaviour changes.
- **SC-007** *(unsupported fallback resilience)*: When the **first**
  fast-path syscall on a connection returns one of the unsupported
  errnos (`ENOSYS` / `EINVAL` / `EPERM` / `EOPNOTSUPP`) **before any
  byte has moved** — simulated via seccomp in a test harness, by
  mocking the syscall wrapper, or by injecting the errno at the
  `splice.rs` boundary — that connection MUST fall back to the
  userspace path in a functionally-transparent way (FR-006). Per
  FR-006, connections that have already moved bytes do not benefit
  from this scenario and are out of SC-007's scope. The integration
  suite, when forced to take the fallback path on every connection,
  passes with zero observable difference in functional outcomes
  from a normally-supported run.

## Assumptions

- The deployment OS for the data plane is Linux 5.10+ in production
  (matches the current Docker base image). Older kernels still work via
  the `Unsupported` fallback path.
- The Tokio runtime is the canonical async runtime. `AsyncFd` is not
  introduced; `TcpStream::readable() / writable() / try_io` is used to
  drive `splice` under the Tokio reactor.
- `nix` is already a workspace dependency (used elsewhere); no new crate
  is added.
- Bandwidth caps are the **only** v0.11 feature that requires per-byte
  userspace accounting. `concurrent_connections` and
  `new_connections_per_sec` gate at accept time. Verify this assumption
  holds for every future cap-shaped feature before the project ships one.
- The userspace path is the canonical reference. The fast path is a
  byte-stable specialization; whenever in doubt, the userspace path is
  the source of truth.

## Out of Scope

- **`io_uring`** as a runtime / data-plane primitive. The product stays
  on Tokio; `io_uring` would be a Tokio-replacement-grade change, not a
  fast-path overlay.
- **eBPF / XDP / TC kernel bypass.** Out of scope: these require root /
  CAP_BPF, custom dataplane programs, and conflict with the project's
  `unsafe_code = "forbid"` convention.
- **UDP zero-copy.** `splice` over `SOCK_DGRAM` does not match the
  per-packet semantics the v0.4 UDP path needs. UDP enhancements are a
  separate backlog candidate (`UDP/QUIC strengthening`).
- **SO_REUSEPORT multi-queue listener sharding.** Helpful for high
  short-conn-rate workloads, which this spec explicitly does not target.
- **Splice integration with bandwidth caps** (the "Approach B" rejected
  during brainstorming). Per-chunk token accounting would invalidate the
  fast path's premise.
- **Operator-facing `fast_path: true` flag** (the "Approach C" rejected
  during brainstorming). Eligibility is internal-implementation; surfacing
  it as a rule field would force operators to know about Linux pipe
  internals.
- **TLS termination.** This feature operates strictly below TLS; SNI
  remains peek-and-route only.
- **Kernel TLS (kTLS).** Offloading TLS encrypt/decrypt to the kernel
  would let SNI-routed flows splice ciphertext directly between
  kernel buffers without ever touching userspace. Compelling, but
  requires TLS-key custody in the data plane, which the v2.0
  Constitution explicitly disallows for portunus-client. Revisit only
  if the auth/threat model changes.
- **Global host-wide disable / config surface beyond the env kill
  switch.** `PORTUNUS_DISABLE_SPLICE=1` is the only off-ramp.
  Operators do not get a config-file knob, a CLI flag, a per-rule
  flag, or a Web UI toggle for this. If the fast path is wrong for a
  deployment, the env var disables it; that is the entire interface.

## Dependencies

- v0.9 (009-tls-sni-routing): the fast path is invoked **after** SNI peek
  and replay; it does not modify peek behaviour.
- v0.10 (010-proxy-protocol-and-peek-histogram): the fast path is
  invoked **after** the PROXY prelude write completes; it does not modify
  prelude behaviour.
- v0.11 (011-rate-limiting-qos): the eligibility predicate consults
  per-rule and per-owner bandwidth-cap state. `concurrent_connections`
  and `new_connections_per_sec` gates remain on the accept path and are
  unaffected.

## Open Questions

None outstanding at spec time. Plan-level decisions (exact `splice`
syscall wrapper, raw-fd handling, pipe-pair pool vs. per-connection
allocation, default pipe-size target on common kernels) are deliberately
kept out of the spec and will be locked during `/speckit-plan`.
Locked-in design choices that came out of brainstorming and are no
longer open: (a) borrowed-reference signatures (no socket ownership
transfer), (b) `try_io` over `AsyncFd`, (c) `Unsupported`-only fallback
permitted before first moved byte.

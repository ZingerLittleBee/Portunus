# Phase 0 — Research: PROXY Protocol & Peek Histogram

**Feature**: 010-proxy-protocol-and-peek-histogram
**Date**: 2026-05-09

## R-001 — PROXY protocol lives on `RuleTarget`, not `Rule`

**Decision**: Model the opt-in as a per-target attribute with three states:
`None`, `V1`, `V2`.

**Rationale**:
- The spec requires mixed-target rules where only some upstreams expect PROXY.
- `forward-client` already chooses a concrete target before dialing in the
  multi-target path, so the prelude decision naturally belongs there.

**Alternatives considered**:
- Rule-level setting. Rejected because it cannot represent mixed-target rules.

## R-002 — Encode PROXY protocol without a new dependency

**Decision**: Hand-build PROXY v1/v2 headers using standard library byte
formatting and existing socket address types.

**Rationale**:
- The protocol surface we need is tiny: TCP over IPv4/IPv6, no TLVs, no UNIX.
- The feature spec and repo guidance both prefer zero new workspace deps.

**Alternatives considered**:
- Add a dedicated PROXY protocol crate. Rejected for dependency weight.

## R-003 — Report original client source and accepted local destination

**Decision**: The emitted PROXY header source tuple comes from the accepted
client socket's peer address. The destination tuple comes from the accepted
socket's concrete local address.

**Rationale**:
- This matches the operator-visible contract backends rely on.
- It correctly handles wildcard binds (`0.0.0.0`, `::`) by using the actual
  accepted endpoint rather than the configured bind string.

## R-004 — PROXY prelude write failure is a target connect failure

**Decision**: If the upstream connect succeeds but writing the PROXY header
fails, treat that target the same as any other dial/connect failure in the
existing failover path.

**Rationale**:
- The operator explicitly opted into PROXY. Falling back to raw forwarding would
  silently violate the contract and leak the forward-client address.

## R-005 — Capability gate follows the existing version-gate pattern

**Decision**: The server refuses PROXY-enabled rules for clients whose last
reported `Hello.client_version` is below `0.10.0`, returning
`proxy_protocol_unsupported_by_client`.

**Rationale**:
- Older clients would ignore unknown target fields and behave like v0.9,
  creating a silent downgrade.

## R-006 — Peek histogram uses classic fixed buckets

**Decision**: The client accumulates classic histogram buckets for SNI listener
peek duration and reports cumulative bucket counts, sum, and count over the
existing `StatsReport`.

**Rationale**:
- The repo already uses classic Prometheus collectors.
- Prometheus `histogram_quantile()` works directly on classic histogram bucket
  series, which satisfies the feature's operator query requirement.

## R-007 — Histogram observes all terminal peek outcomes

**Decision**: Record one observation for every SNI listener peek outcome:
success, timeout, and parse failure.

**Rationale**:
- Operators need the tail to include timed-out peeks, not only successful ones.
- This aligns the histogram with the existing failure counters instead of making
  them incomparable.

## R-008 — Histogram stays listener-scoped, not rule-scoped

**Decision**: Metrics are keyed by `(client, listen_port)` and reported as
listener stats, not rule stats.

**Rationale**:
- Peek happens before rule selection, so misses/parse failures do not have
  honest rule attribution.

## R-009 — Bucket layout

**Decision**: Use fixed buckets covering `100µs` through `3s`, with denser
resolution below `10ms` and an inclusive `3.0s` tail bucket.

**Rationale**:
- Operators need to distinguish normal sub-10ms operation from degraded
  hundreds-of-ms and timeout tails.

**Candidate bucket set**:
`[0.0001, 0.00025, 0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 3.0]`

## R-010 — Storage remains additive

**Decision**: Persist the new per-target PROXY mode additively in the existing
server rule storage and default absent values to "not opted in".

**Rationale**:
- Existing rules must survive the upgrade with unchanged behaviour.

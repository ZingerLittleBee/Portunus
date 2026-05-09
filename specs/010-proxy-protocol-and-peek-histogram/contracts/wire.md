# Contract: Wire Delta — PROXY Protocol & Peek Histogram

## 1. Target delta

`Target` gains an additive optional field describing the upstream PROXY protocol
mode. Absent means legacy raw forwarding.

## 2. Listener stats delta

`SniListenerStats` gains additive histogram payload fields sufficient to publish
classic Prometheus histogram series on the server:
- bucket upper bounds/counts
- cumulative observation count
- cumulative observation sum

Finite bucket counts represent observations less than or equal to the paired
upper bound. Observations above the largest finite bound are represented by the
cumulative count / `+Inf` series only.

## 3. Compatibility

- v0.9 clients ignore the new target field, so the server must capability-gate
  before sending PROXY-enabled rules.
- Absent histogram payload preserves v0.9 metrics behaviour.

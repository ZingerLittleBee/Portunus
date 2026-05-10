# Quickstart: Rate Limiting & QoS

1. Start `portunus-server` and a v0.11 `portunus-client`.
2. Push one TCP rule with `rate_limit.bandwidth_in_bps = 1048576` and
   `rate_limit.concurrent_connections = 100`. Verify
   `portunus_rate_limit_*` collectors appear under `/metrics` for that
   rule.
3. Drive 5 MB/s of TCP traffic at the rule for 30 s; assert measured
   ingress ≤ 1 MB/s ± 10% and `portunus_rate_limit_throttle_seconds_total`
   grows monotonically.
4. Open 100 concurrent connections; assert the 101st is closed with
   RST within 50 ms and `portunus_rate_limit_reject_total{reason="conn_concurrent"}`
   increments by 1.
5. `PUT /v1/rules/{id}` lowering the bandwidth cap to 102400 (100 KB/s);
   assert the in-flight throttled connection's throughput converges to
   ≤ 100 KB/s within 2 s and the connection is **not** closed.
6. Provision two RBAC owners on the same client with rules each. Set
   `PUT /v1/clients/edge-01/owners/alice/rate-limit` with
   `bandwidth_in_bps = 10485760`. Drive owner Alice's combined load to
   50 MB/s and owner Bob's to 20 MB/s; assert Alice's combined ingress
   ≤ 10 MB/s ± 10% and Bob's rules are unaffected.
7. Lower owner Alice's `concurrent_connections` cap below the live
   count; assert no in-flight connection is closed and new connections
   are rejected against the new lower cap.
8. Push a rule with `rate_limit` set against a client whose
   `Hello.client_version` is `0.10.0`; assert the operator API returns
   `422 rate_limit_unsupported_by_client` and the rule does not
   activate anywhere.
9. Open the Web UI, navigate to the rules table, confirm the new
   "Caps" column and the rule editor's QoS section. Open the client
   detail page, switch to the "Owner quotas" tab, confirm Alice's
   envelope is editable and reject / throttle counters are displayed
   for the last 5 min and last 1 h.

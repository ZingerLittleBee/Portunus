# Quickstart: PROXY Protocol & Peek Histogram

1. Start `forward-server` and a v0.10 `forward-client`.
2. Push one TCP rule with two targets, one target carrying `proxy_protocol = "v1"` and one target omitted.
3. Verify the PROXY-enabled backend sees the original client IP while the non-opted-in backend receives the legacy byte stream.
4. Push a rule with `proxy_protocol = "v2"` and verify binary PROXY header emission.
5. Drive SNI traffic through an SNI-mode listener and scrape `/metrics`.
6. Confirm `forward_tls_client_hello_peek_duration_seconds_bucket`, `_sum`, and `_count` exist for the SNI listener and do not exist for a legacy plain-TCP listener.

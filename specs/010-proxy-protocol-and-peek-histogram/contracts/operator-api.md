# Contract: Operator API — PROXY Protocol & Peek Histogram

## 1. `POST /v1/rules` request shape

Targets gain an optional per-target field:

```jsonc
{
  "client": "edge-01",
  "listen_port": 443,
  "protocol": "tcp",
  "targets": [
    { "host": "10.0.1.5", "port": 8443, "priority": 0, "proxy_protocol": "v1" },
    { "host": "10.0.1.6", "port": 8443, "priority": 1 }
  ]
}
```

Accepted values:
- omitted: no PROXY protocol
- `"v1"`
- `"v2"`

Validation errors:
- TCP-only violation → `400 validation.proxy_protocol_on_unsupported_rule`
- unknown enum string → `400 validation.proxy_protocol_invalid`
- capability gate failure → `422 proxy_protocol_unsupported_by_client`

## 2. Rule read surfaces

`GET /v1/rules`, CLI rendering, and management UI rule views expose the
per-target `proxy_protocol` setting.

## 3. Prometheus surface

Additive collector:

`portunus_tls_client_hello_peek_duration_seconds_bucket{client,port,le}`

With companion `_sum` and `_count` series.

Finite buckets cover configured boundaries through `le="3"` only. Observations
above 3 seconds are visible through `_count` / `le="+Inf"` and must not be
reported in finite `le` buckets.

No observations for legacy plain-TCP listeners.

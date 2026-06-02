# Contract: gRPC wire delta (`proto/portunus.proto`)

All changes are **additive** — new fields only, no field removed or renumbered. Old clients
ignore new fields; the new server tolerates their absence. Field numbers below are the next
free number in each message (confirm against the live `.proto` at implementation time).

## `CredentialBundle`

```proto
message CredentialBundle {
  uint32 version = 1;
  string client_name = 2;          // UNCHANGED — now display-only
  string server_endpoint = 3;
  string server_cert_sha256 = 4;
  string server_cert_pem = 5;
  string token = 6;
  string client_id = 7;            // NEW — stable opaque ULID
}
```

Contract tests:
- A bundle with `client_id` populated round-trips through serialize/deserialize.
- A bundle WITHOUT `client_id` (legacy) still parses; the client connects using `token`.

## `OwnerRateLimitUpdate`

```proto
message OwnerRateLimitUpdate {
  string client_name = 1;          // UNCHANGED — display
  string owner_id    = 2;
  optional RateLimit rate_limit = 3;
  OwnerRateLimitAction action   = 4;
  string client_id = 5;            // NEW — authoritative target key
}
```

Server behavior: when `client_id` is present it is authoritative; `client_name` is advisory
display. (Operator-originated messages always carry `client_id` after this change.)

## `TrafficQuotaUpdate`

```proto
message TrafficQuotaUpdate {
  string request_id        = 1;
  string user_id           = 2;
  string client_name       = 3;    // UNCHANGED — display
  TrafficQuotaAction action = 4;
  optional TrafficQuotaState state = 5;
  string client_id = 6;            // NEW — authoritative target key
}
```

## `EnrollClientRequest` / enrollment response

`EnrollClientRequest` (currently `{ string code = 1; }`) is unchanged on the request side —
enrollment is initiated by code. The server **assigns** the `ClientId` and returns it in the
resulting `CredentialBundle.client_id`. If enrollment input also accepts a display name, that
name flows through with relaxed validation.

## `Hello` / `Welcome` — UNCHANGED

Neither message carries a client name today; the connecting client's identity is resolved
server-side from the bearer token. No wire change is needed for transparent legacy
connectivity (see research R-004/R-005).

## Regeneration & historical snapshots

- Regenerate via `tonic-prost` (the normal build path) after editing `proto/portunus.proto`.
- Do **not** edit historical `specs/*/contracts/portunus.proto` snapshots — they are frozen.

# Contract: `GET /v1/rules/{rule_id}/stats/stream`

Server-Sent-Events stream of `RuleStatsSnapshot` values for one rule.
One event per StatsReport tick (5 s by default; matches the existing
`StatsReport` cadence). Same ownership semantics as the non-streaming
`GET /v1/rules/{rule_id}/stats`.

## Request

```
GET /v1/rules/{rule_id}/stats/stream
Authorization: Bearer <operator-token>
Accept: text/event-stream
```

| Path param | Type | Validation |
|---|---|---|
| `rule_id` | u64 | numeric; non-numeric → 422 `invalid_rule_id` |

`Accept: text/event-stream` is recommended but not required; the server
always emits `Content-Type: text/event-stream` regardless.

## Authorization

- `auth_layer` middleware: 401 on bad/missing bearer.
- Handler-side ownership check (identical to the non-streaming variant):
  - Look up the rule. Not found → 404 `rule_not_found`.
  - If caller is `superadmin`, allow.
  - Else if caller's `user_id` equals the rule's `owner_user_id`, allow.
  - Else 403 `not_owner`.

The ownership check happens **once at connect time**. If the rule's
owner changes mid-stream (it can't — owners are immutable in v0.5),
or the rule is removed (it can), the stream closes naturally as the
broadcast sender drops; the client sees `EventSource.onerror` and
reconnects, at which point a fresh ownership / existence check fires.

## Response

### 200 OK (stream)

```
HTTP/1.1 200 OK
Content-Type: text/event-stream
Cache-Control: no-cache, no-transform
X-Accel-Buffering: no
Connection: keep-alive
```

Then a stream of SSE events. Each event is:

```
id: <monotonic event id, e.g. unix ms>
event: stats
data: {...RuleStatsSnapshot JSON, byte-identical to GET /v1/rules/{id}/stats response shape...}

```

(Trailing blank line per SSE protocol.)

The first event is sent immediately after connect with the cache's
current snapshot (if any). Subsequent events fire whenever
`RuleStatsCache::observe` is called for this rule.

If there is no cached snapshot at connect time (no StatsReport yet),
the server still establishes the stream and waits for the first tick;
no `event: stats` is emitted until data arrives. The client's UI
shows a "Waiting for first stats report…" placeholder.

A keepalive comment (`: keepalive\n\n`) is sent every **30 s** to
keep middleboxes from closing idle connections. Browsers ignore
comment lines.

### 401 / 403 / 404 / 422

Standard `ApiError` JSON body (no SSE):

```json
{ "error": { "code": "...", "message": "..." } }
```

When the rule is removed mid-stream, the server closes the connection
cleanly (no "rule_removed" event — SSE doesn't have a typed close
reason; the browser surfaces this as `EventSource.onerror` and the
client reconnects, then gets a 404).

## Backpressure

The server uses a `tokio::sync::broadcast` channel of capacity 16
per rule. A subscriber that can't keep up loses the oldest unsent
events; the server logs a structured `event = "stats_stream.lagged"`
warning with the rule_id and subscriber count once per minute per
rule.

## Browser behaviour

- The browser's `EventSource` automatically reconnects with the last
  `id:` it received as the `Last-Event-ID` request header. The server
  ignores this header — every connection re-sends from the current
  snapshot, not a replay.
- The server emits `retry: 1000\n` on the first event after connect
  to set the browser's retry timer to 1 s; the client's exponential
  backoff is enforced application-side (see R-007 in research.md).

## Side effects

- A connection emits ONE `operator.allow` audit entry at connect time.
- A failed ownership check emits ONE `operator.deny` audit entry.
- Per-event audit entries are NOT emitted (would dominate the ring
  buffer; per-stream audit is sufficient).

## Test plan

`crates/portunus-server/tests/rule_stats_stream_contract.rs` (new):

1. Subscribe as the rule's owner → receive a snapshot within 6 s.
2. Subscribe as superadmin to a rule owned by alice → snapshot
   delivered.
3. Subscribe as bob to alice's rule → 403 `not_owner`.
4. Subscribe to a non-existent rule → 404 `rule_not_found`.
5. Subscribe with no bearer → 401 `unauthenticated`.
6. Two subscribers on the same rule both receive the same snapshot
   (proves broadcast fan-out, R-008).
7. Remove the rule mid-stream → existing subscribers receive
   end-of-stream within 1 s (no further data).
8. Slow consumer (subscriber that never reads) does NOT block other
   subscribers.

## Implementation notes (non-binding)

- Use `axum::response::sse::{Sse, Event, KeepAlive}`.
- Convert the `broadcast::Receiver<RuleStatsSnapshot>` to a
  `tokio_stream::wrappers::BroadcastStream` and then `map` each
  snapshot into `Event::default().event("stats").json_data(snap)`.
- Apply `KeepAlive::new().interval(30s).text("keepalive")`.
- Disable response compression for this route (axum's default doesn't
  compress text/event-stream, but make it explicit if there's a
  `CompressionLayer` in the stack).

<!-- SPECKIT START -->
Active feature: `009-tls-sni-routing` on branch `009-tls-sni-routing`.
v0.9 lets a single forward-client TCP listener fan out to different
upstreams based on the TLS hostname (SNI) the client requests in its
ClientHello. forward-rs stays a pure L4 byte-passthrough — never
decrypts, terminates, or re-encrypts TLS. Implementation lives in the
data plane on `forward-client` (peek + parse + route) and in additive
control-plane fields on `forward-server`; auth seam, credential
hashing, persistence layer, and forwarding hot-path layout are
byte-stable for v0.8 callers. Zero new workspace deps.

Key invariants:
- `(client, single TCP port)` listeners are mode-locked for their
  lifetime (legacy plain-TCP or SNI dispatch); online conversion is
  forbidden — operators must remove the existing rule first.
- Wire fields are additive: `Rule.sni_pattern = 11`,
  `RuleStats.sni_route_*_total = 13/14/15` (11/12 are taken by v0.7),
  new `SniListenerStats` on `StatsReport.sni_listener_stats = 3`.
- Capability gate: `sni_pattern` push to a v0.8 client → 422
  `sni_unsupported_by_client` before any rule activates.
- Data-plane events are tracing-only — they do NOT enter the SQLite
  operator audit ring (D13).

For technical context, project structure, dependency choices, and the
Constitution Check, read the current plan:
- `specs/009-tls-sni-routing/plan.md`
- Supporting artifacts in the same directory: `spec.md`, `design.md`
  (brainstorm output with three rounds of code-review history),
  `research.md` (R-001..R-015 decisions), `data-model.md`,
  `contracts/wire.md`, `contracts/operator-api.md`, `contracts/cli.md`,
  `quickstart.md`, `checklists/requirements.md`.

Inherited baselines (do not re-derive):
- v0.8.0 — `specs/008-sqlite-storage/plan.md`. Server persistent state
  unified into one embedded SQLite at `<data-dir>/state.db`. v0.9
  schema gains one additive column (`rules.sni_pattern TEXT`) and one
  helper partial index; schema-version range shifts `[1,1] → [1,2]`.
- v0.7.0 — `specs/007-multi-target-failover/plan.md`. Rules carry
  `Rule.targets[]` (length 1..=8) with priority + client-side health.
  SNI selects which Rule, then v0.7 target selection + failover apply
  unchanged.
- v0.6.0 — `specs/006-management-web-ui/plan.md`. React+Vite SPA
  embedded via rust-embed; v0.9 adds an `SNI` column on the rules page
  and an optional input on the rule editor.
- v0.5.0 — `specs/005-multi-user-rbac/plan.md`. RBAC envelope unchanged.
  v0.9 metric label conventions (`client, rule, owner` per-rule;
  `client, port` per-listener) follow v0.5+.
- v0.4.0 — `specs/004-udp-forward/plan.md`. UDP target selection
  per-flow on first packet; failover applies to NEW flows only. SNI
  is TCP-only — v0.9 explicitly does NOT touch the UDP path.
- v0.3.0 — `specs/003-domain-name-forward/plan.md`. DNS resolver applies
  per-target; resolution failures count as connect failures for that
  target's health.
- v0.2.0 — `specs/002-port-range-forward/plan.md` (range rules). SNI
  is rejected on port-range rules (FR-002).
- v0.1.0 — `specs/001-tcp-forward-mvp/plan.md` (TCP forwarding MVP).

Project-wide governance: `.specify/memory/constitution.md` (currently v2.0.1 —
TLS + bearer token, NOT mTLS).
<!-- SPECKIT END -->

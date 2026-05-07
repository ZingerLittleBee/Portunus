<!-- SPECKIT START -->
Active feature: `006-management-web-ui` on branch `006-management-web-ui`.
First UI-side feature: ship the long-deferred `TODO(WEB_UI)`. A React +
Vite SPA embedded into `forward-server` via `rust-embed`, consuming the
v0.5 operator HTTP API verbatim plus two additive endpoints
(`GET /v1/audit`, `GET /v1/rules/{id}/stats/stream`) and a small
`GET /v1/users/me` projection.

For technical context, project structure, dependency choices, and the
Constitution Check, read the current plan:
- `specs/006-management-web-ui/plan.md`
- Supporting artifacts in the same directory: `research.md` (R-001..R-014
  decisions), `data-model.md`, `contracts/audit-endpoint.md`,
  `contracts/stats-stream-endpoint.md`, `contracts/ui-routes.md`,
  `quickstart.md`.

Inherited baselines (do not re-derive):
- v0.5.0 — `specs/005-multi-user-rbac/plan.md`. RBAC, identity store,
  audit log emit sites — UI consumes these contracts verbatim.
- v0.4.0 — `specs/004-udp-forward/plan.md`. UDP rule shape + per-rule
  UDP collectors surface in the UI's rule-stats panel.
- v0.3.0 — `specs/003-domain-name-forward/plan.md` (DNS resolver).
- v0.2.0 — `specs/002-port-range-forward/plan.md` (range rules).
- v0.1.0 — `specs/001-tcp-forward-mvp/plan.md` (TCP forwarding MVP).

Project-wide governance: `.specify/memory/constitution.md` (currently v2.0.1 —
TLS + bearer token, NOT mTLS).
<!-- SPECKIT END -->

<!-- SPECKIT START -->
Active feature: `007-multi-target-failover` on branch `007-multi-target-failover`.
v0.7 extends a forwarding rule from a single `(target_host, target_port)` to
an ordered list of targets with priority-ordered failover and per-target
client-side health tracking. The single-target hot path stays byte-identical
to v0.6.0 — multi-target lives in a separate code path entered via
`match targets.len() { 1 => fast_path, _ => failover_path }`.

For technical context, project structure, dependency choices, and the
Constitution Check, read the current plan:
- `specs/007-multi-target-failover/plan.md`
- Supporting artifacts in the same directory: `research.md` (R-001..R-013
  decisions), `data-model.md`, `contracts/proto-rule-extension.md`,
  `contracts/operator-api.md`, `contracts/ui-routes.md`, `quickstart.md`.

Inherited baselines (do not re-derive):
- v0.6.0 — `specs/006-management-web-ui/plan.md`. React+Vite SPA embedded
  via rust-embed; SSE rule-stats stream the v0.7 detail page extends with
  `?per_target=true`.
- v0.5.0 — `specs/005-multi-user-rbac/plan.md`. RBAC envelope unchanged in
  v0.7: targets are NOT part of the grant (FR-021).
- v0.4.0 — `specs/004-udp-forward/plan.md`. UDP target selection happens
  once per flow on first packet; the chosen upstream sticks for the
  flow's lifetime (failover applies to NEW flows only).
- v0.3.0 — `specs/003-domain-name-forward/plan.md`. DNS resolver applies
  per-target; resolution failures count as connect failures for that
  target's health.
- v0.2.0 — `specs/002-port-range-forward/plan.md` (range rules).
- v0.1.0 — `specs/001-tcp-forward-mvp/plan.md` (TCP forwarding MVP).

Project-wide governance: `.specify/memory/constitution.md` (currently v2.0.1 —
TLS + bearer token, NOT mTLS).
<!-- SPECKIT END -->

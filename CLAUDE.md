<!-- SPECKIT START -->
Active feature: `005-multi-user-rbac` on branch `005-multi-user-rbac`.
First operator-side feature: introduces multi-user identity, RBAC,
per-user grants (client / port-range / protocol). Builds on the v0.4.0
data plane without touching it.

For technical context, project structure, dependency choices, and the
Constitution Check, read the current plan:
- `specs/005-multi-user-rbac/plan.md`
- Supporting artifacts in the same directory: `research.md`, `data-model.md`,
  `contracts/operator-api.md` (HTTP + CLI surface),
  `contracts/persistence.md` (`identity.json` schema), `quickstart.md`.

Inherited baselines (do not re-derive):
- v0.4.0 — `specs/004-udp-forward/plan.md` and its supporting artifacts.
  The forwarding hot path (TCP + UDP + DNS + port-range) is unchanged
  in v0.5.0; this feature adds an operator-side authorization layer
  above it.
- v0.3.0 — `specs/003-domain-name-forward/plan.md` (DNS resolver).
- v0.2.0 — `specs/002-port-range-forward/plan.md` (range rules).
- v0.1.0 — `specs/001-tcp-forward-mvp/plan.md` (TCP forwarding MVP,
  including the original `forward-auth` client-token store this
  feature mirrors for operator tokens).

Project-wide governance: `.specify/memory/constitution.md` (currently v2.0.1 —
TLS + bearer token, NOT mTLS).
<!-- SPECKIT END -->

<!-- SPECKIT START -->
Active feature: `003-domain-name-forward` on branch `003-domain-name-forward`.
This is an additive extension on top of v0.2.0 (`002-port-range-forward`),
which itself sits on v0.1.0 (`001-tcp-forward-mvp`).

For technical context, project structure, dependency choices, and the
Constitution Check, read the current plan:
- `specs/003-domain-name-forward/plan.md`
- Supporting artifacts in the same directory: `research.md`, `data-model.md`,
  `contracts/forward.proto` (additive overlay), `contracts/operator-api.md`
  (additive deltas), `contracts/persistence.md` (additive deltas),
  `quickstart.md`.

Inherited baselines (do not re-derive):
- v0.2.0 — `specs/002-port-range-forward/plan.md` and its supporting
  artifacts in the same directory.
- v0.1.0 — `specs/001-tcp-forward-mvp/plan.md` and its `data-model.md`,
  `contracts/forward.proto`, `contracts/operator-api.md`,
  `contracts/persistence.md`, `quickstart.md`.

Project-wide governance: `.specify/memory/constitution.md` (currently v2.0.1 —
TLS + bearer token, NOT mTLS).
<!-- SPECKIT END -->

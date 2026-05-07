<!-- SPECKIT START -->
Active feature: `002-port-range-forward` on branch `002-port-range-forward`.
This is an additive extension on top of v0.1.0 (`001-tcp-forward-mvp`).

For technical context, project structure, dependency choices, and the
Constitution Check, read the current plan:
- `specs/002-port-range-forward/plan.md`
- Supporting artifacts in the same directory: `research.md`, `data-model.md`,
  `contracts/forward.proto` (additive overlay), `contracts/operator-api.md`
  (additive deltas), `contracts/persistence.md` (additive deltas),
  `quickstart.md`.

Inherited baseline (v0.1.0, do not re-derive):
- `specs/001-tcp-forward-mvp/plan.md` and its `data-model.md`,
  `contracts/forward.proto`, `contracts/operator-api.md`,
  `contracts/persistence.md`, `quickstart.md`.

Project-wide governance: `.specify/memory/constitution.md` (currently v2.0.1 —
TLS + bearer token, NOT mTLS).
<!-- SPECKIT END -->

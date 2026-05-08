<!-- SPECKIT START -->
Active feature: `008-sqlite-storage` on branch `008-sqlite-storage`.
v0.8 collapses every server-side persistent JSON file
(`tokens.json`, `identity.json`, `rules.json`) and the in-memory audit
ring buffer into one embedded SQLite database at
`<data-dir>/state.db`. Single-binary deployment unchanged; closes the
constitution-level `TODO(STORAGE_CHOICE)`. Adds a `--data-dir` flag
(separate from `--config-dir`), a backup / restore / reset CLI, and
additive `since` / `until` / `cursor` query parameters on
`GET /v1/audit`. The forwarding hot path (TCP / UDP fast paths, wire
protocol) and the auth seam are not touched. Clean-slate schema — no
migration from existing JSON files since the project is not yet
deployed.

For technical context, project structure, dependency choices, and the
Constitution Check, read the current plan:
- `specs/008-sqlite-storage/plan.md`
- Supporting artifacts in the same directory: `research.md` (R-001..R-015
  decisions), `data-model.md`, `contracts/persistence.md`,
  `contracts/operator-api.md`, `contracts/cli.md`, `quickstart.md`,
  `checklists/requirements.md`.

Inherited baselines (do not re-derive):
- v0.7.0 — `specs/007-multi-target-failover/plan.md`. Rules now carry
  `Rule.targets[]` (length 1..=8) with priority + client-side health.
  v0.8's `rule_targets` table preserves the same shape; multi-target
  control-plane is byte-stable.
- v0.6.0 — `specs/006-management-web-ui/plan.md`. React+Vite SPA
  embedded via rust-embed; the audit page now consumes the new
  `since/until/cursor` envelope when the operator scrolls back.
- v0.5.0 — `specs/005-multi-user-rbac/plan.md`. RBAC envelope unchanged
  in v0.8: `users` / `credentials` / `grants` move into SQLite tables
  with byte-stable trait seams.
- v0.4.0 — `specs/004-udp-forward/plan.md`. UDP target selection
  per-flow on first packet; failover applies to NEW flows only.
- v0.3.0 — `specs/003-domain-name-forward/plan.md`. DNS resolver applies
  per-target; resolution failures count as connect failures for that
  target's health.
- v0.2.0 — `specs/002-port-range-forward/plan.md` (range rules).
- v0.1.0 — `specs/001-tcp-forward-mvp/plan.md` (TCP forwarding MVP).

Project-wide governance: `.specify/memory/constitution.md` (currently v2.0.1 —
TLS + bearer token, NOT mTLS).
<!-- SPECKIT END -->

# Remove user-side operator API token (self-issued credentials)

- **Status:** Design approved (brainstorming) — pending implementation plan
- **Date:** 2026-06-20
- **Approach:** A — remove the user-facing self-issuance surface; keep the bearer verify path + the static `operator_token`
- **Impact map:** verified by a 7-dimension adversarial workflow (backend HTTP / store / CLI, frontend, tests+e2e, docs, migration). Findings drive the inventory below.

## 1. Context & motivation

Today an operator **user** can self-issue, list, rotate, and revoke long-lived
bearer API tokens against the control plane:

- HTTP: `POST/GET /v1/users/{id}/credentials`, `DELETE .../{cred_id}`,
  `POST .../{cred_id}/rotate`
- CLI: `credential-issue` / `credential-list` / `credential-revoke` /
  `credential-rotate`
- Web UI: the **Credentials** section on the user detail page
  (issue / rotate / revoke + one-time token reveal)

This is a second, parallel token system alongside the data-plane client tokens
and the static `operator_token`. It widens the attack surface (a prior audit
found name-keyed scoping leaks and a fail-open path on adjacent surfaces) and it
costs mental overhead: operators must reason about per-user dynamic bearer
tokens *and* cookie sessions *and* the static machine token.

**Goal:** remove the user-side self-issuance surface entirely, collapsing the
operator auth story to two clear paths — **humans use Web cookie login**,
**machines use the static `operator_token`** — while keeping every kept path
(bearer verify, `operator_token`, `bootstrap-superadmin`, Web onboarding, and
the unrelated data-plane client tokens) fully working.

## 2. Decisions (resolved during brainstorming)

| # | Decision | Choice |
|---|----------|--------|
| D1 | Removal depth | **Approach A** — remove HTTP + CLI + UI + store CRUD for user-issued credentials; keep the bearer verify path and `operator_token`. |
| D2 | `operator_token` (server.toml) | **Keep.** It is the sanctioned machine/CI/e2e path. It is stored *as* a `_legacy` superadmin credential and rides the same `verify` path, so the credential table + verify must stay. |
| D3 | `bootstrap-superadmin` CLI | **Keep** (already documented "Legacy"). It is not a product-UI token; removing it would force a disproportionate, high-risk rewrite of the dev loop, the entire webui e2e fixture, the demo, 5 test fixtures, and many docs, and could strand pure-CLI deployments. |
| D4 | Password-less-user lockout | **`create_user` now requires a password.** Removing issuance + the V013 cleanup would permanently lock out any user that has only a self-issued token and no password. |
| D5 | Vestigial "API token" plumbing | **Remove entirely.** With user tokens gone, `keep_api_tokens`, `api_tokens_revoked`, `UserView.credential_count`, and reset-password's token-revocation half become dead; rip them out (keep session revocation). |

## 3. Resulting authentication model

| Actor | Mechanism | Notes |
|-------|-----------|-------|
| Human (superadmin / user) | **Password + Cookie session** | First superadmin via Web onboarding (`setup_token` → password). Other users created by a superadmin **with a password** (D4). |
| Machine / CI / e2e | **Static `operator_token`** in `server.toml` | Boot-time `bootstrap_legacy_superadmin` mints the `_legacy` superadmin; authenticated by the kept `verify` path. |
| First-superadmin bootstrap (CLI/headless) | **`bootstrap-superadmin`** or `operator_token` | Unchanged (D3). |
| ~~User-issued dynamic API token~~ | **Removed** | No issue / list / rotate / revoke anywhere. |

`setup_token` is a one-time onboarding secret for setting the first password — it
is **not** an API token and is unaffected.

## 4. Scope — remove vs keep

### 4.1 Backend — HTTP (`portunus-server`)

**Remove**
- `operator/credentials.rs` — delete the whole file (four handlers
  `get_credentials` / `post_credential` / `delete_credential` /
  `post_credential_rotate`; `IssueCredentialResponse{token}`, `IssueCredentialBody`,
  `RotateCredentialBody`, `CredentialView`; private helpers `check_owner_or_super`,
  `parse_cred_id`, `parse_user_id`, `api_rbac`, `api_store`). Verified: no symbol is
  referenced outside the file except the routes (also removed).
- `operator/http.rs` — the three credential `.route(...)` calls (`/v1/users/{id}/credentials`,
  `.../{cred_id}`, `.../{cred_id}/rotate`) and the `credentials` entry in the
  `use crate::operator::{…}` import list (else an unused-import fails `-D warnings`).
- `operator/mod.rs` — the `pub mod credentials;` line (sole module wiring; no re-export).

**Keep**
- `ApiError` and all shared error/response machinery (used by every handler).
- `route_layer(auth_middleware)` — order-independent of the removed routes.

### 4.2 Backend — store (`store/operator_store.rs`, `portunus-auth`)

**Remove** (verified: only callers are the removed handlers)
- `issue_credential`, `revoke_credential`, `rotate_credential`.
- `list_credentials` **and** its helper `row_to_credential` — these were
  retained only for `UserView.credential_count`, which D5 removes; once
  `users.rs` stops calling `list_credentials`, all three call sites are gone, so
  these become dead and are removed too. (`count_active_credentials` is already
  unused; remove or leave per implementation taste.)

**Keep** (each has independent callers on the kept bootstrap/verify path)
- `insert_credential`, `hash_token`, `bootstrap_pair`, `bootstrap_legacy_superadmin`,
  `OperatorAuthenticator::verify`, and the `Credential` / `CredentialStatus` /
  `CredentialId` types. The `credentials` **table** and its schema are unchanged.

### 4.3 Backend — CLI (`portunus-server`)

**Remove**
- `main.rs` — `Cmd::CredentialIssue` / `CredentialList` / `CredentialRevoke` /
  `CredentialRotate` variants and their dispatch arms.
- `operator/identity_cli.rs` — `credential_issue` / `credential_list` /
  `credential_revoke` / `credential_rotate` and the `// --- credentials ---` header.

**Keep** — `gen-token`, `bootstrap-superadmin` (D3), `reset-password`,
`user-add` / `user-list` / `user-get` / `user-remove`, `grant-*`.

### 4.4 Frontend (`webui/`)

**Remove**
- `src/api/credentials.ts` (all four hooks) — **but first inline `credentialsKey`**
  into `src/api/users.ts:4/97` as a literal (`["users", userId, "credentials"]`),
  or drop that invalidation, before deleting the module (else build break).
- `pages/UserDetail.tsx` — the credentials `<section>`, the four credential hooks,
  `rotateTarget` state + the rotate `ConfirmDialog`, and the `credentials.data`
  line in the delete-dialog `dependents` list.
- `pages/UsersList.tsx` — the credentials column.
- `api/types.ts` — `CredentialView`, `IssueCredentialBody`, `IssueCredentialResponse`,
  and `UserView.credential_count` (D5).
- i18n: `userDetail.{credentials,noCredentials,issueCredential,rotate,revoke,rotateTitle,rotateBody,rotateConfirm}`
  and `users.credentials` — **deleted symmetrically from `en.json` and `zh-CN.json`**
  (an i18n parity test gates this). Reword `tokenReveal.*` to password-only.

**Keep**
- **`TokenRevealModal`** — shared with the password-reset reveal
  (`revealKind === 'password'`); deleting it would break the human-bootstrap path.
- The client-enrollment components (`EnrollmentInstallGuide`, `CredentialBundleCard`) —
  a separate, untouched token system.

### 4.5 Documentation

Remove the user-credential sections and rewrite token-sourcing guidance to point
machines at the static `operator_token`. EN + ZH must move in lockstep.

- **Remove sections:** the `### Credentials` endpoint + request-body blocks in
  `operator-http.mdx`; the `## Credentials` CLI table in `portunus-server.mdx`;
  the `## API token rotation` section and `credential-issue` tenant step + CLI-surface
  row in `rbac.mdx`; the **Credentials** row + "issue credentials" in `web-ui.mdx`.
- **Rewrite (not delete):** the deployment docs that tell operators to "create a
  token on the Web UI Credentials page" (`docker.mdx`, `systemd.mdx`) → source
  `operator_token` from `server.toml` (`gen-token`); `rbac.mdx` Credential concept
  bullet; the password-reset "revokes API tokens" wording.
- **Keep:** `bootstrap-superadmin` docs (D3); `server.mdx` `operator_token` + `gen-token`
  block (the canonical machine-token doc); all data-plane client-enrollment / credential-bundle
  docs and the `credentials` SQLite-store data-model rows.
- Add a short, single canonical note ("machines authenticate with the static
  `operator_token`; humans use Web cookie login") in `web-ui.mdx` / `rbac.mdx`.

## 5. Data migration — `V013`

Current highest refinery version is `V012`; add `V013__drop_user_credentials.sql`,
picked up automatically (no manifest). Single guarded statement, idempotent and
crash-safe inside refinery's transactional version gate:

```sql
-- Remove all user self-issued operator credentials. Preserve the reserved
-- bootstrap credentials (`_legacy` operator_token, `_superadmin`). The reserved
-- namespace is closed: UserId::reserved is the only constructor of '_'-prefixed
-- ids and FromStr rejects operator-supplied ones, so this predicate is safe and
-- future-proof against any new reserved id.
DELETE FROM credentials WHERE user_id NOT LIKE '\_%' ESCAPE '\';
```

**Why deletion (not "leave inert"):** `verify` matches *any* `active` row
regardless of owner, and once the revoke handlers are gone there is **no remaining
revoke-by-id path**. Leaving rows inert would let stale user tokens authenticate
forever with no way to revoke them — a standing security hole. Deletion is the
safe choice. The `credentials` table schema is unchanged.

## 6. Edge cases & behavior changes

### 6.1 Lockout mitigation — passwords now required (D4)

Today `post_users.initial_password` is `Option`, and the **UserCreate UI does not
send a password at all** — so UI-created users are password-less and reachable
only via an issued credential. After removal + V013 such a user is permanently
unauthenticatable. Fix, applied across all three creation surfaces:

- `post_users` rejects a missing/empty initial password.
- **UserCreate UI** gains a required initial-password step. Recommended mechanism
  (decide in the plan): auto-generate a temporary password, reveal it once via the
  **kept `TokenRevealModal`**, and set `password_change_required = true` — mirroring
  the onboarding/temporary-password pattern and reusing kept components.
- **CLI `user-add`** gains a password source (a `--password*` flag, or auto-generate
  + print a temporary password once, consistent with the UI).
- Existing password-less users are recovered by a superadmin via the kept
  `reset-password` path.

### 6.2 Vestigial plumbing removal (D5)

With user tokens gone, remove the now-dead "API token" plumbing; **keep session
revocation** on password reset:

- `reset_password_state` — drop the `revoke_api_tokens` branch; keep `revoke_sessions`.
- CLI `reset-password` — remove the `--keep-api-tokens` flag and threaded param.
- HTTP — remove `keep_api_tokens` (request) and `api_tokens_revoked` (response) from
  the change-password / reset-password contracts; remove `PasswordResetSummary.api_tokens_revoked`.
- `UserView.credential_count` — removed (cascades to the store-fn removals in §4.2).

## 7. Test impact

- **Rust unit (`operator_store.rs`):** remove `revoke_credential_blocks_verify`,
  `rotate_credential_swaps_active`; re-seed `issue_credential_then_verify_round_trip`
  and `remove_user_cascades_credentials_and_grants` via low-level `insert_credential`
  (to keep User-role `verify` coverage) or fold into a kept test. Keep
  `bootstrap_legacy_then_blocks_second_bootstrap` (proves the kept verify path).
- **Rust contract/e2e:** delete `tests/http_credentials_contract.rs` and
  `tests/credential_rotate_self_service.rs`; rewrite the per-user-token sections of
  `rbac_smoke.rs`, `identity_cli_smoke.rs`, and `traffic_quotas.rs` — these mint a
  per-user bearer via `credential-issue` purely to act *as* a non-superadmin. With
  no per-user-token source left, those as-user assertions must be deleted or
  re-derived from a surviving owner-scoped surface (decide per test in the plan).
  Update `http_auth_onboarding_contract.rs` / `operator_api_v07_compat.rs` to drop
  `credential_count` / `api_tokens_revoked` assertions.
- **webui e2e (Playwright):** `fixtures/helpers.ts::provisionUserWithToken` stops
  returning a usable `.token`; rework it (provision without a token) and drop/repoint
  the `Bearer`-dependent steps in `us2` / `us3` / `quickstart`. Delete the
  rotate-credential test and the `/users/bob/credentials` deny goto in `us2`; delete
  the issue-credential assertion in `us1`. The superadmin token (`fixtures/server.ts`)
  is **unaffected** — it comes from the kept `bootstrap-superadmin`.
- **webui unit:** update `user-detail-layout.test.ts` to drop the credentials section;
  keep the (unrelated) `client.test.ts` fetch-`credentials` and client-enrollment tests.
- **Non-test consumers of `credential-issue`:** update `scripts/demo.sh:333`.

## 8. Out of scope (explicitly unchanged)

- Data-plane **client** tokens (`client_tokens`, enrollment, credential bundles).
- `operator_token`, `bootstrap-superadmin`, `gen-token`, Web onboarding / `setup_token`.
- The bearer `verify` path and the `credentials` table schema.
- The adjacent RBAC audit findings (name-keyed `GET /v1/clients` scoping,
  `delete_grant` REMOVE-push, `get_rule_stats` fail-open) — tracked separately.

## 9. Risks & verification

- **Operator lock-out via wrong migration predicate** — mitigated: the predicate
  preserves the closed reserved namespace; verified both bootstrap credentials are
  owned by `_legacy` / `_superadmin` and that no user-id rename path exists.
- **Breaking the kept verify / `operator_token` path** — verified independent:
  `verify` issues its own `SELECT` over `credentials` and shares no code with the
  removed CRUD fns.
- **Dangling import after deleting `credentials.ts`** — `users.ts` must inline
  `credentialsKey` first (§4.4).
- **i18n parity test red** — delete credential keys from both locales together.
- **Wire/CLI changes** — removing routes, CLI subcommands, and the
  `keep_api_tokens`/`api_tokens_revoked`/`credential_count` fields is operator-visible:
  needs a `CHANGELOG.md` entry and a version bump (the credential surface and the
  contract-field removals are breaking for any external consumer).

## 10. Compatibility & rollout

- **Wire:** removed endpoints return 404; removed response fields disappear. Breaking
  for anyone scripting against `/v1/users/{id}/credentials` or reading
  `credential_count` / `api_tokens_revoked`.
- **Upgrade:** transparent for humans (cookie login) and machines (`operator_token`).
  Pre-existing user-issued tokens stop working at the V013 migration (intended).
- **CHANGELOG + version:** add a breaking-change entry; bump per semver.

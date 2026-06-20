# Remove User-Side Operator API Token — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the user-facing self-issued operator API-token surface (HTTP `/v1/users/{id}/credentials` issue/list/rotate/revoke, the `credential-*` CLI subcommands, and the Web UI Credentials section) while keeping the bearer `verify` path, the static `operator_token`, and `bootstrap-superadmin` fully working.

**Architecture:** Surgical removal across `portunus-server` (operator HTTP, SQLite store, CLI) and `webui` (React SPA), plus a one-shot SQLite migration (`V013`) that purges existing user-issued credential rows. Tasks are ordered so the workspace builds and `cargo test --workspace` / `pnpm build` stay green at every commit. Frontend changes land first (the UI just stops calling endpoints; extra wire fields are ignored), then the backend HTTP → store → CLI removals, then the vestigial "API token" plumbing, then the password-required hardening, then docs and changelog.

**Tech Stack:** Rust (edition 2024, axum, rusqlite/refinery), React + Vite + TypeScript, react-i18next, TanStack Query.

**Reference spec:** `docs/superpowers/specs/2026-06-20-remove-user-api-token-design.md`

**Branch:** `feat/remove-user-api-token` (already checked out; design committed).

**Build/test env note:** every `cargo` invocation against `portunus-server` needs `PORTUNUS_SKIP_WEBUI=1` (the `build.rs` errors without a built `webui/dist`). All commands below include it.

---

## File map

**Backend — remove**
- `crates/portunus-server/src/operator/credentials.rs` (delete file)
- `crates/portunus-server/tests/http_credentials_contract.rs` (delete file)
- `crates/portunus-server/tests/credential_rotate_self_service.rs` (delete file)

**Backend — modify**
- `crates/portunus-server/src/operator/mod.rs` (drop `pub mod credentials;`)
- `crates/portunus-server/src/operator/http.rs` (drop 3 routes + `credentials` import)
- `crates/portunus-server/src/operator/users.rs` (drop `credential_count`; require password)
- `crates/portunus-server/src/store/operator_store.rs` (remove `issue_/revoke_/rotate_/list_credentials`, `row_to_credential`, `count_active_credentials`; trim `reset_password_state` + `PasswordResetSummary`)
- `crates/portunus-server/src/operator/web_auth.rs` (trim `keep_api_tokens` / `api_tokens_revoked`)
- `crates/portunus-server/src/operator/password_cli.rs` (drop `keep_api_tokens` param)
- `crates/portunus-server/src/operator/identity_cli.rs` (remove `credential_*` fns)
- `crates/portunus-server/src/main.rs` (remove `Credential*` cmds; drop `--keep-api-tokens`)
- `crates/portunus-e2e/tests/rbac_smoke.rs`, `identity_cli_smoke.rs`, `traffic_quotas.rs` (rewrite)
- contract tests asserting `credential_count` / `api_tokens_revoked` (update)

**Backend — create**
- `crates/portunus-server/src/store/migrations/V013__drop_user_credentials.sql`

**Frontend — remove**
- `webui/src/api/credentials.ts` (delete file)

**Frontend — modify**
- `webui/src/api/users.ts`, `webui/src/api/types.ts`
- `webui/src/pages/UserDetail.tsx`, `webui/src/pages/UsersList.tsx`
- `webui/src/components/UserCreateForm.tsx`
- `webui/src/i18n/en.json`, `webui/src/i18n/zh-CN.json`
- `webui/tests/unit/user-detail-layout.test.ts`, webui Playwright specs + `fixtures/helpers.ts`

**Docs / meta**
- `docs/content/docs/server-client/**` + `docs/content/docs/zh/server-client/**`
- `CHANGELOG.md`, `scripts/demo.sh`

---

## Task 1: Add the V013 credential-purge migration

**Files:**
- Create: `crates/portunus-server/src/store/migrations/V013__drop_user_credentials.sql`
- Test: `crates/portunus-server/src/store/operator_store.rs` (add one `#[cfg(test)]` test)

- [ ] **Step 1: Confirm the next migration version**

Run: `ls crates/portunus-server/src/store/migrations/`
Expected: highest existing file is `V012__client_id_pk_flip.sql` (so `V013` is next).

- [ ] **Step 2: Write the migration file**

Create `crates/portunus-server/src/store/migrations/V013__drop_user_credentials.sql`:

```sql
-- v2.3 remove-user-api-token: delete all user self-issued operator
-- credentials. Preserve the reserved bootstrap credentials only:
--   `_legacy`     -> operator_token (server.toml) shortcut
--   `_superadmin` -> bootstrap-superadmin CLI
-- The reserved namespace is closed: UserId::reserved is the only
-- constructor of '_'-prefixed ids and FromStr rejects operator-supplied
-- ones, so this predicate can never delete a bootstrap credential and is
-- future-proof against any new reserved id. Idempotent (plain DELETE) and
-- crash-safe inside refinery's transactional version gate.
DELETE FROM credentials WHERE user_id NOT LIKE '\_%' ESCAPE '\';
```

- [ ] **Step 3: Write a test asserting the predicate keeps reserved creds and purges user creds**

Add to the `#[cfg(test)] mod tests` block in `crates/portunus-server/src/store/operator_store.rs` (use the same `tmp_store()`/open helper the surrounding tests use; match the existing test style). Seed via low-level paths so the test does not depend on `issue_credential` (removed in Task 4):

```rust
#[test]
fn v013_predicate_purges_user_creds_keeps_reserved() {
    use portunus_auth::token::hash_token;

    let store = open_test_store(); // existing helper used by other tests
    let op = SqliteOperatorStore::new(Arc::new(store));

    // Reserved operator_token superadmin (kept).
    op.bootstrap_legacy_superadmin("legacy-tok").unwrap();

    // A regular user with a self-issued credential (purged).
    let alice = User {
        id: UserId::from_str("alice").unwrap(),
        display_name: "Alice".into(),
        role: OperatorRole::User,
        created_at: Utc::now(),
        disabled: false,
    };
    op.add_user_with_password(alice.clone(), None, false).unwrap();
    let alice_hash = hash_token("alice-tok");
    op.store
        .with_write_tx(|tx| {
            tx.execute(
                "INSERT INTO credentials \
                 (credential_id, user_id, hash, label, status, issued_at, revoked_at, last_used_at) \
                 VALUES (?, 'alice', ?, NULL, 'active', ?, NULL, NULL)",
                rusqlite::params![
                    CredentialId::new().to_string(),
                    alice_hash,
                    Utc::now().to_rfc3339(),
                ],
            )?;
            Ok(())
        })
        .unwrap();

    // Run the V013 predicate (the migration body) directly.
    op.store
        .with_write_tx(|tx| {
            tx.execute(
                r"DELETE FROM credentials WHERE user_id NOT LIKE '\_%' ESCAPE '\'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    // operator_token still authenticates; alice's token is gone.
    assert!(matches!(
        op.verify("legacy-tok"),
        Ok(Some(identity)) if identity.role == OperatorRole::Superadmin
    ));
    assert!(matches!(op.verify("alice-tok"), Ok(None)));
}
```

If `open_test_store()` / the exact `verify` return shape differ, adapt to the conventions already present in the `tests` module (search for an existing `verify` or `bootstrap_legacy_superadmin` test and mirror its setup). The `op.store` field access works because the test is in the same module.

- [ ] **Step 4: Run the test**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib store::operator_store::tests::v013_predicate_purges_user_creds_keeps_reserved -- --nocapture`
Expected: PASS (migrations apply cleanly on open; predicate keeps `_legacy`, purges `alice`).

- [ ] **Step 5: Commit**

```bash
git add crates/portunus-server/src/store/migrations/V013__drop_user_credentials.sql crates/portunus-server/src/store/operator_store.rs
git commit -m "feat(server): add V013 migration purging user-issued credentials"
```

---

## Task 2: Remove the Web UI credentials surface

Frontend-only. After this task the UI no longer calls the credential endpoints; the backend still serves them (removed in Task 3) and still returns `credential_count` / accepts `keep_api_tokens` (removed in Tasks 4/6) — both harmless to an updated client.

**Files:**
- Delete: `webui/src/api/credentials.ts`
- Modify: `webui/src/api/users.ts`, `webui/src/api/types.ts`, `webui/src/pages/UserDetail.tsx`, `webui/src/pages/UsersList.tsx`, `webui/src/i18n/en.json`, `webui/src/i18n/zh-CN.json`
- Test: `webui/tests/unit/user-detail-layout.test.ts`

- [ ] **Step 1: Inline `credentialsKey` in `users.ts`, drop its credentials wiring**

In `webui/src/api/users.ts`:
- Delete line 4: `import { credentialsKey } from "@/api/credentials";`
- In `useResetUserPassword` `onSuccess` (lines 94-98), delete the credentials invalidation line:
  `void qc.invalidateQueries({ queryKey: credentialsKey(userId) });`
- Remove `keep_api_tokens?: boolean;` from `ResetUserPasswordBody` (line 76) and `api_tokens_revoked: number;` from `ResetUserPasswordResponse` (line 82).

- [ ] **Step 2: Delete the credentials API module**

```bash
git rm webui/src/api/credentials.ts
```

- [ ] **Step 3: Remove the credentials types and `credential_count`**

In `webui/src/api/types.ts`:
- Delete the `credential_count: number;` line from `UserView` (line 19).
- Delete the whole `// /v1/users/{id}/credentials` block: `CredentialView`, `IssueCredentialBody`, `IssueCredentialResponse` (lines 44-69).

- [ ] **Step 4: Strip the credentials section + rotate dialog + keep-api-tokens from `UserDetail.tsx`**

In `webui/src/pages/UserDetail.tsx`:
- Imports: remove `KeyRound` and `RotateCcw` from the `lucide-react` import (line 5; keep `Trash2`). Delete the entire `@/api/credentials` import block (lines 8-14).
- Remove hooks/state: `const credentials = useCredentialsList(userId);` (62), `const issue = useIssueCredential(userId);` and `const revokeCred = useRevokeCredential(userId);` (86-87), `const [rotateTarget, setRotateTarget] = useState<string | null>(null);` (96), `const [keepApiTokens, setKeepApiTokens] = useState(false);` (100), `const rotate = useRotateCredential(userId, rotateTarget ?? "");` (103).
- Delete the entire credentials `<section>` (lines 136-190).
- In the delete `ConfirmDialog` `dependents` (lines 232-235), remove the credentials spread so it reads:
  ```tsx
  dependents={[
    ...((accessEntries.data ?? []).map((e) => `quota ${e.client_name}`)),
  ]}
  ```
- Delete the rotate `ConfirmDialog` block (lines 260-276).
- In the reset-password `ConfirmDialog`: remove `setKeepApiTokens(false);` from `onOpenChange` (285); remove `keep_api_tokens: keepApiTokens,` from the `mutateAsync` body (300); delete the entire keep-api-tokens `<Field>` (lines 337-348).
- Keep `TokenRevealModal` (import line 32 and the render at 244-258), `issuedToken`/`revealKind` state, and `setRevealKind("password")` in the reset path — these serve the temporary-password reveal.

- [ ] **Step 5: Remove the credentials column from `UsersList.tsx`**

In `webui/src/pages/UsersList.tsx`, delete the `credentials` column object (lines 65-70):
```tsx
{
  key: "credentials",
  header: t("users.credentials"),
  render: (u) => u.credential_count,
  width: "120px",
},
```

- [ ] **Step 6: Remove credential i18n keys (both locales, symmetric)**

In **both** `webui/src/i18n/en.json` and `webui/src/i18n/zh-CN.json`:
- Under `users`: delete the `"credentials": ...` line (line 135).
- Under `userDetail`: delete `credentials` (155), `noCredentials` (157), `issueCredential` (159), `rotate` (160), `revoke` (161), `rotateTitle` (166), `rotateBody` (167), `rotateConfirm` (168), `keepApiTokens` (176).
- Under `tokenReveal`: the modal now only ever shows passwords. Set `title` and `description` to the password wording and reword `tokenLabel`. In `en.json`:
  ```json
  "tokenReveal": {
    "title": "Temporary password set",
    "description": "Copy this temporary password now — it will not be shown again.",
    "passwordTitle": "Temporary password set",
    "passwordDescription": "Copy this temporary password now — it will not be shown again. The user must change it on next login.",
    "copy": "Copy",
    "copied": "Copied",
    "dismiss": "Dismiss",
    "tokenLabel": "Temporary password (one-time)"
  },
  ```
  Apply the equivalent reword in `zh-CN.json` (translate `title`/`description`/`tokenLabel` to match `passwordTitle`/`passwordDescription`).
- Fix trailing commas after each deletion so both files remain valid JSON.
- Before deleting `rotate`/`revoke`, confirm they are credential-only:
  Run: `rg -n "userDetail\.(rotate|revoke)\b" webui/src` — expected: matches only in the deleted `UserDetail.tsx` credentials section.

- [ ] **Step 7: Update the layout unit test**

In `webui/tests/unit/user-detail-layout.test.ts`, the test "does not wrap the credentials, quota, and traffic sections in outer cards" (line 11) references the removed section. Update its name and assertions to cover only the quota + traffic sections (drop any `credentials`/credential-card selector). Mirror the existing assertions for the kept sections.

- [ ] **Step 8: Build + unit tests**

Run: `cd webui && pnpm build && pnpm test`
Expected: `tsc -b` passes (no dangling `@/api/credentials` import, no `credential_count`/`CredentialView` references), vite build under budget, and i18n-coverage + layout unit tests green.

- [ ] **Step 9: Commit**

```bash
git add webui/src webui/tests
git commit -m "feat(webui): remove user credential issue/list/rotate/revoke UI"
```

---

## Task 3: Remove the HTTP credential surface

Deletes the handlers, routes, module wiring, and the contract/e2e tests that drive them — all in one commit so `cargo test --workspace` stays green.

**Files:**
- Delete: `crates/portunus-server/src/operator/credentials.rs`, `crates/portunus-server/tests/http_credentials_contract.rs`, `crates/portunus-server/tests/credential_rotate_self_service.rs`
- Modify: `crates/portunus-server/src/operator/mod.rs`, `crates/portunus-server/src/operator/http.rs`, `crates/portunus-e2e/tests/rbac_smoke.rs`, `crates/portunus-e2e/tests/identity_cli_smoke.rs`, `crates/portunus-e2e/tests/traffic_quotas.rs`

- [ ] **Step 1: Delete the handler module + its contract tests**

```bash
git rm crates/portunus-server/src/operator/credentials.rs \
       crates/portunus-server/tests/http_credentials_contract.rs \
       crates/portunus-server/tests/credential_rotate_self_service.rs
```

- [ ] **Step 2: Remove the module declaration**

In `crates/portunus-server/src/operator/mod.rs`, delete line 14: `pub mod credentials;`

- [ ] **Step 3: Remove the routes and the `credentials` import**

In `crates/portunus-server/src/operator/http.rs`:
- In the `use crate::operator::{ ... }` block (lines 32-34), remove `credentials,` so it reads:
  ```rust
  use crate::operator::{
      audit_http, grants, stats_stream, users, users_me, web_auth,
  };
  ```
- Delete the three credential `.route(...)` calls (lines 81-92):
  ```rust
  .route(
      "/v1/users/{user_id}/credentials",
      get(credentials::get_credentials).post(credentials::post_credential),
  )
  .route(
      "/v1/users/{user_id}/credentials/{cred_id}",
      delete(credentials::delete_credential),
  )
  .route(
      "/v1/users/{user_id}/credentials/{cred_id}/rotate",
      post(credentials::post_credential_rotate),
  )
  ```

- [ ] **Step 4: Rewrite the rust e2e tests that mint per-user tokens**

These tests POST to the removed routes to obtain a non-superadmin bearer. Rework each:

- `crates/portunus-e2e/tests/identity_cli_smoke.rs` — `cli_user_credential_grant_roundtrip`: delete the `credential-issue` / `credential-list` / `credential-rotate` steps and the self-service rotation assertion. Keep the `user-add` / `grant-add` / `grant-list` / `grant-revoke` / `user-list` / `user-remove` coverage, authed with `TEST_OPERATOR_TOKEN`. Rename the test to drop "credential" (e.g. `cli_user_grant_roundtrip`).
- `crates/portunus-e2e/tests/rbac_smoke.rs` — `rbac_walkthrough_happy_and_violation_paths`: delete the `alice_token` minting (POST `/v1/users/alice/credentials`), the credentials-read tenant-isolation checks, and the self-service rotation section. Keep the user/grant/violation-code (403) coverage authed with the superadmin `operator_token`. Where a section's only purpose was "act as a non-superadmin via a self-issued token", delete it (log the dropped coverage in the commit body — do not silently shrink).
- `crates/portunus-e2e/tests/traffic_quotas.rs` — the monthly-quota test issues a credential for alice solely to push a rule as `owner=alice`. Repoint it to push the rule as the superadmin (`operator_token`) and key the quota on the superadmin owner id, preserving quota-enforcement coverage. Note in the commit body that this drops the "non-superadmin owner" specificity (no per-user token source remains).

- [ ] **Step 5: Build + workspace tests**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test --workspace`
Expected: compiles (no `credentials::` references, no unused `credentials` import), and all tests pass (deleted contract tests gone; rewritten e2e tests green). The store fns `issue_/revoke_/rotate_/list_credentials` are now unused by production but still compile (they are `pub`, so no `dead_code` warning).

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(server): remove user credential HTTP routes and handlers"
```

---

## Task 4: Remove the credential store functions + `credential_count`

**Files:**
- Modify: `crates/portunus-server/src/operator/users.rs`, `crates/portunus-server/src/store/operator_store.rs`, plus contract tests asserting `credential_count`

- [ ] **Step 1: Drop `credential_count` from the user view**

In `crates/portunus-server/src/operator/users.rs`:
- Remove `pub credential_count: usize,` from `UserView` (line 91).
- Change `UserView::from_user` to drop the `credential_count` param and field:
  ```rust
  impl UserView {
      fn from_user(u: &User, grant_count: usize) -> Self {
          Self {
              user_id: u.id.as_str().to_string(),
              display_name: u.display_name.clone(),
              role: match u.role {
                  OperatorRole::Superadmin => "superadmin".to_string(),
                  OperatorRole::User => "user".to_string(),
              },
              disabled: u.disabled,
              created_at: u.created_at,
              grant_count,
          }
      }
  }
  ```
- In `get_users` (lines 190-194), drop the `list_credentials` call:
  ```rust
  for u in users {
      let grant_count = state.operator_store.list_grants(Some(&u.id)).len();
      out.push(UserView::from_user(&u, grant_count));
  }
  ```
- In `get_user` (lines 209-211), drop the `list_credentials` call:
  ```rust
  let grant_count = state.operator_store.list_grants(Some(&id)).len();
  Ok(Json(UserView::from_user(&user, grant_count)))
  ```

- [ ] **Step 2: Remove the now-unused store functions**

In `crates/portunus-server/src/store/operator_store.rs`, delete these methods (now uncalled after Tasks 3-4):
- `issue_credential` (lines 969-1003)
- `revoke_credential` (lines 1005-1046)
- `rotate_credential` (lines 1048-1112)
- `list_credentials` (lines 137-156)
- `row_to_credential` (free fn near line 1340 — the helper used only by `list_credentials`)
- `count_active_credentials` (lines 121-135) — first confirm it is unused:
  Run: `rg -n "count_active_credentials" crates/` → expected: only the definition. If a caller exists, leave the fn and skip this deletion.

- [ ] **Step 3: Remove/rewrite the store unit tests for the deleted fns**

In the `operator_store.rs` `#[cfg(test)] mod tests`:
- Delete `revoke_credential_blocks_verify` and `rotate_credential_swaps_active`.
- `issue_credential_then_verify_round_trip` (User-role `verify` coverage) and `remove_user_cascades_credentials_and_grants`: re-seed the credential via a raw `INSERT INTO credentials (...)` inside `with_write_tx` (mirror the seeding in Task 1 Step 3, using `hash_token`) instead of the removed `issue_credential`, then keep the `verify` / cascade assertions. Keep `bootstrap_legacy_then_blocks_second_bootstrap` unchanged.

- [ ] **Step 4: Update contract tests asserting `credential_count`**

Run: `rg -n "credential_count" crates/portunus-server/tests/`
For each hit (e.g. `http_auth_onboarding_contract.rs`, `operator_api_v07_compat.rs`), remove the `credential_count` assertion and any `"credential_count": 0` expected-JSON field. Keep the surrounding user-view assertions.

- [ ] **Step 5: Build + workspace tests**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test --workspace && PORTUNUS_SKIP_WEBUI=1 cargo clippy -p portunus-server --all-targets -- -D warnings`
Expected: compiles with no unused-fn/`dead_code` warnings; all tests pass.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(server): drop credential store fns and credential_count"
```

---

## Task 5: Remove the `credential-*` CLI subcommands

**Files:**
- Modify: `crates/portunus-server/src/main.rs`, `crates/portunus-server/src/operator/identity_cli.rs`

- [ ] **Step 1: Remove the clap variants**

In `crates/portunus-server/src/main.rs`, delete the four `Cmd` variants `CredentialIssue` (239-248), `CredentialList` (249-255), `CredentialRevoke` (256-261), `CredentialRotate` (262-271), including the `/// Issue a fresh credential...` doc comment on line 239.

- [ ] **Step 2: Remove the dispatch arms**

In `main.rs`, delete the four match arms (lines 683-711): `Cmd::CredentialIssue { .. } => ...` through `Cmd::CredentialRotate { .. } => identity_cli::credential_rotate(...)`. Leave `Cmd::ResetPassword` (712) intact.

- [ ] **Step 3: Remove the CLI client functions**

In `crates/portunus-server/src/operator/identity_cli.rs`, delete the entire credentials section: the `// ---------------- credentials ----------------` header and `credential_issue`, `credential_list`, `credential_revoke`, `credential_rotate` (the contiguous block ending just before `// ---------------- grants ----------------`). Remove any now-unused imports the credential fns relied on (let the compiler flag them).

- [ ] **Step 4: Build + clippy**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-server && PORTUNUS_SKIP_WEBUI=1 cargo clippy -p portunus-server --all-targets -- -D warnings`
Expected: compiles; clap accepts the reduced command set; no unused-import warnings.

- [ ] **Step 5: Sanity-check the CLI surface**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo run -p portunus-server -- --help`
Expected: no `credential-issue` / `credential-list` / `credential-revoke` / `credential-rotate`; `gen-token`, `bootstrap-superadmin`, `reset-password`, `user-*`, `grant-*` still present.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(server): remove credential-* CLI subcommands"
```

---

## Task 6: Remove the vestigial API-token plumbing

`keep_api_tokens` / `api_tokens_revoked` / `revoke_api_tokens` are dead once user tokens are gone. Remove them; keep session revocation.

**Files:**
- Modify: `crates/portunus-server/src/store/operator_store.rs`, `crates/portunus-server/src/operator/web_auth.rs`, `crates/portunus-server/src/operator/password_cli.rs`, `crates/portunus-server/src/main.rs`, plus password contract tests + frontend already handled in Task 2

- [ ] **Step 1: Trim `PasswordResetSummary` and `reset_password_state`**

In `crates/portunus-server/src/store/operator_store.rs`:
- Remove `pub api_tokens_revoked: usize,` from `PasswordResetSummary` (line 58).
- Change `reset_password_state` to drop the `revoke_api_tokens` param and its UPDATE branch:
  ```rust
  pub(crate) fn reset_password_state(
      &self,
      user_id: &UserId,
      hash: &str,
      password_change_required: bool,
      revoke_sessions: bool,
  ) -> Result<PasswordResetSummary, IdentityStoreError> {
      let uid_for_err = user_id.clone();
      let now = Utc::now().to_rfc3339();
      self.store
          .with_write_tx(|tx| {
              if !user_exists(tx, user_id)? {
                  return Err(StoreError::Conflict {
                      detail: "user_not_found".into(),
                  });
              }
              tx.execute(
                  "UPDATE users SET password_hash = ?, password_change_required = ? \
                   WHERE user_id = ?",
                  params![hash, i32::from(password_change_required), user_id.as_str()],
              )
              .map_err(map_rusqlite)?;

              let sessions_revoked = if revoke_sessions {
                  tx.execute(
                      "UPDATE web_sessions \
                       SET revoked_at = COALESCE(revoked_at, ?) \
                       WHERE user_id = ? AND revoked_at IS NULL",
                      params![now, user_id.as_str()],
                  )
                  .map_err(map_rusqlite)?
              } else {
                  0
              };

              Ok(PasswordResetSummary { sessions_revoked })
          })
          .map_err(|e| match e {
              StoreError::Conflict { detail } if detail == "user_not_found" => {
                  IdentityStoreError::UserNotFound(uid_for_err)
              }
              other => IdentityStoreError::WriteFailed(other.to_string()),
          })
  }
  ```

- [ ] **Step 2: Trim the HTTP reset/change-password contract**

In `crates/portunus-server/src/operator/web_auth.rs`:
- Remove `pub keep_api_tokens: Option<bool>,` from `AdminPasswordResetRequest` (line 81, with its `#[serde(default)]`).
- Remove `pub api_tokens_revoked: usize,` from `PasswordResetResponse` (line 88).
- In `reset_user_password` (417-468), delete the `revoke_api_tokens` line (429), drop the 5th arg from the `reset_password_state` call (438), drop `api_tokens_revoked` from the response (445), and drop the `"api_tokens_revoked"` / `"api_tokens_kept"` audit detail lines (461, 464). Resulting call:
  ```rust
  let summary = state
      .operator_store
      .reset_password_state(target, &new_hash, password_change_required, true)
      .map_err(api_store)?;

  let response = PasswordResetResponse {
      user_id: target.as_str().to_string(),
      sessions_revoked: summary.sessions_revoked,
      temporary_password: generated.then_some(new_password),
  };
  ```
  And the audit `details` keep `sessions_revoked`, `temporary_password_generated`, `password_change_required` only.

- [ ] **Step 3: Trim the CLI reset-password path**

In `crates/portunus-server/src/operator/password_cli.rs`, change `reset_password` to drop the `keep_api_tokens` param:
- Remove the `keep_api_tokens: bool,` param (line 21).
- Remove `let revoke_api_tokens = !keep_api_tokens;` (39) and pass only 4 args to `reset_password_state` (drop line 46).
- Remove `"api_tokens_revoked"` (63) and `"api_tokens_kept"` (66) from audit details.
- Change the success print (74-79) to drop `api_tokens_revoked`:
  ```rust
  println!(
      "password_reset=ok user_id={} sessions_revoked={}",
      user_id.as_str(),
      summary.sessions_revoked
  );
  ```

In `crates/portunus-server/src/main.rs`:
- Remove the `--keep-api-tokens` arg from `Cmd::ResetPassword` (the `/// Keep active bearer API tokens...` doc + `#[arg(long)] keep_api_tokens: bool,`, lines 281-283).
- In the dispatch arm (712-723), drop `keep_api_tokens` from the destructure and the `password_cli::reset_password(...)` call:
  ```rust
  Cmd::ResetPassword {
      user_id,
      password_stdin,
      temporary,
  } => password_cli::reset_password(&data_dir, &user_id, password_stdin, temporary),
  ```

- [ ] **Step 4: Update password contract tests**

Run: `rg -n "api_tokens_revoked|keep_api_tokens|api_tokens_kept" crates/portunus-server/tests/ crates/portunus-e2e/tests/`
For each hit, remove the field from request bodies / expected responses / assertions (e.g. password reset contract tests). Keep `sessions_revoked` assertions.

- [ ] **Step 5: Build + workspace tests + clippy**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test --workspace && PORTUNUS_SKIP_WEBUI=1 cargo clippy --workspace --all-targets -- -D warnings`
Expected: compiles and passes; no references to the removed fields remain.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(server): drop vestigial keep_api_tokens / api_tokens_revoked plumbing"
```

---

## Task 7: Require a password at user creation

Closes the lockout: a user created without a password and without a (now-removed) issuable token would be unauthenticatable.

**Files:**
- Modify: `crates/portunus-server/src/operator/users.rs`, `webui/src/components/UserCreateForm.tsx`
- Test: `crates/portunus-server/tests/` (user-creation contract), `webui` (build)

- [ ] **Step 1: Write the failing backend test**

Add to the user-creation contract test (find it with `rg -n "post_users|/v1/users\"" crates/portunus-server/tests/`, e.g. an `http_*users*` contract file; if none exists, add a focused `#[tokio::test]` mirroring the existing contract-test harness). Assert that creating a user with no `initial_password` is rejected:

```rust
// POST /v1/users with no initial_password -> 422 initial_password_required
let resp = post_json(&app, "/v1/users", json!({
    "user_id": "nopass",
    "display_name": "No Pass",
    "role": "user",
})).await;
assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
```

- [ ] **Step 2: Run it to confirm it fails**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --test <file> <test_name>`
Expected: FAIL (currently `post_users` allows a missing password and returns 201).

- [ ] **Step 3: Require the password in `post_users`**

In `crates/portunus-server/src/operator/users.rs`, replace the `password_change_required`-only guard (lines 144-150) with an unconditional requirement, and make the change-required flag default true for the password:

```rust
let initial_password = match body.initial_password.as_deref() {
    Some(p) if !p.is_empty() => p,
    _ => {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "initial_password_required",
            "a user must be created with an initial password",
        ));
    }
};
let password_hash = hash_password(initial_password).map_err(password_error)?;
state
    .operator_store
    .add_user_with_password(
        user.clone(),
        Some(password_hash.as_str()),
        body.password_change_required,
    )
    .map_err(api_store)?;
```

(Removes the now-dead `body.initial_password.is_none() && body.password_change_required` branch and the `.transpose()` optional-hash path.)

- [ ] **Step 4: Run the test to confirm it passes**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --test <file> <test_name>`
Expected: PASS.

- [ ] **Step 5: Make the UI password field required**

In `webui/src/components/UserCreateForm.tsx`:
- Tighten the schema (line 43): `initial_password: z.string().min(8, t("userCreate.passwordRequired")),`
- Always send the password (replace the conditional spread at 65-70):
  ```tsx
  const res = await create.mutateAsync({
    user_id: values.user_id,
    display_name: values.display_name,
    initial_password: values.initial_password,
    password_change_required: values.force_password_change,
  });
  ```
- Remove the `disabled={!initialPassword || create.isPending}` gating on the force-password-change checkbox (line 126) so it is always enabled: `disabled={create.isPending}`. The `initialPassword` watch (line 56) can stay or be removed if now unused (let `tsc` flag it).
- Add the `userCreate.passwordRequired` key to `en.json` and `zh-CN.json` (e.g. EN: `"passwordRequired": "Password must be at least 8 characters."`).

- [ ] **Step 6: Update any caller that creates a password-less user**

Run: `rg -n "POST.*\"/v1/users\"|/v1/users\b" crates/portunus-e2e/tests webui/tests scripts` and the rust e2e/webui fixtures. For each user-creation that omits a password, add an `initial_password`. (E.g. webui `fixtures/helpers.ts::provisionUserWithToken` and the rust e2e user-add calls — these were already touched in Tasks 3/8; ensure they pass a password.)

- [ ] **Step 7: Build + tests**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test --workspace && cd webui && pnpm build && pnpm test`
Expected: all green.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "feat(server): require an initial password when creating a user"
```

---

## Task 8: Rewrite webui Playwright e2e for the removed surface

The webui superadmin token is unaffected (it comes from the kept `bootstrap-superadmin`), but per-user tokens and the credentials UI are gone.

**Files:**
- Modify: `webui/tests/e2e/fixtures/helpers.ts`, `webui/tests/e2e/us1-superadmin.spec.ts`, `us2-tenant-isolation.spec.ts`, `us3-audit-and-metrics.spec.ts`, `quickstart-walkthrough.spec.ts`

- [ ] **Step 1: Rework `provisionUserWithToken`**

In `webui/tests/e2e/fixtures/helpers.ts`, `provisionUserWithToken` POSTs `/v1/users/{id}/credentials` to return a `.token`. Change it to provision the user **with a password** (no credential) and stop returning a token — rename to `provisionUser` and drop the `token`/`credentialId` from its return. Update every import/caller.

- [ ] **Step 2: Update the specs**

- `us1-superadmin.spec.ts`: delete the "Issue credential" button + one-time-token-reveal assertions; keep login → dashboard → create-user.
- `us2-tenant-isolation.spec.ts`: delete the "rotate credential" test entirely; in the first test, drop or repoint the `goto("/users/bob/credentials")` deny check to `/users/bob`.
- `us3-audit-and-metrics.spec.ts` / `quickstart-walkthrough.spec.ts`: replace `Bearer ${...token}` API calls that relied on a per-user token. If a step only needs the user to exist, use `provisionUser`; if it needs an authenticated API call, use the superadmin token from the fixture.

- [ ] **Step 3: Run the e2e suite**

Run the webui Playwright suite per `webui/README.md` (e.g. `cd webui && pnpm test:e2e`).
Expected: green; no spec drives a removed route or the removed UI.

- [ ] **Step 4: Commit**

```bash
git add webui/tests/e2e
git commit -m "test(webui): update e2e for removed credential surface"
```

---

## Task 9: Documentation (EN + ZH, in lockstep)

**Files:** `docs/content/docs/server-client/**` and `docs/content/docs/zh/server-client/**` (see the design's §4.5 inventory).

- [ ] **Step 1: Remove the credential reference sections**

Delete, in EN and the ZH mirror:
- `api/operator-http.mdx`: the `### Credentials` endpoint table and the `### POST /v1/users/{id}/credentials` request-body block.
- `cli/portunus-server.mdx`: the `## Credentials` CLI table (rows `credential-issue`/`list`/`revoke`/`rotate`).
- `features/rbac.mdx`: the `## API token rotation` section, the `credential-issue` tenant-setup step, and the `credential-*` CLI-surface row.
- `features/web-ui.mdx`: the **Credentials** "What you get" row and "issue credentials" under Users.

- [ ] **Step 2: Rewrite the token-sourcing guidance**

Repoint machine-token instructions from "create a token on the Web UI Credentials page" to the static `operator_token` (`gen-token` → `server.toml`):
- `deployment/docker.mdx`, `deployment/systemd.mdx`, `operations/troubleshooting.mdx`, `cli/portunus-server.mdx` intro (`PORTUNUS_OPERATOR_TOKEN` sourcing), `features/rbac.mdx` Credential concept bullet, the password-reset "revokes API tokens" wording, `api/operator-http.mdx` auth intro.
- Add one canonical line in `web-ui.mdx` / `rbac.mdx`: "machines authenticate with the static `operator_token`; humans use Web cookie login."

- [ ] **Step 3: Leave the kept docs untouched**

Confirm no edits to: `configuration/server.mdx` `operator_token`/`gen-token` block (except it stays as the canonical machine-token doc), `bootstrap-superadmin` references (D3: kept), and all data-plane client-enrollment / credential-bundle docs.

Run: `rg -n "credential-issue|/v1/users/.*/credentials|API token rotation" docs/content`
Expected: no remaining hits in user-facing pages (historical `specs/`/`plans/` `.md` are out of scope).

- [ ] **Step 4: Commit**

```bash
git add docs/content
git commit -m "docs: remove user API-token guidance, point machines at operator_token"
```

---

## Task 10: Changelog, version bump, demo script

**Files:** `CHANGELOG.md`, `Cargo.toml` workspace version, `scripts/demo.sh`

- [ ] **Step 1: Update `scripts/demo.sh`**

Remove the `credential-issue` invocation (line ~333). If the demo needs a per-user authenticated call, use the superadmin `operator_token`; otherwise just provision the user.

Run: `rg -n "credential-issue|/credentials" scripts/demo.sh`
Expected: no hits.

- [ ] **Step 2: Add the CHANGELOG entry**

In `CHANGELOG.md`, add a new version section documenting the breaking changes: removed `POST/GET/DELETE /v1/users/{id}/credentials` and `.../rotate`; removed `credential-issue`/`credential-list`/`credential-revoke`/`credential-rotate` CLI; removed `keep_api_tokens` (request) / `api_tokens_revoked` (response) / `credential_count` (user view) fields; user creation now requires an initial password; V013 migration purges existing user-issued credentials. Note the kept paths (`operator_token`, `bootstrap-superadmin`, Web cookie login).

- [ ] **Step 3: Bump the version**

Bump the workspace `version` in the root `Cargo.toml` per semver (these are operator-visible breaking changes → new major or the project's next planned major/minor). Match the CHANGELOG heading.

Run: `PORTUNUS_SKIP_WEBUI=1 cargo build --workspace`
Expected: compiles with the new version.

- [ ] **Step 4: Commit**

```bash
git add CHANGELOG.md Cargo.toml Cargo.lock scripts/demo.sh
git commit -m "chore(release): note user API-token removal and bump version"
```

---

## Final verification

- [ ] **Full workspace gate**

Run: `PORTUNUS_SKIP_WEBUI=1 cargo test --workspace && PORTUNUS_SKIP_WEBUI=1 cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`
Expected: all green.

- [ ] **Frontend gate**

Run: `cd webui && pnpm build && pnpm test`
Expected: green, bundle within budget.

- [ ] **Surface sweep**

Run: `rg -n "issue_credential|rotate_credential|revoke_credential|credential_count|keep_api_tokens|api_tokens_revoked|credential-issue|useIssueCredential|CredentialView" crates webui --glob '!**/migrations/**'`
Expected: no production references remain (only possibly historical `docs/superpowers/**` design/plan files).

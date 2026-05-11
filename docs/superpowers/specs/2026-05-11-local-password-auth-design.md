# Local Password Auth and Onboarding Design

## Status

Draft approved for planning. This document captures the agreed direction for
replacing Web UI bearer-token login with local username/password login.

## Context

Portunus currently authenticates both CLI/API callers and Web UI users with the
same operator bearer credential. The Web UI stores that bearer in
`sessionStorage` and sends it on every `/v1/*` request. That model is acceptable
for CLI automation tokens, but it is not a good primary browser-login model:
the browser sees a long-lived bearer secret, logout is client-side, and there is
no first-class password or recovery workflow.

The existing user/grant/RBAC model remains valid. This design changes how
humans log in to the Web UI; it does not remove API tokens.

External guidance used:

- OWASP Session Management Cheat Sheet: browser auth should use server-managed
  session identifiers in cookies with `HttpOnly`, `Secure`, and `SameSite`.
- OWASP Password Storage Cheat Sheet: passwords should be stored with a
  password hashing scheme that encodes algorithm and work factor, suitable for
  future upgrades.
- OWASP Forgot Password Cheat Sheet: password reset identifiers must be random,
  single-use, expire, and reset flows should invalidate sessions without
  leaking account existence.

## Goals

- Web UI users log in with local username and password.
- The first `superadmin` is created through a one-time onboarding flow.
- Normal users are created only by an admin because grants must be assigned
  deliberately.
- Existing bearer credentials remain available as API/CLI tokens.
- Password-reset and break-glass flows are explicit and documented.
- No public, reusable recovery backdoor exists after initialization.

## Non-Goals

- No public self-registration.
- No email-based self-service password reset in the initial local-password
  implementation.
- No OIDC/SAML/LDAP in this feature. Those can be added later without changing
  the local user/grant model.
- No recovery of old passwords. Passwords are reset, never revealed.

## Authentication Model

### Web UI Login

The login page collects `username` and `password`. On success, the server
creates a Web session and sets a cookie:

- `HttpOnly`
- `Secure` when TLS is enabled
- `SameSite=Lax` by default
- path scoped to the operator UI/API surface
- idle timeout and absolute timeout

The Web UI no longer stores operator bearer tokens in `sessionStorage` for
normal login.

### API and CLI Tokens

Bearer credentials remain as explicit API tokens. They keep the existing
one-time display, hash-at-rest, rotate, revoke, and `last_used_at` behavior.

The UI should label them as "API tokens" rather than "login tokens" to avoid
teaching users the wrong mental model.

## Onboarding

When the system has no active `superadmin`, the Web UI shows an onboarding
screen instead of the normal login page.

The onboarding flow creates exactly one first admin:

- username
- display name
- password
- password confirmation

The server accepts onboarding only while no active `superadmin` exists. After
the first admin is created, the onboarding endpoint returns a closed/forbidden
result forever unless the database is explicitly reset by an operator.

Onboarding must not create API tokens automatically. If the admin needs CLI
access, they can issue an API token from the UI after login.

## User Creation

Only a `superadmin` can create users.

The create-user flow should support:

- username
- display name
- role
- initial password or one-time temporary password
- force password change on first login
- disabled flag

Grants remain a separate admin action. This avoids implying that creating a
user automatically grants any forwarding capability.

## Password Reset

### Normal User Forgot Password

Normal users do not self-reset through a public "forgot password" form in the
initial implementation.

Instead, a `superadmin` resets the password from the Web UI:

1. Open the target user.
2. Choose "Reset password".
3. Enter a new password or generate a one-time temporary password.
4. Mark the account as `password_change_required`.
5. Invalidate all active Web sessions for that user.
6. Leave API tokens active by default, with an explicit checkbox to revoke them.

The reset event is written to audit as `operator.password_reset` with actor,
target user, outcome, and whether sessions/tokens were revoked. The password is
never logged.

### Last Superadmin Forgot Password

There must be no public Web endpoint that resets the final `superadmin`.
Recovery requires server shell access and data-directory permissions:

```bash
portunus-server --data-dir /var/lib/portunus reset-password _superadmin
```

The command should:

1. Open the SQLite store directly.
2. Require the target user to exist.
3. Refuse to create a new superadmin.
4. Prompt for a new password twice, or generate a one-time temporary password.
5. Store the new password hash.
6. Set `password_change_required` when using a temporary password.
7. Invalidate all Web sessions for that user.
8. Write an audit event without recording the password.

This is the break-glass path. If an operator has neither a working superadmin
password nor server/data-dir access, Portunus should not provide a remote bypass.
The remaining options are restoring from backup or rebuilding the identity
store intentionally.

## Documentation Requirements

When the feature is implemented, user-facing docs must be updated in both
English and Chinese.

Required pages:

- Web UI feature page: describe username/password login and API-token
  separation.
- Deployment docs: describe first-run onboarding.
- RBAC docs: describe admin-created users, password state, and API tokens.
- Troubleshooting docs: include password-reset and last-superadmin recovery.
- CLI docs: document `reset-password <user_id>`.

Required procedures:

- First-run onboarding creates the first admin.
- Admin creates a normal user and assigns grants.
- Admin resets a normal user's password.
- Last superadmin password recovery through local CLI.
- API token issuance/rotation remains separate from Web login.

The troubleshooting page must explicitly state that onboarding does not reopen
after initialization and that there is no remote reset path for the final admin.

## Storage Changes

Add password/session state without changing grant semantics:

- `users.username` or reuse existing `user_id` as login name.
- `users.password_hash` in PHC-style format.
- `users.password_change_required`.
- `web_sessions` table with hashed session IDs, user ID, created time, last
  seen time, expiry, and revoked time.
- optional login throttling table keyed by user and remote address.

Password hashing should use a maintained Rust password-hashing crate. The
preferred algorithm is Argon2id unless implementation research finds a better
fit for this repository.

## API Shape

Unauthenticated routes:

- `GET /v1/auth/status`: returns whether onboarding is required.
- `POST /v1/auth/onboarding`: create first admin only when no active
  `superadmin` exists.
- `POST /v1/auth/login`: username/password login, creates session cookie.
- `POST /v1/auth/logout`: revokes current session cookie.

Authenticated routes:

- `GET /v1/users/me`: works with Web session cookie or API bearer token.
- `POST /v1/users/{user_id}/password`: superadmin resets a user's password.
- `POST /v1/users/me/password`: user changes their own password after
  reauthenticating.

The existing bearer-token middleware should become one branch of a single auth
seam: first authenticate Web session cookie, then bearer API token, then reject.

## Security Notes

- Login failures use generic errors to avoid user enumeration.
- Login and password reset attempts are rate-limited.
- Password reset invalidates Web sessions.
- Admin password reset does not automatically log in the target user.
- The final-superadmin recovery path is local CLI only.
- API tokens are never stored in browser storage for normal Web login.

## Testing Requirements

- Fresh store shows onboarding required.
- Onboarding creates exactly one `superadmin`.
- Onboarding is rejected after any active `superadmin` exists.
- Login creates a session cookie and `GET /v1/users/me` succeeds.
- Logout revokes the session.
- Password reset invalidates existing sessions.
- Non-superadmin cannot create users or reset other users' passwords.
- CLI `reset-password` updates an existing user and refuses missing users.
- Bearer API tokens continue to work for CLI/API callers.

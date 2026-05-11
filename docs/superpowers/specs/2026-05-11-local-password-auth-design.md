# Local Password Auth and Onboarding Design

## Status

Draft approved for planning, revised after security review. This document
captures the agreed direction for replacing Web UI bearer-token login with
local username/password login. Target feature version: v1.1.0.

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
- OWASP Cross-Site Request Forgery Prevention Cheat Sheet: cookie-authenticated
  state-changing requests need explicit CSRF defenses.

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

- cookie name: `portunus_session`
- `HttpOnly`
- `Secure` when TLS is enabled
- `SameSite=Lax` by default
- path scoped to the operator UI/API surface
- idle timeout: 8 hours
- absolute timeout: 7 days

Session identifiers must be generated with a cryptographically secure random
number generator and carry at least 128 bits of entropy. Only a hash of the
session identifier is stored in SQLite.

Successful login always creates a fresh session identifier and revokes any
pre-login anonymous/session placeholder. This prevents session fixation.

Sessions are not bound to IP address or user agent because operator networks
can change under VPN, mobile tethering, and reverse proxies. The server may
record IP and user agent metadata for audit and troubleshooting, but metadata
changes do not invalidate a session.

Expired and revoked sessions are ignored immediately. Cleanup can be lazy on
session lookup plus periodic at server startup; correctness must not depend on
the cleanup job having run.

The Web UI no longer stores operator bearer tokens in `sessionStorage` for
normal login.

### API and CLI Tokens

Bearer credentials remain as explicit API tokens. They keep the existing
one-time display, hash-at-rest, rotate, revoke, and `last_used_at` behavior.

The UI should label them as "API tokens" rather than "login tokens" to avoid
teaching users the wrong mental model.

### CSRF Protection

Cookie-authenticated state-changing requests require CSRF protection. The
server accepts `GET` and `HEAD` without a CSRF header but rejects
cookie-authenticated `POST`, `PUT`, `PATCH`, and `DELETE` unless all of these
conditions hold:

1. `Origin` is present and exactly matches the configured operator origin.
2. `Content-Type` is `application/json` for requests with a body.
3. A non-secret custom header such as `X-Portunus-CSRF: 1` is present.

The custom header prevents plain HTML forms from triggering writes. The origin
check prevents cross-origin JavaScript from using a valid cookie. Bearer API
token requests do not require CSRF protection because browsers do not attach
the `Authorization` header automatically.

`POST /v1/auth/logout` is also a cookie-authenticated state-changing request.
It requires the same CSRF checks even though it is listed with the auth routes.
Silent cross-site logout is lower impact than account takeover, but it is still
an avoidable footgun and costs almost nothing to block.

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

An active superadmin is defined as a row with `role = 'superadmin'` and
`disabled = 0`. Soft-deleted users, if added later, must not count as active.

The onboarding write is atomic. The implementation must perform the
"no active superadmin" check and the first-admin insert inside a single SQLite
transaction, using `BEGIN IMMEDIATE` or an equivalent single-writer path so two
concurrent onboarding requests cannot both succeed.

Onboarding is unauthenticated by necessity, so it must also require a setup
token. On first start with no active superadmin, the server exposes onboarding
only when the request includes a one-time setup token generated by the CLI or
printed to the local server console. The setup token is not persisted after the
first admin is created.

If the server restarts before onboarding completes, it generates a new setup
token for that process. Any token from a previous process becomes invalid. The
token also expires after 30 minutes; operators can restart or rerun the setup
token command to get a fresh one.

Deployment docs must instruct operators to keep the operator UI bound to
localhost or firewall-restricted until onboarding is complete. The setup token
is defense in depth; it is not an excuse to expose an uninitialized control
plane to the public internet. That way lies goblins.

## User Creation

Only a `superadmin` can create users.

The login username is the existing `user_id`. Do not add a separate
`users.username` column in the initial implementation. Reusing `user_id` keeps
URL paths, audit records, grants, and login identity aligned.

The create-user flow should support:

- user ID
- display name
- role
- initial password or one-time temporary password
- force password change on first login
- disabled flag

Grants remain a separate admin action. This avoids implying that creating a
user automatically grants any forwarding capability.

Passwords must allow long passphrases and all Unicode scalar values accepted by
the chosen password hashing library. The initial policy is:

- minimum length: 12 characters
- maximum length: 1024 bytes after UTF-8 encoding
- no composition rules
- reject passwords found in an optional future compromised-password checker only
  if that checker is explicitly configured

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
6. Revoke that user's API tokens by default.
7. Allow an explicit "keep API tokens" checkbox only when the admin knows the
   reset is not related to suspected compromise.

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
8. Revoke that user's API tokens by default unless `--keep-api-tokens` is
   passed.
9. Write an audit event without recording the password.

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
- First-run onboarding requires the setup token.
- Admin creates a normal user and assigns grants.
- Admin resets a normal user's password.
- Last superadmin password recovery through local CLI.
- API token issuance/rotation remains separate from Web login.

The troubleshooting page must explicitly state that onboarding does not reopen
after initialization and that there is no remote reset path for the final admin.

## Storage Changes

Add password/session state without changing grant semantics:

- `users.password_hash` in PHC-style format.
- `users.password_change_required`.
- `web_sessions` table with hashed session IDs, user ID, created time, last
  seen time, expiry, and revoked time.
- `login_attempts` or equivalent throttling state keyed by user and remote
  address.

Password hashing should use a maintained Rust password-hashing crate. The
preferred algorithm is Argon2id unless implementation research finds a better
fit for this repository.

## API Shape

Unauthenticated routes:

- `GET /v1/auth/status`: returns whether onboarding is required.
- `POST /v1/auth/onboarding`: create first admin only when no active
  `superadmin` exists and the setup token is valid.
- `POST /v1/auth/login`: username/password login, creates session cookie.
- `POST /v1/auth/logout`: revokes current session cookie.

Although logout appears with unauthenticated auth routes, it mutates the current
cookie session and therefore requires a valid session cookie and CSRF checks.

Authenticated routes:

- `GET /v1/users/me`: works with Web session cookie or API bearer token.
- `POST /v1/users/{user_id}/password`: superadmin resets a user's password.
- `POST /v1/users/me/password`: user changes their own password after
  reauthenticating.

The existing bearer-token middleware should become one branch of a single auth
seam: first authenticate Web session cookie, then bearer API token, then reject.

`POST /v1/users/me/password` reauthenticates by requiring both
`current_password` and `new_password` in the JSON body. A separate short-lived
reauth token is not part of the initial implementation.

`POST /v1/auth/login` does not accept bearer tokens as a substitute for a
password. If a request already carries a valid bearer token, the bearer is
ignored for login and the username/password body still decides the outcome.

## Migration and Compatibility

Existing CLI and automation callers continue using bearer API tokens through
the `Authorization: Bearer <token>` path. No compatibility window is required
for CLI scripts because bearer authentication remains supported.

The Web UI stops supporting pasted bearer-token login. On first load after the
upgrade, the UI clears the old `portunus.token` `sessionStorage` key and shows
the username/password login or onboarding screen.

Existing bearer credentials are preserved as API tokens. They are not silently
converted into passwords, and no password is generated for existing users
unless an admin explicitly sets or resets one.

## Security Notes

- Login failures use generic errors to avoid user enumeration.
- Login, onboarding, and password reset attempts are rate-limited. Rate limits
  are mandatory, not optional.
- Login throttling uses exponential delay or bounded temporary lockout. It must
  avoid permanent user-controlled lockout that lets attackers deny service to a
  known account.
- Cookie-authenticated writes require CSRF protection.
- Password reset invalidates Web sessions.
- Admin password reset does not automatically log in the target user.
- The final-superadmin recovery path is local CLI only.
- API tokens are never stored in browser storage for normal Web login.
- Audit event names, error codes, and reason strings remain stable English
  machine identifiers. User-facing Web UI text, toasts, forms, and docs must
  use the existing i18n bundles for English and Chinese.

## Testing Requirements

- Fresh store shows onboarding required.
- Onboarding creates exactly one `superadmin`.
- Onboarding is rejected after any active `superadmin` exists.
- Two concurrent onboarding requests cannot both create a `superadmin`.
- Onboarding rejects missing or invalid setup tokens.
- Setup tokens from a previous server process are rejected after restart.
- Expired setup tokens are rejected.
- Login creates a session cookie and `GET /v1/users/me` succeeds.
- Login rotates the session identifier.
- Session cookies carry the required `HttpOnly`, `Secure` when applicable,
  `SameSite`, path, and expiry attributes.
- Logout revokes the session.
- Logout without CSRF protection is rejected for cookie-authenticated sessions.
- Expired and revoked sessions are rejected even before cleanup runs.
- Password reset invalidates existing sessions.
- Password reset revokes API tokens by default unless explicitly preserved.
- Non-superadmin cannot create users or reset other users' passwords.
- CLI `reset-password` updates an existing user and refuses missing users.
- Bearer API tokens continue to work for CLI/API callers.
- Cookie-authenticated writes without CSRF headers or with a mismatched origin
  are rejected.
- Bearer-authenticated writes do not require CSRF headers.
- Login, onboarding, and password reset rate limits trigger under repeated
  failures.

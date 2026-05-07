# Contract: SPA Routes & Role Gates

URL routes the SPA registers in React Router. Each entry includes:
the URL, the page component, the role gate, and the URL params that
drive page state (filters, ids).

The "Role gate" column maps to the `permissions.ts` predicates:
- **public** — reachable without a valid session (only the login screen).
- **any** — any authenticated user (`role` ∈ {superadmin, user}).
- **superadmin** — superadmin only; non-superadmins navigated here see
  a `<PermissionDenied />` placeholder, no data is fetched.

| Path | Page | Role gate | URL params | Notes |
|---|---|---|---|---|
| `/login` | `LoginPage` | public | none | Redirects to `/` if a valid token is already cached. |
| `/` | `Dashboard` | any | none | Connected clients count + active rules count + role badge. |
| `/users` | `UsersList` | superadmin | `?q=<filter>` | Virtualised list. |
| `/users/new` | `UserCreate` | superadmin | none | Create form. |
| `/users/:user_id` | `UserDetail` | any | none | Tenant sees only their own. Other ids → `<PermissionDenied />`. |
| `/users/:user_id/credentials` | `CredentialsList` | any (own) / superadmin (any) | none | Owner-or-super gate per v0.5. |
| `/users/:user_id/credentials/new` | `CredentialIssue` | any (own) / superadmin | none | Token shown once in modal. |
| `/grants` | `GrantsList` | any | `?user_id=<id>` | superadmin sees all + filter; tenant only sees own. |
| `/grants/new` | `GrantCreate` | superadmin | none | Form: client + port range + protocol. |
| `/rules` | `RulesList` | any | `?client=<name>&owner=<id>` | Tenant: server filters server-side. Superadmin: optional `owner` filter. |
| `/rules/new` | `RulePush` | any | none | Form scoped to caller's grants. |
| `/rules/:rule_id` | `RuleDetail` | owner / superadmin | none | Live stats panel (SSE). |
| `/clients` | `ClientsList` | any | none | Connected indicator. |
| `/clients/new` | `ClientProvision` | superadmin | none | provision-client form. |
| `/audit` | `AuditLog` | superadmin | `?outcome=allow\|deny` | Last 100 entries; client filter; NDJSON export. |
| `/metrics` | `Metrics` | any | none | Raw `/metrics` text + a few key gauges parsed out. |
| `/settings` | `Settings` | any | none | Theme + language toggle. |
| `*` (catch-all) | `NotFound` | public | none | 404-style fallback. |

## URL state conventions

- Active filters live in the query string (`?q=`, `?owner=`, `?outcome=`)
  so browser back/forward "just works" (FR-023).
- Pagination is **not** in the URL because lists are fully virtualised
  client-side (R-005 / Q1 of clarify): there are no pages to bookmark.
- The active `:user_id` / `:rule_id` is in the URL path; deep links
  to a specific resource work.

## Auth-redirect contract

- A user with no cached token who visits any non-`/login` URL is
  redirected to `/login?next=<original-path>`. After successful login,
  the SPA navigates to the `next` value (sanitised: must be a relative
  path, no scheme/host, starts with `/`).
- A user whose cached token returns 401 from any API call (including
  the initial `/v1/users/me` probe) is redirected to `/login` with a
  `?reason=session_expired` query and a non-blocking banner.

## Permission-denied contract

- A superadmin-only route accessed with role `user` MUST render
  `<PermissionDenied />` immediately, BEFORE firing any API request.
  This is enforced by `<AuthGate role="superadmin">` reading the
  cached `Identity` from TanStack Query.
- The placeholder includes:
  - One-line explanation ("This page is only available to superadmins").
  - A "Back to dashboard" button.
  - NO mention of the resource id or content (avoid leaking existence
    information).

## Test plan

End-to-end (Playwright) covers, for every entry above, the four
canonical states:

1. unauthenticated visit → `/login` redirect.
2. authenticated as superadmin → page renders with data.
3. authenticated as `user` (alice) on a superadmin-only path → 
   `<PermissionDenied />`, no API call.
4. authenticated as `user` on an owner-gated path → renders own
   resources, denied on others.

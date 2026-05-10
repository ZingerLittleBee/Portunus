# Quickstart: 006-management-web-ui

End-to-end walkthrough demonstrating every spec acceptance scenario
through a freshly-built `portunus-server` binary plus the embedded UI.
Mirrors the structure of `specs/005-multi-user-rbac/quickstart.md`.

## 0. Prerequisites

- Rust toolchain matching workspace MSRV (1.88).
- Node 20 LTS + `pnpm` 9 for building the SPA.
- A modern browser (Chrome / Firefox / Safari / Edge — latest two
  releases).
- ≈ 5 minutes of wall-clock budget (SC-001).

## 1. Build the SPA and the server

```sh
# In webui/, install + build the SPA. This produces webui/dist/.
cd webui
pnpm install --frozen-lockfile
pnpm build           # vite build && size-limit (fails if > 500 KB gz)

# Back to the repo root, build the server binary with embedded assets.
cd ..
cargo build --release -p portunus-server
```

Verify the binary contains the assets:

```sh
strings target/release/portunus-server | grep -m1 'index.html'
# Expected: a path/marker hint that rust-embed sees index.html
```

Skip the SPA build during backend-only iteration:

```sh
PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-server
# Compiles a UI-less binary; release pipelines never set this env var.
```

## 2. Start the server with a known operator token

Same `operator_token` shortcut introduced in v0.5:

```sh
mkdir -p /tmp/forward-006
cat > /tmp/forward-006/server.toml <<'TOML'
control_listen        = "127.0.0.1:7443"
operator_http_listen  = "127.0.0.1:7080"
metrics_listen        = "127.0.0.1:7081"
tls_cert_path         = "/tmp/forward-006/server.crt"
tls_key_path          = "/tmp/forward-006/server.key"
token_store_path      = "/tmp/forward-006/tokens.json"
operator_store_path   = "/tmp/forward-006/identity.json"
operator_token        = "T006-quickstart-token"
TOML

target/release/portunus-server --config-dir /tmp/forward-006 serve &
SERVER_PID=$!
sleep 0.5
```

Open http://127.0.0.1:7080/ in your browser. You should see the
**Login** page.

## 3. Login (US1 acceptance #1)

Paste `T006-quickstart-token` into the bearer field and click "Sign in".

Expected:
- Redirect to `/` (Dashboard).
- The header shows your role badge: **Superadmin**.
- The dashboard cards show **0 connected clients** and **0 active rules**.

Open the browser dev-tools and verify:
- `sessionStorage.portunus.token` is set (string, ≥ 32 chars).
- `localStorage.portunus.token` is **not** set.
- The token does NOT appear in the URL or in `document.body.innerText`.
- Network tab: every `/v1/*` request carries
  `Authorization: Bearer T006-...`.

(SC-006 token-leak audit checklist — full procedure under § 9.)

## 4. Add a tenant + credential + grant (US1 acceptance #2-3)

In the UI:

1. Navigate to **Users**, click "**+ New user**".
   - id `alice`, display_name `Alice — payments`. Submit.
2. On Alice's detail page, click "**Issue credential**", confirm.
   - A modal pops with the new bearer token. Copy it
     (e.g. `ALICE_TOKEN_NEW`).
   - Close the modal; the token is no longer visible anywhere.
3. Navigate to **Grants**, click "**+ New grant**".
   - User: alice; Client: `client-a`; Listen ports: `30000..30010`;
     Protocols: ☑️ TCP. Submit.

The Users list (after grant + credential) should show alice with
"1 credential, 1 grant".

Time-to-here from § 2: should be **< 60 s** of wall-clock (mirrors
SC-001 from spec 005).

## 5. Constrained tenant pushes a rule within their grant (US1 acceptance #4-5)

1. Spin up a forwarding client (existing v0.5 path, in another shell):

   ```sh
   target/release/portunus-server provision-client client-a \
       --out /tmp/forward-006/client-a.bundle.json
   target/release/portunus-client \
       --bundle /tmp/forward-006/client-a.bundle.json &
   CLIENT_PID=$!
   ```

2. Open a second browser tab (fresh `sessionStorage`), go to
   http://127.0.0.1:7080/, paste `ALICE_TOKEN_NEW`. Header now
   shows role **User**.

3. Navigate to **Rules**, click "**+ New rule**".
   - Client: client-a; Listen port: 30005;
     Target: 127.0.0.1:9000; Protocol: TCP. Submit.
   - The new rule appears in the list with `owner=alice` and state
     transitioning **Pending → Active** within the ack timeout
     (≤ 1 s under loopback).

4. Click into the rule. The **Live stats** panel shows
   `bytes_in=0`, `active_connections=0`. The status indicator
   reads "Streaming" (SSE connected).

5. Generate some traffic:

   ```sh
   nc -lk 9000 &
   echo hello | nc 127.0.0.1 30005
   ```

   Within 6 seconds the live stats panel updates with non-zero bytes
   (SC-004).

## 6. RBAC violations (US1 acceptance §3.1)

Stay logged in as alice. Try each of these and confirm the UI rejects
with a clear inline form error matching the server's `RbacError` code:

- New rule with port `30099` → `port_outside_grant`.
- New rule with protocol UDP → `protocol_not_granted`.
- New rule with client `client-b` → `client_not_granted`.

## 7. Read-side filtering (US2 acceptance)

Stay logged in as alice (User role).

1. **Users** in the navigation: hidden (rendered for superadmins only).
2. Direct-navigate to http://127.0.0.1:7080/users → `<PermissionDenied />`
   placeholder, no API call (Network tab confirms).
3. Direct-navigate to http://127.0.0.1:7080/users/_legacy → 
   `<PermissionDenied />`.
4. **Rules** list: shows exactly one row, the rule alice pushed.
5. **Audit log** in the navigation: hidden.

## 8. Audit log + JSON export (US3)

Switch back to the superadmin tab.

1. Navigate to **Audit log**. The table shows the last several
   `operator.allow` and `operator.deny` events from this session,
   newest-first.
2. Filter dropdown: **Outcome → deny**. Only deny rows remain (the
   3 RBAC violations alice attempted in § 6, plus any others). Filter
   is client-side (Network tab shows no new request).
3. Click "**Download as JSON**". Browser downloads
   `audit-<timestamp>.ndjson` containing the currently visible rows.
   `head -1` of the file is a complete `AuditEntry` JSON object.
4. From a non-superadmin tab (alice), navigate to
   http://127.0.0.1:7080/audit → `<PermissionDenied />`,
   no audit data fetched.

## 9. Token-leak audit (SC-006)

Before logging out, run this checklist in the dev-tools:

- **Application → Session Storage**: `portunus.token` is the only key,
  contains the bearer.
- **Application → Local Storage**: only `portunus.theme` and
  `portunus.lang`. No token.
- **Application → Cookies**: empty (we don't use cookies).
- **Network**: every request to `/v1/*` has a single `Authorization`
  header carrying the bearer. The bearer does NOT appear in any
  request URL, query string, or `Referer` header. Right-click any
  request → **Copy as cURL**: confirm only `-H 'Authorization: Bearer …'`.
- **Console**: a fresh page load shows no log lines containing the
  bearer prefix.
- **Elements**: search the DOM for the first 8 chars of the bearer.
  Zero matches.

## 10. Theme + language (US4)

1. Open **Settings**. Toggle **Theme → Dark**. The page transitions
   without reload.
2. Toggle **Language → 简体中文**. Every navigation item, button, and
   table header switches within 200 ms (no full reload).
3. Reload the browser. Both preferences persist.

## 11. Restart roundtrip

```sh
kill $SERVER_PID
wait $SERVER_PID 2>/dev/null
target/release/portunus-server --config-dir /tmp/forward-006 serve &
SERVER_PID=$!
sleep 0.5
```

In the still-open superadmin tab, refresh. The token in
`sessionStorage` is still valid post-restart (token store is on disk),
so the dashboard loads. Alice's rule (in-memory only — v0.5 design)
is gone; the Rules list is empty.

In the alice tab, refresh. Same outcome on the auth side; the rule
list is empty.

## 12. Cleanup

```sh
kill $SERVER_PID $CLIENT_PID
wait $SERVER_PID $CLIENT_PID 2>/dev/null
rm -rf /tmp/forward-006
```

## Acceptance criteria summary

| Spec SC | Demonstrated in this quickstart |
|---|---|
| SC-001 (< 5 min onboarding) | § 1-5 wall-clock |
| SC-002 (100% read coverage) | navigation through every list page |
| SC-003 (RBAC isolation) | § 7 (alice cannot see admin pages or other users' data) |
| SC-004 (≤ 6 s live stats lag) | § 5 step 5 |
| SC-005 (≤ 3 MB binary growth, ≤ 500 KB JS) | size-limit gate at build (§ 1) + `ls -lh target/release/portunus-server` diff against v0.5.0 |
| SC-006 (zero token leak) | § 9 checklist |
| SC-007 (en + zh-CN i18n) | § 10 |

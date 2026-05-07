# Quickstart: Multi-User RBAC

**Feature**: 005-multi-user-rbac
**Phase**: 1 (design)

This walkthrough demonstrates the four user stories end-to-end on a
single host using loopback. Run the steps top-to-bottom; each block
is a self-contained shell snippet.

The exit-code map (used by the assertions below) is documented in
`contracts/operator-api.md` § CLI Exit Codes.

## 0. Prerequisites

- v0.5.0 binaries `forward-server` and `forward-client` built and
  on `$PATH` (e.g. `cargo build --release && ln -sf
  target/release/{forward-server,forward-client} /usr/local/bin/`).
- A `server.toml` from `deploy/server.toml.example`, with at least
  the gRPC listener and operator HTTP listener pointing at
  `127.0.0.1:0` (kernel-assigned ephemeral ports).
- The server NOT yet started.

## 1. Bootstrap the first superadmin (US2 — admin lifecycle)

We have two paths; pick one. Both end with the same on-disk state.

### Path A — config-file shortcut (recommended for fresh deployments)

```sh
# Generate a 256-bit token, then put it in server.toml.
TOKEN=$(forward-server gen-token)        # 43-char URL-safe-base64
echo "operator_token = \"$TOKEN\"" >> server.toml
forward-server --config server.toml &
SERVER_PID=$!

# Note the token elsewhere — you cannot retrieve it later.
echo "Superadmin token: $TOKEN"
```

The server creates `_superadmin` user + the credential on first
start. The token in `server.toml` is hashed and stored in
`identity.json`; the config line can be removed afterward.

### Path B — one-shot CLI (recommended for upgrading from v0.4)

```sh
forward-server --config server.toml &
SERVER_PID=$!

# bootstrap-superadmin prints the token exactly once.
OUTPUT=$(forward-server --config server.toml bootstrap-superadmin \
            --name "ops on-call")
TOKEN=$(echo "$OUTPUT" | sed -n 's/.*token=\([A-Za-z0-9_-]*\).*/\1/p')
echo "Superadmin token: $TOKEN"
```

Re-running `bootstrap-superadmin` against a non-empty store exits
`2 already_bootstrapped` — verified below.

```sh
forward-server --config server.toml bootstrap-superadmin --name "second" \
   ; test $? -eq 2
```

### Wall-clock checkpoint

Time from "no `identity.json`" to "superadmin token in hand" should
be under 10 seconds (SC-001 lower bound; the full SC-001 cycle adds
the user-and-grant steps below).

## 2. Add a tenant user, issue a credential, grant capability (US2)

```sh
export FORWARD_OPERATOR_TOKEN=$TOKEN

# Create the tenant.
forward-server user-add alice --display-name "Alice — payments"

# Issue a credential for her. The token is in the response body.
ISSUE=$(forward-server credential-issue alice --label "laptop" --format json)
ALICE_TOKEN=$(echo "$ISSUE" | jq -r '.token')

# Grant her: client-a, ports 30000..30010, TCP only.
forward-server grant-add alice \
    --client client-a \
    --listen-ports 30000..30010 \
    --protocols tcp \
    --note "payments staging"

# Verify.
forward-server user-list --format table
forward-server grant-list --user alice --format table
```

Time-to-here from a fresh deployment: should be **under 60 seconds**
of wall-clock (SC-001 hard target). The bottleneck is operator
typing speed, not the server.

## 3. Constrained tenant pushes a rule within their grants (US1)

```sh
# Provision a forwarding client (existing v0.4 path).
CLIENT_TOKEN=$(forward-server provision-client client-a --format json | jq -r '.token')
forward-client --token "$CLIENT_TOKEN" --server 127.0.0.1:<grpc-port> &
CLIENT_PID=$!

# alice pushes a rule that fits her grant. Should succeed.
unset FORWARD_OPERATOR_TOKEN
export FORWARD_OPERATOR_TOKEN=$ALICE_TOKEN

forward-server push-rule \
    --client client-a \
    --listen-port 30005 \
    --target 10.0.0.5:80 \
    --protocol tcp
# Exit 0; output includes `"owner": "alice"`.
```

### 3.1 Reject violations (US1 acceptance scenarios 2–4)

```sh
# Port outside grant — exit 5, code = port_outside_grant.
forward-server push-rule --client client-a --listen-port 30099 \
    --target 10.0.0.5:80 --protocol tcp
test $? -eq 5

# Wrong protocol — exit 5, code = protocol_not_granted.
forward-server push-rule --client client-a --listen-port 30005 \
    --target 10.0.0.5:80 --protocol udp
test $? -eq 5

# Wrong client — exit 5, code = client_not_granted.
forward-server push-rule --client client-b --listen-port 30005 \
    --target 10.0.0.5:80 --protocol tcp
test $? -eq 5
```

## 4. Tenant inspects only their own rules (US3)

```sh
# Add a second user with their own rule.
export FORWARD_OPERATOR_TOKEN=$TOKEN  # back to superadmin
forward-server user-add bob --display-name "Bob"
BOB_TOKEN=$(forward-server credential-issue bob --format json | jq -r '.token')
forward-server grant-add bob --client client-a --listen-ports 31000..31000 --protocols tcp
export FORWARD_OPERATOR_TOKEN=$BOB_TOKEN
forward-server push-rule --client client-a --listen-port 31000 \
    --target 10.0.0.5:80 --protocol tcp

# bob sees only his rule.
forward-server rule-list --format json | jq '.rules | length'   # → 1

# alice sees only hers.
export FORWARD_OPERATOR_TOKEN=$ALICE_TOKEN
forward-server rule-list --format json | jq '.rules | length'   # → 1

# superadmin sees both.
export FORWARD_OPERATOR_TOKEN=$TOKEN
forward-server rule-list --format json | jq '.rules | length'   # → 2

# Cross-tenant read attempt.
ALICE_RULE_ID=$(forward-server rule-list --owner alice --format json \
                | jq -r '.rules[0].id')
export FORWARD_OPERATOR_TOKEN=$BOB_TOKEN
forward-server rule-stats $ALICE_RULE_ID --format json
test $? -eq 5    # not_owner
```

## 5. Tenant rotates their own credential (US4)

```sh
export FORWARD_OPERATOR_TOKEN=$ALICE_TOKEN

# Find the credential id alice currently holds.
ALICE_CRED_ID=$(forward-server credential-list alice --format json \
                | jq -r '.credentials[] | select(.status=="active") | .credential_id')

# Rotate.
ROT=$(forward-server credential-rotate $ALICE_CRED_ID --format json)
ALICE_TOKEN_NEW=$(echo "$ROT" | jq -r '.token')

# Old token now rejected.
forward-server rule-list --token "$ALICE_TOKEN"        ; test $? -eq 4

# New token works.
forward-server rule-list --token "$ALICE_TOKEN_NEW"    ; test $? -eq 0
```

## 6. Verify SC-005: revoke-grant cascade

```sh
export FORWARD_OPERATOR_TOKEN=$TOKEN

# Find alice's grant and her active rule.
ALICE_GRANT_ID=$(forward-server grant-list --user alice --format json \
                 | jq -r '.grants[0].grant_id')
ALICE_RULE_ID=$(forward-server rule-list --owner alice --format json \
                | jq -r '.rules[0].id')

# Revoke the grant. Capture wall-clock to confirm SC-005 (< 5 s).
START=$(date +%s.%N)
RESPONSE=$(forward-server grant-revoke $ALICE_GRANT_ID --format json)
END=$(date +%s.%N)
ELAPSED=$(echo "$END - $START" | bc)
echo "Revoke + cascade took ${ELAPSED}s"
test $(echo "$ELAPSED < 5" | bc) -eq 1

# The cascaded rule is in the response.
echo "$RESPONSE" | jq '.removed_rules | contains([" + $ALICE_RULE_ID + "])'

# alice's active-rules list is now empty.
export FORWARD_OPERATOR_TOKEN=$ALICE_TOKEN_NEW
forward-server rule-list --format json | jq '.rules | length'    # → 0
```

## 7. Verify SC-006: restart roundtrip

```sh
kill $SERVER_PID
wait $SERVER_PID 2>/dev/null

forward-server --config server.toml &
SERVER_PID=$!
sleep 0.5

# Superadmin token still works.
export FORWARD_OPERATOR_TOKEN=$TOKEN
forward-server user-list --format json | jq '.users | length'    # → 3 (_superadmin, alice, bob)

# alice's rotated token still works.
export FORWARD_OPERATOR_TOKEN=$ALICE_TOKEN_NEW
forward-server rule-list --format json    # exit 0

# bob's grants and credentials persisted.
export FORWARD_OPERATOR_TOKEN=$BOB_TOKEN
forward-server rule-list --format json | jq '.rules | length'    # → 1
```

## 8. Verify SC-004: legacy operator workflow byte-identical (with token added)

```sh
# A v0.4.0-style HTTP request, with one new header.
curl -s -H "Authorization: Bearer $TOKEN" \
     http://127.0.0.1:<operator-port>/v1/rules \
     | jq '.rules[0]'
# Response shape is a byte-superset of v0.4.0: same fields plus `owner`.
```

## 9. Cleanup

```sh
kill $SERVER_PID $CLIENT_PID
wait $SERVER_PID $CLIENT_PID 2>/dev/null
rm -f identity.json
```

## Acceptance criteria summary

After running this walkthrough, the following success criteria from
the spec are observable:

| SC | Demonstrated by |
|---|---|
| SC-001 (< 60 s onboarding) | Section 2 timing |
| SC-002 (≤ +5 ms push-rule latency) | Validated in `forward-server/tests/cli_push_rule.rs`, NOT by this walkthrough |
| SC-003 (100% violation rejection) | Sections 3.1, 4 (cross-tenant read) |
| SC-004 (byte-identical wire shapes) | Section 8 |
| SC-005 (cascade within 5 s) | Section 6 timing assertion |
| SC-006 (restart roundtrip) | Section 7 |
| SC-007 (operator-CLI answers "who pushed X") | Section 4 (`rule-list --format table` shows `owner` column) |

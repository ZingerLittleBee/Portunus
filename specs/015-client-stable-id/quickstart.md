# Quickstart: Client Stable Identifier

Validate the feature end-to-end after implementation. Commands assume repo root and the
project's standard dev setup.

## Build & test gates

```sh
# Core type + relaxed validation
cargo test -p portunus-core id::

# Server store + migration (skip embedded webui build)
PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server --lib store::

# Whole workspace (wire contract + integration + e2e)
PORTUNUS_SKIP_WEBUI=1 cargo test --workspace

# Lints / format (CI gates on -D warnings)
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check

# Data-plane perf flatness (expected unchanged)
cargo bench -p portunus-client --bench data_plane

# Web UI (≤500 KB gz)
cd webui && pnpm install --frozen-lockfile && pnpm build
```

## Manual acceptance walkthrough (maps to spec Success Criteria)

1. **Friendly name (SC-001)**: Create a client named `Acme Prod – East` (uppercase, space,
   en-dash) and one named `北京边缘节点`. Both succeed; both appear in the list verbatim, each
   with a distinct id.
2. **Reject bad names (FR-011)**: Try `""`, a 256-byte name, and a name with a `\t`. Each is
   rejected with a specific message.
3. **Rename is identity-safe (SC-002)**: Give a client a rule, a quota, and some traffic; note
   its id; rename it; confirm the id is unchanged and the rule/quota/history still resolve to
   it. With the client connected, rename again and confirm the session keeps forwarding.
4. **Stable links (SC-003)**: Open `/clients/<id>`, copy the URL, rename the client, reload —
   same client. Repeat with a `--client-id` CLI command.
5. **Duplicate names (FR-013)**: Create a second client with the same display name — accepted,
   no warning; both disambiguated by id in the list.
6. **Upgrade existing DB (SC-004/SC-006)**: Point the new server at a populated `V010`
   database. On start, every client gains an id, no rows are orphaned. Restart again — no
   re-migration, no error.
7. **Legacy client reconnects (SC-005)**: Using a credential bundle created before the
   upgrade (token, no id), connect to the upgraded server — authenticates and forwards with no
   reconfiguration.

## Things that MUST stay true

- `proto` change is additive; a pre-upgrade client still connects.
- `webui/src/components/UserCreateForm.tsx:44` regex (userId) is untouched.
- `portunus-forwarder` data-plane library is untouched.
- A CHANGELOG entry records: new `client_id` wire field, relaxed client-name rules, rename.

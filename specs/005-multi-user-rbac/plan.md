# Implementation Plan: Multi-User RBAC for the Forward Server

**Branch**: `005-multi-user-rbac` | **Date**: 2026-05-07 | **Spec**: [spec.md](./spec.md)
**Input**: Feature specification from `/specs/005-multi-user-rbac/spec.md`

## Summary

Introduce **operator-side identity, role, and grant** as first-class
server entities, and enforce them on every operator request before any
side effect. This is the first feature that makes Constitution
Principle V observable: today every `/v1/*` request is implicitly
superadmin (the operator HTTP listener is loopback-only and
unauthenticated — see R-001), so "multi-tenant isolation" exists in
the constitution but not in the wire.

Two reuse anchors keep the change additive:

1. **Forwarding hot path is untouched.** `forwarder/{proxy.rs,udp/}` are
   not modified. The data plane neither knows nor cares which operator
   pushed a rule. Owner is stamped at rule-creation time and read back
   only by the operator surface (responses, metrics labels).
2. **The existing `forward-auth` crate is the template.** Its
   `Authenticator` trait + `FileTokenStore` already encodes the exact
   pattern we need (atomic-write JSON file, blake3-hashed tokens, single
   verification seam) — for the *client→server* gRPC channel. We add a
   sibling `OperatorAuthenticator` + `FileOperatorStore` for the
   *operator→server* HTTP channel without disturbing the existing seam.

The wire / persistence / CLI surfaces evolve as follows:

- **No proto changes.** The gRPC client↔server channel does not transmit
  operator identity. `RuleStats` going server-side does not need owner
  info (server already knows it).
- **Operator HTTP API gains a mandatory `Authorization: Bearer <token>`
  header** on every existing endpoint. Response shapes are
  byte-identical for the success path; new fields (`owner` on rule
  list / get) are additive (omitted from current v0.4.0 client decoders
  per JSON-superset rules). New endpoints land under
  `/v1/users`, `/v1/users/{id}/credentials`, `/v1/grants`. New
  rejection reasons: `unauthenticated`, `credential_invalid`,
  `client_not_granted`, `port_outside_grant`, `protocol_not_granted`,
  `not_owner`, `role_required`.
- **Operator CLI gains** `user-{add,list,remove}`,
  `credential-{issue,rotate,revoke,list}`, `grant-{add,list,revoke}`,
  and a one-shot `bootstrap-superadmin` for environments without an
  `operator_token` in `server.toml`. Existing rule subcommands learn
  to pick up the bearer token from env (`FORWARD_OPERATOR_TOKEN`) or
  `--token <value>`.
- **Persistence (NEW)**: `operator_store_path` in `server.toml`, by
  default `<config_dir>/identity.json`. Same atomic-write protocol as
  `FileTokenStore` (`write tmp → fsync → rename → fsync(parent)`).
  Schema versioned (`{"version": 1, "users": [...], "grants": [...]}`).
- **Prometheus metrics** gain an `owner` label on the existing per-rule
  collectors. **Cardinality budget preserved**: each rule has exactly
  one owner, so adding the label does not multiply rows
  (`{client, rule, owner}` ⇒ same row count as `{client, rule}`).
- **Audit log**: every authentication and authorization decision emits
  one structured log line at INFO (allow) or WARN (deny), routed
  through the existing tracing/JSON sink (Constitution IV). Carries
  `actor_user_id`, `action`, `resource`, `decision`, `reason?`.
  Persistent audit DB is out of scope (deferred to a future feature).

The feature is **additive on top of v0.4.0** in spirit, but with **one
breaking operator change**: every operator request must present a
bearer token. This is unavoidable because the spec's whole purpose is
to authenticate operators. The mitigation is the `bootstrap-superadmin`
subcommand and the documented quickstart (one-shot setup before any
use). See R-002 for the trade-off.

## Technical Context

**Language/Version**: Rust 1.88 (constitution-pinned MSRV via `tonic`).
**Primary Dependencies**: existing — `tokio`, `axum`, `tonic`, `prost`,
                          `rustls`, `prometheus`, `tracing`,
                          `serde_json`, `chrono`, `blake3` (via
                          `forward-core::fingerprint`), `base64` (via
                          `forward-auth`). **One new external
                          dependency**: `tower` already in the tree
                          (transitive via `axum`); we use its `Layer`
                          API directly for the new auth middleware.
                          No new top-level deps in `Cargo.toml`.
**Storage**: New JSON file `identity.json` (path configurable via
             `operator_store_path` in `server.toml`). Atomic-write
             protocol mirrors `forward-auth::file_store` verbatim
             (write tmp → fsync → rename → fsync(parent)). Rationale
             for sticking to JSON-file storage rather than introducing
             SQLite/Postgres: data shape is small and bounded
             (operators have tens of users, hundreds of grants);
             atomic-write JSON is already a proven pattern in the
             codebase; introducing a SQL dependency for this scale
             would dwarf the feature. The constitution's
             `TODO(STORAGE_CHOICE)` remains deferred to a future
             feature where data shape forces it (e.g., persistent
             audit log retention).
**Testing**: `cargo test` per crate (unit + integration);
             `forward-server` tests use real HTTP via `reqwest`
             against an in-process `axum` router (existing pattern
             from v0.4.0); `forward-e2e` adds an `rbac_smoke.rs`
             integration covering US1 + US2 acceptance scenarios
             with real tokens, real HTTP, real client+server. Auth
             middleware tests use real `axum::Router` with a real
             `FileOperatorStore` over a temp dir. **No socket
             mocks, no auth mocks** (Constitution III).
**Target Platform**: Linux primary (musl static binary). macOS for
                     development. fsync semantics on macOS are
                     weaker than Linux — the existing
                     `FileTokenStore` already accepts that; the new
                     `FileOperatorStore` follows the same convention.
**Project Type**: Multi-crate Cargo workspace (unchanged): `forward-core`,
                  `forward-proto`, `forward-auth`, `forward-server`,
                  `forward-client`, `forward-e2e`. The new code lives
                  in `forward-auth` (data structures + store) and
                  `forward-server` (axum middleware, HTTP handlers,
                  CLI, RBAC enforcement).
**Performance Goals**:
  - **Operator-path latency budget**: authentication + authorization
    on the `push-rule` path adds ≤ 5 ms median over the v0.4.0
    baseline (SC-002). This is one blake3 hash (32-byte input,
    sub-microsecond) plus a `HashMap<[u8;32], UserId>` lookup plus a
    grant scan over O(grants-per-user) (typical < 10). Comfortably
    under budget. We will measure to confirm rather than micro-optimise
    speculatively.
  - **Data plane regression budget**: existing `data_plane.rs` and
    `udp_data_plane.rs` benches must stay within ±5% of the v0.4.0
    baseline (SC-006 floor + Constitution II). Re-run on every PR
    that touches `forwarder/`. **Expected delta: 0%** because this
    feature does not modify `forwarder/`.
  - **Persistence write latency**: a single write of `identity.json`
    completes in < 10 ms on tmpfs / SSD; this is dominated by fsync,
    not the JSON serialization (file size < 100 KB for any plausible
    tenant count). Acceptable because writes happen on operator
    actions (interactive cadence), not on the data plane.
**Constraints**:
  - **Operator API auth header is mandatory once `identity.json`
    exists.** A server with no `identity.json` and no
    `operator_token` in config rejects every operator request with
    `bootstrap_required`. There is no implicit unauthenticated mode
    in v0.5.0. Migration story: documented `bootstrap-superadmin`
    subcommand creates the first superadmin token (printed exactly
    once to stdout; never persisted retrievably). See R-002.
  - **Cardinality**: the existing per-rule collectors gain an
    `owner` label. Per-rule cardinality stays at one row per rule per
    collector (each rule has exactly one owner). This preserves the
    SC-004 cardinality budget from v0.4.0 verbatim.
  - **Grant matching is a closed set, not a union**: a push request
    is authorized iff ONE single grant fully covers the request's
    (client, full listen-port range, protocol). Range rules whose
    listen range straddles two grants are **rejected**, even if the
    union of grants would cover them. Rationale (R-005): predictable
    operator semantics; "build me a grant that covers this rule" is
    easier to reason about than "build me a set of grants whose
    union covers this rule".
  - **Bootstrap token is single-use surface**:
    `bootstrap-superadmin` may run only once on a given
    `identity.json`. Subsequent invocations exit non-zero with
    `already_bootstrapped`. The store must be deleted (or rotated
    via the regular CLI) to bootstrap again — this is intentional
    to prevent silent superadmin minting.
  - **Credential rotation is atomic** and the old credential is
    invalidated within the same write transaction. There is no
    "grace window" where both old and new credentials work; that
    would weaken the leak-recovery story.
  - **`identity.json` writes are serialized** by an `RwLock<HashMap>`
    in `FileOperatorStore` — same pattern as `FileTokenStore`. No
    cross-process coordination is needed because only one
    `forward-server` process touches the file at a time
    (single-binary deployment, Constitution Tech Constraint).
  - **Owner stamp on Rules is in-memory only** (R-006): rules are
    not persisted across server restart per the existing
    `rules.rs` design comment. After a restart, both rules and
    their owner stamps are gone — operators re-push rules under
    their own identity. This matches v0.4.0 behavior (rules already
    don't survive restart) and avoids introducing a new persistence
    surface.
**Scale/Scope**: Practical operator deployments hold O(10) users,
                 O(100) grants, O(1 k) rules. The JSON file at this
                 scale serializes in < 1 ms and stays well under any
                 OS atomic-rename limit. No paging, no streaming
                 needed for v0.5.0.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Pass? | Notes |
|---|---|---|
| I. Security by Default (TLS + bearer token, no plaintext) | ✅ (with second-reviewer flag per Dev Workflow) | Adds a NEW credential type (operator bearer token) with the SAME shape as the existing client token: 256-bit `OsRng` random, blake3-hashed at rest (`token::generate_token` / `token::hash_token` reused verbatim from `forward-auth`), never logged, returned to the operator exactly once at issuance. The new `OperatorAuthenticator` trait satisfies Principle I's "single seam" requirement: every operator request flows through one `axum::middleware::from_fn_with_state` that calls `OperatorAuthenticator::verify`. Swapping the scheme later (e.g., OIDC) means writing one new impl, not editing every handler. Bootstrap subcommand prevents silent superadmin minting (refuses if any superadmin exists). The operator HTTP listener still binds loopback by default (existing `serve.rs` policy); auth is now defense-in-depth rather than the only barrier. **Triggers the Dev Workflow "second reviewer with security context" rule** because this PR touches credential / token handling. |
| II. Performance Is a Feature | ✅ | The forwarding hot path (`forwarder/proxy.rs`, `forwarder/udp/`) is **not modified**. The two existing data-plane criterion benches (`data_plane.rs`, `udp_data_plane.rs`) continue as the regression gate; expected delta is **0%**. The auth check on the operator HTTP path adds one blake3 hash + one `HashMap` lookup per request (sub-microsecond); operator throughput is interactive (operators issue dozens of requests per minute, not tens of thousands per second), so a microbench is overkill. We will assert SC-002 (≤ +5 ms median push-rule latency) via the existing `forward-server/tests/cli_push_rule.rs`-style integration test that already measures wall-clock against a real router. |
| III. Test-First Discipline | ✅ | (a) Contract tests for the new HTTP endpoints land first in `forward-server/tests/http_users_contract.rs`, `http_grants_contract.rs`, `http_credentials_contract.rs` — each asserts the request/response shape from `contracts/operator-api.md` against a real `axum::Router` with a real `FileOperatorStore` over a tmp dir. (b) RBAC enforcement tests (`tests/rbac_push_rule.rs`) assert each rejection reason from FR-008 with real HTTP + real store. (c) Bootstrap test (`tests/bootstrap_superadmin.rs`) asserts the one-shot semantics. (d) End-to-end (`forward-e2e/tests/rbac_smoke.rs`) wires real `forward-server` + real `forward-client` + real operator CLI to validate US1 + US2 acceptance scenarios. (e) Backward-compat test (`tests/legacy_no_auth_rejected.rs`) asserts that a request without `Authorization` header now gets `unauthenticated`, NOT 200 (this is the one operator-visible breaking change; the test pins the reason code so future reverts are caught). **No socket mocks, no auth mocks** — auth tests use a real store over a real temp dir. |
| IV. Observability & Operability | ✅ | Audit-log seam: the auth middleware emits one structured log line per request via the existing `tracing` + `tracing_subscriber::fmt::json()` pipeline. INFO on allow (`event = "operator.allow"`, includes `actor`, `action`, `resource`); WARN on deny (`event = "operator.deny"`, additionally includes `reason`). Existing per-rule Prometheus collectors gain an `owner` label; cardinality unchanged (one row per rule). New collector `forward_operator_requests_total{outcome, reason}` tracks aggregate auth outcomes for dashboards. **Graceful reload** (Constitution IV): the operator store is reloaded from disk on SIGHUP if the file mtime changed; in-flight requests complete against the snapshot they entered with. **Logs MUST NOT include raw credentials** — verified by a unit test that scans WARN/INFO records for the literal token string and fails if found. |
| V. Multi-Tenant Isolation | ✅ | This **is** the feature. Every authorization check is expressed in terms of `(actor_user_id, resource)` pairs, never globally. A tenant's allowed `(client, port_range, protocols)` are policy inputs to the rules service, not client-trusted hints. The rule store gains a per-rule `owner_user_id`; non-superadmin reads are filtered server-side before any response is built. Error messages on cross-tenant access return `not_owner` with no leakage of whether the rule exists for another tenant (uniform "not found / not owner" timing — we will validate by inspecting the handler's branch structure, not by side-channel benchmarking, which is overkill at this stage). |

**Gate result**: PASS, with the Dev-Workflow "second reviewer for credential / token handling" requirement noted. Nothing to track in the Complexity Tracking table.

**Post-Phase-1 re-check**: Re-evaluated after `data-model.md`,
`contracts/operator-api.md`, `contracts/persistence.md`, and
`quickstart.md` landed. The `OperatorAuthenticator` trait sits next to
the existing `Authenticator` in `forward-auth/src/lib.rs` (single seam
preserved, R-007). `identity.json` reuses the v0.1 atomic-write
protocol verbatim (R-001). Owner label adds zero cardinality (R-008).
The bootstrap subcommand is one-shot and refuses re-runs (R-002).
PASS, gate clear for `/speckit-tasks`.

## Project Structure

### Documentation (this feature)

```text
specs/005-multi-user-rbac/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/
│   ├── operator-api.md  # Phase 1: HTTP + CLI surface (new endpoints + delta)
│   └── persistence.md   # Phase 1: identity.json schema + write protocol
├── checklists/
│   └── requirements.md  # Already produced by /speckit-specify
└── tasks.md             # /speckit-tasks output (not created here)
```

### Source Code (repository root)

```text
crates/
├── forward-auth/src/                       # NEW peer types live here, next to Authenticator
│   ├── lib.rs                              # MOD: add OperatorIdentity { user_id, role },
│   │                                          #   OperatorAuthenticator trait, OperatorRole enum,
│   │                                          #   Grant struct, RbacError enum
│   ├── operator_store.rs                   # NEW: FileOperatorStore — same atomic-write
│   │                                          #   protocol as file_store.rs; persists
│   │                                          #   users, credentials, grants in one
│   │                                          #   identity.json document
│   ├── token.rs                            # UNCHANGED — generate_token / hash_token reused
│   └── file_store.rs                       # UNCHANGED — client→server token store stays as-is
│
├── forward-server/src/
│   ├── operator/
│   │   ├── auth_layer.rs                   # NEW: tower::Layer that calls
│   │   │                                          #   OperatorAuthenticator::verify on every
│   │   │                                          #   /v1/* request, injects OperatorIdentity
│   │   │                                          #   into request extensions; emits audit log
│   │   ├── rbac.rs                         # NEW: pure functions
│   │   │                                          #   enforce_push(identity, &Rule) -> Result<(),RbacError>,
│   │   │                                          #   enforce_read(identity, &Rule) -> Result<(),RbacError>,
│   │   │                                          #   filter_visible(identity, &[Rule]) -> Vec<&Rule>
│   │   ├── users.rs                        # NEW: HTTP handlers + CLI for /v1/users
│   │   ├── grants.rs                       # NEW: HTTP handlers + CLI for /v1/grants
│   │   ├── credentials.rs                  # NEW: HTTP handlers + CLI for credentials
│   │   ├── bootstrap.rs                    # NEW: one-shot bootstrap-superadmin subcommand
│   │   ├── http.rs                         # MOD: wire auth_layer onto /v1/*; thread
│   │   │                                          #   OperatorIdentity into existing
│   │   │                                          #   post_rules / get_rules / etc.;
│   │   │                                          #   include `owner` field in rule responses
│   │   ├── cli.rs                          # MOD: parse user/grant/credential subcommands;
│   │   │                                          #   read FORWARD_OPERATOR_TOKEN env / --token flag
│   │   ├── rule_cli.rs                     # MOD: filter list/stats by owner unless superadmin
│   │   └── mod.rs                          # MOD: re-export new modules
│   ├── rules.rs                            # MOD: Rule gains `owner_user_id: UserId`;
│   │                                          #   push_rule takes &OperatorIdentity, calls
│   │                                          #   rbac::enforce_push BEFORE any state mutation;
│   │                                          #   on grant revoke / user remove, scan and
│   │                                          #   remove orphaned rules
│   ├── metrics.rs                          # MOD: add `owner` label to per-rule collectors;
│   │                                          #   add forward_operator_requests_total
│   │                                          #   {outcome, reason}
│   ├── state.rs                            # MOD: add Arc<dyn OperatorAuthenticator> +
│   │                                          #   Arc<dyn OperatorAuthorizer> to AppState
│   ├── config.rs                           # MOD: add operator_store_path (default
│   │                                          #   `<config_dir>/identity.json`),
│   │                                          #   operator_token (optional bootstrap shortcut)
│   ├── serve.rs                            # MOD: load FileOperatorStore at startup;
│   │                                          #   register SIGHUP reload handler;
│   │                                          #   refuse-to-serve check if no superadmin and
│   │                                          #   no bootstrap config (exit with hint)
│   ├── main.rs                             # MOD: route bootstrap-superadmin subcommand
│   └── shutdown.rs                         # UNCHANGED
│
├── forward-server/tests/
│   ├── http_users_contract.rs              # NEW: contract test against /v1/users
│   ├── http_grants_contract.rs             # NEW: contract test against /v1/grants
│   ├── http_credentials_contract.rs        # NEW: contract test against credential endpoints
│   ├── rbac_push_rule.rs                   # NEW: each FR-008 rejection reason exercised
│   ├── rbac_read_filtering.rs              # NEW: list/stats filter to owned rules
│   ├── bootstrap_superadmin.rs             # NEW: one-shot semantics
│   ├── legacy_no_auth_rejected.rs          # NEW: pin the breaking change behavior
│   ├── audit_log_redaction.rs              # NEW: unit test that no log line contains a token
│   ├── identity_persistence.rs             # NEW: restart roundtrip preserves users/grants
│   └── cli_push_rule.rs                    # MOD: existing tests learn to set FORWARD_OPERATOR_TOKEN
│
├── forward-e2e/tests/
│   ├── common/mod.rs                       # MOD: helper to bootstrap superadmin + issue
│   │                                          #   per-test user; default token plumbed into
│   │                                          #   spawn_server fixtures; existing v0.4 e2e
│   │                                          #   tests bootstrap a superadmin transparently
│   └── rbac_smoke.rs                       # NEW: end-to-end US1 (constrained user push +
│   │                                          #   reject) + US2 (superadmin CRUD lifecycle
│   │                                          #   + restart roundtrip)
│
└── deploy/
    └── server.toml.example                 # MOD: add operator_store_path key,
                                            #   operator_token bootstrap shortcut, comments
                                            #   on the new /v1 auth requirement
```

**Structure Decision**: same multi-crate workspace as v0.1.0 → v0.4.0.
The new code distributes across two existing crates: `forward-auth`
gains the data structures + store (mirroring `Authenticator` /
`FileTokenStore`) so the auth seam stays in one crate; `forward-server`
gains the axum middleware, HTTP/CLI handlers, and the rbac functions.
**No new crate** — the volume is small and would create gratuitous
cross-crate boundaries. **No proto changes** — operator identity is a
server-side concept, never on the gRPC client↔server wire.

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

(none)

**Note on `TODO(STORAGE_CHOICE)`**: The constitution's persistent-store
TODO (SQLite vs Postgres) remains deferred. This feature explicitly
sticks to the JSON-file pattern proven by `FileTokenStore`. Rationale
in `research.md` § R-001. Revisit when a future feature needs query
capabilities the JSON file cannot serve (e.g., persistent audit log
search).

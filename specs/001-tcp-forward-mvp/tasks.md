---
description: "Tasks for 001-tcp-forward-mvp"
---

# Tasks: Single-Tenant Token-Authenticated Control Plane with Single-Port TCP Forwarding (MVP)

**Input**: Design documents from `/specs/001-tcp-forward-mvp/`
**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/forward.proto, contracts/operator-api.md, contracts/persistence.md, quickstart.md
**Constitution**: v2.0.0 — Principle III (Test-First Discipline) is NON-NEGOTIABLE. Test tasks within each phase MUST be written and observed failing before the corresponding implementation tasks begin.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies on incomplete tasks)
- **[Story]**: Maps to User Stories from spec.md (US1, US2, US3)
- File paths are workspace-relative; create directories as needed

## Path Conventions

Cargo workspace at repo root with crates under `crates/`:
- `crates/forward-proto/`, `crates/forward-core/`, `crates/forward-auth/`,
  `crates/forward-server/`, `crates/forward-client/`, `crates/forward-e2e/`
- Wire schema lives at `proto/forward.proto`
- Per-crate tests under `<crate>/tests/`; benchmarks under `<crate>/benches/`

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Bring up the workspace, code-gen pipeline, and shared tooling.

- [X] T001 Create root `Cargo.toml` declaring `[workspace]` with `members = ["crates/*"]` and resolver = "2"; pin MSRV to **1.88** and edition to **2024** in `[workspace.package]`. Add a `[workspace.dependencies]` block with the locked versions from `plan.md` Technical Context so member crates inherit via `<crate>.workspace = true`
- [X] T002 [P] Create `proto/forward.proto` by copying `specs/001-tcp-forward-mvp/contracts/forward.proto` verbatim
- [X] T003 Create `crates/forward-proto/` crate. Add `tonic-prost-build = "0.14"` as a build-dependency and `tonic = "0.14"` + `tonic-prost = "0.14"` + `prost = "0.14"` as runtime deps. `build.rs` invokes `tonic_prost_build::compile_protos("../../proto/forward.proto")`. `src/lib.rs` re-exports the generated module as `pub mod v1 { tonic::include_proto!("forward.v1"); }` (note: tonic 0.14 split codegen into `tonic-prost-build` and runtime into `tonic-prost`)
- [X] T004 [P] Add workspace-level `rustfmt.toml` and `clippy.toml`; configure `[workspace.lints]` in root `Cargo.toml` for `rust = { unsafe_code = "forbid" }` and `clippy = { pedantic = "warn" }`
- [X] T005 [P] Add `.cargo/config.toml` with target-dir override and any common rustflags (e.g., `link-arg=-fuse-ld=lld` if available)
- [X] T006 [P] Add minimal CI config at `.github/workflows/ci.yml` running `cargo build`, `cargo test`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo fmt --check`

**Checkpoint**: `cargo build` succeeds; generated proto types accessible via `forward_proto::v1::*`.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Core libraries every user story depends on. **No user-story work begins until this phase is complete.**

- [X] T007 Create `crates/forward-core/` crate; add `src/lib.rs`, `src/error.rs` defining `ForwardError` taxonomy (variants: `ClientAlreadyExists`, `ClientNotConnected`, `RuleNotFound`, `PortInUse`, `ActivationFailed { reason: String }`, `AuthFailed { reason: String }`, `Io(std::io::Error)`, `Tls(String)`)
- [X] T008 [P] In `crates/forward-core/src/id.rs`, define newtype IDs: `ClientName(String)` with DNS-label validation (1–63 chars, lowercase, regex `^[a-z0-9](-?[a-z0-9])*$`), `RuleId(u64)`, `RequestId(Ulid)` using `ulid` crate; unit tests for valid/invalid `ClientName` inputs
- [X] T009 [P] In `crates/forward-core/src/fingerprint.rs`, implement `pub fn sha256_hex(der: &[u8]) -> String` returning lowercase 64-char hex; unit test with a known-vector DER blob
- [X] T010 [P] In `crates/forward-core/src/config.rs`, define `ServerConfig` and `ClientConfig` structs per `data-model.md` with `serde::Deserialize`; `from_toml_path(path) -> Result<Self, ForwardError>`; unit tests load + reject malformed
- [X] T011 Create `crates/forward-auth/` crate; in `src/lib.rs` declare `pub trait Authenticator { fn verify(&self, token: &str) -> Result<ClientIdentity, AuthError>; fn issue(&self, name: ClientName) -> Result<String, AuthError>; fn revoke(&self, name: &ClientName) -> Result<(), AuthError>; }` plus `pub struct ClientIdentity { pub client_name: ClientName }` and `pub enum AuthError`
- [X] T012 [P] In `crates/forward-auth/src/token.rs`, implement `pub fn generate_token() -> String` (32 bytes from `OsRng`, URL-safe base64 no padding) and `pub fn hash_token(t: &str) -> [u8; 32]` (blake3); unit tests: token length 43 chars, distinct across 1000 calls, hash deterministic
- [X] T013 In `crates/forward-auth/src/file_store.rs`, implement `FileTokenStore` against the schema in `contracts/persistence.md`: `load(path)`, atomic `save(path)` (tmp + fsync + rename + parent fsync), `Authenticator` impl. Unit tests: roundtrip; rejects unknown `version`; rejects duplicate `client_name`; concurrent-reader test using `tempfile`
- [X] T014 [P] In `crates/forward-auth/src/file_store.rs`, add property test using `proptest`: random sequence of `issue/revoke/save/load` operations always yields a file readable as the same in-memory state
- [X] T015 Create `crates/forward-server/` binary crate; `src/main.rs` with `clap` parser exposing top-level subcommands `serve`, `provision-client`, `revoke`, `list-clients`, `push-rule`, `remove-rule`, `list-rules`, `rule-stats`; subcommand handlers stubbed with `unimplemented!()`
- [X] T016 [P] Create `crates/forward-client/` binary crate; `src/main.rs` with `clap` parser exposing `--bundle <path>`, `--reconnect-initial-delay-ms`, etc.; main loop stubbed with `unimplemented!()`
- [X] T017 [P] In `crates/forward-server/src/tls.rs`, implement self-signed cert generation via `rcgen` (ECDSA P-256, CN = hostname, 10y validity); read/write PEM at configurable paths; refuse to start if key file mode > 0600; unit test: round-trip generate→write→read→fingerprint matches
- [X] T018 [P] In `crates/forward-client/src/pinned_verifier.rs`, implement `struct PinnedVerifier { expected_sha256: [u8; 32] }` impl `rustls::client::danger::ServerCertVerifier`: compute SHA-256 over leaf `CertificateDer`, constant-time compare. Unit tests: matching fingerprint → `Ok`; mismatched fingerprint → `Err(rustls::Error::General(...))`
- [X] T019 [P] In `crates/forward-server/src/shutdown.rs`, implement `pub struct Shutdown { token: CancellationToken }` with `signal_handler()` that listens for SIGINT/SIGTERM via `tokio::signal::unix` and triggers cancel; integration test using `tokio::test` to verify cancel propagates to a child token
- [X] T020 [P] In `crates/forward-client/src/shutdown.rs`, mirror the above for the client process
- [X] T021 Create `crates/forward-e2e/` test-only crate (`[lib] doctest = false`); add `tests/common/mod.rs` with helpers `spawn_server(config_dir) -> ServerHandle` and `spawn_client(bundle_path) -> ClientHandle` that run the binaries via `assert_cmd` against a `tempfile::TempDir`

**Checkpoint**: All foundational crates compile; unit tests for `forward-core` and `forward-auth` pass. User-story phases may now proceed.

---

## Phase 3: User Story 1 — Provision a Trusted Client (Priority: P1) 🎯 MVP

**Goal**: From a clean install, the operator can provision a client, transfer the bundle, start the client process, and see it appear as connected. mTLS replaced by TLS+token; certificate pinned by fingerprint.

**Independent Test**: Following `quickstart.md` Steps 1–3, then `list-clients` shows `edge-01` as connected with remote address and connect timestamp within 5 seconds (SC-001 partial; matches spec acceptance scenarios US1.1–US1.4).

### Tests for User Story 1 (write FIRST, observe FAILING)

- [X] T022 [P] [US1] Contract test: gRPC `Channel` rejected with `UNAUTHENTICATED` when bearer-token metadata is absent — `crates/forward-e2e/tests/auth_failures.rs::test_missing_token_rejected`
- [X] T023 [P] [US1] Contract test: connection with revoked token returns `UNAUTHENTICATED` reason `token_revoked` — `crates/forward-e2e/tests/auth_failures.rs::test_revoked_token_rejected`
- [X] T024 [P] [US1] Contract test: client refuses TLS handshake (without sending token) when server cert fingerprint mismatches the bundle — `crates/forward-e2e/tests/auth_failures.rs::test_pin_mismatch_rejected`
- [X] T025 [P] [US1] Integration test: `forward-server provision-client edge-01` writes a `.bundle.json` matching the schema in `data-model.md` and returns exit 0; second invocation returns exit 2 (`client_already_exists`) — `crates/forward-server/tests/provisioning.rs`
- [X] T026 [P] [US1] Integration test: after server + client are up, `list-clients --format json` includes `{"client_name":"edge-01","connected":true,...}` within 5s — `crates/forward-e2e/tests/happy_path.rs::test_list_clients_after_connect`
- [X] T027 [P] [US1] E2E test covering all 4 acceptance scenarios of US1 — `crates/forward-e2e/tests/happy_path.rs::test_user_story_1_acceptance`

### Implementation for User Story 1

- [X] T028 [US1] In `crates/forward-server/src/operator/cli.rs`, implement `provision_client(name)` handler: validate name → call `FileTokenStore::issue` → write `<out>.bundle.json` (mode 0600) with `server_endpoint`, `server_cert_sha256`, `client_name`, plaintext `token`; emit `audit.provision` log
- [X] T029 [US1] In the same file, implement `revoke(name)` handler: call `FileTokenStore::revoke`; if client currently connected, fire its `cancel_token`; emit `audit.revoke` log
- [X] T030 [US1] In the same file, implement `list_clients()` handler that joins `FileTokenStore` entries with `ConnectedClients` map and renders text/json per `contracts/operator-api.md`
- [X] T031 [US1] Create `crates/forward-server/src/operator/http.rs` with `axum::Router` exposing `POST /v1/clients`, `POST /v1/clients/{name}/revoke`, `GET /v1/clients`; wire to the same handlers as the CLI; bind only to `operator_http_listen`. Add an inline test asserting `listener.local_addr()?.ip().is_loopback()` — satisfies FR-022 verification
- [X] T032 [US1] Create `crates/forward-server/src/grpc/service.rs` implementing the Tonic `Control` service generated by `forward-proto`. Implement `Channel`: receive `Hello` (optional), reply with `Welcome { server_version, server_time_unix_ms }`, then enter the bidirectional pump (rules-out / status-in handled in US2)
- [X] T033 [US1] Create `crates/forward-server/src/grpc/interceptor.rs`: `tonic` interceptor that reads `authorization` metadata, strips `Bearer `, calls `Authenticator::verify`, inserts the resulting `ClientIdentity` into request extensions; on failure returns `Status::unauthenticated(reason)`
- [X] T034 [US1] Create `crates/forward-server/src/clients.rs` with `ConnectedClients = Arc<RwLock<HashMap<ClientName, ConnectedClient>>>`; service handler in T032 calls `register(client_name, remote_addr, cancel_token, outbound_tx)` on stream open and `unregister(client_name)` on stream close; emit `client.connected` / `client.disconnected` audit logs
- [X] T035 [US1] In `crates/forward-server/src/main.rs`, wire the `serve` subcommand: load config → load `FileTokenStore` → build `rustls::ServerConfig` from `tls.rs` → start Tonic server with the interceptor + service → start operator HTTP server → start metrics HTTP server (added by T063 in US3 — placeholder no-op until then, but the bind point and shutdown wiring are reserved here) → await `Shutdown::signal_handler`
- [X] T036 [US1] In `crates/forward-client/src/control.rs`, implement `connect_once(bundle, ctx) -> Result<Channel, Error>`: build `tokio_rustls::TlsConnector` with `PinnedVerifier`, connect to `server_endpoint`, attach `authorization: Bearer <token>` metadata, open `Channel` bidi stream, send `Hello`, await `Welcome`
- [X] T037 [US1] In the same file, implement `run_with_reconnect(bundle, ctx)`: full-jitter exponential backoff (`base 500ms`, factor 2, cap 30s — Decision 10), reset on success, exit on `cancel_token` or unrecoverable error (e.g., `token_revoked`); structured logs `control.connecting`, `control.tls_pinned`, `control.connected`, `control.tls_pinned_mismatch`, `auth.failure`
- [X] T038 [US1] In `crates/forward-client/src/main.rs`, wire the binary: load bundle → build TLS pinned verifier → `run_with_reconnect` → await shutdown
- [X] T038a [P] [US1] Scale test: spin up an in-process server + 100 in-process clients (each with its own provisioned bundle), assert all 100 reach `connected` state and `list-clients --format json` returns the full set in <1s — `crates/forward-e2e/tests/scale.rs::test_100_clients_connected_within_one_second` (covers SC-004a)

**Checkpoint**: User Story 1 fully functional and demoable. SC-003 (auth accept/reject) passes via T022–T024 + T027; SC-004a (100-client scale) passes via T038a.

---

## Phase 4: User Story 2 — Push a TCP Forwarding Rule and Verify Traffic (Priority: P2)

**Goal**: With a connected client, the operator pushes one TCP rule and a real 100 MB stream traverses it byte-for-byte.

**Independent Test**: Quickstart Steps 4–6 produce a matching SHA-256 between source and destination of a 100 MB transfer (SC-002).

### Tests for User Story 2 (write FIRST, observe FAILING)

- [X] T039 [P] [US2] Contract test: `push-rule edge-01 18080 ...` while `edge-01` is disconnected returns exit 4 / HTTP 422 with `client_not_connected` — `crates/forward-e2e/tests/rule_failure_lifecycle.rs::test_push_to_disconnected_client`
- [X] T040 [P] [US2] Contract test: pushing a rule whose listen port is already bound on the client puts the rule into `Failed(port_in_use)`; a second `push-rule` for the same port also returns `port_in_use` until `remove-rule` is called — `crates/forward-e2e/tests/rule_failure_lifecycle.rs::test_failed_blocks_port_reuse`
- [X] T041 [P] [US2] Integration test: end-to-end 100 MB stream through a forwarding rule arrives byte-equal — `crates/forward-client/tests/tcp_forward_loopback.rs::test_100mb_byte_equal`
- [X] T042 [P] [US2] Integration test: after `remove-rule`, the listener stops accepting new connections within 1 second; in-flight connections drain — `crates/forward-client/tests/tcp_forward_loopback.rs::test_remove_drains`
- [X] T043 [P] [US2] E2E test covering all 5 acceptance scenarios of US2, **including an explicit `assert!(activation_latency < Duration::from_secs(1))` check on the push→Active wall time** — covers FR-012's 1-second activation bound — `crates/forward-e2e/tests/happy_path.rs::test_user_story_2_acceptance`

### Implementation for User Story 2

- [X] T044 [US2] Create `crates/forward-server/src/rules.rs` implementing `ServerRuleStore`: `Arc<RwLock<HashMap<RuleId, Rule>>>` + secondary index `HashMap<(ClientName, u16), RuleId>`; methods `push(client, listen_port, target_host, target_port) -> Result<RuleId, ForwardError>` (returns `PortInUse` if `(client, port)` already exists in `Active` or `Failed`), `mark_active(rule_id)`, `mark_failed(rule_id, reason)`, `remove(rule_id)`, `get(rule_id)`, `list(filter)`; unit tests for state-machine transitions per `data-model.md`
- [X] T045 [US2] In `crates/forward-server/src/operator/cli.rs`, add `push_rule`, `remove_rule`, `list_rules` handlers per `contracts/operator-api.md` exit-code spec
- [X] T046 [US2] In `crates/forward-server/src/operator/http.rs`, add the corresponding routes (`POST /v1/rules`, `DELETE /v1/rules/{id}`, `GET /v1/rules`)
- [X] T047 [US2] In `crates/forward-server/src/grpc/service.rs`, hook `push-rule` into `outbound_tx` so the connected client receives `ServerMessage::RuleUpdate { action: PUSH, rule, request_id }` within ~1ms; await `RuleStatus` from the client (with `--ack-timeout`, default 2s) and translate to operator response
- [X] T048 [US2] In the same file, handle inbound `ClientMessage::RuleStatus`: `ACTIVATED` → `ServerRuleStore::mark_active`; `FAILED` → `mark_failed(reason)`; emit `rule.activated` / `rule.failed` audit logs with `request_id`
- [X] T049 [US2] In `crates/forward-client/src/control.rs`, on receiving `RuleUpdate { PUSH, rule }`, dispatch to forwarder; on `REMOVE`, fire that rule's per-rule `cancel_token`
- [X] T050 [US2] Create `crates/forward-client/src/forwarder/proxy.rs` with `pub async fn proxy(inbound: TcpStream, target: (String, u16), shutdown: CancellationToken) -> std::io::Result<(u64, u64)>` using `tokio::io::copy_bidirectional` wrapped in `select!` against the shutdown token
- [X] T051 [US2] Create `crates/forward-client/src/forwarder/mod.rs` implementing per-rule listener: `pub async fn run(rule: Rule, status_tx: Sender<RuleStatus>, shutdown: CancellationToken)`. Bind `TcpListener` (handle `port_in_use` and `permission_denied` → emit `RuleStatus::FAILED`); accept loop spawns `proxy()` for each connection; respects shutdown token
- [X] T052 [US2] In `crates/forward-client/src/forwarder/mod.rs`, implement drain semantics for rule removal: cancel listener immediately (stops accept within 1s — FR-014/FR-016), then `JoinSet::join_all` in-flight proxies up to `shutdown_drain_timeout_secs`
- [X] T053 [US2] In `crates/forward-client/src/control.rs`, ensure `RuleStatus` messages are sent back via the existing bidi stream's outbound channel (created in US1 T036)
- [X] T054 [US2] Wire all rule-lifecycle structured logs (server: `audit.rule_push`, `audit.rule_remove`, `rule.activated`, `rule.failed`, `rule.removed`; client: `rule.received`, `rule.activated`, `rule.failed`, `rule.removed`); each carries `request_id`, `client_name`, `rule_id`
- [X] T054a [P] [US2] Author criterion benchmark harness in `crates/forward-client/benches/data_plane.rs` measuring single-rule loopback throughput and p99 added latency — Constitution Principle II requires this to ship with the first hot-path-touching change (US2). No regression threshold yet (no baseline); Phase 6 task T065 extends this with baseline capture
- [X] T054b [P] [US2] Concurrency integration test: 5 rules on a single client, each with 100 concurrent forwarded TCP connections sustained for 30 s, assert no dropped or corrupted connections — `crates/forward-client/tests/tcp_forward_concurrency.rs::test_5_rules_100_conns_sustained` (covers SC-004)
- [X] T054c [P] [US2] Restart-recovery integration test: provision client, push rule, transfer some bytes, kill the client process, restart it with the same bundle, re-push the same rule, assert that forwarding resumes within 5 s of the re-push call — `crates/forward-e2e/tests/restart_recovery.rs::test_repush_after_client_restart_resumes_within_5s` (covers SC-005)

**Checkpoint**: User Stories 1 + 2 both pass independently. SC-002 verified by T041; SC-004 by T054b; SC-005 by T054c; Q4 lifecycle by T040; FR-012 1-s activation by T043; Constitution II benchmark harness in place via T054a.

---

## Phase 5: User Story 3 — Observe Activity via Structured Logs and Per-Rule Stats (Priority: P3)

**Goal**: Operator can query per-rule byte counts and active connections; Prometheus endpoint exposes the same; structured logs are machine-parseable.

**Independent Test**: Quickstart Step 6 `rule-stats` returns `bytes_in` within ±1KB of actual transferred bytes (SC-007); `curl http://127.0.0.1:7081/metrics | grep forward_rule` shows the matching counter.

### Tests for User Story 3 (write FIRST, observe FAILING)

- [X] T055 [P] [US3] Integration test: per-rule `bytes_in`/`bytes_out` from `rule-stats` is within ±1KB of actual after 5s settle — `crates/forward-e2e/tests/observability.rs::test_stats_within_tolerance`
- [X] T056 [P] [US3] Integration test: `GET /metrics` returns valid Prometheus text including `forward_rule_bytes_in_total`, `forward_rule_bytes_out_total`, `forward_rule_active_connections`, `forward_clients_connected`, `forward_auth_failures_total` — `crates/forward-e2e/tests/observability.rs::test_prometheus_endpoint`
- [X] T057 [P] [US3] E2E test covering all 3 acceptance scenarios of US3, including log-shape assertions (every event has timestamp / event / client_name / rule_id where applicable) — `crates/forward-e2e/tests/observability.rs::test_user_story_3_acceptance`

### Implementation for User Story 3

- [X] T058 [US3] Create `crates/forward-client/src/forwarder/stats.rs` with `pub struct RuleStats { bytes_in: AtomicU64, bytes_out: AtomicU64, active_connections: AtomicU32 }`; modify `proxy()` from T050 to increment counters on accept/close and after `copy_bidirectional` returns
- [X] T059 [US3] In `crates/forward-client/src/control.rs`, spawn a periodic task (interval = `stats_report_interval_secs`, default 5s) that walks all active rules and sends `ClientMessage::StatsReport { stats: [...] }` over the bidi stream
- [X] T060 [US3] In `crates/forward-server/src/grpc/service.rs`, on inbound `StatsReport`, update a `RuleStatsCache: Arc<RwLock<HashMap<RuleId, RuleStats>>>` and feed deltas into the Prometheus collectors (T062)
- [X] T061 [US3] In `crates/forward-server/src/operator/cli.rs` and `http.rs`, implement `rule-stats <rule_id>` returning current cache values
- [X] T062 [US3] Create `crates/forward-server/src/metrics.rs` registering Prometheus collectors per Decision 9: `forward_clients_connected` (gauge), `forward_rule_bytes_in_total{client,rule}` (counter), `forward_rule_bytes_out_total{client,rule}` (counter), `forward_rule_active_connections{client,rule}` (gauge), `forward_auth_failures_total{reason}` (counter)
- [X] T063 [US3] Spawn an axum service on `metrics_listen` (default `127.0.0.1:7081`) serving `GET /metrics` via `prometheus::TextEncoder`; wire shutdown token; replace the placeholder reserved in T035 with this real handler. Add an inline test asserting `listener.local_addr()?.ip().is_loopback()` — second half of FR-022 verification
- [X] T064 [US3] Audit every `tracing::info!`/`warn!`/`error!` call across both binaries to ensure: (a) JSON layer is enabled by default, (b) correlation `request_id` is propagated through `RuleUpdate`/`RuleStatus` and into client logs, (c) the redaction layer elides any field whose name matches `token|secret|private_key`. Add a tiny `tracing_layer` test confirming redaction

**Checkpoint**: All three user stories pass independently. SC-006 + SC-007 verified.

---

## Phase 6: Polish & Cross-Cutting Concerns

- [X] T065 [P] Extend the criterion benchmark harness from T054a: capture a baseline measurement file (`benches/baselines/v0.1.0.json`) and document the numbers in `CHANGELOG.md` so the next hot-path-touching spec can wire a CI regression gate against this baseline
- [X] T066 [P] Write `README.md` at repo root: install, basic flow, link to `quickstart.md`
- [X] T067 [P] Create `CHANGELOG.md` with the v0.1.0 entry summarising MVP scope
- [ ] T068 Run `quickstart.md` end-to-end on two real Linux hosts; capture wall-clock time for SC-001 (target < 5 minutes) — **manual; pending operator dry-run on real Linux pair**
- [X] T069 Workspace lint pass: `cargo clippy --workspace --all-targets -- -D warnings`; fix all findings (no `allow` annotations without inline justification)
- [X] T070 Security review pass: grep production code for `token|secret|key` to confirm none reach `tracing::*` or `println!`; verify `tokens.json` and `server.key` are written with mode 0600; verify `Authenticator` is the only path to identity (no shortcut `if client_name == "..."` in handlers)
- [X] T071 Constitution Check final verification: confirm Principle I (TLS+token only, no mTLS code in tree, single auth seam in `forward-auth`), Principle II (criterion bench exists), Principle III (every PR's tests written test-first per git history), Principle IV (Prometheus endpoint live, logs JSON, drain on shutdown verified by T042), Principle V (`ClientIdentity` carried through every handler — no global identity assumptions)

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: no deps; T002–T006 fully parallel after T001
- **Foundational (Phase 2)**: depends on Setup; **blocks all user stories**
- **User Stories (Phases 3–5)**: each depends on Foundational (Phase 2); can be worked on in parallel by separate developers but must be merged in order P1 → P2 → P3 because P2 builds on US1's connection lifecycle and P3 builds on US2's rule lifecycle
- **Polish (Phase 6)**: depends on all desired user stories complete

### User Story Dependencies (sequential merge order)

- **US1**: depends only on Foundational. Standalone MVP.
- **US2**: builds on US1's `Channel` stream and `ConnectedClients` map. Requires US1 merged.
- **US3**: builds on US2's `Rule` and stats reporting; requires US2 merged.

### Within Each User Story

- Tests (T022–T027 / T039–T043 / T055–T057) MUST be written and observed FAILING before the corresponding implementation tasks. **Constitution III is non-negotiable.**
- Within Implementation: server-side store/types → server-side handlers → client-side handlers → wiring in `main.rs`

### Parallel Opportunities

- All Setup `[P]` tasks (T002, T004, T005, T006) can run concurrently after T001
- All Foundational `[P]` tasks can run concurrently after their declared deps (T008/T009/T010 after T007; T012/T014 after T011; T017/T018/T019/T020 after T011 + T015 + T016)
- All US1 tests (T022–T027) can be authored in parallel — different test files
- All US2 tests (T039–T043), all US3 tests (T055–T057) likewise
- Within US1 implementation: T028–T030 (CLI handlers) after T013; T031 after T028–T030; T032 after T011 + T034; T036–T037 after T018; T038 after T036–T037; T038a (100-client scale test) after T035 + T038
- Within US2 implementation: T044 first, then T045–T049 in parallel; T050–T052 sequential; T053–T054 last; T054a (criterion harness) and T054b (5×100 concurrency) after T050; T054c (restart recovery) after T052
- Within US3 implementation: T058 first; T059–T060 then; T062 in parallel with T060; T061 + T063 last

---

## Parallel Example: User Story 1

```bash
# Tests in parallel (different files):
Task: "Contract test for missing-token rejection in crates/forward-e2e/tests/auth_failures.rs"
Task: "Contract test for revoked-token rejection in crates/forward-e2e/tests/auth_failures.rs (separate fn)"
Task: "Contract test for pin-mismatch rejection in crates/forward-e2e/tests/auth_failures.rs (separate fn)"
Task: "Integration test for provisioning idempotency in crates/forward-server/tests/provisioning.rs"
Task: "Integration test for list-clients in crates/forward-e2e/tests/happy_path.rs"
Task: "E2E acceptance test in crates/forward-e2e/tests/happy_path.rs"

# Foundational implementations in parallel:
Task: "ID newtypes in crates/forward-core/src/id.rs"
Task: "Fingerprint helpers in crates/forward-core/src/fingerprint.rs"
Task: "Config types in crates/forward-core/src/config.rs"
Task: "Token gen/hash in crates/forward-auth/src/token.rs"
```

---

## Implementation Strategy

### MVP First (User Story 1 only)

1. Phase 1: Setup
2. Phase 2: Foundational (CRITICAL — blocks all stories)
3. Phase 3: US1
4. **STOP and VALIDATE**: run T027 e2e test; manually walk Quickstart Steps 1–3
5. Cut a `v0.0.1-mvp-trust` tag

### Incremental delivery

- US1 → demo "trusted connection works" → tag v0.0.1
- + US2 → demo "100 MB byte-equal forward" → tag v0.0.2 (data-plane proven)
- + US3 → demo "Prometheus + JSON logs + per-rule stats" → tag v0.1.0 (MVP complete)
- Then Phase 6: bench + docs + final security review → release

### Parallel team strategy

After Phase 2 completes:
- Dev A owns US1 + the contract tests in `forward-e2e`
- Dev B owns US2 + data-plane tests in `forward-client`
- Dev C owns US3 + observability tests
- Merge order is still sequential (US1 first) because of inter-story dependencies, but development can overlap — Dev B can implement US2 against a mocked server stream until US1 lands.

---

## Notes

- `[P]` tasks operate on different files OR the same file's distinct test functions (Cargo runs `tests/*.rs` in parallel processes by default).
- Every task in a user-story phase carries the `[USx]` label for traceability back to spec.md.
- TDD discipline (Constitution III): a task without a preceding failing test is a bug in the workflow, not the code.
- The `forward-auth` crate is the **only** place auth logic lives. Any task that introduces auth-flavoured logic outside it (e.g., a CLI that reads tokens directly) is wrong by Constitution Principle I.
- After every task or small batch, commit. The optional `/speckit-git-commit` hook is registered for `before_implement` and `after_implement` to make this easy.

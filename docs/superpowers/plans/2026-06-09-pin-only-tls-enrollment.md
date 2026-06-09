# Pin-only TLS Enrollment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the SHA-256 pin the single source of TLS trust for both client enrollment and the runtime control plane, dropping the embedded server certificate from the enrollment URI and the `CredentialBundle`.

**Architecture:** A new shared `PinnedCertVerifier` (rustls `ServerCertVerifier`) enforces `sha256(DER(leaf)) == pin` at the handshake layer. Both `enroll.rs` and `control.rs` dial through it via tonic 0.14's `Endpoint::tls_config_with_verifier`. The certificate PEM is removed from `proto`, the client bundle struct, the URI, and the enrollment RPC response.

**Tech Stack:** Rust 2024, tonic 0.14 (`tls-aws-lc`), rustls 0.23 (aws-lc-rs), `portunus_core::fingerprint`.

Design: `docs/superpowers/specs/2026-06-09-pin-only-tls-enrollment-design.md`.

---

### Task 1: `PinnedCertVerifier` shared TLS helper

**Files:**
- Create: `crates/portunus-client/src/tls.rs`
- Modify: `crates/portunus-client/src/main.rs` (add `mod tls;`)
- Test: inline `#[cfg(test)]` in `tls.rs`

- [ ] **Step 1: Write failing tests** — accept matching DER, reject mismatched DER. Generate a throwaway self-signed cert in-test (reuse the same cert-gen path the server tests use, or `rcgen` if already a dev-dep; otherwise hand a known DER + its sha256). Assert `verify_server_cert` returns `Ok` for the matching pin and `Err(InvalidCertificate(..))` for a different fingerprint.
- [ ] **Step 2: Run, expect FAIL** (`PinnedCertVerifier` undefined).
- [ ] **Step 3: Implement** `PinnedCertVerifier { expected_sha256, provider }` per the design doc, plus `pub fn new(pin: &str) -> Result<Self, TlsError>` (validate 64-hex) and `pub fn pinned_endpoint(endpoint: &str, pin: &str) -> Result<Endpoint, TlsError>` returning `Endpoint::from_shared(format!("https://{endpoint}"))?.tls_config_with_verifier(ClientTlsConfig::new(), Arc::new(verifier))?`.
- [ ] **Step 4: Run, expect PASS.**
- [ ] **Step 5: Commit** `feat(client): add PinnedCertVerifier for fingerprint-pinned TLS`.

Notes: signature methods delegate to `rustls::crypto::verify_tls12_signature` / `verify_tls13_signature` using `self.provider.signature_verification_algorithms`. Provider from `rustls::crypto::CryptoProvider::get_default()` (installed in `main.rs`), falling back to `aws_lc_rs::default_provider()` if unset (test context).

### Task 2: Remove `server_cert_pem` from proto

**Files:**
- Modify: `proto/portunus.proto` (`CredentialBundle`)
- Test: `crates/portunus-proto/tests/enrollment_wire_compat.rs`

- [ ] **Step 1:** Update the wire-compat test to build/decode a `CredentialBundle` with no `server_cert_pem` (only `version, client_name, server_endpoint, server_cert_sha256, token, client_id`).
- [ ] **Step 2: Run, expect FAIL/compile error** (field still present).
- [ ] **Step 3:** In `portunus.proto`, remove `string server_cert_pem = 5;`, add `reserved 5;` and `reserved "server_cert_pem";` inside `CredentialBundle`. (build.rs regenerates on next build.)
- [ ] **Step 4: Run** `cargo test -p portunus-proto`, expect PASS.
- [ ] **Step 5: Commit** `refactor(proto): drop server_cert_pem from CredentialBundle`.

### Task 3: Drop `server_cert_pem` from the client bundle struct

**Files:**
- Modify: `crates/portunus-client/src/bundle.rs`
- Test: inline tests in `bundle.rs`

- [ ] **Step 1:** Add/adjust test: a bundle JSON without `server_cert_pem` round-trips through `read_from`/`write_to`; a malformed (`non-64-hex`) pin is rejected; remove any test asserting `verify_pin_consistency` against a PEM.
- [ ] **Step 2: Run, expect FAIL** (field/param mismatch).
- [ ] **Step 3:** Remove `server_cert_pem` field; remove `verify_pin_consistency` and its `leaf_der_from_pem` call; drop the `server_cert_pem` parameter from `from_enrollment`. In `read_from`, validate the pin is 64 ascii-hex (replace the old consistency check). Keep `server_cert_sha256`.
- [ ] **Step 4: Run** `cargo test -p portunus-client --lib bundle`, expect PASS.
- [ ] **Step 5: Commit** `refactor(client): bundle keeps pin only, no cert PEM`.

### Task 4: Pin-only enrollment URI + dial

**Files:**
- Modify: `crates/portunus-client/src/enroll.rs`
- Test: inline tests in `enroll.rs`

- [ ] **Step 1:** Update `EnrollmentUri` tests: `parse` accepts `portunus://h:7443/enroll?pin=sha256:<64hex>&code=X` (no `cert`), still rejects missing `pin`/`code`, ignores stray keys. Remove `cert`-roundtrip/`BadCert` tests.
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3:** Remove `server_cert_pem` from `EnrollmentUri`; delete `cert` parsing branch and `BadCert` variant. In `enroll()`, replace the `Certificate::from_pem`/`ClientTlsConfig::ca_certificate` block with `crate::tls::pinned_endpoint(&parsed.endpoint, &parsed.pin_sha256)?`. Drop the `server_cert_pem` arg when building the final bundle from the RPC response.
- [ ] **Step 4: Run** `cargo test -p portunus-client --lib enroll`, expect PASS.
- [ ] **Step 5: Commit** `refactor(client): pin-only enrollment URI and dial`.

### Task 5: Runtime control-plane dials via the pinned verifier

**Files:**
- Modify: `crates/portunus-client/src/control.rs:65-78`

- [ ] **Step 1:** Replace the `Certificate::from_pem(bundle.server_cert_pem)` + `ca_certificate` + `domain_name` block in `connect_once` with `crate::tls::pinned_endpoint(&bundle.server_endpoint, &bundle.server_cert_sha256)?` (map error to `ControlError::Tls`). Update the pinning-model comment to describe fingerprint pinning. Remove now-unused `Certificate`/`ClientTlsConfig` imports if dead.
- [ ] **Step 2: Run** `cargo build -p portunus-client`, expect clean.
- [ ] **Step 3: Commit** `refactor(client): runtime dial uses PinnedCertVerifier`.

### Task 6: Server stops emitting the certificate

**Files:**
- Modify: `crates/portunus-server/src/operator/cli.rs` (`enrollment_uri`)
- Modify: `crates/portunus-server/src/grpc/enrollment.rs` (RPC response + URI comment)

- [ ] **Step 1:** Update/curate server tests that assert the URI contains `cert=` to assert it does NOT, and that it contains `pin=sha256:` and `code=`.
- [ ] **Step 2: Run, expect FAIL.**
- [ ] **Step 3:** In `enrollment_uri`, drop the base64 cert and the `&cert={}` from the format string (now `portunus://{}/enroll?pin=sha256:{}&code={}`). In `grpc/enrollment.rs`, stop setting `server_cert_pem` on the `CredentialBundle` response and fix the `// uri: ...&cert=...` comment.
- [ ] **Step 4: Run** `PORTUNUS_SKIP_WEBUI=1 cargo test -p portunus-server`, expect PASS.
- [ ] **Step 5: Commit** `refactor(server): omit server cert from enrollment URI and bundle`.

### Task 7: Workspace verification + e2e

**Files:** any straggler test fixtures (`portunus-e2e`).

- [ ] **Step 1:** `cargo build --workspace` — fix any remaining `server_cert_pem` references.
- [ ] **Step 2:** `PORTUNUS_SKIP_WEBUI=1 cargo test --workspace` — expect green; update e2e enrollment fixtures if they hand-build a bundle/URI with a cert.
- [ ] **Step 3:** `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- [ ] **Step 4:** `cargo fmt --all`.
- [ ] **Step 5: Commit** `test: green workspace for pin-only enrollment`.

### Task 8: Manual smoke (optional but recommended)

- [ ] `make demo DEMO_ARGS="--no-wait"` (the adapted harness uses the live enroll flow) — confirm edges connect via the now-short URI and forwarding PASSes. Note: this requires the demo.sh v2.0 fix from branch `fix/demo-v2-client-id-flow`; smoke only if that is merged/cherry-picked.

---

## Self-Review

- **Spec coverage:** URI cert removal (T4,T6), proto field (T2), bundle field (T3), shared verifier (T1), runtime dial (T5), server response (T6), tests (T7). All design "Files touched" rows mapped. ✔
- **Placeholders:** none — each task names exact files, the verifier code lives in the design doc referenced from T1, and commands have expected results. ✔
- **Type consistency:** `pinned_endpoint(endpoint, pin) -> Result<Endpoint, TlsError>` defined in T1, consumed identically in T4/T5; `server_cert_sha256` is the surviving field name across proto/bundle. ✔

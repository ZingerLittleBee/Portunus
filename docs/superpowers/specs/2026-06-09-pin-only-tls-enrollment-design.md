# Pin-only TLS enrollment & runtime trust

**Status:** Approved (design phase) · **Date:** 2026-06-09 ·
**Branch:** `feat/pin-only-tls-enrollment`

## Problem

The client enrollment command is enormous. The enrollment URI embeds the
server's **entire TLS certificate** as a base64 blob:

```
portunus://localhost:7443/enroll?pin=sha256:<64-hex>&code=<code>&cert=<~1.5 KB base64 PEM>
```

The `cert=` parameter alone is ~90% of the command, producing the
wall-of-text the operator must copy/paste (`接入客户端` dialog, step 2).

The URI already carries `pin=sha256:<fingerprint>`. The pin is the
SHA-256 of the certificate DER — a cryptographic commitment to the exact
same certificate. Shipping **both** the full cert and its fingerprint is
redundant: the pin alone is sufficient to establish trust.

## Goal

Make the pin the **single source of trust** everywhere a client talks to
the server, and drop the certificate PEM from the wire and from on-disk
state:

- enrollment URI: `cert=` removed → command shrinks ~90%.
- `CredentialBundle` (proto + client struct): `server_cert_pem` removed.
- A shared `PinnedCertVerifier` enforces `sha256(DER(leaf)) == pin` at the
  TLS handshake layer, used by **both** the enrollment dial and the
  runtime control-plane dial.

## Non-goals

- **Backward compatibility.** This is a v2.0 development-phase change; old
  bundles / old URIs that still carry `cert=` are not supported. The proto
  field is removed (with a `reserved` marker), not deprecated.
- Changing the pin algorithm. It stays `sha256(DER(server leaf cert))`,
  lowercase hex — identical to today's `portunus_core::fingerprint`.
- Certificate rotation / multi-pin. Enrollments are single-use and
  short-TTL; a pin commits to one cert, exactly as the embedded cert does
  today.

## Security argument

Trust today rests on the enrollment URI being delivered over a trusted
channel — that premise is unchanged. Within that premise:

- **Pin ≡ cert.** SHA-256 is collision-resistant; an attacker cannot
  present a different certificate with the same fingerprint. "Trust this
  cert" and "trust this cert's SHA-256" are cryptographically equivalent.
  This is the SSH `known_hosts` / HPKP model.
- **MITM resistance is unchanged.** The verifier rejects at the handshake
  layer when `sha256(leaf) != pin`, so the enrollment `code` and the
  issued bearer token are never transmitted to an impostor server.
- **The one implementation risk** is writing the custom
  `ServerCertVerifier` correctly. This is covered by tests that assert a
  mismatched pin aborts the connection (see Testing).

Net: equivalent protocol-level security, with less material on the wire
and on disk.

## Current state (what changes)

| Location | Today | After |
| --- | --- | --- |
| `proto/portunus.proto` `CredentialBundle` | `server_cert_pem = 5` | field removed, `reserved 5;` |
| `crates/portunus-client/src/bundle.rs` | `server_cert_pem` field + `verify_pin_consistency` | both removed; pin is authoritative |
| `crates/portunus-client/src/enroll.rs` | `EnrollmentUri.server_cert_pem`, parses `cert=`, dials with `ca_certificate(cert)` | no `cert`; dials via `PinnedCertVerifier` |
| `crates/portunus-client/src/control.rs` `connect_once` | `Certificate::from_pem(bundle.cert)` + `ca_certificate` | `PinnedCertVerifier::new(bundle.pin)` |
| `crates/portunus-server/src/operator/cli.rs` `enrollment_uri` | appends `&cert=<base64 PEM>` | omits `cert` |
| `crates/portunus-server/src/grpc/enrollment.rs` | sets `server_cert_pem` on response | leaves it unset / not present |

The server keeps its own certificate (`AppState.server_cert_pem` is still
used for SAN checks and serving TLS); it just stops *handing it out*.

## Design

### Component: `PinnedCertVerifier` (new)

New module `crates/portunus-client/src/tls.rs`. A small unit with one
job: accept exactly the certificate whose DER SHA-256 equals the expected
pin.

```rust
#[derive(Debug)]
pub struct PinnedCertVerifier {
    expected_sha256: String,                  // lowercase hex, 64 chars
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for PinnedCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let got = portunus_core::fingerprint::sha256_hex(end_entity.as_ref());
        if got.eq_ignore_ascii_case(&self.expected_sha256) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    // Signature checks delegate to the active crypto provider — we are
    // pinning the cert, not bypassing TLS.
    fn verify_tls12_signature(/* … */) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message, cert, dss, &self.provider.signature_verification_algorithms)
    }
    fn verify_tls13_signature(/* … */) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message, cert, dss, &self.provider.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}
```

- **What it does:** pins one cert by fingerprint; ignores hostname/CA
  chain (matching today's "self-signed cert is the only CA" behavior).
- **How you use it:** `PinnedCertVerifier::new(pin_hex)`; pass
  `Arc<dyn ServerCertVerifier>` to tonic.
- **Depends on:** `rustls`, the installed `aws_lc_rs` crypto provider
  (already installed in `main.rs`), and `portunus_core::fingerprint`.

### Shared dial helper

Both call sites build the channel the same way, via tonic 0.14's
`Endpoint::tls_config_with_verifier` (`_tls-any` feature, covered by the
existing `tls-aws-lc` feature):

```rust
let verifier = Arc::new(PinnedCertVerifier::new(pin)?);
let endpoint = Endpoint::from_shared(format!("https://{endpoint}"))?
    .tls_config_with_verifier(ClientTlsConfig::new(), verifier)?; // no ca_certificate / domain_name
```

`ClientTlsConfig::new()` must be left bare — per tonic's docs, methods
that configure the *default* verifier (`ca_certificate`, `domain_name`)
must not be combined with `tls_config_with_verifier`. SNI still derives
from the URI host; the verifier ignores it, so an IP endpoint is fine.

A thin helper (e.g. `crate::tls::pinned_endpoint(endpoint, pin)`) returns
the configured `Endpoint`, keeping `enroll.rs` and `control.rs` identical
at the dial seam.

### Wire / format changes

**Enrollment URI** (server `enrollment_uri`):
```
portunus://<endpoint>/enroll?pin=sha256:<64-hex>&code=<code>
```

**`EnrollmentUri::parse`** (client): drop the `cert` field and its
`BadCert` error variant; an unknown query key is still ignored (forward-
compat). `pin` and `code` remain required.

**Proto `CredentialBundle`:** remove `server_cert_pem = 5`, add
`reserved 5;` and `reserved "server_cert_pem";`. Regenerate via
`tonic-prost-build`.

**Client `CredentialBundle` struct:** remove `server_cert_pem`; remove
`verify_pin_consistency` (nothing to cross-check — the pin is the
authority). `read_from` simply validates the pin is well-formed
(64-hex). `from_enrollment` loses the `server_cert_pem` parameter.

### Data flow (unchanged shape, lighter payload)

1. Operator: `POST /v1/client-enrollments` → server returns
   `{client_name, expires_at, command, uri}` with the short URI.
2. Edge: `portunus-client enroll '<short-uri>'` → dials with
   `PinnedCertVerifier(pin from URI)`, calls `Enroll(code)`.
3. Server returns `CredentialBundle{client_name, client_id,
   server_endpoint, server_cert_sha256, token}` (no PEM).
4. Client writes the bundle (pin, no PEM).
5. Runtime: `connect_once` dials with `PinnedCertVerifier(bundle.pin)`.

### Error handling

- Pin mismatch (enroll or runtime): handshake fails →
  `ControlError::Tls` / `EnrollError::Transport`. The `code`/token are
  never sent. Message names "pin mismatch" for operator clarity.
- Malformed pin in URI/bundle: rejected at parse/load with a clear error
  (existing `BadPin` / a new bundle validation error).

## Testing

- **Unit (`tls.rs`):** `PinnedCertVerifier` accepts the matching cert DER,
  rejects a different cert DER (`verify_server_cert` returns
  `InvalidCertificate`).
- **Unit (`enroll.rs`):** `EnrollmentUri::parse` round-trips
  `pin`+`code`, rejects missing `pin`/`code`, ignores stray keys, and no
  longer requires/parses `cert`.
- **Unit (`bundle.rs`):** bundle (de)serializes without `server_cert_pem`;
  malformed pin rejected.
- **Proto wire test:** `tests/enrollment_wire_compat.rs` updated to the
  pin-only `CredentialBundle`.
- **E2E:** existing enrollment → connect flow passes end to end with the
  short URI (the `make demo` harness exercises this in practice).
- **Negative:** a tampered pin (point client at a server with a different
  cert) fails the handshake and never enrolls.

## Files touched

- `proto/portunus.proto`
- `crates/portunus-client/src/{tls.rs (new), enroll.rs, control.rs, bundle.rs, main.rs (mod tls)}`
- `crates/portunus-server/src/operator/cli.rs`
- `crates/portunus-server/src/grpc/enrollment.rs`
- Tests across `portunus-proto`, `portunus-client`, `portunus-server`, `portunus-e2e`
- Doc/comment touch-ups (`grpc/enrollment.rs` URI comment, any cert= references)

No Web UI change: the `接入客户端` dialog renders the server-provided
`command`/`uri` verbatim, so it shrinks automatically.

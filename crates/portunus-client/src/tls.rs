//! Fingerprint-pinned TLS for talking to the control plane.
//!
//! The client trusts exactly one server certificate, identified by the
//! SHA-256 of its DER encoding (the "pin"). This is the single source of
//! TLS trust for both enrollment and the runtime control stream — no CA
//! chain, no hostname check, no embedded certificate PEM. It mirrors the
//! SSH `known_hosts` model: the pin commits to one certificate, and
//! SHA-256 collision resistance makes pinning the fingerprint as strong as
//! shipping the whole certificate.

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, SignatureScheme};
use thiserror::Error;
use tonic::transport::{ClientTlsConfig, Endpoint};

#[derive(Debug, Error)]
pub enum TlsError {
    #[error("invalid pin: expected 64 hex chars")]
    BadPin,
    #[error("transport: {0}")]
    Transport(String),
}

/// Stable, greppable message surfaced when the server's leaf certificate
/// fingerprint does not match the pinned value. It is carried as a rustls
/// `CertificateError::Other` so the message flows through tonic's transport
/// error chain into the client's `control.connect_failed` log line, letting
/// tests assert on the *cause* (pin mismatch) rather than the generic
/// connect-failed event name.
pub(crate) const PIN_MISMATCH_MESSAGE: &str =
    "server certificate fingerprint does not match the pinned value";

/// Error type whose `Display` is the stable [`PIN_MISMATCH_MESSAGE`].
///
/// The message is held as a field (rather than a unit struct) so it surfaces
/// in BOTH the `Display` and the derived `Debug` rendering. `rustls::Error`'s
/// top-level `std::error::Error::source()` is an empty impl, so the inner
/// cause is NOT reachable via the source chain — but `rustls::Error`'s
/// `Display`/`Debug` embed the `CertificateError::Other(OtherError(_))`
/// Debug form, which renders this struct's field. The client's
/// `control.connect_failed` log line includes `error=%e` over a
/// `format_chain` that captures `debug={e:?}`, so the message reaches the
/// log line.
#[derive(Debug)]
struct PinMismatch(&'static str);

impl std::fmt::Display for PinMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for PinMismatch {}

/// A rustls verifier that accepts exactly the certificate whose DER
/// SHA-256 equals `expected_sha256`. Hostname and CA-chain checks are
/// intentionally skipped — the pin is the whole trust decision.
#[derive(Debug)]
pub struct PinnedCertVerifier {
    expected_sha256: String,
    provider: Arc<CryptoProvider>,
}

impl PinnedCertVerifier {
    /// Build a verifier for a 64-char lowercase-hex SHA-256 pin.
    pub fn new(pin: &str) -> Result<Self, TlsError> {
        if !portunus_core::fingerprint::is_valid_sha256_hex(pin) {
            return Err(TlsError::BadPin);
        }
        // The process installs aws-lc-rs as the default provider in main();
        // fall back to it explicitly so unit tests work without that setup.
        let provider = CryptoProvider::get_default()
            .cloned()
            .unwrap_or_else(|| Arc::new(rustls::crypto::aws_lc_rs::default_provider()));
        Ok(Self {
            expected_sha256: pin.to_ascii_lowercase(),
            provider,
        })
    }
}

impl ServerCertVerifier for PinnedCertVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let got = portunus_core::fingerprint::sha256_hex(end_entity.as_ref());
        if got.eq_ignore_ascii_case(&self.expected_sha256) {
            Ok(ServerCertVerified::assertion())
        } else {
            // Carry a stable, human-readable cause so the failure is
            // distinguishable from any other transport error downstream.
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::Other(rustls::OtherError(Arc::new(PinMismatch(
                    PIN_MISMATCH_MESSAGE,
                )))),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Build a tonic [`Endpoint`] that dials `endpoint` over TLS, trusting only
/// the certificate whose fingerprint matches `pin`. `ClientTlsConfig` is
/// left bare: per tonic, the default-verifier setters (`ca_certificate` /
/// `domain_name`) must not be combined with a custom verifier. SNI is
/// derived from the URI host and ignored by the verifier, so an IP
/// endpoint is fine.
pub fn pinned_endpoint(endpoint: &str, pin: &str) -> Result<Endpoint, TlsError> {
    let verifier = Arc::new(PinnedCertVerifier::new(pin)?);
    Endpoint::from_shared(format!("https://{endpoint}"))
        .map_err(|e| TlsError::Transport(e.to_string()))?
        .tls_config_with_verifier(ClientTlsConfig::new(), verifier)
        .map_err(|e| TlsError::Transport(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn self_signed_der() -> Vec<u8> {
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        cert.cert.der().to_vec()
    }

    fn a_time() -> UnixTime {
        UnixTime::since_unix_epoch(Duration::from_secs(1_700_000_000))
    }

    #[test]
    fn accepts_matching_fingerprint() {
        let der = self_signed_der();
        let pin = portunus_core::fingerprint::sha256_hex(&der);
        let verifier = PinnedCertVerifier::new(&pin).unwrap();
        let cert = CertificateDer::from(der);
        let res = verifier.verify_server_cert(
            &cert,
            &[],
            &ServerName::try_from("localhost").unwrap(),
            &[],
            a_time(),
        );
        assert!(res.is_ok(), "matching fingerprint should verify");
    }

    #[test]
    fn rejects_mismatched_fingerprint() {
        let der = self_signed_der();
        let other = self_signed_der();
        let other_pin = portunus_core::fingerprint::sha256_hex(&other);
        assert_ne!(
            portunus_core::fingerprint::sha256_hex(&der),
            other_pin,
            "two self-signed certs must differ"
        );
        let verifier = PinnedCertVerifier::new(&other_pin).unwrap();
        let cert = CertificateDer::from(der);
        let res = verifier.verify_server_cert(
            &cert,
            &[],
            &ServerName::try_from("localhost").unwrap(),
            &[],
            a_time(),
        );
        assert!(
            matches!(res, Err(rustls::Error::InvalidCertificate(_))),
            "mismatched fingerprint must be rejected, got {res:?}"
        );
        // The rejection must carry the stable pin-mismatch message so
        // downstream logs (and e2e tests) can assert on the cause.
        let err = res.unwrap_err();
        assert!(
            err.to_string().contains(PIN_MISMATCH_MESSAGE),
            "mismatch error must surface the stable marker, got: {err}"
        );
    }

    #[test]
    fn rejects_malformed_pin() {
        assert!(PinnedCertVerifier::new("nothex").is_err());
        assert!(PinnedCertVerifier::new(&"a".repeat(63)).is_err());
        assert!(PinnedCertVerifier::new(&"g".repeat(64)).is_err());
        assert!(PinnedCertVerifier::new(&"a".repeat(64)).is_ok());
    }

    #[test]
    fn pinned_endpoint_builds_for_ip() {
        let pin = "a".repeat(64);
        assert!(pinned_endpoint("127.0.0.1:7443", &pin).is_ok());
    }

    #[test]
    fn pinned_endpoint_rejects_bad_pin() {
        assert!(matches!(
            pinned_endpoint("127.0.0.1:7443", "short"),
            Err(TlsError::BadPin)
        ));
    }
}

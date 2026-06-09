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
        if pin.len() != 64 || !pin.bytes().all(|b| b.is_ascii_hexdigit()) {
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
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::ApplicationVerificationFailure,
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

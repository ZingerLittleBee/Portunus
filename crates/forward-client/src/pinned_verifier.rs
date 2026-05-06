//! Pinned-certificate verifier for the gRPC TLS handshake.
//!
//! The control plane uses TLS without a certificate authority chain (the
//! operator runs both ends, FR-001/FR-002 + Constitution Principle I).
//! Instead, the client has the SHA-256 of the server's leaf cert burned
//! into its `bundle.json` and refuses any handshake whose leaf cert does
//! not match — defeating both untrusted-network MITM and TOFU drift.
//!
//! Wired into `connect_once` in T036.

#![allow(dead_code)]

use std::sync::Arc;

use forward_core::fingerprint;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error as TlsError, SignatureScheme};

/// rustls verifier that pins the server's leaf certificate by SHA-256.
///
/// Signature verification (the algorithmic primitives invoked during the
/// TLS handshake) is delegated to the active rustls [`CryptoProvider`] —
/// we only override the *certificate-chain* validation step. The trust
/// model is "we trust exactly this one leaf and nothing else": no CA, no
/// chain walk, no name validation.
#[derive(Debug)]
pub struct PinnedVerifier {
    expected_sha256: [u8; 32],
    provider: Arc<CryptoProvider>,
}

impl PinnedVerifier {
    pub fn new(expected_sha256: [u8; 32]) -> Result<Arc<Self>, TlsError> {
        let provider = CryptoProvider::get_default()
            .cloned()
            .ok_or_else(|| TlsError::General("no rustls CryptoProvider installed".into()))?;
        Ok(Arc::new(Self {
            expected_sha256,
            provider,
        }))
    }

    pub fn with_provider(expected_sha256: [u8; 32], provider: Arc<CryptoProvider>) -> Arc<Self> {
        Arc::new(Self {
            expected_sha256,
            provider,
        })
    }

    pub fn from_hex(expected_sha256_hex: &str) -> Result<Arc<Self>, TlsError> {
        if expected_sha256_hex.len() != 64 {
            return Err(TlsError::General(format!(
                "expected 64-char hex fingerprint, got {} chars",
                expected_sha256_hex.len()
            )));
        }
        let bytes = hex_decode(expected_sha256_hex)
            .ok_or_else(|| TlsError::General("invalid hex in fingerprint".into()))?;
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        Self::new(arr)
    }
}

impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        let presented = fingerprint::sha256_bytes(end_entity.as_ref());
        if fingerprint::ct_eq(&presented, &self.expected_sha256) {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(TlsError::General(format!(
                "pinned fingerprint mismatch: expected {}, got {}",
                fingerprint::hex(&self.expected_sha256),
                fingerprint::hex(&presented),
            )))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
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
    ) -> Result<HandshakeSignatureValid, TlsError> {
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

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    let bytes = s.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    let nib = |b: u8| -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    };
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for c in bytes.chunks_exact(2) {
        out.push((nib(c[0])? << 4) | nib(c[1])?);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};

    fn install_provider() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    }

    fn fake_now() -> UnixTime {
        UnixTime::since_unix_epoch(std::time::Duration::from_secs(1_700_000_000))
    }

    fn fake_name() -> ServerName<'static> {
        ServerName::try_from("example.test").unwrap()
    }

    #[test]
    fn matching_fingerprint_passes() {
        install_provider();
        let der = b"this-is-not-a-real-cert-but-the-fingerprint-is-deterministic";
        let expected = fingerprint::sha256_bytes(der);
        let verifier = PinnedVerifier::new(expected).unwrap();
        let cert = CertificateDer::from(der.to_vec());
        verifier
            .verify_server_cert(&cert, &[], &fake_name(), &[], fake_now())
            .expect("matching pin must succeed");
    }

    #[test]
    fn mismatched_fingerprint_fails() {
        install_provider();
        let actual_der = b"actual-cert";
        let bogus_der = b"different-cert";
        let expected_for_bogus = fingerprint::sha256_bytes(bogus_der);
        let verifier = PinnedVerifier::new(expected_for_bogus).unwrap();
        let cert = CertificateDer::from(actual_der.to_vec());
        let err = verifier
            .verify_server_cert(&cert, &[], &fake_name(), &[], fake_now())
            .expect_err("mismatched pin must fail");
        match err {
            TlsError::General(msg) => assert!(msg.contains("pinned fingerprint mismatch")),
            other => panic!("expected General error, got {other:?}"),
        }
    }

    #[test]
    fn from_hex_round_trip() {
        install_provider();
        let der = b"fixture";
        let hex = fingerprint::sha256_hex(der);
        let v = PinnedVerifier::from_hex(&hex).unwrap();
        let cert = CertificateDer::from(der.to_vec());
        v.verify_server_cert(&cert, &[], &fake_name(), &[], fake_now())
            .unwrap();
    }

    #[test]
    fn from_hex_rejects_bad_length() {
        let err = PinnedVerifier::from_hex("deadbeef").unwrap_err();
        match err {
            TlsError::General(msg) => assert!(msg.contains("64-char")),
            other => panic!("got {other:?}"),
        }
    }
}

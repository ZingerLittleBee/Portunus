//! Cryptographic fingerprint helpers used by both server (to print the
//! pinning value into a `CredentialBundle`) and client (to compare against
//! the leaf cert presented during TLS).

use blake3::Hasher;

/// SHA-256 hex (lowercase, 64 chars) of the given bytes.
///
/// Used over the DER encoding of the server's leaf certificate. blake3 is
/// not used here because the value MUST be SHA-256 — that's what every TLS
/// inspection tool prints, and we want operators to verify it manually with
/// `openssl x509 -fingerprint -sha256` or similar.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    hex(&h.finalize())
}

/// SHA-256 raw bytes of the given input.
#[must_use]
pub fn sha256_bytes(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().into()
}

/// blake3 32-byte hash. Used by `portunus-auth` for token hashing where the
/// input has ≥128 bits of entropy (no salt needed).
#[must_use]
pub fn blake3_32(bytes: &[u8]) -> [u8; 32] {
    let mut h = Hasher::new();
    h.update(bytes);
    *h.finalize().as_bytes()
}

#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(TABLE[(b >> 4) as usize] as char);
        out.push(TABLE[(b & 0xf) as usize] as char);
    }
    out
}

/// Returns true iff s is exactly 64 ASCII hex digits (a SHA-256 hex pin).
/// Shared by the enrollment URI parser, the credential-bundle validator,
/// and the TLS pin verifier so the trust surfaces cannot silently diverge.
#[must_use]
pub fn is_valid_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Constant-time equality for hash-sized byte arrays. Used in token verify
/// and pinning compare paths.
#[must_use]
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_known_vector() {
        // NIST SHA-256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sha256_empty() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn ct_eq_true_for_match() {
        assert!(ct_eq(b"abcd", b"abcd"));
    }

    #[test]
    fn ct_eq_false_for_mismatch() {
        assert!(!ct_eq(b"abcd", b"abce"));
        assert!(!ct_eq(b"abcd", b"abc"));
    }

    #[test]
    fn hex_lowercase() {
        assert_eq!(hex(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
    }

    #[test]
    fn blake3_deterministic() {
        let a = blake3_32(b"hello");
        let b = blake3_32(b"hello");
        assert_eq!(a, b);
    }

    #[test]
    fn is_valid_sha256_hex_accepts_and_rejects() {
        // 64 hex chars, lowercase and uppercase both accepted.
        assert!(is_valid_sha256_hex(&"a".repeat(64)));
        assert!(is_valid_sha256_hex(&"A".repeat(64)));
        // Wrong length: 63 and 65 chars rejected.
        assert!(!is_valid_sha256_hex(&"a".repeat(63)));
        assert!(!is_valid_sha256_hex(&"a".repeat(65)));
        // Non-hex character (correct length) rejected.
        assert!(!is_valid_sha256_hex(&"g".repeat(64)));
    }

    #[test]
    fn sha256_bytes_matches_hex_form() {
        // `sha256_bytes` is the raw-byte form of `sha256_hex`; hex of the
        // raw bytes must equal the string helper for the same input.
        let raw = sha256_bytes(b"abc");
        assert_eq!(raw.len(), 32);
        assert_eq!(hex(&raw), sha256_hex(b"abc"));
        // Known NIST vector for SHA-256("abc"), first byte 0xba.
        assert_eq!(raw[0], 0xba);
    }

    #[test]
    fn blake3_distinct_inputs_distinct_hashes() {
        assert_ne!(blake3_32(b"hello"), blake3_32(b"world"));
    }
}

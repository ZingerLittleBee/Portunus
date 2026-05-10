//! Token primitives: generation and hashing.
//!
//! Generation: 32 random bytes from `OsRng`, encoded as URL-safe base64
//! without padding → 43 ASCII chars. ≥256 bits of entropy, well above
//! Constitution Principle I's 128-bit floor.
//!
//! Hashing: blake3 (32-byte output). The input is high-entropy random, so
//! a salt-less fast hash is appropriate (no rainbow-table or brute-force
//! threat model). Verifying a presented token = blake3 of the candidate
//! compared in constant time against the stored hash.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::{RngCore, rngs::OsRng};

/// Generate a fresh bearer token. 43 ASCII chars (URL-safe base64 of
/// 32 random bytes, no padding).
#[must_use]
pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// 32-byte blake3 hash of a token. Determinstic; safe to compare against
/// a stored hash in constant time.
#[must_use]
pub fn hash_token(token: &str) -> [u8; 32] {
    portunus_core::fingerprint::blake3_32(token.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn token_is_43_chars() {
        let t = generate_token();
        assert_eq!(t.len(), 43, "token = {t:?}");
    }

    #[test]
    fn tokens_are_distinct() {
        let mut seen = HashSet::new();
        for _ in 0..1000 {
            assert!(seen.insert(generate_token()), "collision!");
        }
    }

    #[test]
    fn token_is_url_safe_base64() {
        let t = generate_token();
        // Decoding must succeed and round-trip to 32 bytes.
        let bytes = URL_SAFE_NO_PAD.decode(t.as_bytes()).unwrap();
        assert_eq!(bytes.len(), 32);
    }

    #[test]
    fn hash_is_deterministic() {
        let t = "deterministic-fixture-token";
        assert_eq!(hash_token(t), hash_token(t));
    }

    #[test]
    fn distinct_tokens_distinct_hashes() {
        assert_ne!(hash_token("a"), hash_token("b"));
    }
}

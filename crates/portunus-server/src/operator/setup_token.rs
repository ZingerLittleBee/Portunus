//! Onboarding setup-token helpers.

use chrono::{DateTime, Duration, Utc};
use portunus_auth::token::{generate_token, hash_token};
use portunus_core::fingerprint;

pub(crate) const DEFAULT_SETUP_TOKEN_TTL: Duration = Duration::minutes(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SetupTokenRecord {
    hash_hex: String,
    expires_at: DateTime<Utc>,
}

impl SetupTokenRecord {
    #[must_use]
    pub(crate) fn new(now: DateTime<Utc>, ttl: Duration) -> (String, Self) {
        let raw = generate_token();
        let record = Self {
            hash_hex: fingerprint::hex(&hash_token(&raw)),
            expires_at: now + ttl,
        };
        (raw, record)
    }

    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn from_stored(hash_hex: String, expires_at: DateTime<Utc>) -> Self {
        Self {
            hash_hex,
            expires_at,
        }
    }

    #[must_use]
    #[allow(dead_code)]
    pub(crate) fn verify(&self, raw: &str, now: DateTime<Utc>) -> bool {
        if now >= self.expires_at {
            return false;
        }
        let presented = fingerprint::hex(&hash_token(raw));
        let needle = presented.as_bytes();
        self.hash_hex.len() == needle.len() && fingerprint::ct_eq(self.hash_hex.as_bytes(), needle)
    }

    #[must_use]
    pub(crate) fn hash_hex(&self) -> &str {
        &self.hash_hex
    }

    #[must_use]
    pub(crate) fn expires_at(&self) -> DateTime<Utc> {
        self.expires_at
    }
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone};

    use super::*;

    #[test]
    fn setup_token_verifies_then_expires() {
        let now = Utc.with_ymd_and_hms(2026, 5, 11, 9, 0, 0).unwrap();
        let (raw, record) = SetupTokenRecord::new(now, Duration::minutes(30));

        assert_eq!(record.expires_at(), now + Duration::minutes(30));
        assert!(record.verify(&raw, now + Duration::minutes(1)));
        assert!(!record.verify(&raw, now + Duration::minutes(31)));
        assert!(!record.verify("wrong", now + Duration::minutes(1)));
    }
}

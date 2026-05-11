//! Web UI session token helpers.
//!
//! Shared by login, logout, and the session-aware auth middleware.

use axum::http::{HeaderMap, header};
use chrono::{DateTime, Duration, Utc};
use portunus_auth::token::{generate_token, hash_token};
use portunus_core::fingerprint;

pub(crate) const SESSION_COOKIE: &str = "portunus_session";
pub(crate) const IDLE_TIMEOUT: Duration = Duration::hours(8);
pub(crate) const ABSOLUTE_TIMEOUT: Duration = Duration::days(7);

#[must_use]
pub(crate) fn generate_session_secret() -> String {
    generate_token()
}

#[must_use]
pub(crate) fn hash_session_secret(secret: &str) -> String {
    fingerprint::hex(&hash_token(secret))
}

#[must_use]
pub(crate) fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let cookie = headers.get(header::COOKIE)?.to_str().ok()?;
    cookie.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == name).then_some(value)
    })
}

#[must_use]
pub(crate) fn session_is_expired(
    last_seen: DateTime<Utc>,
    absolute: DateTime<Utc>,
    now: DateTime<Utc>,
) -> bool {
    now > absolute || now > last_seen + IDLE_TIMEOUT
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn web_session_secret_hash_is_deterministic_and_not_raw_secret() {
        let secret = generate_session_secret();
        let hash_a = hash_session_secret(&secret);
        let hash_b = hash_session_secret(&secret);

        assert_eq!(hash_a, hash_b);
        assert_ne!(hash_a, secret);
    }

    #[test]
    fn web_session_expiry_checks_idle_and_absolute_timeouts() {
        let created_at = Utc.with_ymd_and_hms(2026, 5, 7, 10, 0, 0).unwrap();

        assert!(!session_is_expired(
            created_at,
            created_at + ABSOLUTE_TIMEOUT,
            created_at + IDLE_TIMEOUT,
        ));
        assert!(session_is_expired(
            created_at,
            created_at + ABSOLUTE_TIMEOUT,
            created_at + IDLE_TIMEOUT + Duration::seconds(1),
        ));
        assert!(session_is_expired(
            created_at,
            created_at + Duration::hours(1),
            created_at + Duration::hours(1) + Duration::seconds(1),
        ));
    }
}

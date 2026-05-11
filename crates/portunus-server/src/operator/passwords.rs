//! Local-password hashing and policy.

use argon2::{
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
    password_hash::{SaltString, rand_core::OsRng},
};
use thiserror::Error;

pub const MIN_PASSWORD_CHARS: usize = 12;
pub const MAX_PASSWORD_BYTES: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PasswordError {
    #[error("password_too_short")]
    TooShort,
    #[error("password_too_long")]
    TooLong,
    #[error("password_invalid")]
    Invalid,
    #[error("password_hash_failed")]
    HashFailed,
}

pub fn validate_password(password: &str) -> Result<(), PasswordError> {
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(PasswordError::TooShort);
    }
    if password.len() > MAX_PASSWORD_BYTES {
        return Err(PasswordError::TooLong);
    }
    Ok(())
}

pub fn hash_password(password: &str) -> Result<String, PasswordError> {
    validate_password(password)?;
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|_| PasswordError::HashFailed)
}

pub fn verify_password(password: &str, encoded: &str) -> Result<(), PasswordError> {
    let parsed = PasswordHash::new(encoded).map_err(|_| PasswordError::Invalid)?;
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .map_err(|_| PasswordError::Invalid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_rejects_too_short_password() {
        assert_eq!(
            validate_password("short").unwrap_err(),
            PasswordError::TooShort
        );
    }

    #[test]
    fn policy_rejects_over_1024_utf8_bytes() {
        let pw = "a".repeat(1025);
        assert_eq!(validate_password(&pw).unwrap_err(), PasswordError::TooLong);
    }

    #[test]
    fn hash_round_trip_verifies() {
        let hash = hash_password("correct horse battery staple").expect("hash");
        verify_password("correct horse battery staple", &hash).expect("verify");
        assert_eq!(
            verify_password("wrong horse battery staple", &hash).unwrap_err(),
            PasswordError::Invalid
        );
        assert!(hash.starts_with("$argon2"));
    }
}

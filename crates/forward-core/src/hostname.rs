//! RFC 1123 strict-hostname newtype. Spec: `003-domain-name-forward`
//! `data-model.md` § `Hostname` (FR-001, R-005).
//!
//! Validation is centralized here so every push path — operator CLI,
//! HTTP body handler, persistence reload — applies the same rule. A
//! `Hostname` value, once constructed, satisfies every rule for its
//! entire lifetime; comparison and hashing are ASCII case-insensitive.

use std::fmt;
use std::hash::{Hash, Hasher};

use thiserror::Error;

#[derive(Debug, Clone, Eq)]
pub struct Hostname(String);

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum HostnameError {
    #[error("hostname_empty")]
    Empty,

    #[error("hostname_total_too_long: {0} octets (max 253)")]
    TotalTooLong(usize),

    #[error("hostname_label_empty: empty label at position {position}")]
    LabelEmpty { position: usize },

    #[error("hostname_label_too_long: label {label:?} is {len} octets (max 63)")]
    LabelTooLong { label: String, len: usize },

    #[error("hostname_invalid_char: label {label:?} contains {ch:?}")]
    InvalidChar { label: String, ch: char },

    #[error("hostname_label_hyphen_boundary: label {label:?} starts or ends with '-'")]
    HyphenBoundary { label: String },

    #[error("hostname_all_numeric: {0:?} is an all-numeric form (use IP literal)")]
    AllNumeric(String),
}

impl Hostname {
    /// Validate `input` against RFC 1123 strict syntax and return a
    /// normalized (lowercase, trailing-dot-stripped) `Hostname`.
    pub fn new(input: &str) -> Result<Self, HostnameError> {
        let trimmed = input.strip_suffix('.').unwrap_or(input);
        if trimmed.is_empty() {
            return Err(HostnameError::Empty);
        }
        if trimmed.len() > 253 {
            return Err(HostnameError::TotalTooLong(trimmed.len()));
        }

        let mut all_numeric = true;
        for (position, label) in trimmed.split('.').enumerate() {
            if label.is_empty() {
                return Err(HostnameError::LabelEmpty { position });
            }
            if label.len() > 63 {
                return Err(HostnameError::LabelTooLong {
                    label: label.to_string(),
                    len: label.len(),
                });
            }
            if label.starts_with('-') || label.ends_with('-') {
                return Err(HostnameError::HyphenBoundary {
                    label: label.to_string(),
                });
            }
            for ch in label.chars() {
                if !ch.is_ascii_alphanumeric() && ch != '-' {
                    return Err(HostnameError::InvalidChar {
                        label: label.to_string(),
                        ch,
                    });
                }
                if !ch.is_ascii_digit() {
                    all_numeric = false;
                }
            }
        }

        if all_numeric {
            return Err(HostnameError::AllNumeric(trimmed.to_string()));
        }

        Ok(Self(trimmed.to_ascii_lowercase()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Hostname {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl PartialEq for Hostname {
    fn eq(&self, other: &Self) -> bool {
        self.0.eq_ignore_ascii_case(&other.0)
    }
}

impl Hash for Hostname {
    fn hash<H: Hasher>(&self, state: &mut H) {
        for byte in self.0.bytes() {
            state.write_u8(byte.to_ascii_lowercase());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_simple_fqdn() {
        let h = Hostname::new("api.example.com").unwrap();
        assert_eq!(h.as_str(), "api.example.com");
    }

    #[test]
    fn accepts_single_label() {
        assert!(Hostname::new("single").is_ok());
    }

    #[test]
    fn accepts_trailing_dot() {
        let h = Hostname::new("api.example.com.").unwrap();
        assert_eq!(h.as_str(), "api.example.com");
    }

    #[test]
    fn accepts_digits_and_hyphens_inside_labels() {
        assert!(Hostname::new("a1.b2-c3.example").is_ok());
    }

    #[test]
    fn lowercases_and_compares_case_insensitive() {
        let lower = Hostname::new("api.example.com").unwrap();
        let upper = Hostname::new("API.Example.COM").unwrap();
        assert_eq!(lower, upper);
        assert_eq!(upper.as_str(), "api.example.com");

        use std::collections::hash_map::DefaultHasher;
        let mut a = DefaultHasher::new();
        let mut b = DefaultHasher::new();
        lower.hash(&mut a);
        upper.hash(&mut b);
        assert_eq!(a.finish(), b.finish());
    }

    #[test]
    fn rejects_empty() {
        assert_eq!(Hostname::new(""), Err(HostnameError::Empty));
        assert_eq!(Hostname::new("."), Err(HostnameError::Empty));
    }

    #[test]
    fn rejects_underscore() {
        let err = Hostname::new("foo_bar.example").unwrap_err();
        assert!(matches!(err, HostnameError::InvalidChar { ch: '_', .. }));
    }

    #[test]
    fn rejects_idn_unicode() {
        let err = Hostname::new("münchen.example").unwrap_err();
        assert!(matches!(err, HostnameError::InvalidChar { .. }));
    }

    #[test]
    fn rejects_whitespace() {
        let err = Hostname::new("foo bar.example").unwrap_err();
        assert!(matches!(err, HostnameError::InvalidChar { ch: ' ', .. }));
    }

    #[test]
    fn rejects_label_over_63_octets() {
        let label = "a".repeat(64);
        let err = Hostname::new(&format!("{label}.example")).unwrap_err();
        assert!(matches!(err, HostnameError::LabelTooLong { len: 64, .. }));
    }

    #[test]
    fn accepts_label_at_63_octets() {
        let label = "a".repeat(63);
        assert!(Hostname::new(&format!("{label}.example")).is_ok());
    }

    #[test]
    fn rejects_total_over_253_octets() {
        // 4 × ('a' × 63 + '.') = 256 octets total.
        let label = "a".repeat(63);
        let name = format!("{label}.{label}.{label}.{label}");
        let err = Hostname::new(&name).unwrap_err();
        assert!(matches!(err, HostnameError::TotalTooLong(255)));
    }

    #[test]
    fn rejects_leading_hyphen() {
        let err = Hostname::new("-foo.example").unwrap_err();
        assert!(matches!(err, HostnameError::HyphenBoundary { .. }));
    }

    #[test]
    fn rejects_trailing_hyphen() {
        let err = Hostname::new("foo-.example").unwrap_err();
        assert!(matches!(err, HostnameError::HyphenBoundary { .. }));
    }

    #[test]
    fn rejects_consecutive_dots() {
        let err = Hostname::new("foo..example").unwrap_err();
        assert!(matches!(err, HostnameError::LabelEmpty { position: 1 }));
    }

    #[test]
    fn rejects_all_numeric() {
        // Caught here so the IP-literal classifier in `Target::parse`
        // is the canonical owner of numeric forms.
        let err = Hostname::new("12345").unwrap_err();
        assert!(matches!(err, HostnameError::AllNumeric(_)));
    }

    #[test]
    fn rejects_srv_underscore_prefix() {
        let err = Hostname::new("_https._tcp.example").unwrap_err();
        assert!(matches!(err, HostnameError::InvalidChar { ch: '_', .. }));
    }
}

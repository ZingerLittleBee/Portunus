//! Runtime resolution of the advertised credential-bundle endpoint.
//!
//! Resolve-once-at-creation, replay-at-redeem. Explicit operator config
//! (SQLite override / CLI / env) that is malformed or not SAN-covered is
//! a hard error; implicit candidates (request-Host derive, loopback)
//! skip-and-fall-through.

pub mod grammar;
pub mod host_header;
pub mod resolve;
pub mod san;

pub use resolve::ResolveInputs;
pub use resolve::resolve_advertised_endpoint;
pub use san::CertSanSet;

/// Which tier produced the resolved endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EndpointSource {
    Override,
    Seed,
    Derived,
    Loopback,
}

/// Successful resolution result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAdvertisedEndpoint {
    pub endpoint: String,
    pub source: EndpointSource,
}

/// Which explicit tier a hard error came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigTier {
    /// Tier 1 — SQLite operator override.
    Override,
    /// Tier 2 — CLI flag / env seed.
    Seed,
}

/// Resolution failure. All variants are terminal — creation never
/// fabricates an unusable endpoint.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveEndpointError {
    #[error("configured advertised endpoint is malformed ({tier:?}): {reason}")]
    ConfiguredEndpointInvalid { tier: ConfigTier, reason: String },
    #[error(
        "configured advertised endpoint host {host} is not covered by the server certificate SAN ({tier:?})"
    )]
    ConfiguredEndpointNotCovered { tier: ConfigTier, host: String },
    #[error("no certificate-SAN-covered advertised endpoint candidate is available")]
    NoSanCoveredCandidate,
}

impl ResolveEndpointError {
    /// Stable machine code for HTTP 422 bodies.
    #[must_use]
    pub fn http_code(&self) -> &'static str {
        match self {
            Self::ConfiguredEndpointInvalid { .. } => "endpoint_invalid",
            Self::ConfiguredEndpointNotCovered { .. } | Self::NoSanCoveredCandidate => {
                "endpoint_not_in_cert_san"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_codes_are_stable() {
        assert_eq!(
            ResolveEndpointError::ConfiguredEndpointInvalid {
                tier: ConfigTier::Seed,
                reason: "x".into()
            }
            .http_code(),
            "endpoint_invalid"
        );
        assert_eq!(
            ResolveEndpointError::NoSanCoveredCandidate.http_code(),
            "endpoint_not_in_cert_san"
        );
    }
}

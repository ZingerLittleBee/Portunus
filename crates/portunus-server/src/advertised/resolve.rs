//! Tiered, SAN-filtered, fail-closed endpoint resolution.

use super::{
    ConfigTier, EndpointSource, ResolveEndpointError, ResolvedAdvertisedEndpoint,
    grammar::validate_authority, host_header::host_from_header, san::CertSanSet,
};

/// Inputs gathered by the caller (handler / offline path).
pub struct ResolveInputs<'a> {
    /// Tier 1 — SQLite operator override (already `None`-filtered for empty).
    pub override_value: Option<String>,
    /// Tier 2 — CLI flag / env seed.
    pub seed: Option<String>,
    /// Tier 3 — raw HTTP `Host` header, if this is a request-driven path.
    pub req_host: Option<&'a str>,
    /// Server's resolved control-plane port (for tiers 3 & 4).
    pub control_port: u16,
    /// Parsed leaf-cert SAN set.
    pub san: &'a CertSanSet,
}

/// Resolve the advertised endpoint per the spec's tiered contract.
///
/// # Errors
/// - `ConfiguredEndpointInvalid` — explicit tier present but malformed.
/// - `ConfiguredEndpointNotCovered` — explicit tier well-formed but host
///   not SAN-covered.
/// - `NoSanCoveredCandidate` — no explicit config and neither derive nor
///   loopback is SAN-covered.
pub fn resolve_advertised_endpoint(
    inputs: &ResolveInputs<'_>,
) -> Result<ResolvedAdvertisedEndpoint, ResolveEndpointError> {
    debug_assert!(
        inputs.override_value.as_deref() != Some(""),
        "override_value must be None-filtered for empty by the caller"
    );
    // Tier 1 — explicit SQLite override.
    if let Some(v) = inputs.override_value.as_deref() {
        return finalize_explicit(
            v,
            ConfigTier::Override,
            EndpointSource::Override,
            inputs.san,
        );
    }
    // Tier 2 — explicit CLI/env seed.
    if let Some(v) = inputs.seed.as_deref() {
        return finalize_explicit(v, ConfigTier::Seed, EndpointSource::Seed, inputs.san);
    }
    // Tier 3 — implicit auto-derive from request Host.
    if let Some(resolved) = try_derive_from_host(inputs) {
        return Ok(resolved);
    }
    // Tier 4 — implicit loopback fallback.
    let loopback = format!("127.0.0.1:{}", inputs.control_port);
    if let Ok((host, _)) = validate_authority(&loopback)
        && inputs.san.covers(host)
    {
        return Ok(ResolvedAdvertisedEndpoint {
            endpoint: loopback,
            source: EndpointSource::Loopback,
        });
    }
    tracing::warn!(
        event = "advertised.no_covered_candidate",
        "no SAN-covered advertised endpoint candidate available"
    );
    Err(ResolveEndpointError::NoSanCoveredCandidate)
}

/// Tier 3 — implicit auto-derive from the request `Host` header.
///
/// Returns `Some` only when a SAN-covered candidate is produced. Any
/// failure (no header, unusable header, grammar-invalid derived
/// candidate, or uncovered host) yields `None` so the caller falls
/// through to tier 4. Implicit tiers never panic.
fn try_derive_from_host(inputs: &ResolveInputs<'_>) -> Option<ResolvedAdvertisedEndpoint> {
    let raw = inputs.req_host?;
    let candidate = host_from_header(raw, inputs.control_port)?;
    // `host_from_header` already grammar-validates, but re-validate
    // defensively: a drift in that invariant must not panic on a
    // request-handler path — treat it as uncovered (return None).
    let (host, _) = validate_authority(&candidate).ok()?;
    if inputs.san.covers(host) {
        return Some(ResolvedAdvertisedEndpoint {
            endpoint: candidate,
            source: EndpointSource::Derived,
        });
    }
    tracing::warn!(
        event = "advertised.derive_uncovered",
        host = %host,
        "request-Host derived endpoint not SAN-covered; falling through"
    );
    None
}

fn finalize_explicit(
    value: &str,
    tier: ConfigTier,
    source: EndpointSource,
    san: &CertSanSet,
) -> Result<ResolvedAdvertisedEndpoint, ResolveEndpointError> {
    let (host, _) = validate_authority(value)
        .map_err(|reason| ResolveEndpointError::ConfiguredEndpointInvalid { tier, reason })?;
    if !san.covers(host) {
        return Err(ResolveEndpointError::ConfiguredEndpointNotCovered {
            tier,
            host: host.to_string(),
        });
    }
    Ok(ResolvedAdvertisedEndpoint {
        endpoint: value.to_string(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE_PEM: &str = include_str!("testdata/san_fixture.pem");

    fn san() -> CertSanSet {
        CertSanSet::from_pem(FIXTURE_PEM).unwrap()
    }

    fn base(san: &CertSanSet) -> ResolveInputs<'_> {
        ResolveInputs {
            override_value: None,
            seed: None,
            req_host: None,
            control_port: 7443,
            san,
        }
    }

    #[test]
    fn tier1_override_wins_when_covered() {
        let s = san();
        let mut i = base(&s);
        i.override_value = Some("public.example:7443".into());
        i.seed = Some("localhost:7443".into());
        let r = resolve_advertised_endpoint(&i).unwrap();
        assert_eq!(r.endpoint, "public.example:7443");
        assert_eq!(r.source, EndpointSource::Override);
    }

    #[test]
    fn tier1_malformed_is_hard_error_not_downgraded() {
        let s = san();
        let mut i = base(&s);
        i.override_value = Some("https://public.example:7443".into());
        i.seed = Some("localhost:7443".into());
        assert!(matches!(
            resolve_advertised_endpoint(&i),
            Err(ResolveEndpointError::ConfiguredEndpointInvalid {
                tier: ConfigTier::Override,
                ..
            })
        ));
    }

    #[test]
    fn tier2_seed_uncovered_is_hard_error_even_if_loopback_covered() {
        let s = san();
        let mut i = base(&s);
        i.seed = Some("not.in.cert:7443".into());
        // loopback 127.0.0.1 IS covered by the fixture, but seed is explicit.
        assert!(matches!(
            resolve_advertised_endpoint(&i),
            Err(ResolveEndpointError::ConfiguredEndpointNotCovered {
                tier: ConfigTier::Seed,
                ..
            })
        ));
    }

    #[test]
    fn tier2_bad_env_seed_is_invalid() {
        let s = san();
        let mut i = base(&s);
        i.seed = Some("x:bad".into());
        assert!(matches!(
            resolve_advertised_endpoint(&i),
            Err(ResolveEndpointError::ConfiguredEndpointInvalid {
                tier: ConfigTier::Seed,
                ..
            })
        ));
    }

    #[test]
    fn tier3_derive_used_when_covered() {
        let s = san();
        let mut i = base(&s);
        i.req_host = Some("public.example:443");
        let r = resolve_advertised_endpoint(&i).unwrap();
        assert_eq!(r.endpoint, "public.example:7443");
        assert_eq!(r.source, EndpointSource::Derived);
    }

    #[test]
    fn tier3_uncovered_falls_through_to_loopback() {
        let s = san();
        let mut i = base(&s);
        i.req_host = Some("not.in.cert:443");
        let r = resolve_advertised_endpoint(&i).unwrap();
        assert_eq!(r.endpoint, "127.0.0.1:7443");
        assert_eq!(r.source, EndpointSource::Loopback);
    }

    #[test]
    fn no_host_no_config_uses_loopback() {
        let s = san();
        let r = resolve_advertised_endpoint(&base(&s)).unwrap();
        assert_eq!(r.endpoint, "127.0.0.1:7443");
        assert_eq!(r.source, EndpointSource::Loopback);
    }

    #[test]
    fn tier1_malformed_takes_precedence_over_malformed_seed() {
        let s = san();
        let mut i = base(&s);
        i.override_value = Some("https://bad:7443".into());
        i.seed = Some("also-bad".into());
        assert!(matches!(
            resolve_advertised_endpoint(&i),
            Err(ResolveEndpointError::ConfiguredEndpointInvalid {
                tier: ConfigTier::Override,
                ..
            })
        ));
    }

    #[test]
    fn tier3_unusable_host_header_falls_through_to_loopback() {
        let s = san();
        let mut i = base(&s);
        // host_from_header rejects IPv6 literals → tier 3 skipped.
        i.req_host = Some("[::1]:443");
        let r = resolve_advertised_endpoint(&i).unwrap();
        assert_eq!(r.endpoint, "127.0.0.1:7443");
        assert_eq!(r.source, EndpointSource::Loopback);
    }
}

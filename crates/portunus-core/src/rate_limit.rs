//! Rate-limit envelope (spec 011-rate-limiting-qos).
//!
//! A `RateLimit` bundles four optional caps + companion burst
//! overrides. Used both as `Rule.rate_limit` (per-rule envelope) and
//! as the value side of the per-owner cap envelope keyed
//! `(client_name, owner_id)`. Each cap is independently optional;
//! `None` means uncapped on that dimension.
//!
//! Default burst is `1 × rate` per cap. Operators can override via
//! `*_burst` companion fields; validation clamps to
//! `rate / 100 ≤ burst ≤ rate * 60` (R-011 in `research.md`). The
//! `concurrent_connections_burst` slot is reserved for future use and
//! current validation rejects any non-`None` value (concurrent caps
//! are hard ceilings, not token buckets).
//!
//! Spec: `specs/011-rate-limiting-qos/data-model.md` § 1.1.
//! Reject reasons mirror the proto `RateLimitRejectReason` enum.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Lower bound on `burst / rate`. Below this the bucket refills
/// faster than 10 ms per token, which gives operators no shaping
/// granularity worth talking about and risks overflow on the lazy-
/// refill timer math.
pub const BURST_RATE_RATIO_MIN_PERCENT: u64 = 1; // burst ≥ rate / 100

/// Upper bound on `burst / rate`, in seconds. A 60 s burst is
/// effectively "no cap for the first minute of any sustained surge",
/// which is almost always an operator typo rather than intent.
pub const BURST_RATE_RATIO_MAX_SECONDS: u64 = 60; // burst ≤ rate * 60

/// Bundle of four optional caps. Field semantics line up 1:1 with the
/// `RateLimit` proto message (`Rule.rate_limit = 12`).
///
/// Validation lives in [`validate`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RateLimit {
    /// Bytes/sec ingress (downstream → upstream). `None` = uncapped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bandwidth_in_bps: Option<u64>,

    /// Bytes/sec egress (upstream → downstream). `None` = uncapped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bandwidth_out_bps: Option<u64>,

    /// New TCP connections per second (or new UDP flows per second).
    /// `None` = uncapped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_connections_per_sec: Option<u32>,

    /// Concurrent TCP connections (or live UDP NAT bindings).
    /// `None` = uncapped. Hard ceiling; no token bucket.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrent_connections: Option<u32>,

    /// Override default `bandwidth_in_bps × 1s` burst pool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bandwidth_in_burst: Option<u64>,

    /// Override default `bandwidth_out_bps × 1s` burst pool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bandwidth_out_burst: Option<u64>,

    /// Override default `new_connections_per_sec × 1s` burst pool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_connections_burst: Option<u32>,
    // `concurrent_connections_burst` is intentionally absent: concurrent
    // is a hard ceiling, not a token bucket. The proto holds no slot
    // for it. Validation rejects any attempt to set one before it
    // reaches this struct.
}

/// Reject reason. Mirrors proto `RateLimitRejectReason` 1:1; the
/// `Unspecified` variant is the proto default and is never
/// constructed by the rate limiter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RejectReason {
    ConnConcurrent,
    ConnRate,
    UdpFlowRate,
    OwnerConcurrent,
    OwnerConnRate,
    OwnerUdpFlowRate,
}

impl RejectReason {
    /// Stable lowercase label used for the Prometheus `reason` value.
    /// Mirrors `crates/portunus-server/src/metrics.rs`.
    #[must_use]
    pub fn as_metric_label(self) -> &'static str {
        match self {
            Self::ConnConcurrent => "conn_concurrent",
            Self::ConnRate => "conn_rate",
            Self::UdpFlowRate => "udp_flow_rate",
            Self::OwnerConcurrent => "owner_concurrent",
            Self::OwnerConnRate => "owner_conn_rate",
            Self::OwnerUdpFlowRate => "owner_udp_flow_rate",
        }
    }

    /// True when the reason is attributable to a per-owner cap. Used
    /// by the metrics fold to decide whether the `owner` label is
    /// non-empty.
    #[must_use]
    pub fn is_owner_scope(self) -> bool {
        matches!(
            self,
            Self::OwnerConcurrent | Self::OwnerConnRate | Self::OwnerUdpFlowRate
        )
    }
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum RateLimitError {
    /// A cap value was zero. Operators who want to disable a rule
    /// should toggle `enabled` instead. (Spec edge case + FR-020.)
    #[error("rate_limit_cap_zero: {field} must be > 0; use null to leave uncapped")]
    CapZero { field: &'static str },

    /// A `*_burst` field was supplied without its companion `rate`
    /// being set. `*_burst` is an override of the default
    /// `1 × rate`; with no rate there is nothing to override.
    #[error("rate_limit_burst_without_rate: {field} burst override has no companion rate cap set")]
    BurstWithoutRate { field: &'static str },

    /// A `*_burst` value violated the `[rate / 100, rate * 60]`
    /// envelope.
    #[error(
        "rate_limit_burst_range: {field} burst {burst} outside permitted [rate/100, rate*60] = [{lo}, {hi}] for rate {rate}"
    )]
    BurstRange {
        field: &'static str,
        rate: u64,
        burst: u64,
        lo: u64,
        hi: u64,
    },
}

/// Validate a `RateLimit` envelope. Returns `Ok(())` when every
/// supplied cap is `> 0`, every supplied burst has a companion cap,
/// and every burst stays inside `[rate/100, rate*60]`. Fail-fast on
/// the first violation — operators want one clear error per push.
///
/// # Errors
///
/// Returns the first failure encountered; subsequent fields are not
/// inspected.
pub fn validate(rl: &RateLimit) -> Result<(), RateLimitError> {
    // 1. Cap-zero check (each cap if Some(0)).
    check_nonzero_u64(rl.bandwidth_in_bps, "bandwidth_in_bps")?;
    check_nonzero_u64(rl.bandwidth_out_bps, "bandwidth_out_bps")?;
    check_nonzero_u32(rl.new_connections_per_sec, "new_connections_per_sec")?;
    check_nonzero_u32(rl.concurrent_connections, "concurrent_connections")?;

    // 2. Burst-without-rate + 3. Burst range checks for each pair.
    check_burst_u64("bandwidth_in", rl.bandwidth_in_bps, rl.bandwidth_in_burst)?;
    check_burst_u64(
        "bandwidth_out",
        rl.bandwidth_out_bps,
        rl.bandwidth_out_burst,
    )?;
    check_burst_u32(
        "new_connections",
        rl.new_connections_per_sec,
        rl.new_connections_burst,
    )?;

    Ok(())
}

fn check_nonzero_u64(v: Option<u64>, field: &'static str) -> Result<(), RateLimitError> {
    if let Some(0) = v {
        return Err(RateLimitError::CapZero { field });
    }
    Ok(())
}

fn check_nonzero_u32(v: Option<u32>, field: &'static str) -> Result<(), RateLimitError> {
    if let Some(0) = v {
        return Err(RateLimitError::CapZero { field });
    }
    Ok(())
}

fn check_burst_u64(
    family: &'static str,
    rate: Option<u64>,
    burst: Option<u64>,
) -> Result<(), RateLimitError> {
    let Some(burst) = burst else {
        return Ok(());
    };
    let Some(rate) = rate else {
        return Err(RateLimitError::BurstWithoutRate {
            field: burst_field_name_u64(family),
        });
    };
    let lo = rate / 100;
    let hi = rate.saturating_mul(BURST_RATE_RATIO_MAX_SECONDS);
    let lo_effective = lo.max(1); // burst < 1 makes no sense even at tiny rates
    if burst < lo_effective || burst > hi {
        return Err(RateLimitError::BurstRange {
            field: burst_field_name_u64(family),
            rate,
            burst,
            lo: lo_effective,
            hi,
        });
    }
    Ok(())
}

fn check_burst_u32(
    family: &'static str,
    rate: Option<u32>,
    burst: Option<u32>,
) -> Result<(), RateLimitError> {
    let Some(burst) = burst else {
        return Ok(());
    };
    let Some(rate) = rate else {
        return Err(RateLimitError::BurstWithoutRate {
            field: burst_field_name_u32(family),
        });
    };
    let rate64 = u64::from(rate);
    let burst64 = u64::from(burst);
    let lo = rate64 / 100;
    let hi = rate64.saturating_mul(BURST_RATE_RATIO_MAX_SECONDS);
    let lo_effective = lo.max(1);
    if burst64 < lo_effective || burst64 > hi {
        return Err(RateLimitError::BurstRange {
            field: burst_field_name_u32(family),
            rate: rate64,
            burst: burst64,
            lo: lo_effective,
            hi,
        });
    }
    Ok(())
}

fn burst_field_name_u64(family: &'static str) -> &'static str {
    match family {
        "bandwidth_in" => "bandwidth_in_burst",
        "bandwidth_out" => "bandwidth_out_burst",
        _ => "burst",
    }
}

fn burst_field_name_u32(family: &'static str) -> &'static str {
    match family {
        "new_connections" => "new_connections_burst",
        _ => "burst",
    }
}

/// Return the effective burst pool for a `(rate, override)` pair.
/// `None` rate yields `None`; `None` override yields `Some(rate)`
/// (1-second default).
#[must_use]
pub fn effective_burst_u64(rate: Option<u64>, burst: Option<u64>) -> Option<u64> {
    let rate = rate?;
    Some(burst.unwrap_or(rate))
}

/// `u32` analogue of [`effective_burst_u64`].
#[must_use]
pub fn effective_burst_u32(rate: Option<u32>, burst: Option<u32>) -> Option<u32> {
    let rate = rate?;
    Some(burst.unwrap_or(rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_envelope_validates() {
        assert!(validate(&RateLimit::default()).is_ok());
    }

    #[test]
    fn cap_zero_rejected() {
        let rl = RateLimit {
            bandwidth_in_bps: Some(0),
            ..Default::default()
        };
        assert!(matches!(
            validate(&rl),
            Err(RateLimitError::CapZero {
                field: "bandwidth_in_bps"
            })
        ));
    }

    #[test]
    fn burst_without_rate_rejected() {
        let rl = RateLimit {
            bandwidth_in_burst: Some(1024),
            ..Default::default()
        };
        assert!(matches!(
            validate(&rl),
            Err(RateLimitError::BurstWithoutRate {
                field: "bandwidth_in_burst"
            })
        ));
    }

    #[test]
    fn burst_in_range_accepted() {
        let rl = RateLimit {
            bandwidth_in_bps: Some(1_000_000),
            bandwidth_in_burst: Some(2_000_000), // 2 s burst — inside [10_000, 60_000_000]
            ..Default::default()
        };
        assert!(validate(&rl).is_ok());
    }

    #[test]
    fn burst_below_floor_rejected() {
        let rl = RateLimit {
            bandwidth_in_bps: Some(1_000_000),
            bandwidth_in_burst: Some(100), // below 10_000 floor
            ..Default::default()
        };
        assert!(matches!(
            validate(&rl),
            Err(RateLimitError::BurstRange { .. })
        ));
    }

    #[test]
    fn burst_above_ceiling_rejected() {
        let rl = RateLimit {
            bandwidth_in_bps: Some(1_000_000),
            bandwidth_in_burst: Some(70_000_000), // above 60×rate ceiling
            ..Default::default()
        };
        assert!(matches!(
            validate(&rl),
            Err(RateLimitError::BurstRange { .. })
        ));
    }

    #[test]
    fn effective_burst_defaults_to_rate() {
        assert_eq!(effective_burst_u64(Some(1000), None), Some(1000));
        assert_eq!(effective_burst_u64(Some(1000), Some(5000)), Some(5000));
        assert_eq!(effective_burst_u64(None, Some(5000)), None);
    }

    #[test]
    fn reject_reason_owner_scope() {
        assert!(!RejectReason::ConnConcurrent.is_owner_scope());
        assert!(RejectReason::OwnerConcurrent.is_owner_scope());
    }
}

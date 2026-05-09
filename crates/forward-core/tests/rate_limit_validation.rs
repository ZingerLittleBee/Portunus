//! Contract tests for the `forward_core::rate_limit::validate`
//! envelope (spec 011-rate-limiting-qos, T006).
//!
//! These tests pin the operator-visible validation contract that
//! the operator HTTP API uses to map each kind of failure to a
//! stable subcategory code:
//!
//! - `validation.rate_limit_cap_zero`
//! - `validation.rate_limit_burst_without_rate`
//! - `validation.rate_limit_burst_range`
//! - `validation.rate_limit_burst_unsupported`
//!
//! Coverage rationale matches spec.md FR-020 + Edge Cases:
//! - Zero is rejected on every cap dimension.
//! - Burst overrides require a companion `rate`.
//! - Burst values that fall outside `[rate / 100, rate * 60]` are
//!   rejected (R-011 in research.md).
//! - The `concurrent_connections_burst` slot is reserved (the public
//!   `RateLimit` struct intentionally has no field for it, so the
//!   reserved-rejection check happens at the operator-API layer
//!   before reaching the envelope; see contracts/wire.md §4 and the
//!   forward-server contract test in T009).

use forward_core::rate_limit::{RateLimit, RateLimitError, validate};

#[test]
fn empty_envelope_validates() {
    assert!(validate(&RateLimit::default()).is_ok());
}

#[test]
fn cap_zero_rejected_on_each_dimension() {
    let cases: &[(&str, RateLimit)] = &[
        (
            "bandwidth_in_bps",
            RateLimit {
                bandwidth_in_bps: Some(0),
                ..RateLimit::default()
            },
        ),
        (
            "bandwidth_out_bps",
            RateLimit {
                bandwidth_out_bps: Some(0),
                ..RateLimit::default()
            },
        ),
        (
            "new_connections_per_sec",
            RateLimit {
                new_connections_per_sec: Some(0),
                ..RateLimit::default()
            },
        ),
        (
            "concurrent_connections",
            RateLimit {
                concurrent_connections: Some(0),
                ..RateLimit::default()
            },
        ),
    ];
    for (expected_field, rl) in cases {
        match validate(rl) {
            Err(RateLimitError::CapZero { field }) => assert_eq!(
                field, *expected_field,
                "cap-zero error must name the offending field"
            ),
            other => panic!("expected CapZero on {expected_field}, got {other:?}"),
        }
    }
}

#[test]
fn burst_without_rate_rejected_on_each_pair() {
    let cases: &[(&str, RateLimit)] = &[
        (
            "bandwidth_in_burst",
            RateLimit {
                bandwidth_in_burst: Some(1024),
                ..RateLimit::default()
            },
        ),
        (
            "bandwidth_out_burst",
            RateLimit {
                bandwidth_out_burst: Some(1024),
                ..RateLimit::default()
            },
        ),
        (
            "new_connections_burst",
            RateLimit {
                new_connections_burst: Some(10),
                ..RateLimit::default()
            },
        ),
    ];
    for (expected_field, rl) in cases {
        match validate(rl) {
            Err(RateLimitError::BurstWithoutRate { field }) => assert_eq!(
                field, *expected_field,
                "burst-without-rate must name the offending field"
            ),
            other => panic!("expected BurstWithoutRate on {expected_field}, got {other:?}"),
        }
    }
}

#[test]
fn burst_at_rate_default_accepted() {
    // Hidden default is 1× rate; an explicit override that equals
    // rate must pass (it's the same as omitting the override).
    let rl = RateLimit {
        bandwidth_in_bps: Some(1_000_000),
        bandwidth_in_burst: Some(1_000_000),
        ..RateLimit::default()
    };
    assert!(validate(&rl).is_ok());
}

#[test]
fn burst_at_floor_accepted() {
    // burst >= rate / 100 — exactly at the floor is permitted.
    let rl = RateLimit {
        bandwidth_in_bps: Some(1_000_000),
        bandwidth_in_burst: Some(10_000), // 1_000_000 / 100
        ..RateLimit::default()
    };
    assert!(validate(&rl).is_ok());
}

#[test]
fn burst_at_ceiling_accepted() {
    // burst <= rate * 60 — exactly at the ceiling is permitted.
    let rl = RateLimit {
        bandwidth_in_bps: Some(1_000_000),
        bandwidth_in_burst: Some(60_000_000), // 1_000_000 * 60
        ..RateLimit::default()
    };
    assert!(validate(&rl).is_ok());
}

#[test]
fn burst_below_floor_rejected() {
    let rl = RateLimit {
        bandwidth_in_bps: Some(1_000_000),
        bandwidth_in_burst: Some(100), // < 10_000 floor
        ..RateLimit::default()
    };
    assert!(matches!(
        validate(&rl),
        Err(RateLimitError::BurstRange {
            field: "bandwidth_in_burst",
            ..
        })
    ));
}

#[test]
fn burst_above_ceiling_rejected() {
    let rl = RateLimit {
        bandwidth_in_bps: Some(1_000_000),
        bandwidth_in_burst: Some(70_000_000), // > 60_000_000 ceiling
        ..RateLimit::default()
    };
    assert!(matches!(
        validate(&rl),
        Err(RateLimitError::BurstRange {
            field: "bandwidth_in_burst",
            ..
        })
    ));
}

#[test]
fn burst_range_carries_diagnostic_payload() {
    // Operators see this in the HTTP body — the bounds and offered
    // value matter for debugging.
    let rl = RateLimit {
        bandwidth_in_bps: Some(1_000_000),
        bandwidth_in_burst: Some(50),
        ..RateLimit::default()
    };
    match validate(&rl) {
        Err(RateLimitError::BurstRange {
            field,
            rate,
            burst,
            lo,
            hi,
        }) => {
            assert_eq!(field, "bandwidth_in_burst");
            assert_eq!(rate, 1_000_000);
            assert_eq!(burst, 50);
            assert_eq!(lo, 10_000);
            assert_eq!(hi, 60_000_000);
        }
        other => panic!("expected BurstRange, got {other:?}"),
    }
}

#[test]
fn new_connections_burst_obeys_same_window() {
    // u32 cap-rate analogue. burst < rate/100 still rejects.
    let rl = RateLimit {
        new_connections_per_sec: Some(1000),
        new_connections_burst: Some(2), // < floor of 10
        ..RateLimit::default()
    };
    assert!(matches!(
        validate(&rl),
        Err(RateLimitError::BurstRange {
            field: "new_connections_burst",
            ..
        })
    ));
}

#[test]
fn fail_fast_first_failure_wins() {
    // Multiple failures present — validate should stop on the first
    // it encounters, matching the operator-visible "one error per
    // push" contract.
    let rl = RateLimit {
        bandwidth_in_bps: Some(0),           // CapZero
        new_connections_burst: Some(99_999), // BurstWithoutRate
        ..RateLimit::default()
    };
    assert!(matches!(
        validate(&rl),
        Err(RateLimitError::CapZero {
            field: "bandwidth_in_bps"
        })
    ));
}

#[test]
fn full_envelope_passes() {
    // A realistic operator-shaped envelope with every cap and
    // every burst override at sane values.
    let rl = RateLimit {
        bandwidth_in_bps: Some(1_048_576), // 1 MB/s
        bandwidth_out_bps: Some(1_048_576),
        new_connections_per_sec: Some(50),
        concurrent_connections: Some(200),
        bandwidth_in_burst: Some(2_097_152), // 2× rate
        bandwidth_out_burst: Some(2_097_152),
        new_connections_burst: Some(100), // 2× rate
    };
    assert!(validate(&rl).is_ok());
}

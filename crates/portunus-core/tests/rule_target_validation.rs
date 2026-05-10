//! Validation tests for `RuleTarget` (T008).
//!
//! Spec coverage: V-T1..V-T4 + V-R5 (FR-001, FR-005) from
//! `specs/007-multi-target-failover/data-model.md` § 1.

use portunus_core::{MAX_TARGETS_PER_RULE, RuleTarget, RuleTargetError, rule_target};

fn t(host: &str, port: u16, priority: u32) -> RuleTarget {
    RuleTarget {
        host: host.to_string(),
        port,
        priority,
        proxy_protocol: None,
    }
}

#[test]
fn empty_list_rejected() {
    let err = rule_target::validate(&[]).unwrap_err();
    assert!(matches!(err, RuleTargetError::Empty));
}

#[test]
fn single_target_accepted() {
    rule_target::validate(&[t("primary.example.com", 80, 0)]).unwrap();
}

#[test]
fn max_targets_accepted() {
    let targets: Vec<_> = (0..MAX_TARGETS_PER_RULE)
        .map(|i| {
            t(
                &format!("host{i}.example.com"),
                80,
                u32::try_from(i).unwrap(),
            )
        })
        .collect();
    rule_target::validate(&targets).unwrap();
}

#[test]
fn nine_targets_rejected() {
    let targets: Vec<_> = (0..=MAX_TARGETS_PER_RULE)
        .map(|i| {
            t(
                &format!("host{i}.example.com"),
                80,
                u32::try_from(i).unwrap(),
            )
        })
        .collect();
    let err = rule_target::validate(&targets).unwrap_err();
    assert!(matches!(err, RuleTargetError::TooMany(n) if n == MAX_TARGETS_PER_RULE + 1));
}

#[test]
fn empty_host_rejected() {
    let err = rule_target::validate(&[t("", 80, 0)]).unwrap_err();
    assert!(matches!(err, RuleTargetError::EmptyHost { index: 0 }));
}

#[test]
fn invalid_host_syntax_rejected() {
    // Underscore is invalid in RFC 1123 hostnames; the existing
    // Target::parse rejects it.
    let err = rule_target::validate(&[t("foo_bar.example", 80, 0)]).unwrap_err();
    assert!(matches!(err, RuleTargetError::InvalidHost { index: 0, .. }));
}

#[test]
fn port_zero_rejected() {
    let err = rule_target::validate(&[t("a.test", 0, 0)]).unwrap_err();
    assert!(matches!(
        err,
        RuleTargetError::InvalidPort { index: 0, port: 0 }
    ));
}

#[test]
fn duplicate_host_port_rejected_even_at_different_priorities() {
    // Two entries sharing (host, port) — priorities differ, V-T3 still
    // fires. Operators must dedupe (the system has no use for "try
    // the same upstream twice" given the Failed-state propagates
    // immediately to the next attempt).
    let err = rule_target::validate(&[t("a.test", 80, 0), t("b.test", 80, 1), t("a.test", 80, 5)])
        .unwrap_err();
    match err {
        RuleTargetError::Duplicate { first, second, .. } => {
            assert_eq!(first, 0);
            assert_eq!(second, 2);
        }
        other => panic!("expected Duplicate, got {other:?}"),
    }
}

#[test]
fn same_host_different_port_accepted() {
    // Same host on different ports is a perfectly fine fan-out — e.g.
    // two replicas behind a single proxy.
    rule_target::validate(&[t("a.test", 80, 0), t("a.test", 8080, 1)]).unwrap();
}

#[test]
fn same_priority_value_accepted_ties_broken_by_row_order() {
    // The selection algorithm (data-model.md §3) sorts by
    // (priority, row_index). Two targets with priority=0 are legal —
    // the validator does NOT enforce priority uniqueness.
    rule_target::validate(&[t("a.test", 80, 0), t("b.test", 80, 0)]).unwrap();
}

#[test]
fn ipv4_literal_accepted() {
    rule_target::validate(&[t("192.0.2.1", 80, 0)]).unwrap();
}

#[test]
fn bracketed_ipv6_accepted() {
    rule_target::validate(&[t("[2001:db8::1]", 80, 0)]).unwrap();
}

#[test]
fn unbracketed_ipv6_rejected() {
    // The existing Target::parse demands bracketed IPv6; V-T1 rides
    // that behavior so operators get one consistent host syntax.
    let err = rule_target::validate(&[t("2001:db8::1", 80, 0)]).unwrap_err();
    assert!(matches!(err, RuleTargetError::InvalidHost { index: 0, .. }));
}

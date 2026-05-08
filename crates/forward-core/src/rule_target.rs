//! Multi-target rule entity (spec 007-multi-target-failover).
//!
//! A `RuleTarget` is a single upstream within a forwarding rule. Lives
//! inline on `Rule` in v0.7+. The pre-existing `forward_core::Target`
//! type classifies a host string (IP vs DNS) — `RuleTarget` reuses it
//! for V-T1 host validation.
//!
//! Spec: `specs/007-multi-target-failover/data-model.md` § 1.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::target::{Target, TargetError};

/// Hard cap on the number of targets a rule may carry. Beyond this the
/// strict-priority policy stops being a useful operator model
/// (priority order beyond rank ~3 has no sensible semantics, and a
/// 100-target rule is almost certainly a misuse). Future weighted
/// policies can lift this cap.
pub const MAX_TARGETS_PER_RULE: usize = 8;

/// One upstream within a forwarding rule.
///
/// Stored inline on `Rule.targets` in priority order. Lower `priority`
/// = higher preference. Two targets MAY share a `priority` value;
/// stable ties broken by row order on the operator's submission.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RuleTarget {
    pub host: String,
    pub port: u16,
    pub priority: u32,
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum RuleTargetError {
    /// The targets list was empty. `Rule.targets` MUST have at least
    /// one entry (FR-001); the legacy single-target shape is promoted
    /// to a one-element list at parse time, so an empty list reaching
    /// validation indicates the operator submitted neither shape.
    #[error("targets_empty: rule must carry at least one target")]
    Empty,

    /// More than `MAX_TARGETS_PER_RULE` entries (V-T4).
    #[error(
        "targets_too_many: at most {} targets allowed per rule, got {0}",
        MAX_TARGETS_PER_RULE
    )]
    TooMany(usize),

    /// `host` is the empty string (V-T1 lower bound). The host
    /// classifier rejects empty too, but this error message is
    /// clearer for operators who supplied `{"host":"","port":80}`.
    #[error("target_invalid_host: target {index} has empty host")]
    EmptyHost { index: usize },

    /// `host` failed `forward_core::Target::parse` (V-T1 upper bound).
    #[error("target_invalid_host: target {index} has invalid host {host:?} ({source})")]
    InvalidHost {
        index: usize,
        host: String,
        #[source]
        source: TargetError,
    },

    /// `port` is 0 or > 65535 — though `u16` already constrains the
    /// upper bound so this only ever fires for 0 (V-T2).
    #[error("target_invalid_port: target {index} has invalid port {port} (must be 1..=65535)")]
    InvalidPort { index: usize, port: u16 },

    /// Two targets share the same `(host, port)` pair (FR-005 / V-T3).
    /// The duplicate-detection compares the host strings byte-equal
    /// after they were each accepted by `Target::parse`, which
    /// canonicalises whitespace but is otherwise case-preserving.
    #[error(
        "targets_duplicate: targets at indices {first} and {second} share (host,port) ({host}:{port})"
    )]
    Duplicate {
        first: usize,
        second: usize,
        host: String,
        port: u16,
    },
}

/// Validate a fully-assembled `targets` list against V-T1..V-T4 + V-R5.
///
/// Returns the list back on success so callers can chain the validation
/// into the construction of a `Rule`.
///
/// # Errors
///
/// Returns the first failure encountered. Validation is intentionally
/// fail-fast — operators want one clear error per push, not a
/// kitchen-sink report.
pub fn validate(targets: &[RuleTarget]) -> Result<(), RuleTargetError> {
    if targets.is_empty() {
        return Err(RuleTargetError::Empty);
    }
    if targets.len() > MAX_TARGETS_PER_RULE {
        return Err(RuleTargetError::TooMany(targets.len()));
    }

    for (index, t) in targets.iter().enumerate() {
        if t.host.is_empty() {
            return Err(RuleTargetError::EmptyHost { index });
        }
        if t.port == 0 {
            return Err(RuleTargetError::InvalidPort {
                index,
                port: t.port,
            });
        }
        if let Err(source) = Target::parse(&t.host) {
            return Err(RuleTargetError::InvalidHost {
                index,
                host: t.host.clone(),
                source,
            });
        }
    }

    // O(N^2) duplicate detection. With MAX_TARGETS_PER_RULE = 8 this
    // is at most 28 comparisons per rule — well below any threshold
    // worth a HashSet allocation.
    for i in 0..targets.len() {
        for j in (i + 1)..targets.len() {
            if targets[i].host == targets[j].host && targets[i].port == targets[j].port {
                return Err(RuleTargetError::Duplicate {
                    first: i,
                    second: j,
                    host: targets[i].host.clone(),
                    port: targets[i].port,
                });
            }
        }
    }

    Ok(())
}

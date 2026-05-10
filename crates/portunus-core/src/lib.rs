//! Shared primitives for `Portunus`.
//!
//! This crate is intentionally small: error taxonomy, ID newtypes, certificate
//! fingerprint helpers, and config-file loading. Authentication lives in
//! `portunus-auth` (Constitution Principle I — single auth seam).

pub mod config;
pub mod error;
pub mod fingerprint;
pub mod hostname;
pub mod id;
pub mod log_redact;
pub mod peek_histogram;
pub mod port_range;
pub mod rate_limit;
pub mod rule_target;
pub mod target;

pub use error::PortunusError;
pub use hostname::{Hostname, HostnameError};
pub use id::{ClientName, ClientNameError, RequestId, RuleId};
pub use peek_histogram::PEEK_HISTOGRAM_BUCKETS_SECS;
pub use port_range::{PortRange, PortRangeError};
pub use rate_limit::{RateLimit, RateLimitError, RejectReason};
pub use rule_target::{MAX_TARGETS_PER_RULE, ProxyProtocolVersion, RuleTarget, RuleTargetError};
pub use target::{Target, TargetError};

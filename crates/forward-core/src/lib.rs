//! Shared primitives for `forward-rs`.
//!
//! This crate is intentionally small: error taxonomy, ID newtypes, certificate
//! fingerprint helpers, and config-file loading. Authentication lives in
//! `forward-auth` (Constitution Principle I — single auth seam).

pub mod config;
pub mod error;
pub mod fingerprint;
pub mod hostname;
pub mod id;
pub mod log_redact;
pub mod port_range;
pub mod target;

pub use error::ForwardError;
pub use hostname::{Hostname, HostnameError};
pub use id::{ClientName, ClientNameError, RequestId, RuleId};
pub use port_range::{PortRange, PortRangeError};
pub use target::{Target, TargetError};

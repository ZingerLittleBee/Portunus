//! Shared primitives for `forward-rs`.
//!
//! This crate is intentionally small: error taxonomy, ID newtypes, certificate
//! fingerprint helpers, and config-file loading. Authentication lives in
//! `forward-auth` (Constitution Principle I — single auth seam).

pub mod config;
pub mod error;
pub mod fingerprint;
pub mod id;
pub mod log_redact;
pub mod port_range;

pub use error::ForwardError;
pub use id::{ClientName, ClientNameError, RequestId, RuleId};
pub use port_range::{PortRange, PortRangeError};

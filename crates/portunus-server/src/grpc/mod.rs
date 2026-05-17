//! gRPC control-plane service: `portunus.v1.Control`.
//!
//! - [`interceptor`] reads the bearer token, calls into `portunus-auth`, and
//!   inserts the resulting `ClientIdentity` into request extensions.
//! - [`service`] holds the server-side `Control` impl. The `Channel`
//!   bidirectional stream registers the client in [`crate::clients`] and
//!   pumps server→client `RuleUpdate` (US2) / client→server `RuleStatus`
//!   and `StatsReport` (US2/US3).

pub mod enrollment;
pub mod interceptor;
pub mod service;

//! 011-rate-limiting-qos — portunus-client data-plane rate limiting.
//!
//! Hand-rolled token buckets ([`bucket`]) plus per-rule and per-owner
//! limiters ([`scope`], landed in T018). The no-cap fast path is
//! branch-free: callers wrap their limiter in `Option<Arc<…>>` and
//! the `is_none` branch compiles to a single null check.
//!
//! Several items in these submodules (e.g. `BandwidthAcquire`,
//! `RuleRateLimiter::acquire_bandwidth`, `RateLimitScopeManager`)
//! are not yet wired from the binary's hot paths — they're
//! consumed by upcoming tasks (T020 bandwidth throttle, T024+
//! per-owner registry, T033 hot-reload swap). The blanket
//! `allow(dead_code)` prevents the binary-crate dead-code analyser
//! from flagging them until those landings ship.

#![allow(dead_code)]

pub mod bucket;
pub mod copy;
pub mod scope;
pub mod stats;

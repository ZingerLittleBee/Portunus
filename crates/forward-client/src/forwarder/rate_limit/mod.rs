//! 011-rate-limiting-qos — forward-client data-plane rate limiting.
//!
//! Hand-rolled token buckets ([`bucket`]) plus per-rule and per-owner
//! limiters ([`scope`], landed in T018). The no-cap fast path is
//! branch-free: callers wrap their limiter in `Option<Arc<…>>` and
//! the `is_none` branch compiles to a single null check.

pub mod bucket;

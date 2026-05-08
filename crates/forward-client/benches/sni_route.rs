//! Bench scaffold for SNI routing. Spec 009-tls-sni-routing T005 placeholder.
//!
//! Real benches land in:
//! - T087 (`SniRoutingTable::lookup` ns/op at 100/1000/10000 routes)
//! - T088 (connection-setup-latency vs v0.7 plain-TCP baseline)
//!
//! This file currently keeps the `[[bench]]` Cargo entry buildable.

fn main() {}

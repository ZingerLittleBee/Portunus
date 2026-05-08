//! SNI routing data plane for `forward-client`.
//!
//! Spec: 009-tls-sni-routing. Modules in this tree implement the
//! ClientHello peek + parse + lookup pipeline used by SNI listeners
//! when at least one TCP single-port rule on the listener carries a
//! non-empty `sni_pattern`. Legacy plain-TCP listeners never enter
//! these modules — see `crates/forward-client/src/forwarder/mod.rs`
//! for the dispatch decision.

pub mod client_hello;
pub mod listener;
pub mod peek;
pub mod route_table;

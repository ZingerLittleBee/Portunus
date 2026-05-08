//! Hand-rolled TLS ClientHello parser. Spec 009-tls-sni-routing R-001.
//!
//! Reads only the `server_name` extension; skips everything else by
//! length. Tracks handshake-fragment reassembly across multiple TLS
//! records (RFC 8446 §5.1).
//!
//! NOTE (Phase 1 — T002 scaffold): bodies stubbed. T036 in Phase 3
//! fills in the real parser.

#![allow(dead_code)]

#[derive(Debug, PartialEq, Eq)]
pub enum ParseOutcome {
    /// Need more bytes — caller continues reading.
    Truncated,
    /// Successful parse. `Some(host)` when `server_name` extension
    /// is present; `None` when ClientHello is valid but lacks SNI.
    Ok(Option<String>),
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    /// Bytes are not TLS (wrong record type / version).
    NotTls,
    /// TLS record looks structurally invalid.
    Malformed,
}

/// Parse a (possibly partial) ClientHello buffer.
///
/// T036: implement.
pub fn parse(_bytes: &[u8]) -> Result<ParseOutcome, ParseError> {
    unimplemented!("009-tls-sni-routing T036")
}

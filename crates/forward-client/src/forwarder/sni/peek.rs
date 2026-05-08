//! Async ClientHello peek. Spec 009-tls-sni-routing R-009.
//!
//! Reads from the accepted TCP stream until `client_hello::parse`
//! returns `Ok` or one of the budgets (3 s timeout, 64 KiB cap) is
//! exhausted.
//!
//! NOTE (Phase 1 — T002 scaffold): body stubbed. T038 in Phase 3
//! fills in the real implementation.

#![allow(dead_code)]

use std::time::Duration;

use tokio::net::TcpStream;

#[derive(Debug)]
pub enum PeekError {
    Timeout { bytes_read: usize },
    NotTls,
    Malformed,
    SizeCap,
    Io(std::io::Error),
}

pub const PEEK_TIMEOUT: Duration = Duration::from_secs(3);
pub const PEEK_BYTE_CAP: usize = 64 * 1024;

/// Peek the ClientHello and return the captured buffer + parsed SNI.
/// T038: implement.
pub async fn read_client_hello(
    _stream: &mut TcpStream,
) -> Result<(Vec<u8>, Option<String>), PeekError> {
    unimplemented!("009-tls-sni-routing T038")
}

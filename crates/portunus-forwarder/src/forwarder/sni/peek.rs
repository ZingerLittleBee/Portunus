//! Async ClientHello peek. Spec 009-tls-sni-routing R-009.
//!
//! Reads from the accepted TCP stream until `client_hello::parse`
//! returns `Ok` or one of the budgets is exhausted:
//!   * **Timeout**: 3 s wall-clock since the first read.
//!   * **Size cap**: 64 KiB total bytes accumulated. Real TLS
//!     ClientHellos rarely exceed 1 KiB; the cap protects against a
//!     malicious peer who streams forever without ever finishing a
//!     handshake (R-009).
//!
//! Returns `(captured_bytes, parsed_sni)` on success. The captured
//! buffer MUST be replayed to the upstream verbatim by
//! `proxy::proxy(_, preread, _)` (T041) so the L4 byte-for-byte
//! invariant holds — we are *peeking*, not consuming.
//!
//! Error → tracing event mapping (R-009 / contracts/operator-api.md §5):
//! - `Timeout`        → `tls.client_hello_timeout` (WARN)
//! - `NotTls`         → `tls.parse_failed` (WARN)
//! - `Malformed`      → `tls.parse_failed` (WARN)
//! - `SizeCap`        → `tls.parse_failed` (WARN)
//! - `Io(_)`          → `tls.parse_failed` (WARN)
//!
//! The listener (T040) catches and emits the right event; this
//! module just returns the typed error.

use std::time::Duration;

use tokio::io::AsyncReadExt;

use super::client_hello::{ParseError, ParseOutcome, parse};

#[derive(Debug)]
pub enum PeekError {
    /// Timeout fired after `PEEK_TIMEOUT` of inactivity / partial
    /// data. `bytes_read` is the count accumulated so far.
    Timeout { bytes_read: usize },
    /// Bytes do not look like TLS at all.
    NotTls,
    /// Bytes are TLS handshake but the inner framing is corrupt.
    Malformed,
    /// Buffer reached `PEEK_BYTE_CAP` without `parse` succeeding.
    /// Treat as a malicious peer.
    SizeCap,
    /// Peer closed the connection / read returned 0 / underlying
    /// I/O error. The inner `io::Error` is carried for diagnostics
    /// (logged by the listener); not part of the public surface.
    Io(#[allow(dead_code)] std::io::Error),
}

#[cfg(test)]
impl PeekError {
    /// Map to the operator-API tracing event name (contracts/operator-api.md §5).
    /// Test-only: the listener inlines this mapping at the call site.
    pub(crate) fn tracing_event(&self) -> &'static str {
        match self {
            PeekError::Timeout { .. } => "tls.client_hello_timeout",
            _ => "tls.parse_failed",
        }
    }
}

pub const PEEK_TIMEOUT: Duration = Duration::from_secs(3);
pub const PEEK_BYTE_CAP: usize = 64 * 1024;
/// Per-read chunk size; a typical TLS 1.2/1.3 ClientHello fits in one
/// kernel buffer, so 4 KiB strikes a balance between syscall amortisation
/// and the size cap.
const CHUNK: usize = 4 * 1024;

/// Peek the ClientHello and return the captured buffer + parsed SNI.
///
/// Generic over `AsyncRead + Unpin` so tests can drive it with
/// `tokio::io::duplex()` without spinning up a real TCP listener.
/// Production callers pass a `&mut TcpStream`.
pub async fn read_client_hello<R>(stream: &mut R) -> Result<(Vec<u8>, Option<String>), PeekError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    read_client_hello_with(stream, PEEK_TIMEOUT, PEEK_BYTE_CAP).await
}

/// Internal entry point used by the public `read_client_hello` and by
/// the unit tests (so they can use a sub-second timeout).
pub async fn read_client_hello_with<R>(
    stream: &mut R,
    timeout: Duration,
    cap: usize,
) -> Result<(Vec<u8>, Option<String>), PeekError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let deadline = tokio::time::Instant::now() + timeout;
    let mut buf: Vec<u8> = Vec::with_capacity(CHUNK);

    loop {
        // First try parse — works the moment the buffer holds a
        // complete ClientHello record. Empty buffer always returns
        // Truncated (covered in client_hello unit tests).
        match parse(&buf) {
            Ok(ParseOutcome::Ok(sni)) => return Ok((buf, sni)),
            Ok(ParseOutcome::Truncated) => {} // need more bytes
            Err(ParseError::NotTls) => return Err(PeekError::NotTls),
            Err(ParseError::Malformed) => return Err(PeekError::Malformed),
        }

        if buf.len() >= cap {
            return Err(PeekError::SizeCap);
        }

        // Read more bytes with a deadline. We read one chunk per
        // iteration so the timeout still applies even if the peer
        // is dripping bytes. Capacity grows in CHUNK steps.
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Err(PeekError::Timeout {
                bytes_read: buf.len(),
            });
        }
        let remaining = deadline - now;
        let mut chunk = vec![0u8; CHUNK.min(cap - buf.len())];

        let read_fut = stream.read(&mut chunk);
        let n = match tokio::time::timeout(remaining, read_fut).await {
            Ok(Ok(0)) => {
                // Peer closed — treat as I/O error so the listener
                // surfaces tls.parse_failed (no SNI, no TLS bytes
                // worth chasing). The peer simply gave up.
                return Err(PeekError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "peer closed before ClientHello complete",
                )));
            }
            Ok(Ok(n)) => n,
            Ok(Err(e)) => return Err(PeekError::Io(e)),
            Err(_elapsed) => {
                return Err(PeekError::Timeout {
                    bytes_read: buf.len(),
                });
            }
        };
        chunk.truncate(n);
        buf.extend_from_slice(&chunk);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forwarder::sni::client_hello::build_client_hello;
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;

    /// Helper: feed `bytes` into a duplex pipe in chunks of `chunk`
    /// with `gap` ms between writes. The reader half is what
    /// `read_client_hello` peeks. Returns the reader half so the
    /// test can drive the peek.
    fn pipe_with(bytes: Vec<u8>, chunk: usize, gap: Duration) -> tokio::io::DuplexStream {
        let (mut writer, reader) = tokio::io::duplex(64 * 1024);
        tokio::spawn(async move {
            for piece in bytes.chunks(chunk.max(1)) {
                let _ = writer.write_all(piece).await;
                let _ = writer.flush().await;
                if !gap.is_zero() {
                    tokio::time::sleep(gap).await;
                }
            }
            // Don't drop the writer prematurely; it's fine to close
            // after the full payload landed.
        });
        reader
    }

    #[tokio::test]
    async fn happy_path_one_shot() {
        let bytes = build_client_hello(Some("api.example.com"));
        let mut reader = pipe_with(bytes.clone(), bytes.len(), Duration::ZERO);
        let (captured, sni) = read_client_hello(&mut reader).await.expect("peek");
        assert!(captured.starts_with(&bytes));
        assert_eq!(sni.as_deref(), Some("api.example.com"));
    }

    #[tokio::test]
    async fn drip_feed_assembles() {
        let bytes = build_client_hello(Some("dripped.example.com"));
        let mut reader = pipe_with(bytes.clone(), 7, Duration::from_millis(2));
        let (captured, sni) = read_client_hello(&mut reader).await.expect("peek");
        assert_eq!(captured.len(), bytes.len());
        assert_eq!(sni.as_deref(), Some("dripped.example.com"));
    }

    #[tokio::test]
    async fn timeout_when_peer_silent() {
        // Open a duplex stream and never write — read_client_hello
        // must time out.
        let (_writer, mut reader) = tokio::io::duplex(64 * 1024);
        let err = read_client_hello_with(&mut reader, Duration::from_millis(50), PEEK_BYTE_CAP)
            .await
            .expect_err("must time out");
        let PeekError::Timeout { bytes_read: 0 } = err else {
            panic!("got {err:?}");
        };
        assert_eq!(err.tracing_event(), "tls.client_hello_timeout");
    }

    #[tokio::test]
    async fn not_tls_payload_rejected() {
        let mut reader = pipe_with(b"GET / HTTP/1.1\r\n\r\n".to_vec(), 64, Duration::ZERO);
        let err = read_client_hello(&mut reader).await.expect_err("not TLS");
        let PeekError::NotTls = err else {
            panic!("got {err:?}");
        };
        assert_eq!(err.tracing_event(), "tls.parse_failed");
    }

    #[tokio::test]
    async fn no_sni_extension_returns_none() {
        let bytes = build_client_hello(None);
        let mut reader = pipe_with(bytes.clone(), bytes.len(), Duration::ZERO);
        let (_captured, sni) = read_client_hello(&mut reader).await.expect("peek");
        assert_eq!(sni, None);
    }

    #[tokio::test]
    async fn malformed_tls_record_rejected() {
        // Record type/version look like TLS, but the advertised body
        // length (0x8000 = 32 KiB) exceeds the parser's per-record cap
        // so `parse` returns `ParseError::Malformed` on the first
        // iteration — mapped here to `PeekError::Malformed`.
        let payload = vec![0x16, 0x03, 0x03, 0x80, 0x00];
        let mut reader = pipe_with(payload, 64, Duration::ZERO);
        let err = read_client_hello(&mut reader)
            .await
            .expect_err("malformed TLS");
        let PeekError::Malformed = err else {
            panic!("got {err:?}");
        };
        assert_eq!(err.tracing_event(), "tls.parse_failed");
    }

    #[tokio::test]
    async fn zero_timeout_returns_timeout_before_first_read() {
        // A zero timeout makes the deadline equal the start instant, so
        // the in-loop `now >= deadline` guard fires before we ever issue
        // a read. Buffer is still empty, so `bytes_read` is 0.
        let (_writer, mut reader) = tokio::io::duplex(64 * 1024);
        let err = read_client_hello_with(&mut reader, Duration::ZERO, PEEK_BYTE_CAP)
            .await
            .expect_err("must time out");
        let PeekError::Timeout { bytes_read: 0 } = err else {
            panic!("got {err:?}");
        };
        assert_eq!(err.tracing_event(), "tls.client_hello_timeout");
    }

    #[tokio::test]
    async fn read_io_error_surfaces_as_io() {
        // A reader whose first poll_read yields an I/O error (other than
        // EOF) must surface as `PeekError::Io`, not Timeout or SizeCap.
        let mut reader = tokio_test::io::Builder::new()
            .read_error(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "boom",
            ))
            .build();
        let err = read_client_hello_with(&mut reader, Duration::from_secs(1), PEEK_BYTE_CAP)
            .await
            .expect_err("io error");
        let PeekError::Io(ref e) = err else {
            panic!("got {err:?}");
        };
        assert_eq!(e.kind(), std::io::ErrorKind::ConnectionReset);
        assert_eq!(err.tracing_event(), "tls.parse_failed");
    }

    #[tokio::test]
    async fn size_cap_bounds_garbage_streamer() {
        // Cap intentionally tiny so we hit it without writing 64 KiB.
        // Record header advertises a 1 KiB body — within the parser's
        // 16 KiB record cap, so it returns Truncated until we run
        // out of buffer. Then ship enough bytes (>= cap) so we hit
        // SizeCap before the parser finishes.
        let mut payload = vec![0x16, 0x03, 0x03, 0x04, 0x00]; // record advertises 1024-byte body
        payload.extend_from_slice(&vec![0u8; 600]); // partial body
        let mut reader = pipe_with(payload, 64, Duration::ZERO);
        let err = read_client_hello_with(&mut reader, Duration::from_secs(1), 256)
            .await
            .expect_err("must size-cap");
        let PeekError::SizeCap = err else {
            panic!("got {err:?}");
        };
    }

    #[tokio::test]
    async fn peer_closes_before_hello_surfaces_io_eof() {
        // Writer half dropped immediately with no bytes written: the
        // first read returns Ok(0), which maps to `PeekError::Io` with
        // an `UnexpectedEof` kind (the "peer closed" branch).
        let (writer, mut reader) = tokio::io::duplex(64 * 1024);
        drop(writer);
        let err = read_client_hello_with(&mut reader, Duration::from_secs(1), PEEK_BYTE_CAP)
            .await
            .expect_err("peer closed");
        let PeekError::Io(ref e) = err else {
            panic!("got {err:?}");
        };
        assert_eq!(e.kind(), std::io::ErrorKind::UnexpectedEof);
        assert_eq!(err.tracing_event(), "tls.parse_failed");
    }
}

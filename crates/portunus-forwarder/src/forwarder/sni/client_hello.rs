//! Hand-rolled TLS ClientHello parser. Spec 009-tls-sni-routing R-001.
//!
//! Reads only the `server_name` extension; skips everything else by
//! length. Operates on a single record's worth of bytes (R-015) — the
//! caller (`peek::read_client_hello`) feeds the buffer incrementally
//! and re-invokes after each `read`, so a multi-record fragmented
//! ClientHello (rare, but legal per RFC 5246/8446 §5.1) is handled at
//! that layer rather than here.
//!
//! Wire format reference (RFC 5246 §6.2 / RFC 8446 §5):
//!
//! ```text
//! TLSPlaintext (record header):
//!   type            uint8        // 0x16 = handshake
//!   legacy_version  uint16       // 0x0301..0x0303
//!   length          uint16       // up to 2^14 = 16384
//!   fragment        opaque[length]
//!
//! Handshake (inside fragment):
//!   msg_type        uint8        // 0x01 = ClientHello
//!   length          uint24
//!   body            opaque[length]
//!
//! ClientHello body:
//!   legacy_version           uint16
//!   random                   opaque[32]
//!   legacy_session_id        opaque<0..32>      // 1-byte length
//!   cipher_suites            opaque<2..2^16-2>  // 2-byte length
//!   legacy_compression_methods opaque<1..2^8-1> // 1-byte length
//!   extensions               opaque<8..2^16-1>  // 2-byte length
//!
//! Extension (server_name, type 0x0000):
//!   extension_type  uint16
//!   extension_data  opaque<0..2^16-1>           // 2-byte length
//!     server_name_list  opaque<1..2^16-1>       // 2-byte length
//!       NameType       uint8                    // 0x00 = host_name
//!       HostName       opaque<1..2^16-1>        // 2-byte length
//! ```
//!
//! Behaviour:
//! - Returns `Truncated` whenever any length-prefixed field would
//!   read past the end of `bytes`. The caller MUST then read more
//!   bytes and retry.
//! - Returns `Ok(None)` for a structurally valid ClientHello whose
//!   extensions list contains no `server_name` extension OR whose
//!   `server_name_list` is empty.
//! - Returns `Ok(Some(host))` when a `server_name` of NameType
//!   `host_name` (0x00) is present. The hostname is lowercased and
//!   trimmed; trailing-dot tolerated.
//! - Returns `Err(NotTls)` when the record header doesn't look like
//!   TLS handshake at all (wrong content type, totally bogus
//!   version). The caller maps this to the `tls.parse_failed`
//!   tracing event.
//! - Returns `Err(Malformed)` when the bytes ARE TLS handshake but
//!   the inner framing is corrupt (length mismatches, illegal
//!   structure). Same operator surface as `NotTls`.

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

/// Maximum on-wire ClientHello body we tolerate inside one record.
/// TLS records cap at 2^14 fragment bytes per RFC 5246 §6.2.1; we
/// match that here so a malicious peer can't hand-craft a value to
/// drag the parser into wasted work. The `peek` layer enforces an
/// outer 64 KiB cap across all bytes read.
const MAX_HANDSHAKE_BODY: usize = 1 << 14;

/// `0x16` — TLS handshake content type.
const RECORD_TYPE_HANDSHAKE: u8 = 0x16;

/// `0x01` — ClientHello message type inside the handshake layer.
const HS_MSG_CLIENT_HELLO: u8 = 0x01;

/// Server Name Indication extension (RFC 6066 §3).
const EXT_SERVER_NAME: u16 = 0x0000;

/// `host_name` NameType.
const SNI_NAME_TYPE_HOST: u8 = 0x00;

/// Parse a (possibly partial) ClientHello buffer. See module doc
/// for behaviour and outcomes.
pub fn parse(bytes: &[u8]) -> Result<ParseOutcome, ParseError> {
    // Record header (5 bytes).
    if bytes.len() < 5 {
        return Ok(ParseOutcome::Truncated);
    }
    if bytes[0] != RECORD_TYPE_HANDSHAKE {
        return Err(ParseError::NotTls);
    }
    // Major version 0x03 covers SSL 3.0 / TLS 1.0..1.3 (TLS 1.3 keeps
    // the legacy_version at 0x0303 in the record header). Anything
    // else is "definitely not TLS we care about".
    if bytes[1] != 0x03 {
        return Err(ParseError::NotTls);
    }
    let record_len = u16::from_be_bytes([bytes[3], bytes[4]]) as usize;
    if record_len == 0 || record_len > MAX_HANDSHAKE_BODY {
        return Err(ParseError::Malformed);
    }
    if bytes.len() < 5 + record_len {
        return Ok(ParseOutcome::Truncated);
    }
    let fragment = &bytes[5..5 + record_len];

    // Handshake header (4 bytes).
    if fragment.len() < 4 {
        return Err(ParseError::Malformed);
    }
    if fragment[0] != HS_MSG_CLIENT_HELLO {
        // Other handshake types (ServerHello etc.) cannot be the
        // first thing on a fresh accepted stream — treat as
        // malformed, NOT NotTls (the record type was correct).
        return Err(ParseError::Malformed);
    }
    let hs_len =
        ((fragment[1] as usize) << 16) | ((fragment[2] as usize) << 8) | (fragment[3] as usize);
    if hs_len > fragment.len() - 4 {
        // Body advertised longer than this record carries — caller
        // could in principle reassemble across records, but R-015
        // rejects that pattern (real clients send ClientHello in one
        // record). Treat as Malformed.
        return Err(ParseError::Malformed);
    }
    let body = &fragment[4..4 + hs_len];

    // ClientHello fixed prefix: legacy_version (2) + random (32) = 34.
    if body.len() < 34 {
        return Err(ParseError::Malformed);
    }
    let mut cur = Cursor::new(&body[34..]);

    // legacy_session_id: u8 length + bytes.
    let sid_len = cur.read_u8()? as usize;
    cur.skip(sid_len)?;

    // cipher_suites: u16 length + bytes.
    let cipher_suites_len = cur.read_u16()? as usize;
    cur.skip(cipher_suites_len)?;

    // legacy_compression_methods: u8 length + bytes.
    let compression_methods_len = cur.read_u8()? as usize;
    cur.skip(compression_methods_len)?;

    // extensions: u16 length. Absent if we're at the end (TLS 1.0
    // permits this; TLS 1.2+ requires it).
    if cur.is_eof() {
        return Ok(ParseOutcome::Ok(None));
    }
    let ext_total = cur.read_u16()? as usize;
    let ext_bytes = cur.take(ext_total)?;

    // Walk extensions.
    let mut ec = Cursor::new(ext_bytes);
    while !ec.is_eof() {
        let ext_type = ec.read_u16()?;
        let ext_data_len = ec.read_u16()? as usize;
        let ext_data = ec.take(ext_data_len)?;
        if ext_type != EXT_SERVER_NAME {
            continue;
        }
        // server_name_list: u16 length + entries.
        let mut sc = Cursor::new(ext_data);
        let list_len = sc.read_u16()? as usize;
        let list_bytes = sc.take(list_len)?;
        let mut lc = Cursor::new(list_bytes);
        while !lc.is_eof() {
            let name_type = lc.read_u8()?;
            let name_len = lc.read_u16()? as usize;
            let name_bytes = lc.take(name_len)?;
            if name_type == SNI_NAME_TYPE_HOST && !name_bytes.is_empty() {
                let raw = std::str::from_utf8(name_bytes)
                    .map_err(|_| ParseError::Malformed)?
                    .trim()
                    .trim_end_matches('.');
                if raw.is_empty() {
                    continue;
                }
                return Ok(ParseOutcome::Ok(Some(raw.to_ascii_lowercase())));
            }
            // Unknown name type — skip silently per RFC 6066.
        }
        // server_name extension parsed but no host_name found.
        return Ok(ParseOutcome::Ok(None));
    }

    Ok(ParseOutcome::Ok(None))
}

/// Tiny zero-allocation cursor over a byte slice. Returns
/// `ParseError::Malformed` when any read would advance past the end.
/// Returning Truncated from the cursor is impossible — the outer
/// `parse` already framed the body to `record_len`, so a short read
/// at this layer is by definition a wire framing bug, not a need
/// for more bytes.
struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn is_eof(&self) -> bool {
        self.pos >= self.buf.len()
    }

    fn read_u8(&mut self) -> Result<u8, ParseError> {
        if self.pos + 1 > self.buf.len() {
            return Err(ParseError::Malformed);
        }
        let v = self.buf[self.pos];
        self.pos += 1;
        Ok(v)
    }

    fn read_u16(&mut self) -> Result<u16, ParseError> {
        if self.pos + 2 > self.buf.len() {
            return Err(ParseError::Malformed);
        }
        let v = u16::from_be_bytes([self.buf[self.pos], self.buf[self.pos + 1]]);
        self.pos += 2;
        Ok(v)
    }

    fn skip(&mut self, n: usize) -> Result<(), ParseError> {
        if self.pos + n > self.buf.len() {
            return Err(ParseError::Malformed);
        }
        self.pos += n;
        Ok(())
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], ParseError> {
        if self.pos + n > self.buf.len() {
            return Err(ParseError::Malformed);
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
}

/// Build a minimal but valid TLS 1.2 ClientHello carrying the given
/// SNI hostname. Used by unit + integration tests across the
/// `forwarder::sni` tree (avoids an external openssl dependency for
/// fixtures — see T020..T024 for capture-based fixtures used in
/// the real-world e2e suite).
///
/// The width-clamping casts (`as u16`, `as u8`) are intentional:
/// fixture inputs are bounded short hostnames so truncation is
/// structurally impossible.
#[doc(hidden)]
#[allow(clippy::cast_possible_truncation)]
#[must_use]
pub fn build_client_hello(sni: Option<&str>) -> Vec<u8> {
    let mut body = Vec::with_capacity(256);
    body.extend_from_slice(&[0x03, 0x03]); // legacy_version TLS 1.2
    body.extend_from_slice(&[0xab; 32]); // random
    body.push(0); // session_id length 0
    // cipher_suites: one suite — TLS_AES_128_GCM_SHA256 (0x1301)
    body.extend_from_slice(&[0x00, 0x02, 0x13, 0x01]);
    // compression_methods: one method — null
    body.extend_from_slice(&[0x01, 0x00]);

    let mut exts = Vec::with_capacity(64);
    if let Some(host) = sni {
        // server_name extension.
        let mut sn_list = Vec::with_capacity(host.len() + 5);
        sn_list.push(0x00); // name_type = host_name
        sn_list.extend_from_slice(&u16::try_from(host.len()).unwrap().to_be_bytes());
        sn_list.extend_from_slice(host.as_bytes());
        let mut sn_ext_data = Vec::with_capacity(sn_list.len() + 2);
        sn_ext_data.extend_from_slice(&(sn_list.len() as u16).to_be_bytes());
        sn_ext_data.extend_from_slice(&sn_list);
        exts.extend_from_slice(&[0x00, 0x00]); // ext_type
        exts.extend_from_slice(&(sn_ext_data.len() as u16).to_be_bytes());
        exts.extend_from_slice(&sn_ext_data);
    }
    body.extend_from_slice(&u16::try_from(exts.len()).unwrap().to_be_bytes());
    body.extend_from_slice(&exts);

    let hs_len = body.len();
    let mut hs = Vec::with_capacity(hs_len + 4);
    hs.push(0x01); // ClientHello
    hs.push(((hs_len >> 16) & 0xff) as u8);
    hs.push(((hs_len >> 8) & 0xff) as u8);
    hs.push((hs_len & 0xff) as u8);
    hs.extend_from_slice(&body);

    let mut record = Vec::with_capacity(hs.len() + 5);
    record.push(0x16); // handshake
    record.extend_from_slice(&[0x03, 0x01]); // legacy version 1.0
    record.extend_from_slice(&(hs.len() as u16).to_be_bytes());
    record.extend_from_slice(&hs);
    record
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tls12_extracts_sni() {
        // T032: happy path — synthesised TLS 1.2 ClientHello with SNI.
        let bytes = build_client_hello(Some("example.com"));
        let outcome = parse(&bytes).expect("parse");
        assert_eq!(outcome, ParseOutcome::Ok(Some("example.com".to_string())));
    }

    #[test]
    fn parse_truncated_then_complete() {
        // T033: feed bytes one at a time; assert Truncated until the
        // record body is whole, then Ok(...).
        let bytes = build_client_hello(Some("api.example.com"));
        for cut in 0..bytes.len() {
            let prefix = &bytes[..cut];
            match parse(prefix) {
                Ok(ParseOutcome::Truncated) => {} // expected
                Ok(ParseOutcome::Ok(_)) => {
                    panic!("got Ok at cut={cut} before all bytes consumed");
                }
                Err(e) => panic!("got Err({e:?}) at cut={cut} (prefix len {})", prefix.len()),
            }
        }
        assert_eq!(
            parse(&bytes).expect("parse"),
            ParseOutcome::Ok(Some("api.example.com".to_string()))
        );
    }

    #[test]
    fn parse_lowercases_sni() {
        let bytes = build_client_hello(Some("API.Example.COM"));
        let outcome = parse(&bytes).expect("parse");
        assert_eq!(
            outcome,
            ParseOutcome::Ok(Some("api.example.com".to_string()))
        );
    }

    #[test]
    fn parse_strips_trailing_dot() {
        // RFC 6066 §3 permits trailing dot; canonicalise to bare host.
        let bytes = build_client_hello(Some("api.example.com."));
        let outcome = parse(&bytes).expect("parse");
        assert_eq!(
            outcome,
            ParseOutcome::Ok(Some("api.example.com".to_string()))
        );
    }

    #[test]
    fn parse_no_sni_returns_ok_none() {
        // Valid ClientHello with NO server_name extension.
        let bytes = build_client_hello(None);
        let outcome = parse(&bytes).expect("parse");
        assert_eq!(outcome, ParseOutcome::Ok(None));
    }

    #[test]
    fn not_tls_record_type_rejected() {
        let bytes = b"GET / HTTP/1.1\r\n\r\n".to_vec();
        let err = parse(&bytes).expect_err("not TLS");
        assert_eq!(err, ParseError::NotTls);
    }

    #[test]
    fn empty_buffer_is_truncated() {
        assert_eq!(parse(&[]).unwrap(), ParseOutcome::Truncated);
    }

    #[test]
    fn record_too_long_is_malformed() {
        // Fake record header advertising 32 KiB (> MAX_HANDSHAKE_BODY).
        let bytes = vec![0x16, 0x03, 0x03, 0x80, 0x00];
        let err = parse(&bytes).expect_err("oversize");
        assert_eq!(err, ParseError::Malformed);
    }

    #[test]
    fn second_handshake_message_first_is_malformed() {
        // Record looks fine but the inner handshake msg_type is
        // ServerHello (0x02) instead of ClientHello.
        let mut bytes = build_client_hello(Some("example.com"));
        bytes[5] = 0x02;
        let err = parse(&bytes).expect_err("not ClientHello");
        assert_eq!(err, ParseError::Malformed);
    }

    // T020..T024: real-wire ClientHello fixtures captured against
    // OpenSSL 3.6.2 with `-servername example.com`. See
    // `crates/portunus-forwarder/tests/fixtures/tls/README.md` for the
    // capture procedure. These tests lock the fixtures' parse
    // outcomes so a parser regression OR fixture rot surfaces here.

    const TLS10_FIXTURE: &[u8] =
        include_bytes!("../../../tests/fixtures/tls/client_hello_tls10.bin");
    const TLS11_FIXTURE: &[u8] =
        include_bytes!("../../../tests/fixtures/tls/client_hello_tls11.bin");
    const TLS12_FIXTURE: &[u8] =
        include_bytes!("../../../tests/fixtures/tls/client_hello_tls12.bin");
    const TLS13_FIXTURE: &[u8] =
        include_bytes!("../../../tests/fixtures/tls/client_hello_tls13.bin");
    const FRAGMENTED_FIXTURE: &[u8] =
        include_bytes!("../../../tests/fixtures/tls/client_hello_fragmented.bin");

    #[test]
    fn parse_real_tls10_clienthello_extracts_sni() {
        let outcome = parse(TLS10_FIXTURE).expect("parse tls1.0 fixture");
        assert_eq!(outcome, ParseOutcome::Ok(Some("example.com".to_string())));
    }

    #[test]
    fn parse_real_tls11_clienthello_extracts_sni() {
        let outcome = parse(TLS11_FIXTURE).expect("parse tls1.1 fixture");
        assert_eq!(outcome, ParseOutcome::Ok(Some("example.com".to_string())));
    }

    #[test]
    fn parse_real_tls12_clienthello_extracts_sni() {
        let outcome = parse(TLS12_FIXTURE).expect("parse tls1.2 fixture");
        assert_eq!(outcome, ParseOutcome::Ok(Some("example.com".to_string())));
    }

    #[test]
    fn parse_real_tls13_clienthello_extracts_sni() {
        // TLS 1.3 keeps the legacy_version at 0x0303 in the record
        // header; the fixture exercises real-wire extension shapes
        // including PQ-hybrid `X25519MLKEM768` keyshare.
        let outcome = parse(TLS13_FIXTURE).expect("parse tls1.3 fixture");
        assert_eq!(outcome, ParseOutcome::Ok(Some("example.com".to_string())));
    }

    #[test]
    fn parse_fragmented_clienthello_is_malformed() {
        // R-015 explicitly rejects multi-record ClientHellos. The
        // fragmented fixture is the canonical negative case: feeding
        // bytes from the SECOND record (which starts mid-handshake-
        // body) MUST yield `ParseError::Malformed`, not Truncated or
        // a spurious Ok.
        let err = parse(FRAGMENTED_FIXTURE).expect_err("fragmented must be malformed");
        assert_eq!(err, ParseError::Malformed);
    }

    /// Wrap a raw ClientHello `body` (everything after the 4-byte
    /// handshake header) into a full TLS handshake record. Lets tests
    /// craft bodies the public `build_client_hello` helper cannot —
    /// e.g. a body that ends before the extensions field, or one whose
    /// length prefixes overrun the buffer.
    #[allow(clippy::cast_possible_truncation)]
    fn wrap_record(body: &[u8]) -> Vec<u8> {
        let hs_len = body.len();
        let mut hs = Vec::with_capacity(hs_len + 4);
        hs.push(HS_MSG_CLIENT_HELLO);
        hs.push(((hs_len >> 16) & 0xff) as u8);
        hs.push(((hs_len >> 8) & 0xff) as u8);
        hs.push((hs_len & 0xff) as u8);
        hs.extend_from_slice(body);

        let mut record = Vec::with_capacity(hs.len() + 5);
        record.push(RECORD_TYPE_HANDSHAKE);
        record.extend_from_slice(&[0x03, 0x01]); // legacy version 1.0
        record.extend_from_slice(&(hs.len() as u16).to_be_bytes());
        record.extend_from_slice(&hs);
        record
    }

    /// Fixed ClientHello prefix: legacy_version (2) + random (32).
    fn body_prefix() -> Vec<u8> {
        let mut body = Vec::with_capacity(34);
        body.extend_from_slice(&[0x03, 0x03]); // legacy_version TLS 1.2
        body.extend_from_slice(&[0xab; 32]); // random
        body
    }

    #[test]
    fn wrong_major_version_is_not_tls() {
        // Record content type is handshake but the major version byte
        // is not 0x03 — definitely not TLS we care about (line 108).
        let mut bytes = build_client_hello(Some("example.com"));
        bytes[1] = 0x02;
        let err = parse(&bytes).expect_err("bad version");
        assert_eq!(err, ParseError::NotTls);
    }

    #[test]
    fn zero_length_record_is_malformed() {
        // record_len == 0 is rejected before any fragment slicing.
        let bytes = vec![RECORD_TYPE_HANDSHAKE, 0x03, 0x03, 0x00, 0x00];
        let err = parse(&bytes).expect_err("zero len");
        assert_eq!(err, ParseError::Malformed);
    }

    #[test]
    fn fragment_shorter_than_handshake_header_is_malformed() {
        // A whole record carrying fewer than 4 fragment bytes cannot
        // hold a handshake header (line 121).
        let bytes = vec![
            RECORD_TYPE_HANDSHAKE,
            0x03,
            0x03,
            0x00,
            0x03,
            0x01,
            0x00,
            0x00,
        ];
        let err = parse(&bytes).expect_err("short fragment");
        assert_eq!(err, ParseError::Malformed);
    }

    #[test]
    fn handshake_len_overruns_fragment_is_malformed() {
        // hs_len advertised larger than the fragment carries — R-015
        // rejects cross-record reassembly (line 131..137).
        let mut bytes = build_client_hello(Some("example.com"));
        // Bump the 24-bit handshake length far past the record.
        bytes[6] = 0xff;
        bytes[7] = 0xff;
        let err = parse(&bytes).expect_err("overlong hs_len");
        assert_eq!(err, ParseError::Malformed);
    }

    #[test]
    fn body_shorter_than_fixed_prefix_is_malformed() {
        // Handshake body under 34 bytes can't hold legacy_version +
        // random (line 142).
        let body = vec![0x03, 0x03, 0xab, 0xab]; // only 4 bytes
        let bytes = wrap_record(&body);
        let err = parse(&bytes).expect_err("short body");
        assert_eq!(err, ParseError::Malformed);
    }

    #[test]
    fn session_id_length_overruns_is_malformed() {
        // session_id length byte claims more bytes than remain
        // (Cursor::skip overrun, lines 147-148 / 243).
        let mut body = body_prefix();
        body.push(0x05); // session_id length 5, but no bytes follow
        let bytes = wrap_record(&body);
        let err = parse(&bytes).expect_err("sid overrun");
        assert_eq!(err, ParseError::Malformed);
    }

    #[test]
    fn cipher_suites_length_read_past_end_is_malformed() {
        // Buffer ends right after session_id, so reading the 2-byte
        // cipher_suites length overruns (lines 151 / 234).
        let mut body = body_prefix();
        body.push(0x00); // session_id length 0; nothing after
        let bytes = wrap_record(&body);
        let err = parse(&bytes).expect_err("cipher len missing");
        assert_eq!(err, ParseError::Malformed);
    }

    #[test]
    fn cipher_suites_skip_overruns_is_malformed() {
        // cipher_suites length claims 4 bytes but only 0 remain
        // (lines 151-152 skip overrun).
        let mut body = body_prefix();
        body.push(0x00); // session_id length 0
        body.extend_from_slice(&[0x00, 0x04]); // cipher_suites len 4, none follow
        let bytes = wrap_record(&body);
        let err = parse(&bytes).expect_err("cipher skip overrun");
        assert_eq!(err, ParseError::Malformed);
    }

    #[test]
    fn compression_methods_overrun_is_malformed() {
        // compression_methods length claims bytes that are not present
        // (lines 155-156).
        let mut body = body_prefix();
        body.push(0x00); // session_id length 0
        body.extend_from_slice(&[0x00, 0x00]); // cipher_suites len 0
        body.push(0x03); // compression len 3, none follow
        let bytes = wrap_record(&body);
        let err = parse(&bytes).expect_err("compression overrun");
        assert_eq!(err, ParseError::Malformed);
    }

    #[test]
    fn body_without_extensions_field_is_ok_none() {
        // TLS 1.0 permits omitting the extensions block entirely. The
        // cursor reaches EOF right after compression_methods, so parse
        // returns Ok(None) without reading an extensions length
        // (line 160-161).
        let mut body = body_prefix();
        body.push(0x00); // session_id length 0
        body.extend_from_slice(&[0x00, 0x02, 0x13, 0x01]); // cipher_suites
        body.extend_from_slice(&[0x01, 0x00]); // compression_methods
        let bytes = wrap_record(&body);
        let outcome = parse(&bytes).expect("parse");
        assert_eq!(outcome, ParseOutcome::Ok(None));
    }

    #[test]
    fn extensions_total_length_overruns_is_malformed() {
        // extensions length byte present but claims more bytes than
        // the body carries (lines 163-164 / take overrun).
        let mut body = body_prefix();
        body.push(0x00); // session_id length 0
        body.extend_from_slice(&[0x00, 0x00]); // cipher_suites len 0
        body.push(0x00); // compression len 0
        body.extend_from_slice(&[0xff, 0xff]); // extensions total 65535, none follow
        let bytes = wrap_record(&body);
        let err = parse(&bytes).expect_err("ext total overrun");
        assert_eq!(err, ParseError::Malformed);
    }

    #[test]
    fn extension_header_overruns_is_malformed() {
        // extensions block declared, but a single extension's
        // type/length header overruns the declared block (lines
        // 169-171).
        let mut body = body_prefix();
        body.push(0x00); // session_id length 0
        body.extend_from_slice(&[0x00, 0x00]); // cipher_suites len 0
        body.push(0x00); // compression len 0
        // extensions total = 3, but an extension needs >=4 bytes for
        // type(2)+len(2) — the take overruns.
        body.extend_from_slice(&[0x00, 0x03]); // ext total 3
        body.extend_from_slice(&[0x00, 0x00, 0x05]); // partial ext
        let bytes = wrap_record(&body);
        let err = parse(&bytes).expect_err("ext header overrun");
        assert_eq!(err, ParseError::Malformed);
    }

    #[test]
    fn non_sni_extension_is_skipped() {
        // A single non-server_name extension is walked and skipped via
        // the `continue` branch (line 172-173); parse falls through to
        // Ok(None) at line 200.
        let mut body = body_prefix();
        body.push(0x00); // session_id length 0
        body.extend_from_slice(&[0x00, 0x00]); // cipher_suites len 0
        body.push(0x00); // compression len 0
        // One extension: type 0x0017 (extended_master_secret), data len 0.
        let exts: &[u8] = &[0x00, 0x17, 0x00, 0x00];
        body.extend_from_slice(&u16::try_from(exts.len()).unwrap().to_be_bytes());
        body.extend_from_slice(exts);
        let bytes = wrap_record(&body);
        let outcome = parse(&bytes).expect("parse");
        assert_eq!(outcome, ParseOutcome::Ok(None));
    }

    /// Build a ClientHello whose only extension is `server_name` with
    /// the exact extension-data bytes supplied — lets tests craft a
    /// malformed or unusual server_name_list the public builder won't.
    fn build_with_sni_ext_data(ext_data: &[u8]) -> Vec<u8> {
        let mut body = body_prefix();
        body.push(0x00); // session_id length 0
        body.extend_from_slice(&[0x00, 0x00]); // cipher_suites len 0
        body.push(0x00); // compression len 0

        let mut exts = Vec::new();
        exts.extend_from_slice(&[0x00, 0x00]); // ext_type = server_name
        exts.extend_from_slice(&u16::try_from(ext_data.len()).unwrap().to_be_bytes());
        exts.extend_from_slice(ext_data);

        body.extend_from_slice(&u16::try_from(exts.len()).unwrap().to_be_bytes());
        body.extend_from_slice(&exts);
        wrap_record(&body)
    }

    #[test]
    fn server_name_list_length_overruns_is_malformed() {
        // server_name extension present but the inner server_name_list
        // length claims more bytes than the extension carries (lines
        // 177-178 take overrun).
        let ext_data: &[u8] = &[0x00, 0xff]; // list_len 255, no entries
        let bytes = build_with_sni_ext_data(ext_data);
        let err = parse(&bytes).expect_err("list len overrun");
        assert_eq!(err, ParseError::Malformed);
    }

    #[test]
    fn server_name_entry_overruns_is_malformed() {
        // Inside the list, a host_name entry declares a name length
        // longer than the list carries (lines 181-183 take overrun).
        // server_name_list: len=4, then name_type=0x00, name_len=0x00ff
        // but only 1 byte follows.
        let ext_data: &[u8] = &[0x00, 0x04, 0x00, 0x00, 0xff, 0x41];
        let bytes = build_with_sni_ext_data(ext_data);
        let err = parse(&bytes).expect_err("entry overrun");
        assert_eq!(err, ParseError::Malformed);
    }

    #[test]
    fn invalid_utf8_hostname_is_malformed() {
        // host_name bytes are not valid UTF-8 (line 186 map_err).
        // list_len = 1(type)+2(len)+2(bytes) = 5.
        let ext_data: &[u8] = &[
            0x00, 0x05, // server_name_list length
            0x00, // name_type = host_name
            0x00, 0x02, // name length 2
            0xff, 0xfe, // invalid UTF-8
        ];
        let bytes = build_with_sni_ext_data(ext_data);
        let err = parse(&bytes).expect_err("bad utf8");
        assert_eq!(err, ParseError::Malformed);
    }

    #[test]
    fn whitespace_and_dot_only_hostname_is_ok_none() {
        // A host_name of "  ." trims to empty after stripping the
        // trailing dot — the `continue` at line 190 skips it, then the
        // list ends and parse returns Ok(None) (line 197).
        let host = b"  .";
        let mut list = Vec::new();
        list.push(0x00u8); // name_type = host_name
        list.extend_from_slice(&u16::try_from(host.len()).unwrap().to_be_bytes());
        list.extend_from_slice(host);
        let mut ext_data = Vec::new();
        ext_data.extend_from_slice(&u16::try_from(list.len()).unwrap().to_be_bytes());
        ext_data.extend_from_slice(&list);
        let bytes = build_with_sni_ext_data(&ext_data);
        let outcome = parse(&bytes).expect("parse");
        assert_eq!(outcome, ParseOutcome::Ok(None));
    }

    #[test]
    fn unknown_name_type_in_list_is_ok_none() {
        // A server_name_list entry with a non-host_name NameType is
        // skipped silently per RFC 6066 (line 193-194); the extension
        // yields no host so parse returns Ok(None) (line 197).
        // name_type = 0x01 (not host_name), name_len = 3, bytes "abc".
        let host = b"abc";
        let mut list = Vec::new();
        list.push(0x01u8); // name_type != host_name
        list.extend_from_slice(&u16::try_from(host.len()).unwrap().to_be_bytes());
        list.extend_from_slice(host);
        let mut ext_data = Vec::new();
        ext_data.extend_from_slice(&u16::try_from(list.len()).unwrap().to_be_bytes());
        ext_data.extend_from_slice(&list);
        let bytes = build_with_sni_ext_data(&ext_data);
        let outcome = parse(&bytes).expect("parse");
        assert_eq!(outcome, ParseOutcome::Ok(None));
    }

    #[test]
    fn empty_server_name_list_is_ok_none() {
        // server_name extension whose server_name_list is empty (the
        // inner while loop never runs) returns Ok(None) per the module
        // doc — covers the post-loop Ok(None) at line 197.
        let ext_data: &[u8] = &[0x00, 0x00]; // server_name_list length 0
        let bytes = build_with_sni_ext_data(ext_data);
        let outcome = parse(&bytes).expect("parse");
        assert_eq!(outcome, ParseOutcome::Ok(None));
    }

    #[test]
    fn cursor_read_u8_eof_is_malformed() {
        // Direct Cursor exercise: read_u8 at EOF returns Malformed
        // (line 225).
        let mut c = Cursor::new(&[]);
        assert_eq!(c.read_u8(), Err(ParseError::Malformed));
    }

    #[test]
    fn cursor_read_u16_short_is_malformed() {
        // read_u16 with only one byte left overruns (line 234).
        let mut c = Cursor::new(&[0x01]);
        assert_eq!(c.read_u16(), Err(ParseError::Malformed));
    }

    #[test]
    fn cursor_take_overrun_is_malformed() {
        // take past the end returns Malformed (line 251); a successful
        // take returns the exact slice.
        let buf = [0x01u8, 0x02, 0x03];
        let mut c = Cursor::new(&buf);
        assert_eq!(c.take(2).expect("take 2"), &[0x01, 0x02]);
        assert_eq!(c.take(5), Err(ParseError::Malformed));
    }
}

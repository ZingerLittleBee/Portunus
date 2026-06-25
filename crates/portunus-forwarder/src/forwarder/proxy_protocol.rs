use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

/// Upper bound on how long the (tiny, ≤ 52-byte) PROXY-protocol prelude
/// write may block before we abandon the connection. Without it, an
/// upstream that completes the TCP handshake but never reads (a 0-window
/// peer or a stalled backend) would wedge `write_all` indefinitely once
/// the kernel send buffer fills — the connection would not respond to
/// shutdown/drain and would leak until the rule-level drain timeout
/// strong-kills it. The prelude is negligible in size, so a short
/// timeout is safe headroom even on high-latency links.
const PRELUDE_WRITE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy)]
pub struct ProxyProtocolPrelude {
    pub version: portunus_core::ProxyProtocolVersion,
    pub source: SocketAddr,
    pub destination: SocketAddr,
}

pub async fn write_prelude(
    outbound: &mut TcpStream,
    prelude: ProxyProtocolPrelude,
) -> io::Result<()> {
    let bytes = encode(prelude.version, prelude.source, prelude.destination)?;
    match tokio::time::timeout(PRELUDE_WRITE_TIMEOUT, outbound.write_all(&bytes)).await {
        Ok(result) => result,
        Err(_elapsed) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "proxy_protocol_prelude_write_timeout",
        )),
    }
}

pub fn encode(
    version: portunus_core::ProxyProtocolVersion,
    source: SocketAddr,
    destination: SocketAddr,
) -> io::Result<Vec<u8>> {
    match version {
        portunus_core::ProxyProtocolVersion::V1 => encode_v1(source, destination),
        portunus_core::ProxyProtocolVersion::V2 => encode_v2(source, destination),
    }
}

fn encode_v1(source: SocketAddr, destination: SocketAddr) -> io::Result<Vec<u8>> {
    let line = match (source, destination) {
        (SocketAddr::V4(src), SocketAddr::V4(dst)) => format!(
            "PROXY TCP4 {} {} {} {}\r\n",
            src.ip(),
            dst.ip(),
            src.port(),
            dst.port()
        ),
        (SocketAddr::V6(src), SocketAddr::V6(dst)) => format!(
            "PROXY TCP6 {} {} {} {}\r\n",
            src.ip(),
            dst.ip(),
            src.port(),
            dst.port()
        ),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "proxy_protocol_family_mismatch",
            ));
        }
    };
    Ok(line.into_bytes())
}

fn encode_v2(source: SocketAddr, destination: SocketAddr) -> io::Result<Vec<u8>> {
    const SIG: [u8; 12] = [
        0x0d, 0x0a, 0x0d, 0x0a, 0x00, 0x0d, 0x0a, 0x51, 0x55, 0x49, 0x54, 0x0a,
    ];
    let mut out = Vec::with_capacity(52);
    out.extend_from_slice(&SIG);
    out.push(0x21);
    match (source, destination) {
        (SocketAddr::V4(src), SocketAddr::V4(dst)) => {
            out.push(0x11);
            out.extend_from_slice(&12u16.to_be_bytes());
            out.extend_from_slice(&src.ip().octets());
            out.extend_from_slice(&dst.ip().octets());
            out.extend_from_slice(&src.port().to_be_bytes());
            out.extend_from_slice(&dst.port().to_be_bytes());
        }
        (SocketAddr::V6(src), SocketAddr::V6(dst)) => {
            out.push(0x21);
            out.extend_from_slice(&36u16.to_be_bytes());
            out.extend_from_slice(&src.ip().octets());
            out.extend_from_slice(&dst.ip().octets());
            out.extend_from_slice(&src.port().to_be_bytes());
            out.extend_from_slice(&dst.port().to_be_bytes());
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "proxy_protocol_family_mismatch",
            ));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    #[test]
    fn proxy_protocol_v1_encodes_ipv4_line() {
        let source = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(203, 0, 113, 45), 54321));
        let dest = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 5), 443));
        let bytes = encode_v1(source, dest).expect("encodes");
        assert_eq!(
            String::from_utf8(bytes).expect("utf8"),
            "PROXY TCP4 203.0.113.45 10.0.0.5 54321 443\r\n"
        );
    }

    #[test]
    fn proxy_protocol_v1_encodes_ipv6_line() {
        let source = SocketAddr::V6(SocketAddrV6::new(
            Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 0xabcd),
            54321,
            0,
            0,
        ));
        let dest = SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 443, 0, 0));
        let bytes = encode_v1(source, dest).expect("encodes");
        assert_eq!(
            String::from_utf8(bytes).expect("utf8"),
            "PROXY TCP6 2001:db8::abcd ::1 54321 443\r\n"
        );
    }

    #[test]
    fn proxy_protocol_v2_encodes_ipv4_header() {
        let source = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(203, 0, 113, 45), 54321));
        let dest = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 5), 443));
        let bytes = encode_v2(source, dest).expect("encodes");
        // 12-byte signature + version/command (0x21) + family/protocol (0x11) +
        // 12-byte address-block length (0x000c) + 4+4 addresses + 2+2 ports.
        assert_eq!(bytes.len(), 28);
        assert_eq!(
            &bytes[..16],
            &[
                0x0d, 0x0a, 0x0d, 0x0a, 0x00, 0x0d, 0x0a, 0x51, 0x55, 0x49, 0x54, 0x0a, 0x21, 0x11,
                0x00, 0x0c,
            ]
        );
        // src ip, dst ip, src port, dst port (big-endian).
        assert_eq!(&bytes[16..20], &[203, 0, 113, 45]);
        assert_eq!(&bytes[20..24], &[10, 0, 0, 5]);
        assert_eq!(&bytes[24..26], &54321u16.to_be_bytes());
        assert_eq!(&bytes[26..28], &443u16.to_be_bytes());
    }

    #[test]
    fn proxy_protocol_v2_rejects_mixed_families() {
        let source = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1234);
        let dest = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 443);
        let err = encode(portunus_core::ProxyProtocolVersion::V2, source, dest)
            .expect_err("mixed families rejected");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(err.to_string(), "proxy_protocol_family_mismatch");
    }

    #[test]
    fn encode_dispatches_v2_to_binary_header() {
        let source = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 1));
        let dest = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 2));
        let via_encode = encode(portunus_core::ProxyProtocolVersion::V2, source, dest)
            .expect("encode dispatches to v2");
        let direct = encode_v2(source, dest).expect("encodes");
        assert_eq!(via_encode, direct);
    }

    #[tokio::test]
    async fn write_prelude_writes_encoded_bytes_to_peer() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let connect_fut = TcpStream::connect(addr);
        let (accept_res, connect_res) = tokio::join!(listener.accept(), connect_fut);
        let (mut server_side, _) = accept_res.unwrap();
        let mut outbound = connect_res.unwrap();

        let source = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(203, 0, 113, 45), 54321));
        let destination = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 5), 443));
        let prelude = ProxyProtocolPrelude {
            version: portunus_core::ProxyProtocolVersion::V1,
            source,
            destination,
        };

        write_prelude(&mut outbound, prelude)
            .await
            .expect("prelude write succeeds");

        let expected = encode(prelude.version, source, destination).expect("encodes");
        let mut buf = vec![0u8; expected.len()];
        server_side
            .read_exact(&mut buf)
            .await
            .expect("reads full prelude");
        assert_eq!(buf, expected);
    }

    #[tokio::test]
    async fn write_prelude_surfaces_encode_error_for_mixed_families() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let connect_fut = TcpStream::connect(addr);
        let (accept_res, connect_res) = tokio::join!(listener.accept(), connect_fut);
        let (_server_side, _) = accept_res.unwrap();
        let mut outbound = connect_res.unwrap();

        let prelude = ProxyProtocolPrelude {
            version: portunus_core::ProxyProtocolVersion::V1,
            source: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1234),
            destination: SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 443),
        };

        let err = write_prelude(&mut outbound, prelude)
            .await
            .expect_err("encode failure propagates before write");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn proxy_protocol_v2_encodes_ipv6_header() {
        let source = SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 1234, 0, 0));
        let dest = SocketAddr::V6(SocketAddrV6::new(
            Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1),
            443,
            0,
            0,
        ));
        let bytes = encode_v2(source, dest).expect("encodes");
        assert_eq!(
            &bytes[..16],
            &[
                0x0d, 0x0a, 0x0d, 0x0a, 0x00, 0x0d, 0x0a, 0x51, 0x55, 0x49, 0x54, 0x0a, 0x21, 0x21,
                0x00, 0x24,
            ]
        );
        assert_eq!(bytes.len(), 52);
    }

    #[test]
    fn proxy_protocol_rejects_mixed_families() {
        let source = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1234);
        let dest = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 443);
        let err = encode(portunus_core::ProxyProtocolVersion::V1, source, dest)
            .expect_err("mixed families rejected");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}

use std::io;
use std::net::SocketAddr;

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

#[derive(Debug, Clone, Copy)]
pub struct ProxyProtocolPrelude {
    pub version: forward_core::ProxyProtocolVersion,
    pub source: SocketAddr,
    pub destination: SocketAddr,
}

pub async fn write_prelude(
    outbound: &mut TcpStream,
    prelude: ProxyProtocolPrelude,
) -> io::Result<()> {
    let bytes = encode(prelude.version, prelude.source, prelude.destination)?;
    outbound.write_all(&bytes).await
}

pub fn encode(
    version: forward_core::ProxyProtocolVersion,
    source: SocketAddr,
    destination: SocketAddr,
) -> io::Result<Vec<u8>> {
    match version {
        forward_core::ProxyProtocolVersion::V1 => encode_v1(source, destination),
        forward_core::ProxyProtocolVersion::V2 => encode_v2(source, destination),
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
        let err = encode(forward_core::ProxyProtocolVersion::V1, source, dest)
            .expect_err("mixed families rejected");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}

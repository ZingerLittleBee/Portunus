//! Generated gRPC types for the Portunus control plane.
//!
//! The wire schema is defined in `proto/portunus.proto`. This crate exists so
//! that the proto code-gen pipeline (driven by `tonic-prost-build`) lives
//! outside the binaries' compile graph, keeping incremental rebuilds fast.

#![allow(clippy::pedantic)]

/// Generated types for the `portunus.v1` package.
pub mod v1 {
    tonic::include_proto!("portunus.v1");
}

impl From<portunus_core::Protocol> for v1::Protocol {
    fn from(p: portunus_core::Protocol) -> Self {
        match p {
            portunus_core::Protocol::Tcp => v1::Protocol::Tcp,
            portunus_core::Protocol::Udp => v1::Protocol::Udp,
        }
    }
}

/// Error returned when converting a wire `v1::Protocol::Unspecified` to
/// `portunus_core::Protocol`. The other proto variants map cleanly.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("wire Protocol::Unspecified cannot be converted to core::Protocol")]
pub struct UnspecifiedProtocolError;

impl TryFrom<v1::Protocol> for portunus_core::Protocol {
    type Error = UnspecifiedProtocolError;
    fn try_from(p: v1::Protocol) -> Result<Self, Self::Error> {
        match p {
            v1::Protocol::Tcp => Ok(portunus_core::Protocol::Tcp),
            v1::Protocol::Udp => Ok(portunus_core::Protocol::Udp),
            v1::Protocol::Unspecified => Err(UnspecifiedProtocolError),
        }
    }
}

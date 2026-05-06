//! Generated gRPC types for the forward-rs control plane.
//!
//! The wire schema is defined in `proto/forward.proto`. This crate exists so
//! that the proto code-gen pipeline (driven by `tonic-prost-build`) lives
//! outside the binaries' compile graph, keeping incremental rebuilds fast.

#![allow(clippy::pedantic)]

/// Generated types for the `forward.v1` package.
pub mod v1 {
    tonic::include_proto!("forward.v1");
}

//! Shared bucket layout for ClientHello peek-duration histograms.

use std::time::Duration;

/// Fixed classic-histogram bucket boundaries in seconds.
///
/// Covers 100 µs through the 3 s peek deadline, with denser resolution in the
/// sub-10 ms range and an inclusive 3.0 s tail bucket for timeout observations.
pub const PEEK_HISTOGRAM_BUCKETS_SECS: &[f64] = &[
    0.0001, 0.00025, 0.0005, 0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 3.0,
];

#[must_use]
pub fn bucket_count() -> usize {
    PEEK_HISTOGRAM_BUCKETS_SECS.len()
}

#[must_use]
pub fn bucket_index(elapsed: Duration) -> usize {
    let secs = elapsed.as_secs_f64();
    PEEK_HISTOGRAM_BUCKETS_SECS
        .iter()
        .position(|upper| secs <= *upper)
        .unwrap_or(PEEK_HISTOGRAM_BUCKETS_SECS.len().saturating_sub(1))
}

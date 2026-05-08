//! 008-sqlite-storage T002 / T027 — operator API criterion bench.
//!
//! Phase-1 scaffolding: bench harness registered so `cargo bench` does
//! not error. The real benches that gate SC-004 (operator API p50/p99
//! within 10% of the v0.7 baseline) are filled in by T027 in Phase 3.
//!
//! Saved baselines for SC-004 comparison live under
//! `specs/008-sqlite-storage/baselines/`; `cargo bench` exports
//! `target/criterion/...` which we diff against the saved JSON in CI.

use criterion::{Criterion, criterion_group, criterion_main};

fn placeholder(c: &mut Criterion) {
    c.bench_function("operator_api/placeholder", |b| {
        b.iter(|| {
            // Replaced in T027 with real operator HTTP latency probes.
        });
    });
}

criterion_group!(operator_api, placeholder);
criterion_main!(operator_api);

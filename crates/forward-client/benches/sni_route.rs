//! 009-tls-sni-routing T087: `SniRoutingTable::lookup` ns/op bench.
//!
//! Measures the cost of the SNI dispatch decision at 100 / 1 000 /
//! 10 000 routes for three traffic shapes:
//!   * `exact_hit`    — hostname matches an exact entry.
//!   * `wildcard_hit` — hostname matches a wildcard suffix in the
//!     longest-first list.
//!   * `miss`         — unknown hostname; table has no fallback.
//!
//! `forward-client` is a binary crate with no lib target, so the
//! lookup data structure is reproduced inline. Any divergence between
//! this bench and `forwarder::sni::route_table::SniRoutingTable` is
//! itself a regression worth catching — review this file alongside
//! the production module if either changes.
//!
//! SC-006: lookup median MUST stay < 1 µs at 100 routes; p99 (visible
//! in criterion's HTML report under `target/criterion`) MUST stay
//! < 100 µs at 100 routes. Treat any regression > +5 % vs the prior
//! baseline as a hot-path budget violation (Constitution Principle II).
//!
//! Run a quick check: `cargo bench -p forward-client --bench sni_route -- --quick`.

#![allow(clippy::cast_possible_truncation)]

use std::collections::HashMap;
use std::hint::black_box;

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};

#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug)]
struct RuleId(u64);

#[derive(Default)]
struct SniRoutingTable {
    exact: HashMap<String, RuleId>,
    /// Suffix (the part after `*.`) → rule_id, longest first.
    wildcards: Vec<(String, RuleId)>,
    fallback: Option<RuleId>,
}

#[derive(Debug, PartialEq, Eq)]
enum SniMatch {
    Hit(RuleId),
    Miss,
}

impl SniRoutingTable {
    fn build(exact_hosts: Vec<String>, wildcard_suffixes: Vec<String>) -> Self {
        let mut exact = HashMap::with_capacity(exact_hosts.len());
        for (i, h) in exact_hosts.into_iter().enumerate() {
            exact.insert(h.to_ascii_lowercase(), RuleId(i as u64));
        }
        let mut wildcards: Vec<(String, RuleId)> = wildcard_suffixes
            .into_iter()
            .enumerate()
            .map(|(i, s)| (s.to_ascii_lowercase(), RuleId(i as u64 + 10_000_000)))
            .collect();
        wildcards.sort_by(|(a, _), (b, _)| b.len().cmp(&a.len()));
        Self {
            exact,
            wildcards,
            fallback: None,
        }
    }

    #[inline]
    fn lookup(&self, sni: Option<&str>) -> SniMatch {
        if let Some(host) = sni {
            let host_lc = host.to_ascii_lowercase();
            if let Some(&rule_id) = self.exact.get(&host_lc) {
                return SniMatch::Hit(rule_id);
            }
            for (suffix, rule_id) in &self.wildcards {
                if wildcard_matches(&host_lc, suffix) {
                    return SniMatch::Hit(*rule_id);
                }
            }
        }
        if let Some(rule_id) = self.fallback {
            return SniMatch::Hit(rule_id);
        }
        SniMatch::Miss
    }
}

#[inline]
fn wildcard_matches(host_lc: &str, suffix: &str) -> bool {
    let needle = format!(".{suffix}");
    let Some(prefix) = host_lc.strip_suffix(&needle) else {
        return false;
    };
    if prefix.is_empty() {
        return false;
    }
    !prefix.contains('.')
}

fn build_corpus(n: usize) -> (SniRoutingTable, Vec<String>, Vec<String>, Vec<String>) {
    // Half exact hosts, half wildcard suffixes. Names are
    // synthesised so the corpus is deterministic at every scale.
    let half = n / 2;
    let exacts: Vec<String> = (0..half)
        .map(|i| format!("svc{i:06}.exact.example.com"))
        .collect();
    let wildcards: Vec<String> = (0..(n - half))
        .map(|i| format!("tier{i:06}.wildcard.example.com"))
        .collect();
    let table = SniRoutingTable::build(exacts.clone(), wildcards.clone());

    // Three lookup samples per shape. Picking different table
    // positions keeps the cache profile honest — a single hot key
    // would underestimate real-world dispatch cost.
    let exact_hits = vec![
        format!("svc{:06}.exact.example.com", 0),
        format!("svc{:06}.exact.example.com", half / 2),
        format!("svc{:06}.exact.example.com", half.saturating_sub(1)),
    ];
    let wildcard_hits = vec![
        format!("tenant.tier{:06}.wildcard.example.com", 0),
        format!("tenant.tier{:06}.wildcard.example.com", (n - half) / 2),
        format!(
            "tenant.tier{:06}.wildcard.example.com",
            (n - half).saturating_sub(1)
        ),
    ];
    let misses = vec![
        "totally.unrelated.example".to_string(),
        "another.miss.example".to_string(),
        "third.miss.example".to_string(),
    ];
    (table, exact_hits, wildcard_hits, misses)
}

fn run_lookups(table: &SniRoutingTable, hosts: &[String]) {
    for h in hosts {
        let _ = black_box(table.lookup(Some(black_box(h.as_str()))));
    }
}

fn bench_lookup(c: &mut Criterion) {
    for &n in &[100usize, 1_000, 10_000] {
        let (table, exact_hits, wildcard_hits, misses) = build_corpus(n);

        let mut group = c.benchmark_group(format!("sni_route_lookup_{n}"));
        group.bench_with_input(BenchmarkId::new("exact_hit", n), &exact_hits, |b, hosts| {
            b.iter_batched(
                || hosts.clone(),
                |hosts| run_lookups(&table, &hosts),
                BatchSize::SmallInput,
            );
        });
        group.bench_with_input(
            BenchmarkId::new("wildcard_hit", n),
            &wildcard_hits,
            |b, hosts| {
                b.iter_batched(
                    || hosts.clone(),
                    |hosts| run_lookups(&table, &hosts),
                    BatchSize::SmallInput,
                );
            },
        );
        group.bench_with_input(BenchmarkId::new("miss", n), &misses, |b, hosts| {
            b.iter_batched(
                || hosts.clone(),
                |hosts| run_lookups(&table, &hosts),
                BatchSize::SmallInput,
            );
        });
        group.finish();
    }
}

criterion_group!(benches, bench_lookup);
criterion_main!(benches);

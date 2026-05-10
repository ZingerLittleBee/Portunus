//! 009-tls-sni-routing T087+T088: SNI dispatch microbenches.
//!
//! T087 (`bench_lookup`): `SniRoutingTable::lookup` ns/op at
//! 100 / 1 000 / 10 000 routes for three traffic shapes:
//!   * `exact_hit`    — hostname matches an exact entry.
//!   * `wildcard_hit` — hostname matches a wildcard suffix in the
//!     longest-first list.
//!   * `miss`         — unknown hostname; table has no fallback.
//!
//! T088 (`bench_setup_latency`): connection-setup latency comparison
//! between SNI listener and v0.7 plain-TCP listener. The userspace
//! difference between the two paths is exactly:
//!
//! - plain TCP listener: `accept → spawn proxy → connect upstream`
//!   (no peek, no parse, no route lookup).
//! - SNI listener: `accept → peek ClientHello → parse → lookup →
//!   connect upstream → replay peeked bytes upstream`.
//!
//! Everything else (TCP accept, kernel scheduling, upstream connect,
//! the bidirectional copy) is identical between the two modes, so the
//! marginal cost is exactly `peek+parse+lookup+replay-prep`. We bench
//! that on a synthesised but realistic TLS 1.2 ClientHello and
//! compare to a `plain_baseline` that does no dispatch work
//! (representing the v0.7 listener's hot path on the same buffer).
//!
//! SC-003 caps the SNI listener at +5 ms p99 over plain TCP. The
//! kernel-side parts of connection setup are equal between modes;
//! only this userspace dispatch differs. If `bench_setup_latency`
//! shows the SNI path within ~5 µs (3 orders below the 5 ms budget),
//! SC-003 has comfortable headroom.
//!
//! `portunus-client` is a binary crate with no lib target, so the
//! lookup data structure AND ClientHello parser are reproduced
//! inline. Any divergence between this bench and the production
//! modules in `forwarder::sni` is itself a regression worth catching
//! — review this file alongside those modules if either changes.
//!
//! SC-006: lookup median MUST stay < 1 µs at 100 routes; p99 (visible
//! in criterion's HTML report under `target/criterion`) MUST stay
//! < 100 µs at 100 routes. Treat any regression > +5 % vs the prior
//! baseline as a hot-path budget violation (Constitution Principle II).
//!
//! Run a quick check: `cargo bench -p portunus-client --bench sni_route -- --quick`.

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

// =============================================================
// T088: setup-latency bench. Reproduces enough of the production
// ClientHello parser inline to keep the bench file self-contained.
// =============================================================

/// Inlined parser shape mirroring
/// `crates/portunus-client/src/forwarder/sni/client_hello.rs`. Only
/// the success path is exercised by the bench (one allocation-free
/// walk through extensions to the SNI). Errors are unwrapped — the
/// fixture is known-good.
fn parse_sni_inline(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 5 || bytes[0] != 0x16 || bytes[1] != 0x03 {
        return None;
    }
    let record_len = u16::from_be_bytes([bytes[3], bytes[4]]) as usize;
    if bytes.len() < 5 + record_len {
        return None;
    }
    let fragment = &bytes[5..5 + record_len];
    if fragment.len() < 4 || fragment[0] != 0x01 {
        return None;
    }
    let hs_len =
        ((fragment[1] as usize) << 16) | ((fragment[2] as usize) << 8) | (fragment[3] as usize);
    if hs_len > fragment.len() - 4 {
        return None;
    }
    let body = &fragment[4..4 + hs_len];
    if body.len() < 34 {
        return None;
    }
    let mut pos = 34;
    // legacy_session_id
    let sid_len = *body.get(pos)? as usize;
    pos += 1 + sid_len;
    // cipher_suites
    let cipher_suites_len = u16::from_be_bytes([*body.get(pos)?, *body.get(pos + 1)?]) as usize;
    pos += 2 + cipher_suites_len;
    // compression_methods
    let compression_methods_len = *body.get(pos)? as usize;
    pos += 1 + compression_methods_len;
    if pos >= body.len() {
        return None;
    }
    let ext_total = u16::from_be_bytes([body[pos], body[pos + 1]]) as usize;
    pos += 2;
    let ext_end = pos + ext_total;
    if ext_end > body.len() {
        return None;
    }
    while pos + 4 <= ext_end {
        let ext_type = u16::from_be_bytes([body[pos], body[pos + 1]]);
        let ext_data_len = u16::from_be_bytes([body[pos + 2], body[pos + 3]]) as usize;
        pos += 4;
        if pos + ext_data_len > ext_end {
            return None;
        }
        let ext_data = &body[pos..pos + ext_data_len];
        pos += ext_data_len;
        if ext_type != 0x0000 {
            continue;
        }
        // server_name_list
        if ext_data.len() < 2 {
            return None;
        }
        let list_len = u16::from_be_bytes([ext_data[0], ext_data[1]]) as usize;
        if 2 + list_len > ext_data.len() {
            return None;
        }
        let list = &ext_data[2..2 + list_len];
        let mut lp = 0;
        while lp + 3 <= list.len() {
            let name_type = list[lp];
            let name_len = u16::from_be_bytes([list[lp + 1], list[lp + 2]]) as usize;
            lp += 3;
            if lp + name_len > list.len() {
                return None;
            }
            if name_type == 0x00 && name_len > 0 {
                let raw = std::str::from_utf8(&list[lp..lp + name_len]).ok()?;
                return Some(raw.trim().trim_end_matches('.').to_ascii_lowercase());
            }
            lp += name_len;
        }
    }
    None
}

/// Build the same minimal TLS 1.2 ClientHello shape as
/// `client_hello::build_client_hello(Some(host))`. Inlined so the
/// bench file stays buildable against a binary crate.
fn build_client_hello_fixture(host: &str) -> Vec<u8> {
    let mut body = Vec::with_capacity(256);
    body.extend_from_slice(&[0x03, 0x03]); // legacy_version TLS 1.2
    body.extend_from_slice(&[0xab; 32]); // random
    body.push(0); // session_id length 0
    body.extend_from_slice(&[0x00, 0x02, 0x13, 0x01]); // 1 cipher: TLS_AES_128_GCM_SHA256
    body.extend_from_slice(&[0x01, 0x00]); // compression: null

    // server_name extension only.
    let mut sn_list = Vec::with_capacity(host.len() + 5);
    sn_list.push(0x00);
    sn_list.extend_from_slice(&(host.len() as u16).to_be_bytes());
    sn_list.extend_from_slice(host.as_bytes());
    let mut sn_ext_data = Vec::with_capacity(sn_list.len() + 2);
    sn_ext_data.extend_from_slice(&(sn_list.len() as u16).to_be_bytes());
    sn_ext_data.extend_from_slice(&sn_list);
    let mut exts = Vec::with_capacity(sn_ext_data.len() + 4);
    exts.extend_from_slice(&[0x00, 0x00]); // ext_type = server_name
    exts.extend_from_slice(&(sn_ext_data.len() as u16).to_be_bytes());
    exts.extend_from_slice(&sn_ext_data);
    body.extend_from_slice(&(exts.len() as u16).to_be_bytes());
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
    record.extend_from_slice(&[0x03, 0x01]); // legacy version
    record.extend_from_slice(&(hs.len() as u16).to_be_bytes());
    record.extend_from_slice(&hs);
    record
}

fn bench_setup_latency(c: &mut Criterion) {
    // 100-route SNI catalogue — matches the SC-006 reference shape
    // ("≤ 100 rules per listener"). The hot host lands as an exact
    // hit so the benched path traces the most common production
    // flow: parse → lookup → return.
    let (table, exact_hits, _wc, _miss) = build_corpus(100);
    let hello = build_client_hello_fixture(&exact_hits[0]);

    let mut group = c.benchmark_group("sni_setup_latency");

    // Plain-TCP baseline: the v0.7 listener does not peek or parse.
    // Its hot-path on a freshly-accepted byte buffer is "pass through
    // to the upstream copy task" — modelled here as the smallest
    // possible inspection of the buffer. This is the work the SNI
    // listener has to ADD on top.
    group.bench_function("plain_baseline", |b| {
        b.iter(|| {
            let _ = black_box(hello.as_slice());
        });
    });

    // SNI dispatch: parse the ClientHello, take the SNI string, and
    // run the route table lookup. The result decides which upstream
    // the connection-setup task connects to. The replay of peeked
    // bytes to the upstream is a single `write_all` (≤ 1 KiB) which
    // is dominated by the kernel send and not benched here.
    group.bench_function("sni_dispatch", |b| {
        b.iter(|| {
            let sni = black_box(parse_sni_inline(black_box(hello.as_slice())));
            let m = match sni {
                Some(host) => table.lookup(Some(black_box(host.as_str()))),
                None => table.lookup(None),
            };
            let _ = black_box(m);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_lookup, bench_setup_latency);
criterion_main!(benches);

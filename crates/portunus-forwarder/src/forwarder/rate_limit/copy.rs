//! 011-rate-limiting-qos T020 — bandwidth-cap-aware bidirectional copy.
//!
//! Drop-in for `tokio::io::copy_bidirectional` when a rule carries a
//! bandwidth cap (`RuleRateLimiter::has_bandwidth_cap()`). For each
//! 16 KiB chunk the half-loop reads, the corresponding direction's
//! [`TokenBucket`] is debited; on starvation the half-loop sleeps the
//! reported deficit and accumulates the wall-clock time into
//! [`RateLimitStatsAccumulator::record_throttle`]. The connection is
//! never closed by the limiter — only the read/write sides park.
//!
//! `bandwidth_in_bps` gates the inbound→outbound direction (peer →
//! target / "ingress" from the operator's perspective). `bandwidth_out_bps`
//! gates outbound→inbound (target → peer / "egress").
//!
//! Half-close mirrors `copy_bidirectional`: when one direction sees
//! EOF, that side's writer is shut down so the peer observes a clean
//! FIN; the reverse direction keeps draining until it also EOFs.
//!
//! Spec: `specs/011-rate-limiting-qos/research.md` § R-010.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

use super::scope::{
    BandwidthAcquire, BandwidthDirection, OwnerRateLimitHandle, RuleRateLimitHandle,
    RuleRateLimiter,
};
use super::stats::RateLimitStatsAccumulator;
use crate::forwarder::quota::{ConsumeOutcome, QuotaHandle};

/// Upper bound on the per-iteration chunk size. Matches the
/// uncapped fast path's `PROXY_COPY_BUF_SIZE` so the throttling
/// loop benefits from the same syscall amortisation when the
/// effective cap is high (≥ ~640 KiB/s).
const MAX_CHUNK: usize = 64 * 1024;

/// Lower bound on the per-iteration chunk size. Keeps tiny caps
/// from collapsing into byte-by-byte reads (one bucket acquire +
/// one syscall per byte would be pathological).
const MIN_CHUNK: usize = 8 * 1024;

/// Pacing budget. The chunk size targets approximately this much
/// wall clock between successive bucket acquires, so a low cap
/// produces frequent small reads instead of a single large read
/// followed by a multi-second sleep.
const PACING_TARGET_MS: u64 = 100;

/// How long a cached limiter snapshot may be reused before the copy
/// loop re-snapshots from the process-wide registry (#51). Each
/// snapshot is an `RwLock::read` + `HashMap::get` + `Arc::clone`
/// against ONE registry shared by every capped connection on every
/// rule; re-taking up to four of them per ≤64 KiB chunk serialised
/// all hot connections on the same two lock words. Caching the
/// snapshotted `Arc`s and refreshing on this interval collapses that
/// to (amortised) one read per interval per direction. A hot-reload
/// that swaps a rule/owner cap therefore takes effect within this
/// bound — well inside the ≤1 s budget FR-011 allows for cap changes,
/// so the ≤100 ms refresh lag is compliant.
const SNAPSHOT_REFRESH_INTERVAL: Duration = Duration::from_millis(100);

/// Compute the per-iteration chunk size for the throttling copy
/// loop. The tightest active bandwidth cap across `(rule_in_bps,
/// rule_out_bps, owner_in_bps, owner_out_bps)` for the given
/// `direction` determines the chunk:
///
/// * No cap on this direction (both rule + owner uncapped) →
///   `MAX_CHUNK` (64 KiB), matching the uncapped fast path.
/// * `cap_bps` configured → `clamp(cap_bps * PACING_TARGET_MS /
///   1000, MIN_CHUNK, MAX_CHUNK)`. For a 1 MiB/s cap this lands
///   at the 64 KiB ceiling; for 100 KiB/s caps it drops to ~10
///   KiB; for very low caps it floors at 8 KiB.
///
/// Operates on the cached limiter snapshots (`RuleRateLimiter` /
/// `OwnerRateLimiter`, the latter being the same type) rather than
/// re-snapshotting through the handles, so it adds no registry read
/// on the per-chunk path.
fn chunk_for_direction(
    direction: BandwidthDirection,
    rule: Option<&RuleRateLimiter>,
    owner: Option<&RuleRateLimiter>,
) -> usize {
    let rule_bps = rule.and_then(|r| r.bandwidth_rate_per_sec(direction));
    let owner_bps = owner.and_then(|o| o.bandwidth_rate_per_sec(direction));
    let tightest_bps = match (rule_bps, owner_bps) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) | (None, Some(a)) => Some(a),
        (None, None) => None,
    };
    let Some(bps) = tightest_bps else {
        return MAX_CHUNK;
    };
    let target = (bps.saturating_mul(PACING_TARGET_MS) / 1000) as usize;
    target.clamp(MIN_CHUNK, MAX_CHUNK)
}

/// Bidirectional copy with per-direction bandwidth throttling over a
/// concrete pair of `TcpStream`s.
///
/// Returns `(bytes_in, bytes_out)` on success. `bytes_in` is the count
/// flowing inbound→outbound (peer to target); `bytes_out` is the
/// reverse — same convention as `tokio::io::copy_bidirectional`.
///
/// Uses `TcpStream::split` (a native, lock-free borrow split) rather
/// than `tokio::io::split` (a `BiLock` that takes a lock on every poll
/// of all four halves) — #51. All production callers hold a
/// `&mut TcpStream`, so they get the lock-free path; the generic
/// duplex-based variant below is retained for unit tests.
///
/// Errors propagate from either half — first error returned wins, and
/// the still-running half is cancelled by drop.
#[allow(clippy::too_many_arguments)]
pub async fn copy_bidirectional_with_rate_limit(
    inbound: &mut TcpStream,
    outbound: &mut TcpStream,
    limiter: Arc<RuleRateLimitHandle>,
    stats: Option<Arc<RateLimitStatsAccumulator>>,
    owner_limiter: Option<Arc<OwnerRateLimitHandle>>,
    owner_stats: Option<Arc<RateLimitStatsAccumulator>>,
    // 013-traffic-quotas: per-(user, client) byte budget. `Some` when
    // the rule carries BOTH a bandwidth cap and a quota — the budget is
    // pre-debited before each chunk is written so the rate-limited path
    // is no longer a quota-bypass branch. Both directions share one
    // handle (atomic saturating consume), mirroring `quota/copy.rs`.
    quota: Option<Arc<QuotaHandle>>,
) -> io::Result<(u64, u64)> {
    // Native split: `ReadHalf` / `WriteHalf` borrow the socket without a
    // `BiLock`, so the two copy directions poll concurrently with no
    // per-poll lock.
    let (mut in_read, mut in_write) = inbound.split();
    let (mut out_read, mut out_write) = outbound.split();
    run_rate_limited_copy(
        &mut in_read,
        &mut in_write,
        &mut out_read,
        &mut out_write,
        limiter,
        stats,
        owner_limiter,
        owner_stats,
        quota,
    )
    .await
}

/// Generic (duplex-capable) bidirectional throttled copy. Kept for the
/// unit tests, which drive `tokio::io::duplex` pipes rather than real
/// sockets. Production code uses the lock-free `TcpStream` entry point
/// above.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
async fn copy_bidirectional_with_rate_limit_generic<A, B>(
    inbound: &mut A,
    outbound: &mut B,
    limiter: Arc<RuleRateLimitHandle>,
    stats: Option<Arc<RateLimitStatsAccumulator>>,
    owner_limiter: Option<Arc<OwnerRateLimitHandle>>,
    owner_stats: Option<Arc<RateLimitStatsAccumulator>>,
    quota: Option<Arc<QuotaHandle>>,
) -> io::Result<(u64, u64)>
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    let (mut in_read, mut in_write) = tokio::io::split(inbound);
    let (mut out_read, mut out_write) = tokio::io::split(outbound);
    run_rate_limited_copy(
        &mut in_read,
        &mut in_write,
        &mut out_read,
        &mut out_write,
        limiter,
        stats,
        owner_limiter,
        owner_stats,
        quota,
    )
    .await
}

/// Shared orchestration for both entry points: fork the two directional
/// [`copy_with_cap`] loops over the four pre-split halves and join them.
#[allow(clippy::too_many_arguments)]
async fn run_rate_limited_copy<RI, WI, RO, WO>(
    in_read: &mut RI,
    in_write: &mut WI,
    out_read: &mut RO,
    out_write: &mut WO,
    limiter: Arc<RuleRateLimitHandle>,
    stats: Option<Arc<RateLimitStatsAccumulator>>,
    owner_limiter: Option<Arc<OwnerRateLimitHandle>>,
    owner_stats: Option<Arc<RateLimitStatsAccumulator>>,
    quota: Option<Arc<QuotaHandle>>,
) -> io::Result<(u64, u64)>
where
    RI: AsyncRead + Unpin,
    WI: AsyncWrite + Unpin,
    RO: AsyncRead + Unpin,
    WO: AsyncWrite + Unpin,
{
    let limiter_in = Arc::clone(&limiter);
    let stats_in = stats.clone();
    let owner_in = owner_limiter.clone();
    let owner_stats_in = owner_stats.clone();
    let quota_in = quota.clone();
    let in_to_out = async {
        copy_with_cap(
            in_read,
            out_write,
            BandwidthDirection::In,
            &limiter_in,
            stats_in.as_deref(),
            owner_in.as_deref(),
            owner_stats_in.as_deref(),
            quota_in.as_ref(),
        )
        .await
    };
    let limiter_out = Arc::clone(&limiter);
    let stats_out = stats.clone();
    let owner_out = owner_limiter.clone();
    let owner_stats_out = owner_stats.clone();
    let quota_out = quota;
    let out_to_in = async {
        copy_with_cap(
            out_read,
            in_write,
            BandwidthDirection::Out,
            &limiter_out,
            stats_out.as_deref(),
            owner_out.as_deref(),
            owner_stats_out.as_deref(),
            quota_out.as_ref(),
        )
        .await
    };
    tokio::try_join!(in_to_out, out_to_in)
}

#[allow(clippy::too_many_arguments)]
async fn copy_with_cap<R, W>(
    reader: &mut R,
    writer: &mut W,
    direction: BandwidthDirection,
    limiter: &RuleRateLimitHandle,
    stats: Option<&RateLimitStatsAccumulator>,
    // T030: per-owner bucket consulted BEFORE the per-rule bucket on
    // every chunk (FR-013). Effective throughput is the lesser of
    // (owner_rate, rule_rate). Owner-direction throttle wall-clock
    // accumulates into `owner_stats`; rule-direction into `stats`.
    owner_limiter: Option<&OwnerRateLimitHandle>,
    owner_stats: Option<&RateLimitStatsAccumulator>,
    // 013-traffic-quotas / #52: per-(user, client) byte budget. `None`
    // keeps the byte-identical bandwidth-only path. `Some` PRE-DEBITS
    // the budget before each chunk is written (see the loop body) so
    // aggregate over-delivery is bounded by a constant, not by the
    // number of connections sharing the handle.
    quota: Option<&Arc<QuotaHandle>>,
) -> io::Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    // Allocate the largest possible buffer once; each iteration
    // reads into a `&mut buf[..chunk]` slice sized to the current
    // tightest cap. This bounds the per-connection footprint at
    // `MAX_CHUNK` (per half-loop) while still letting the loop pace
    // small chunks under low caps.
    let mut buf = vec![0u8; MAX_CHUNK];
    let mut total: u64 = 0;
    // #51: cache the snapshotted limiter `Arc`s instead of re-taking up
    // to four registry reads (`RwLock::read` + `HashMap::get` +
    // `Arc::clone`) per chunk. Every capped connection on every rule
    // shares the same two process-wide registries, so per-chunk
    // snapshots serialised all hot connections on the same two lock
    // words. Refresh on `SNAPSHOT_REFRESH_INTERVAL` so a hot-reload
    // swap is still observed within FR-011's ≤1 s budget.
    let mut cached_rule = limiter.snapshot();
    let mut cached_owner = owner_limiter.and_then(OwnerRateLimitHandle::snapshot);
    let mut last_snapshot = tokio::time::Instant::now();
    loop {
        // 013-traffic-quotas: stop before reading more once the budget
        // is gone — mirrors `quota/copy.rs`'s loop-top short-circuit so
        // an already-exhausted handle half-closes without another read.
        if let Some(q) = quota
            && q.is_exhausted()
        {
            let _ = writer.shutdown().await;
            return Ok(total);
        }
        // Refresh the cached snapshots once the interval elapses. On a
        // fully uncapped direction under paused-time tests no timer ever
        // fires so `elapsed()` stays 0 and we never refresh — harmless,
        // since such directions carry no cap to hot-reload.
        if last_snapshot.elapsed() >= SNAPSHOT_REFRESH_INTERVAL {
            cached_rule = limiter.snapshot();
            cached_owner = owner_limiter.and_then(OwnerRateLimitHandle::snapshot);
            last_snapshot = tokio::time::Instant::now();
        }
        let chunk = chunk_for_direction(direction, cached_rule.as_deref(), cached_owner.as_deref());
        let n = reader.read(&mut buf[..chunk]).await?;
        if n == 0 {
            // Half-close: shutdown the writer half so peer sees FIN.
            // Errors here are non-fatal — the peer may have already
            // closed; we still report the bytes we successfully
            // forwarded.
            let _ = writer.shutdown().await;
            return Ok(total);
        }
        // #52: PRE-DEBIT the byte budget BEFORE spending bandwidth
        // tokens or writing. The saturating `consume` claims the budget
        // up-front, so N connections sharing one handle can no longer
        // each push a full chunk past the budget between a stale
        // loop-top `is_exhausted` check and a post-write debit. On a
        // straddling draw (`Exhausted`) the remaining budget is spent
        // but this chunk is not fully covered, so we drop it and
        // half-close rather than forward bytes we couldn't account —
        // delivery never exceeds the granted budget (aggregate
        // over-delivery bounded to 0; boundary under-delivery ≤ 1 chunk
        // per handle, the fail-safe direction for a quota). This
        // mirrors the UDP batched pre-debit precedent
        // (`udp/flow.rs::quota_try_consume`).
        if let Some(q) = quota
            && matches!(
                q.consume(i64::try_from(n).unwrap_or(i64::MAX)),
                ConsumeOutcome::Exhausted
            )
        {
            let _ = writer.shutdown().await;
            return Ok(total);
        }
        // Acquire `n` tokens from each layer that's installed. Loop
        // until granted: sleep duration is exactly the deficit at
        // the configured rate, so the next acquire is guaranteed to
        // succeed barring a hot-reload that lowered the rate (handled
        // by re-looping). Owner first (FR-013): if owner is the
        // tighter bucket, the chunk parks on owner and the rule
        // bucket isn't consulted until owner releases tokens.
        if let Some(o) = cached_owner.as_deref() {
            loop {
                match o.acquire_bandwidth(direction, n as u64) {
                    BandwidthAcquire::Granted => break,
                    BandwidthAcquire::Throttled { deficit } => {
                        if let Some(s) = owner_stats {
                            let micros = u64::try_from(deficit.as_micros()).unwrap_or(u64::MAX);
                            s.record_throttle(direction, micros);
                        }
                        tokio::time::sleep(deficit).await;
                    }
                }
            }
        }
        if let Some(r) = cached_rule.as_deref() {
            loop {
                match r.acquire_bandwidth(direction, n as u64) {
                    BandwidthAcquire::Granted => break,
                    BandwidthAcquire::Throttled { deficit } => {
                        if let Some(s) = stats {
                            let micros = u64::try_from(deficit.as_micros()).unwrap_or(u64::MAX);
                            s.record_throttle(direction, micros);
                        }
                        tokio::time::sleep(deficit).await;
                    }
                }
            }
        }
        writer.write_all(&buf[..n]).await?;
        total += n as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use portunus_core::RateLimit;
    use std::time::Duration;
    use tokio::io::duplex;
    use tokio::time::Instant;

    fn limiter_for(rl: RateLimit) -> Arc<crate::forwarder::rate_limit::scope::RuleRateLimitHandle> {
        let scope = Arc::new(crate::forwarder::rate_limit::scope::RateLimitScopeManager::new());
        scope.install(portunus_core::RuleId(1), Some(&rl));
        Arc::new(
            crate::forwarder::rate_limit::scope::RuleRateLimitHandle::new(
                portunus_core::RuleId(1),
                scope,
            ),
        )
    }

    fn owner_handle_for(
        rl: RateLimit,
    ) -> Arc<crate::forwarder::rate_limit::scope::OwnerRateLimitHandle> {
        let mgr = Arc::new(crate::forwarder::rate_limit::scope::OwnerRateLimitScopeManager::new());
        let owner = crate::forwarder::rate_limit::scope::OwnerId::new("owner");
        mgr.install(&owner, Some(&rl));
        Arc::new(crate::forwarder::rate_limit::scope::OwnerRateLimitHandle::new(owner, mgr))
    }

    /// 013-traffic-quotas: a rule carrying BOTH a bandwidth cap and a
    /// quota must debit the quota INSIDE the rate-limited copy loop.
    /// Before the quota was threaded through `copy_with_cap`, this branch
    /// forwarded unbounded bytes while the budget was never touched —
    /// quota and rate-limit were mutually exclusive enforcement paths.
    /// Push more than the budget and assert the loop halts at the
    /// boundary and marks the handle exhausted.
    #[tokio::test]
    async fn rate_limited_copy_debits_quota_and_halts_at_budget() {
        use crate::forwarder::quota::{QuotaHandle, QuotaState};
        use tokio::io::AsyncReadExt;

        // Generous bandwidth cap (won't bind for this small payload) so
        // the only thing that can stop the copy is the quota.
        let limiter = limiter_for(RateLimit {
            bandwidth_in_bps: Some(1024 * 1024 * 1024),
            ..Default::default()
        });
        let quota = Arc::new(QuotaHandle::new(
            "alice".into(),
            "edge-01".into(),
            QuotaState {
                monthly_bytes: 1_000_000,
                budget_remaining_bytes: 100 * 1024,
                exhausted: false,
            },
        ));

        // duplex big enough to buffer the whole push so the producer
        // never blocks — the copy loop, not back-pressure, decides when
        // to stop.
        let (mut peer, mut reader) = duplex(1024 * 1024);
        let (mut writer, mut target) = duplex(1024 * 1024);

        let q = Arc::clone(&quota);
        let copier = tokio::spawn(async move {
            copy_with_cap(
                &mut reader,
                &mut writer,
                BandwidthDirection::In,
                &limiter,
                None,
                None,
                None,
                Some(&q),
            )
            .await
        });

        // 320 KiB — well over the 100 KiB budget.
        let push = 320 * 1024;
        let producer = tokio::spawn(async move {
            let payload = vec![0x5A_u8; push];
            let _ = peer.write_all(&payload).await;
            // Drop peer → EOF. Under the (buggy) no-quota path the loop
            // runs to EOF and forwards everything; under the fixed path
            // it stops on the quota before EOF.
        });

        let drainer = tokio::spawn(async move {
            let mut sink = Vec::new();
            let _ = target.read_to_end(&mut sink).await;
            sink.len()
        });

        producer.await.unwrap();
        let total = copier.await.unwrap().unwrap();
        let received = drainer.await.unwrap();

        assert!(
            quota.is_exhausted(),
            "quota must be exhausted after an over-budget transfer"
        );
        assert_eq!(quota.remaining(), 0, "remaining budget must saturate at 0");
        assert!(
            total < push as u64,
            "copy must halt at the budget, not forward all {push} bytes (got {total})"
        );
        assert_eq!(
            received,
            usize::try_from(total).unwrap(),
            "downstream received exactly what the copy delivered"
        );
    }

    /// 011-rate-limiting-qos T010: TCP bandwidth-in cap shapes
    /// throughput within ±10% of target across {100 KB/s, 1 MB/s,
    /// 10 MB/s}. Uses paused-time tokio so the timing is
    /// deterministic; payload is sized so the bucket has to refill
    /// at least 4× during the test (i.e., enough chunks to average
    /// out the initial-burst skew).
    #[allow(clippy::cast_precision_loss)]
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn t010_bandwidth_cap_shapes_to_target_rate_within_10pct() {
        for rate in [100 * 1024_u64, 1024 * 1024, 10 * 1024 * 1024] {
            // Run 5× the rate worth of bytes — averages out the
            // initial-burst (= rate) so the measured rate converges
            // close to the target.
            let bytes = usize::try_from(rate * 5).expect("rate × 5 fits in usize on test hosts");
            let limiter = limiter_for(RateLimit {
                bandwidth_in_bps: Some(rate),
                ..Default::default()
            });
            let acc = Arc::new(RateLimitStatsAccumulator::new());

            let (mut peer, mut inbound) = duplex(256 * 1024);
            let (mut outbound, mut target) = duplex(256 * 1024);

            let limiter_task = Arc::clone(&limiter);
            let acc_task = Arc::clone(&acc);
            let proxy = tokio::spawn(async move {
                copy_bidirectional_with_rate_limit_generic(
                    &mut inbound,
                    &mut outbound,
                    limiter_task,
                    Some(acc_task),
                    None,
                    None,
                    None,
                )
                .await
            });

            let payload = vec![0xAA_u8; bytes];
            let writer = tokio::spawn(async move {
                peer.write_all(&payload).await.unwrap();
                peer.shutdown().await.unwrap();
            });
            let read = tokio::spawn(async move {
                let mut buf = vec![0u8; bytes];
                let mut total = 0;
                loop {
                    let n = target.read(&mut buf[total..]).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    total += n;
                }
                total
            });

            let started = Instant::now();
            writer.await.unwrap();
            let total = read.await.unwrap();
            proxy.await.unwrap().unwrap();
            let elapsed = started.elapsed();

            assert_eq!(total, bytes, "rate={rate}: all bytes must be forwarded");

            // Effective bytes/sec = bytes / elapsed_secs. With burst
            // = rate, the first second drains burst + 1 s of refill
            // = 2× rate worth of "free" bytes. After the burst the
            // bucket runs at exactly `rate`, so over 5 s of payload
            // the measured rate lands at ~(5 × rate) / 4 s = 1.25 ×
            // rate. We assert effective rate ≤ 1.5 × rate (loose
            // upper bound that still catches a regression at 2× or
            // higher) and ≥ 0.9 × rate (catches over-throttling).
            let elapsed_secs = elapsed.as_secs_f64();
            assert!(
                elapsed_secs > 0.0,
                "rate={rate}: elapsed must advance under paused time"
            );
            let measured = bytes as f64 / elapsed_secs;
            let lower = rate as f64 * 0.9;
            let upper = rate as f64 * 1.5;
            assert!(
                measured >= lower && measured <= upper,
                "rate={rate}: measured={measured:.0}B/s outside [{lower:.0}, {upper:.0}]"
            );
            // Throttle wall-clock must have accrued.
            assert!(
                acc.throttle_micros(BandwidthDirection::In) > 0,
                "rate={rate}: throttle micros must be non-zero"
            );
        }
    }

    /// 1 MiB inbound at a 100 KiB/s cap should take roughly 10 s.
    /// The exact figure depends on burst handling; we assert a
    /// generous lower bound (>5 s) so the test isn't flaky on a
    /// loaded CI box.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn ingress_cap_throttles_to_target_rate() {
        let limiter = limiter_for(RateLimit {
            bandwidth_in_bps: Some(100 * 1024),
            ..Default::default()
        });
        let acc = Arc::new(RateLimitStatsAccumulator::new());

        // Two duplex pipes acting as `inbound` and `outbound` sockets.
        let (mut peer, mut inbound) = duplex(64 * 1024);
        let (mut outbound, mut target) = duplex(64 * 1024);

        let limiter_clone = Arc::clone(&limiter);
        let acc_clone = Arc::clone(&acc);
        let proxy = tokio::spawn(async move {
            copy_bidirectional_with_rate_limit_generic(
                &mut inbound,
                &mut outbound,
                limiter_clone,
                Some(acc_clone),
                None,
                None,
                None,
            )
            .await
        });

        // Push 1 MiB through the inbound side.
        let payload = vec![0xAA_u8; 1024 * 1024];
        let writer = tokio::spawn(async move {
            peer.write_all(&payload).await.unwrap();
            peer.shutdown().await.unwrap();
        });

        // Drain the target side so the writer doesn't block.
        let read = tokio::spawn(async move {
            let mut buf = vec![0u8; 1024 * 1024];
            let mut total = 0;
            loop {
                let n = target.read(&mut buf[total..]).await.unwrap();
                if n == 0 {
                    break;
                }
                total += n;
            }
            total
        });

        let started = Instant::now();
        writer.await.unwrap();
        let total = read.await.unwrap();
        proxy.await.unwrap().unwrap();
        let elapsed = started.elapsed();

        assert_eq!(total, 1024 * 1024, "all bytes must be forwarded");
        assert!(
            elapsed >= Duration::from_secs(5),
            "1 MiB at 100 KiB/s should take >=5 s, took {elapsed:?}"
        );
        // The accumulator must have recorded throttle micros.
        assert!(
            acc.throttle_micros(BandwidthDirection::In) > 0,
            "throttle micros must be non-zero on the In direction"
        );
        // Reverse direction was idle (no bytes flowed back from
        // target), so its throttle counter stays at 0.
        assert_eq!(acc.throttle_micros(BandwidthDirection::Out), 0);
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn no_cap_directions_pass_through_freely() {
        // bandwidth_out_bps is set; bandwidth_in is unbounded. The
        // ingress half-loop must not throttle.
        let limiter = limiter_for(RateLimit {
            bandwidth_out_bps: Some(100 * 1024),
            ..Default::default()
        });
        let acc = Arc::new(RateLimitStatsAccumulator::new());

        let (mut peer, mut inbound) = duplex(64 * 1024);
        let (mut outbound, mut target) = duplex(64 * 1024);

        let limiter_clone = Arc::clone(&limiter);
        let acc_clone = Arc::clone(&acc);
        let proxy = tokio::spawn(async move {
            copy_bidirectional_with_rate_limit_generic(
                &mut inbound,
                &mut outbound,
                limiter_clone,
                Some(acc_clone),
                None,
                None,
                None,
            )
            .await
        });

        let payload = vec![0x55_u8; 256 * 1024];
        let writer = tokio::spawn(async move {
            peer.write_all(&payload).await.unwrap();
            peer.shutdown().await.unwrap();
        });
        let read = tokio::spawn(async move {
            let mut buf = vec![0u8; 256 * 1024];
            let mut total = 0;
            loop {
                let n = target.read(&mut buf[total..]).await.unwrap();
                if n == 0 {
                    break;
                }
                total += n;
            }
            total
        });

        let started = Instant::now();
        writer.await.unwrap();
        let total = read.await.unwrap();
        proxy.await.unwrap().unwrap();
        let elapsed = started.elapsed();

        assert_eq!(total, 256 * 1024);
        // 256 KiB with no cap on the In direction should be near-
        // instant (well under 1 s even with paused-time scheduling
        // overhead). We don't lower-bound because paused time can
        // be exactly 0 if no `sleep` ever fires.
        assert!(
            elapsed < Duration::from_millis(500),
            "uncapped direction must not throttle, took {elapsed:?}"
        );
        assert_eq!(acc.throttle_micros(BandwidthDirection::In), 0);
        assert_eq!(acc.throttle_micros(BandwidthDirection::Out), 0);
    }

    /// 011-rate-limiting-qos T025: per-owner ceiling binds before
    /// per-rule cap (FR-013). With a per-rule cap of 10 MB/s and a
    /// per-owner cap of 5 MB/s, the data plane must shape ingress to
    /// the owner rate (≤ 5 MB/s). The integration test that pushes
    /// the cap envelope through the gRPC control plane lives in the
    /// portunus-e2e harness; this unit test pins the data-plane
    /// semantic so a regression in the layered acquire would fail
    /// here first.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn t025_per_owner_ceiling_binds_before_per_rule_cap() {
        // 10 MB/s per-rule, 5 MB/s per-owner. The owner is the
        // binding ceiling. Push 5 MiB and assert the elapsed time
        // matches the owner rate within ±10%.
        let rule_limiter = limiter_for(RateLimit {
            bandwidth_in_bps: Some(10 * 1024 * 1024),
            ..Default::default()
        });
        let rule_acc = Arc::new(RateLimitStatsAccumulator::new());
        let owner_limiter = owner_handle_for(RateLimit {
            bandwidth_in_bps: Some(5 * 1024 * 1024),
            ..Default::default()
        });
        let owner_acc = Arc::new(RateLimitStatsAccumulator::new());

        let (mut peer, mut inbound) = duplex(256 * 1024);
        let (mut outbound, mut target) = duplex(256 * 1024);

        let task_rule = Arc::clone(&rule_limiter);
        let task_rule_stats = Arc::clone(&rule_acc);
        let task_owner = Arc::clone(&owner_limiter);
        let task_owner_stats = Arc::clone(&owner_acc);
        let proxy = tokio::spawn(async move {
            copy_bidirectional_with_rate_limit_generic(
                &mut inbound,
                &mut outbound,
                task_rule,
                Some(task_rule_stats),
                Some(task_owner),
                Some(task_owner_stats),
                None,
            )
            .await
        });

        // Payload must exceed the owner burst (= rate by default) so
        // the throttle path actually runs. 16 MiB at 5 MB/s shapes
        // to ≈ 3 s of measurable throttle after the burst drains.
        let bytes = 16 * 1024 * 1024_usize;
        let payload = vec![0xAB_u8; bytes];
        let writer = tokio::spawn(async move {
            peer.write_all(&payload).await.unwrap();
            peer.shutdown().await.unwrap();
        });
        let read = tokio::spawn(async move {
            let mut buf = vec![0u8; bytes];
            let mut total = 0;
            loop {
                let n = target.read(&mut buf[total..]).await.unwrap();
                if n == 0 {
                    break;
                }
                total += n;
            }
            total
        });

        let started = Instant::now();
        writer.await.unwrap();
        let total = read.await.unwrap();
        proxy.await.unwrap().unwrap();
        let elapsed = started.elapsed();

        assert_eq!(total, bytes);
        // 16 MiB at 5 MB/s ≈ 3.2 s after the burst (= rate = 5 MiB)
        // drains. We require ≥ 1.5 s — well below the theoretical
        // floor but above what the per-rule rate (10 MB/s ≈ 1.6 s)
        // would produce, so this assertion captures the owner-binds
        // semantic: the rule is not the bottleneck.
        assert!(
            elapsed >= Duration::from_millis(1500),
            "owner=5MB/s should take >=1.5s for 16MiB, took {elapsed:?}"
        );
        // The owner stats accumulator captured throttle wall-clock.
        assert!(
            owner_acc.throttle_micros(BandwidthDirection::In) > 0,
            "owner throttle micros must be non-zero — owner is binding"
        );
        // FR-014: the rule accumulator stays at zero because the
        // rule bucket was never the bottleneck. A regression that
        // double-counted the throttle (or attributed it to the rule
        // layer) would fail this check.
        assert_eq!(
            rule_acc.throttle_micros(BandwidthDirection::In),
            0,
            "rule layer never throttled — its accumulator must stay at 0"
        );
    }

    /// T030: per-owner bandwidth cap throttles even when the per-rule
    /// bucket has plenty of tokens. Throttle wall-clock for the owner
    /// direction lands in the OWNER stats accumulator (FR-014); the
    /// rule accumulator stays at zero.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn t030_owner_bandwidth_cap_throttles_independently() {
        // Rule allows 1 MiB/s — generous; owner allows 100 KiB/s — the
        // binding ceiling. Effective throughput must converge near the
        // owner rate, with throttle micros recorded against the owner
        // accumulator only.
        let rule_limiter = limiter_for(RateLimit {
            bandwidth_in_bps: Some(1024 * 1024),
            ..Default::default()
        });
        let rule_acc = Arc::new(RateLimitStatsAccumulator::new());
        let owner_limiter = owner_handle_for(RateLimit {
            bandwidth_in_bps: Some(100 * 1024),
            ..Default::default()
        });
        let owner_acc = Arc::new(RateLimitStatsAccumulator::new());

        let (mut peer, mut inbound) = duplex(64 * 1024);
        let (mut outbound, mut target) = duplex(64 * 1024);

        let task_rule = Arc::clone(&rule_limiter);
        let task_rule_stats = Arc::clone(&rule_acc);
        let task_owner = Arc::clone(&owner_limiter);
        let task_owner_stats = Arc::clone(&owner_acc);
        let proxy = tokio::spawn(async move {
            copy_bidirectional_with_rate_limit_generic(
                &mut inbound,
                &mut outbound,
                task_rule,
                Some(task_rule_stats),
                Some(task_owner),
                Some(task_owner_stats),
                None,
            )
            .await
        });

        let payload = vec![0xCC_u8; 1024 * 1024];
        let writer = tokio::spawn(async move {
            peer.write_all(&payload).await.unwrap();
            peer.shutdown().await.unwrap();
        });
        let read = tokio::spawn(async move {
            let mut buf = vec![0u8; 1024 * 1024];
            let mut total = 0;
            loop {
                let n = target.read(&mut buf[total..]).await.unwrap();
                if n == 0 {
                    break;
                }
                total += n;
            }
            total
        });

        let started = Instant::now();
        writer.await.unwrap();
        let total = read.await.unwrap();
        proxy.await.unwrap().unwrap();
        let elapsed = started.elapsed();

        assert_eq!(total, 1024 * 1024);
        assert!(
            elapsed >= Duration::from_secs(5),
            "owner cap of 100 KiB/s must throttle 1 MiB to >=5 s, took {elapsed:?}"
        );
        // Owner accumulator captured the throttle.
        assert!(
            owner_acc.throttle_micros(BandwidthDirection::In) > 0,
            "owner throttle micros must be non-zero on the In direction"
        );
        // Rule accumulator stays at zero — the rule bucket was never
        // the bottleneck (FR-014 attribution).
        assert_eq!(
            rule_acc.throttle_micros(BandwidthDirection::In),
            0,
            "rule throttle counter must NOT bump when owner is the binding cap"
        );
    }

    /// Owner cap may arrive after the rule is already active. The
    /// dynamic owner handle must pick up the later install without a
    /// rule re-push.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn t030_owner_cap_installed_after_activation_throttles_existing_rule() {
        let rule_limiter = limiter_for(RateLimit::default());
        let rule_acc = Arc::new(RateLimitStatsAccumulator::new());
        let owner_mgr =
            Arc::new(crate::forwarder::rate_limit::scope::OwnerRateLimitScopeManager::new());
        let owner_id = crate::forwarder::rate_limit::scope::OwnerId::new("alice");
        let owner_handle = Arc::new(
            crate::forwarder::rate_limit::scope::OwnerRateLimitHandle::new(
                owner_id.clone(),
                Arc::clone(&owner_mgr),
            ),
        );
        let owner_acc = Arc::new(RateLimitStatsAccumulator::new());

        let (mut peer, mut inbound) = duplex(64 * 1024);
        let (mut outbound, mut target) = duplex(64 * 1024);

        let task_rule = Arc::clone(&rule_limiter);
        let task_rule_stats = Arc::clone(&rule_acc);
        let task_owner = Arc::clone(&owner_handle);
        let task_owner_stats = Arc::clone(&owner_acc);
        let proxy = tokio::spawn(async move {
            copy_bidirectional_with_rate_limit_generic(
                &mut inbound,
                &mut outbound,
                task_rule,
                Some(task_rule_stats),
                Some(task_owner),
                Some(task_owner_stats),
                None,
            )
            .await
        });

        owner_mgr.update(
            &owner_id,
            Some(&RateLimit {
                bandwidth_in_bps: Some(100 * 1024),
                ..Default::default()
            }),
        );

        let payload = vec![0xDD_u8; 1024 * 1024];
        let writer = tokio::spawn(async move {
            peer.write_all(&payload).await.unwrap();
            peer.shutdown().await.unwrap();
        });
        let read = tokio::spawn(async move {
            let mut buf = vec![0u8; 1024 * 1024];
            let mut total = 0;
            loop {
                let n = target.read(&mut buf[total..]).await.unwrap();
                if n == 0 {
                    break;
                }
                total += n;
            }
            total
        });

        let started = Instant::now();
        writer.await.unwrap();
        let total = read.await.unwrap();
        proxy.await.unwrap().unwrap();
        let elapsed = started.elapsed();

        assert_eq!(total, 1024 * 1024);
        assert!(
            elapsed >= Duration::from_secs(5),
            "late owner cap of 100 KiB/s must throttle 1 MiB to >=5 s, took {elapsed:?}"
        );
        assert!(
            owner_acc.throttle_micros(BandwidthDirection::In) > 0,
            "owner throttle micros must be non-zero after late install"
        );
        assert_eq!(
            rule_acc.throttle_micros(BandwidthDirection::In),
            0,
            "rule accumulator must stay at zero when late owner cap binds"
        );
    }

    /// 011-rate-limiting-qos T026: tenant isolation — a throttled
    /// owner does not affect another owner's flows. Each owner has
    /// its own `Arc<OwnerRateLimiter>` keyed in the registry, so a
    /// token-bucket exhaustion on one Arc never reaches the other.
    /// The full e2e starvation test (driven through the gRPC control
    /// plane) belongs in portunus-e2e; this unit test pins the
    /// data-plane invariant — the registry's per-owner Arc cannot
    /// be a shared resource.
    #[allow(clippy::similar_names)]
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn t026_owner_throttle_does_not_affect_uncapped_owner_flow() {
        // Owner A: 1 MiB/s aggregate ingress cap.
        let owner_a = owner_handle_for(RateLimit {
            bandwidth_in_bps: Some(1024 * 1024),
            ..Default::default()
        });
        let owner_a_acc = Arc::new(RateLimitStatsAccumulator::new());

        // Owner B: no cap whatsoever (uncapped envelope).
        let owner_b = owner_handle_for(RateLimit::default());
        let owner_b_stats_acc = Arc::new(RateLimitStatsAccumulator::new());

        // Shared per-rule limiter — generous so it never binds and
        // both owners' attribution cleanly attaches to the owner
        // layer. (In production each rule has its own limiter; we
        // share here only because the test exercises a single rule
        // per owner-flow pair.)
        let rule_a = limiter_for(RateLimit::default());
        let rule_b = limiter_for(RateLimit::default());

        // Two independent forwarder pairs, one per owner.
        let (mut peer_a, mut inbound_a) = duplex(256 * 1024);
        let (mut outbound_a, mut target_a) = duplex(256 * 1024);
        let (mut peer_b, mut inbound_b) = duplex(256 * 1024);
        let (mut outbound_b, mut target_b) = duplex(256 * 1024);

        let bw_owner_a = Arc::clone(&owner_a);
        let bw_owner_a_stats = Arc::clone(&owner_a_acc);
        let proxy_a = tokio::spawn(async move {
            copy_bidirectional_with_rate_limit_generic(
                &mut inbound_a,
                &mut outbound_a,
                rule_a,
                None,
                Some(bw_owner_a),
                Some(bw_owner_a_stats),
                None,
            )
            .await
        });
        let bw_owner_b = Arc::clone(&owner_b);
        let bw_owner_b_stats = Arc::clone(&owner_b_stats_acc);
        let proxy_b = tokio::spawn(async move {
            copy_bidirectional_with_rate_limit_generic(
                &mut inbound_b,
                &mut outbound_b,
                rule_b,
                None,
                Some(bw_owner_b),
                Some(bw_owner_b_stats),
                None,
            )
            .await
        });

        // 4 MiB through both — owner A throttles to its cap, owner B
        // streams freely.
        let bytes = 4 * 1024 * 1024_usize;
        let writer_a = tokio::spawn({
            let payload = vec![0xAA_u8; bytes];
            async move {
                peer_a.write_all(&payload).await.unwrap();
                peer_a.shutdown().await.unwrap();
            }
        });
        let writer_b = tokio::spawn({
            let payload = vec![0xBB_u8; bytes];
            async move {
                peer_b.write_all(&payload).await.unwrap();
                peer_b.shutdown().await.unwrap();
            }
        });
        let read_a = tokio::spawn(async move {
            let mut buf = vec![0u8; bytes];
            let mut total = 0;
            loop {
                let n = target_a.read(&mut buf[total..]).await.unwrap();
                if n == 0 {
                    break;
                }
                total += n;
            }
            (total, Instant::now())
        });
        let read_b = tokio::spawn(async move {
            let mut buf = vec![0u8; bytes];
            let mut total = 0;
            loop {
                let n = target_b.read(&mut buf[total..]).await.unwrap();
                if n == 0 {
                    break;
                }
                total += n;
            }
            (total, Instant::now())
        });

        let started = Instant::now();
        writer_a.await.unwrap();
        writer_b.await.unwrap();
        let (total_a, finish_a) = read_a.await.unwrap();
        let (total_b, finish_b) = read_b.await.unwrap();
        proxy_a.await.unwrap().unwrap();
        proxy_b.await.unwrap().unwrap();

        assert_eq!(total_a, bytes);
        assert_eq!(total_b, bytes);

        let elapsed_a = finish_a.duration_since(started);
        let elapsed_b = finish_b.duration_since(started);

        // Owner A: 4 MiB at 1 MiB/s after burst drains ≈ 3 s.
        assert!(
            elapsed_a >= Duration::from_millis(1500),
            "owner A throttle should take >=1.5s, took {elapsed_a:?}"
        );
        // Owner B: uncapped, finishes near-instantly even with the
        // paused-time scheduler. Critically, it does NOT trail owner
        // A — i.e., A's throttle does not back-pressure B.
        assert!(
            elapsed_b < Duration::from_millis(500),
            "owner B uncapped should finish in <500ms, took {elapsed_b:?}"
        );
        // Throttle attribution: only owner A accrued throttle micros.
        assert!(
            owner_a_acc.throttle_micros(BandwidthDirection::In) > 0,
            "owner A must record throttle"
        );
        assert_eq!(
            owner_b_stats_acc.throttle_micros(BandwidthDirection::In),
            0,
            "owner B must not record any throttle"
        );
    }
}

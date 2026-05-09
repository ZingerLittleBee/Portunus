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

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::scope::{BandwidthAcquire, BandwidthDirection, OwnerRateLimiter, RuleRateLimiter};
use super::stats::RateLimitStatsAccumulator;

/// Chunk size for the half-loops. Matches the default
/// `tokio::io::copy` internal buffer; large enough that per-chunk
/// overhead (one bucket acquire) is amortised, small enough that a
/// throttled flow doesn't park for >100 ms at 100 KB/s caps.
const CHUNK: usize = 16 * 1024;

/// Bidirectional copy with per-direction bandwidth throttling.
///
/// Returns `(bytes_in, bytes_out)` on success. `bytes_in` is the count
/// flowing inbound→outbound (peer to target); `bytes_out` is the
/// reverse — same convention as `tokio::io::copy_bidirectional`.
///
/// Errors propagate from either half — first error returned wins, and
/// the still-running half is cancelled by drop.
pub async fn copy_bidirectional_with_rate_limit<A, B>(
    inbound: &mut A,
    outbound: &mut B,
    limiter: Arc<RuleRateLimiter>,
    stats: Option<Arc<RateLimitStatsAccumulator>>,
    owner_limiter: Option<Arc<OwnerRateLimiter>>,
    owner_stats: Option<Arc<RateLimitStatsAccumulator>>,
) -> io::Result<(u64, u64)>
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    let (mut in_read, mut in_write) = tokio::io::split(inbound);
    let (mut out_read, mut out_write) = tokio::io::split(outbound);

    let limiter_in = Arc::clone(&limiter);
    let stats_in = stats.clone();
    let owner_in = owner_limiter.clone();
    let owner_stats_in = owner_stats.clone();
    let in_to_out = async {
        copy_with_cap(
            &mut in_read,
            &mut out_write,
            BandwidthDirection::In,
            &limiter_in,
            stats_in.as_deref(),
            owner_in.as_deref(),
            owner_stats_in.as_deref(),
        )
        .await
    };
    let limiter_out = Arc::clone(&limiter);
    let stats_out = stats.clone();
    let owner_out = owner_limiter.clone();
    let owner_stats_out = owner_stats.clone();
    let out_to_in = async {
        copy_with_cap(
            &mut out_read,
            &mut in_write,
            BandwidthDirection::Out,
            &limiter_out,
            stats_out.as_deref(),
            owner_out.as_deref(),
            owner_stats_out.as_deref(),
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
    limiter: &RuleRateLimiter,
    stats: Option<&RateLimitStatsAccumulator>,
    // T030: per-owner bucket consulted BEFORE the per-rule bucket on
    // every chunk (FR-013). Effective throughput is the lesser of
    // (owner_rate, rule_rate). Owner-direction throttle wall-clock
    // accumulates into `owner_stats`; rule-direction into `stats`.
    owner_limiter: Option<&OwnerRateLimiter>,
    owner_stats: Option<&RateLimitStatsAccumulator>,
) -> io::Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; CHUNK];
    let mut total: u64 = 0;
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            // Half-close: shutdown the writer half so peer sees FIN.
            // Errors here are non-fatal — the peer may have already
            // closed; we still report the bytes we successfully
            // forwarded.
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
        if let Some(o) = owner_limiter {
            loop {
                match o.acquire_bandwidth(direction, n as u64) {
                    BandwidthAcquire::Granted => break,
                    BandwidthAcquire::Throttled { deficit } => {
                        if let Some(s) = owner_stats {
                            let micros =
                                u64::try_from(deficit.as_micros()).unwrap_or(u64::MAX);
                            s.record_throttle(direction, micros);
                        }
                        tokio::time::sleep(deficit).await;
                    }
                }
            }
        }
        loop {
            match limiter.acquire_bandwidth(direction, n as u64) {
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
        writer.write_all(&buf[..n]).await?;
        total += n as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forwarder::rate_limit::scope::RuleRateLimiter;
    use forward_core::RateLimit;
    use std::time::Duration;
    use tokio::io::duplex;
    use tokio::time::Instant;

    fn limiter_for(rl: RateLimit) -> Arc<RuleRateLimiter> {
        Arc::new(RuleRateLimiter::from_envelope(&rl))
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
            copy_bidirectional_with_rate_limit(
                &mut inbound,
                &mut outbound,
                limiter_clone,
                Some(acc_clone),
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
            copy_bidirectional_with_rate_limit(
                &mut inbound,
                &mut outbound,
                limiter_clone,
                Some(acc_clone),
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

    /// T030: per-owner bandwidth cap throttles even when the per-rule
    /// bucket has plenty of tokens. Throttle wall-clock for the owner
    /// direction lands in the OWNER stats accumulator (FR-014); the
    /// rule accumulator stays at zero.
    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn t030_owner_bandwidth_cap_throttles_independently() {
        use crate::forwarder::rate_limit::scope::OwnerRateLimiter;
        // Rule allows 1 MiB/s — generous; owner allows 100 KiB/s — the
        // binding ceiling. Effective throughput must converge near the
        // owner rate, with throttle micros recorded against the owner
        // accumulator only.
        let rule_limiter = limiter_for(RateLimit {
            bandwidth_in_bps: Some(1024 * 1024),
            ..Default::default()
        });
        let rule_acc = Arc::new(RateLimitStatsAccumulator::new());
        let owner_limiter = Arc::new(OwnerRateLimiter::from_envelope(&RateLimit {
            bandwidth_in_bps: Some(100 * 1024),
            ..Default::default()
        }));
        let owner_acc = Arc::new(RateLimitStatsAccumulator::new());

        let (mut peer, mut inbound) = duplex(64 * 1024);
        let (mut outbound, mut target) = duplex(64 * 1024);

        let task_rule = Arc::clone(&rule_limiter);
        let task_rule_stats = Arc::clone(&rule_acc);
        let task_owner = Arc::clone(&owner_limiter);
        let task_owner_stats = Arc::clone(&owner_acc);
        let proxy = tokio::spawn(async move {
            copy_bidirectional_with_rate_limit(
                &mut inbound,
                &mut outbound,
                task_rule,
                Some(task_rule_stats),
                Some(task_owner),
                Some(task_owner_stats),
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
}

//! 013-traffic-quotas E1: TCP userspace quota-aware bidirectional copy.
//!
//! #52: PRE-DEBIT ordering. Each half-direction:
//!     read(buf) → quota.consume(n)
//!     Granted   → write_all(buf[..n]); total += n
//!     Exhausted → drop this chunk, shutdown writer half, return total
//!
//! The budget is claimed *before* the bytes go on the wire (mirroring
//! the UDP batched precedent `udp/flow.rs::quota_try_consume`). Because
//! `QuotaHandle::consume` is a saturating CAS, only chunks fully covered
//! by the budget are ever written, so with N connections sharing one
//! handle aggregate delivery never exceeds the budget — over-delivery
//! is bounded to 0 regardless of connection count. The trade-off is a
//! one-time boundary UNDER-delivery of the straddling chunk (≤ 1 chunk
//! per handle), the fail-safe direction for a byte quota. A
//! write-after-debit (or the old write-then-debit) ordering would let
//! each in-flight connection push a full chunk past the budget
//! (overshoot ≈ in-flight-chunks × `COPY_BUF_SIZE`).

#![allow(
    dead_code,
    reason = "Consumed by E2 — `copy_uncapped` routes here when a QuotaHandle is attached."
)]

use std::io;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

use super::{ConsumeOutcome, QuotaHandle};

const COPY_BUF_SIZE: usize = 64 * 1024;

/// Run a bidirectional copy with quota enforcement over a concrete pair
/// of `TcpStream`s. Returns the `(bytes_in, bytes_out)` actually
/// delivered to upstream / downstream. On quota exhaustion the copy
/// returns cleanly with the delivered totals; the caller closes the
/// sockets.
///
/// Uses `TcpStream::split` (native, lock-free) rather than
/// `tokio::io::split` (a `BiLock` taken on every half poll) — #51.
pub async fn copy_bidirectional_with_quota(
    inbound: &mut TcpStream,
    outbound: &mut TcpStream,
    quota: Arc<QuotaHandle>,
) -> io::Result<(u64, u64)> {
    let (mut ri, mut wi) = inbound.split();
    let (mut ro, mut wo) = outbound.split();
    let q_fwd = Arc::clone(&quota);
    let q_rev = quota;

    let fwd = async { copy_one_dir(&mut ri, &mut wo, &q_fwd).await };
    let rev = async { copy_one_dir(&mut ro, &mut wi, &q_rev).await };
    let (a, b) = tokio::try_join!(fwd, rev)?;
    Ok((a, b))
}

async fn copy_one_dir<R, W>(reader: &mut R, writer: &mut W, quota: &QuotaHandle) -> io::Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; COPY_BUF_SIZE];
    let mut total: u64 = 0;
    loop {
        if quota.is_exhausted() {
            let _ = writer.shutdown().await;
            return Ok(total);
        }
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            let _ = writer.shutdown().await;
            return Ok(total);
        }
        // #52: pre-debit BEFORE writing. Only a fully-granted chunk is
        // forwarded; a straddling draw spends the remaining budget but
        // the chunk is dropped, so aggregate delivery across all
        // connections sharing this handle can never exceed the budget.
        match quota.consume(i64::try_from(n).unwrap_or(i64::MAX)) {
            ConsumeOutcome::Granted => {
                writer.write_all(&buf[..n]).await?;
                total = total.saturating_add(u64::try_from(n).unwrap_or(0));
            }
            ConsumeOutcome::Exhausted => {
                let _ = writer.shutdown().await;
                return Ok(total);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! Unit-test the directional inner loop (`copy_one_dir`) directly
    //! to avoid the duplex BrokenPipe race that hits a full
    //! bidirectional copy when one side closes before the other has
    //! drained — that race is a property of `tokio::io::duplex`, not
    //! of the code under test. Integration coverage of the
    //! bidirectional orchestrator comes via the E2E test in Phase H
    //! that drives real sockets through `copy_uncapped`.
    use super::*;
    use crate::forwarder::quota::QuotaState;
    use tokio::io::{AsyncWriteExt, duplex};

    fn handle(remaining: i64) -> Arc<QuotaHandle> {
        Arc::new(QuotaHandle::new(
            "alice".into(),
            "edge-01".into(),
            QuotaState {
                monthly_bytes: 1_000_000,
                budget_remaining_bytes: remaining,
                exhausted: false,
            },
        ))
    }

    #[tokio::test]
    async fn copy_one_dir_delivers_all_bytes_under_budget() {
        let (mut writer_a, mut reader_a) = duplex(8 * 1024);
        let (mut reader_b, mut writer_b) = duplex(8 * 1024);
        let quota = handle(10_000);
        let q = Arc::clone(&quota);

        let producer = tokio::spawn(async move {
            writer_a.write_all(&[7u8; 1_000]).await.unwrap();
            drop(writer_a);
        });
        let copier =
            tokio::spawn(async move { copy_one_dir(&mut reader_a, &mut writer_b, &q).await });
        // Drain so writer_b's buffer is consumed; otherwise write_all
        // blocks once the buffer fills.
        let drainer = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut sink = Vec::new();
            reader_b.read_to_end(&mut sink).await.unwrap();
            sink
        });

        producer.await.unwrap();
        let total = copier.await.unwrap().unwrap();
        let sink = drainer.await.unwrap();
        assert_eq!(total, 1_000);
        assert_eq!(sink.len(), 1_000);
        assert_eq!(quota.remaining(), 9_000);
        assert!(!quota.is_exhausted());
    }

    #[tokio::test]
    async fn copy_one_dir_drops_chunk_that_straddles_budget() {
        let (mut writer_a, mut reader_a) = duplex(8 * 1024);
        let (mut reader_b, mut writer_b) = duplex(8 * 1024);
        let quota = handle(64);
        let q = Arc::clone(&quota);

        let producer = tokio::spawn(async move {
            // Push 200 bytes — more than the 64-byte budget.
            writer_a.write_all(&[7u8; 200]).await.unwrap();
            drop(writer_a);
        });
        let copier =
            tokio::spawn(async move { copy_one_dir(&mut reader_a, &mut writer_b, &q).await });
        let drainer = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut sink = Vec::new();
            reader_b.read_to_end(&mut sink).await.unwrap();
            sink
        });

        producer.await.unwrap();
        let total = copier.await.unwrap().unwrap();
        let sink = drainer.await.unwrap();
        // #52 pre-debit: the whole 200-byte push arrives in one read.
        // `consume(200)` on a 64-byte budget saturates to 0 and returns
        // Exhausted (the chunk straddles the budget), so the chunk is
        // DROPPED — never written — and the direction half-closes.
        // Delivery therefore never exceeds the budget (here 0 of this
        // straddling chunk landed). The old write-then-debit path would
        // have forwarded all 200 bytes, over-delivering by 136.
        assert_eq!(total, 0, "straddling chunk is dropped, not forwarded");
        assert_eq!(sink.len(), 0);
        assert!(quota.is_exhausted());
        assert_eq!(quota.remaining(), 0);
    }

    /// #52 regression: N connections sharing one `QuotaHandle` must not
    /// aggregate-overshoot the budget. With the pre-debit ordering every
    /// forwarded chunk is fully covered by a `Granted` draw, and
    /// `consume` is a saturating CAS, so the sum of bytes delivered
    /// across BOTH connections can never exceed the budget — the bound
    /// is a constant (0 overshoot), independent of connection count.
    /// Under the old write-then-debit ordering each connection could
    /// push a full `COPY_BUF_SIZE` chunk past the budget.
    #[tokio::test]
    async fn two_connections_sharing_handle_cannot_overshoot_budget() {
        use tokio::io::AsyncReadExt;

        const BUDGET: i64 = 100_000;
        // Big enough that neither producer blocks even if its copier
        // stops reading early (leftover bytes sit in the buffer, dropped
        // when the reader half is torn down at task end).
        const PIPE: usize = 512 * 1024;
        const PUSH: usize = 256 * 1024;

        let quota = handle(BUDGET);

        async fn drive(quota: Arc<QuotaHandle>) -> u64 {
            let (mut wa, mut ra) = duplex(PIPE);
            let (mut rb, mut wb) = duplex(PIPE);
            let producer = tokio::spawn(async move {
                let _ = wa.write_all(&vec![0x5A_u8; PUSH]).await;
                drop(wa);
            });
            let copier = tokio::spawn(async move { copy_one_dir(&mut ra, &mut wb, &quota).await });
            let drainer = tokio::spawn(async move {
                let mut sink = Vec::new();
                let _ = rb.read_to_end(&mut sink).await;
                sink.len()
            });
            let _ = producer.await;
            let total = copier.await.unwrap().unwrap();
            let received = drainer.await.unwrap();
            assert_eq!(
                received,
                usize::try_from(total).unwrap(),
                "each connection delivers exactly what it forwarded"
            );
            total
        }

        let (total_a, total_b) = tokio::join!(drive(Arc::clone(&quota)), drive(Arc::clone(&quota)));

        let aggregate = total_a + total_b;
        assert!(
            aggregate <= u64::try_from(BUDGET).unwrap(),
            "aggregate delivery {aggregate} must not exceed the {BUDGET}-byte budget \
             (overshoot bound is a constant 0, independent of connection count)"
        );
        assert!(aggregate > 0, "some bytes must have been delivered");
        assert!(quota.is_exhausted());
        assert_eq!(quota.remaining(), 0);
    }

    #[tokio::test]
    async fn copy_one_dir_returns_immediately_when_already_exhausted() {
        let (writer_a, mut reader_a) = duplex(8 * 1024);
        let (_reader_b, mut writer_b) = duplex(8 * 1024);
        drop(writer_a); // EOF — but we shouldn't even read.
        let quota = Arc::new(QuotaHandle::new(
            "alice".into(),
            "edge-01".into(),
            QuotaState {
                monthly_bytes: 100,
                budget_remaining_bytes: 0,
                exhausted: true,
            },
        ));
        let total = copy_one_dir(&mut reader_a, &mut writer_b, &quota)
            .await
            .unwrap();
        assert_eq!(total, 0);
        assert!(quota.is_exhausted());
    }
}

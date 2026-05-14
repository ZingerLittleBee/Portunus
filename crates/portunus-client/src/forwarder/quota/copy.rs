//! 013-traffic-quotas E1: TCP userspace quota-aware bidirectional copy.
//!
//! Records bytes only AFTER `write_all` succeeds — mirrors
//! `rate_limit/copy.rs:213` semantics so a torn IO never lies about
//! delivery. Each half-direction:
//!     read(buf) → write_all(buf[..n]) → quota.consume(n)
//!     if Exhausted → shutdown writer half, return delivered total
//!
//! The quota is consulted *after* the bytes are on the wire so the
//! counters and the consume budget agree byte-for-byte.

#![allow(
    dead_code,
    reason = "Consumed by E2 — `copy_uncapped` routes here when a QuotaHandle is attached."
)]

use std::io;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::{ConsumeOutcome, QuotaHandle};

const COPY_BUF_SIZE: usize = 64 * 1024;

/// Run a bidirectional copy with quota enforcement. Returns the
/// `(bytes_in, bytes_out)` actually delivered to upstream / downstream.
/// On quota exhaustion the copy returns cleanly with the delivered
/// totals; the caller closes the sockets.
pub async fn copy_bidirectional_with_quota<I, O>(
    inbound: &mut I,
    outbound: &mut O,
    quota: Arc<QuotaHandle>,
) -> io::Result<(u64, u64)>
where
    I: AsyncRead + AsyncWrite + Unpin,
    O: AsyncRead + AsyncWrite + Unpin,
{
    let (mut ri, mut wi) = tokio::io::split(inbound);
    let (mut ro, mut wo) = tokio::io::split(outbound);
    let q_fwd = Arc::clone(&quota);
    let q_rev = quota;

    let fwd = async move { copy_one_dir(&mut ri, &mut wo, &q_fwd).await };
    let rev = async move { copy_one_dir(&mut ro, &mut wi, &q_rev).await };
    let (a, b) = tokio::try_join!(fwd, rev)?;
    Ok((a, b))
}

async fn copy_one_dir<R, W>(
    reader: &mut R,
    writer: &mut W,
    quota: &QuotaHandle,
) -> io::Result<u64>
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
        writer.write_all(&buf[..n]).await?;
        total = total.saturating_add(u64::try_from(n).unwrap_or(0));
        match quota.consume(i64::try_from(n).unwrap_or(i64::MAX)) {
            ConsumeOutcome::Granted => {}
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
        let copier = tokio::spawn(async move {
            copy_one_dir(&mut reader_a, &mut writer_b, &q).await
        });
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
    async fn copy_one_dir_exhausts_and_returns_partial() {
        let (mut writer_a, mut reader_a) = duplex(8 * 1024);
        let (mut reader_b, mut writer_b) = duplex(8 * 1024);
        let quota = handle(64);
        let q = Arc::clone(&quota);

        let producer = tokio::spawn(async move {
            // Push 200 bytes — more than the 64-byte budget.
            writer_a.write_all(&[7u8; 200]).await.unwrap();
            drop(writer_a);
        });
        let copier = tokio::spawn(async move {
            copy_one_dir(&mut reader_a, &mut writer_b, &q).await
        });
        let drainer = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            let mut sink = Vec::new();
            reader_b.read_to_end(&mut sink).await.unwrap();
            sink
        });

        producer.await.unwrap();
        let total = copier.await.unwrap().unwrap();
        let sink = drainer.await.unwrap();
        // The first read returns up to 64 KiB (one bufsize). The whole
        // 200-byte producer push fits in a single read, so we deliver
        // 200 bytes, write_all succeeds, then `consume(200)` returns
        // Exhausted (straddled). `total` is the bytes that landed.
        assert_eq!(total, 200);
        assert_eq!(sink.len(), 200);
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

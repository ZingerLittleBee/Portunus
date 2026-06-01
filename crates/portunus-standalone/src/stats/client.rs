//! UDS stats client + 60 s ring buffer + rate calculation.

use std::collections::VecDeque;
use std::path::Path;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::net::UnixStream;

use super::{Hello, RuleSnap, Snapshot};

const RING_WINDOW_MS: u64 = 60_000;

/// Floor for a peer-advertised `refresh_ms`. The value arrives over the wire
/// and is otherwise untrusted: `refresh_ms == 0` would make the ring-capacity
/// division blow up to `usize::MAX` and abort the process on allocation.
const MIN_REFRESH_MS: u64 = 50;

/// Hard ceiling on the ring capacity regardless of `refresh_ms`, as a second
/// line of defence against an absurd capacity request.
const MAX_RING_CAPACITY: usize = 4096;

/// Maximum bytes for a single newline-delimited frame. A well-behaved daemon
/// never approaches this; the cap stops a peer that never sends a newline
/// from growing the read buffer without bound (OOM).
const MAX_LINE_BYTES: u64 = 1 << 20; // 1 MiB

/// Read one newline-delimited line, but never buffer more than
/// `MAX_LINE_BYTES`. Returns the bytes read (0 on EOF). If the cap is hit
/// without a newline, the partial line is returned and the subsequent
/// `serde_json` parse fails with `InvalidData` — the connection is dropped.
pub(crate) async fn read_line_bounded<R>(
    reader: &mut R,
    line: &mut String,
) -> std::io::Result<usize>
where
    R: AsyncBufRead + Unpin,
{
    reader.take(MAX_LINE_BYTES).read_line(line).await
}

#[derive(Debug)]
pub struct Client {
    pub hello: Hello,
    pub ring: VecDeque<Snapshot>,
    pub capacity: usize,
}

impl Client {
    /// Connect, read Hello, return a Client ready to ingest snapshots.
    ///
    /// # Errors
    /// Returns `std::io::Error` if the socket cannot be connected or
    /// the Hello line cannot be read/parsed.
    pub async fn connect(path: &Path) -> std::io::Result<(Self, BufReader<UnixStream>)> {
        let stream = UnixStream::connect(path).await?;
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        read_line_bounded(&mut reader, &mut line).await?;
        let hello: Hello = serde_json::from_str(line.trim_end())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        // RING_WINDOW_MS / refresh_ms samples cover 60 s; +1 so the oldest
        // sample remains available for the rate delta. ceil() ensures we
        // always have at least a full window even if refresh_ms doesn't
        // divide evenly. `refresh_ms` is peer-supplied, so clamp it to a
        // floor (avoid divide-by-zero → usize::MAX → abort) and cap the
        // resulting capacity.
        let refresh_ms = hello.refresh_ms.max(MIN_REFRESH_MS);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let cap = (((RING_WINDOW_MS as f64) / (refresh_ms as f64)).ceil() as usize + 1)
            .min(MAX_RING_CAPACITY);
        Ok((
            Client {
                hello,
                ring: VecDeque::with_capacity(cap),
                capacity: cap,
            },
            reader,
        ))
    }

    /// Append a snapshot; evict the oldest if we exceed capacity.
    pub fn push(&mut self, snap: Snapshot) {
        self.ring.push_back(snap);
        while self.ring.len() > self.capacity {
            self.ring.pop_front();
        }
    }

    /// Bytes-per-second inbound rate for a given rule id, derived from
    /// the most recent two snapshots using `uptime_ms` (monotonic) for dt.
    /// Returns 0 if fewer than two samples or dt is 0.
    #[must_use]
    pub fn in_rate(&self, rule_id: &str) -> u64 {
        self.field_rate(rule_id, |s| s.bytes_in)
    }

    /// Bytes-per-second outbound rate for a given rule id.
    #[must_use]
    pub fn out_rate(&self, rule_id: &str) -> u64 {
        self.field_rate(rule_id, |s| s.out)
    }

    fn field_rate(&self, rule_id: &str, f: impl Fn(&RuleSnap) -> u64) -> u64 {
        if self.ring.len() < 2 {
            return 0;
        }
        let last = self.ring.back().unwrap();
        let prev = self.ring.get(self.ring.len() - 2).unwrap();
        let dt_ms = last.uptime_ms.saturating_sub(prev.uptime_ms);
        if dt_ms == 0 {
            return 0;
        }
        let cur = last.r.iter().find(|r| r.id == rule_id).map_or(0, &f);
        let pre = prev.r.iter().find(|r| r.id == rule_id).map_or(0, &f);
        cur.saturating_sub(pre).saturating_mul(1000) / dt_ms
    }
}

/// One-shot mode: connect, read Hello + one Snapshot, return both as
/// pretty-printed JSON.
///
/// # Errors
/// Returns `std::io::Error` if the socket cannot be connected, the
/// Hello or Snapshot line cannot be read, or serialisation fails.
pub async fn once(path: &Path) -> std::io::Result<String> {
    let (client, mut reader) = Client::connect(path).await?;
    let mut line = String::new();
    read_line_bounded(&mut reader, &mut line).await?;
    let snap: Snapshot = serde_json::from_str(line.trim_end())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let out = serde_json::json!({
        "hello": client.hello,
        "snapshot": snap,
    });
    serde_json::to_string_pretty(&out).map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::{ErrorSnap, ProcessSnap, RuleSnap};

    fn snap(uptime_ms: u64, seq: u64, in_bytes: u64) -> Snapshot {
        Snapshot {
            t_ms: 0,
            uptime_ms,
            seq,
            process: ProcessSnap::default(),
            r: vec![RuleSnap {
                id: "x".into(),
                bytes_in: in_bytes,
                out: 0,
                conns_active: 0,
                conns_total: 0,
                datagrams_in: 0,
                datagrams_out: 0,
                flows_active: 0,
                target_failovers_total: 0,
                err: ErrorSnap::default(),
            }],
        }
    }

    #[test]
    fn ring_capacity_pins_window_at_60_seconds() {
        let hello = Hello {
            v: 1,
            daemon_version: "1.6.0".into(),
            daemon_started_at_ms: 0,
            refresh_ms: 250,
            rules: vec![],
        };
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let cap = ((60_000f64) / (hello.refresh_ms as f64)).ceil() as usize + 1;
        assert_eq!(cap, 241);
    }

    #[test]
    fn rate_uses_uptime_ms_not_wall_clock() {
        let mut c = Client {
            hello: Hello {
                v: 1,
                daemon_version: "x".into(),
                daemon_started_at_ms: 0,
                refresh_ms: 1000,
                rules: vec![],
            },
            ring: VecDeque::new(),
            capacity: 60,
        };
        c.push(snap(1000, 1, 0));
        c.push(snap(2000, 2, 10_000));
        // 10 KB over 1 s → 10_000 B/s
        assert_eq!(c.in_rate("x"), 10_000);
    }

    #[tokio::test]
    async fn malicious_refresh_ms_zero_does_not_blow_capacity() {
        // A peer advertising refresh_ms = 0 must not produce a usize::MAX
        // capacity (which would abort on allocation). Serve a crafted Hello
        // over a real UDS and confirm connect() returns a sane capacity.
        let dir = std::env::temp_dir().join(format!("portunus-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sock = dir.join("evil.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        let server = tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                use tokio::io::AsyncWriteExt;
                let hello = serde_json::json!({
                    "v": 1,
                    "daemon_version": "evil",
                    "daemon_started_at_ms": 0,
                    "refresh_ms": 0,
                    "rules": [],
                });
                let mut buf = serde_json::to_vec(&hello).unwrap();
                buf.push(b'\n');
                let _ = stream.write_all(&buf).await;
                let _ = stream.flush().await;
            }
        });
        let (client, _reader) = Client::connect(&sock).await.unwrap();
        assert!(
            client.capacity <= MAX_RING_CAPACITY,
            "capacity {} exceeded cap",
            client.capacity
        );
        let _ = server.await;
        let _ = std::fs::remove_file(&sock);
    }

    #[test]
    fn rate_is_zero_with_lt_two_snapshots() {
        let mut c = Client {
            hello: Hello {
                v: 1,
                daemon_version: "x".into(),
                daemon_started_at_ms: 0,
                refresh_ms: 1000,
                rules: vec![],
            },
            ring: VecDeque::new(),
            capacity: 60,
        };
        c.push(snap(1000, 1, 500));
        assert_eq!(c.in_rate("x"), 0);
    }
}

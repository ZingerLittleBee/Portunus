//! UDS stats protocol types. Shared by the daemon-side server
//! (`stats::server`) and the client (`stats::client`).
//!
//! Wire format: JSON-lines (one document per `\n`-terminated line)
//! over a Unix domain socket. The server sends exactly one `Hello`
//! immediately after `accept()`, then a `Snapshot` every
//! `refresh_ms`.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hello {
    pub v: u32,
    pub daemon_version: String,
    pub daemon_started_at_ms: u64,
    pub refresh_ms: u64,
    pub rules: Vec<RuleMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleMeta {
    pub id: String,
    pub name: String,
    pub proto: String, // "tcp" | "udp"
    pub listen: String,
    pub targets: Vec<TargetMeta>,
    pub splice_capable: bool,
    pub udp_max_flows: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetMeta {
    pub host: String,
    pub port: u16,
    pub priority: u32,
    pub proxy_protocol: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub t_ms: u64,
    pub uptime_ms: u64,
    pub seq: u64,
    pub process: ProcessSnap,
    pub r: Vec<RuleSnap>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ProcessSnap {
    pub fd_open: Option<u32>,
    pub fd_limit: Option<u64>,
    pub rss_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleSnap {
    pub id: String,
    #[serde(rename = "in")]
    pub bytes_in: u64,
    pub out: u64,
    pub conns_active: u32,
    pub conns_total: u64,
    pub datagrams_in: u64,
    pub datagrams_out: u64,
    pub flows_active: u32,
    pub target_failovers_total: u64,
    pub err: ErrorSnap,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ErrorSnap {
    pub port_in_use: u64,
    pub upstream_connect_failed: u64,
    pub icmp_evict: u64,
    pub emsgsize: u64,
    pub wouldblock: u64,
    pub addflow_dropped: u64,
    pub dns_failures: u64,
    pub flows_dropped_overflow: u64,
}

pub mod client;
pub mod server;
#[cfg(feature = "stats-tui")]
pub mod tui;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_serde_roundtrip() {
        let snap = Snapshot {
            t_ms: 1_748_400_015_234,
            uptime_ms: 304_523,
            seq: 42,
            process: ProcessSnap {
                fd_open: Some(217),
                fd_limit: Some(65535),
                rss_bytes: Some(18_874_368),
            },
            r: vec![RuleSnap {
                id: "118".into(),
                bytes_in: 1024,
                out: 2048,
                conns_active: 3,
                conns_total: 124,
                datagrams_in: 0,
                datagrams_out: 0,
                flows_active: 0,
                target_failovers_total: 0,
                err: ErrorSnap::default(),
            }],
        };
        let s = serde_json::to_string(&snap).unwrap();
        let back: Snapshot = serde_json::from_str(&s).unwrap();
        assert_eq!(back.t_ms, snap.t_ms);
        assert_eq!(back.r.len(), 1);
        assert_eq!(back.r[0].bytes_in, 1024);
        // ensure the JSON uses the short field name "in"
        assert!(s.contains("\"in\":1024"));
    }

    #[test]
    fn hello_serde_roundtrip() {
        let h = Hello {
            v: PROTOCOL_VERSION,
            daemon_version: "1.6.0".into(),
            daemon_started_at_ms: 1_748_400_000_000,
            refresh_ms: 1000,
            rules: vec![RuleMeta {
                id: "abc".into(),
                name: "ssh".into(),
                proto: "tcp".into(),
                listen: "2222".into(),
                targets: vec![TargetMeta {
                    host: "10.0.0.5".into(),
                    port: 22,
                    priority: 0,
                    proxy_protocol: None,
                }],
                splice_capable: true,
                udp_max_flows: None,
            }],
        };
        let s = serde_json::to_string(&h).unwrap();
        let back: Hello = serde_json::from_str(&s).unwrap();
        assert_eq!(back.v, 1);
        assert_eq!(back.rules[0].targets[0].host, "10.0.0.5");
    }
}

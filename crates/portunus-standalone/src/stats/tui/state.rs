//! TUI application state. Holds selection, sort, pause,
//! and an optional client-side baseline for "session reset".

use std::collections::HashMap;
use std::time::Instant;

use crate::stats::tui::probe::ProbeSample;
use crate::stats::{RuleSnap, Snapshot};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Detail,
    Errors,
}

impl Tab {
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Tab::Overview => Tab::Detail,
            Tab::Detail => Tab::Errors,
            Tab::Errors => Tab::Overview,
        }
    }
    #[must_use]
    pub fn prev(self) -> Self {
        match self {
            Tab::Overview => Tab::Errors,
            Tab::Detail => Tab::Overview,
            Tab::Errors => Tab::Detail,
        }
    }
    #[must_use]
    pub fn index(self) -> usize {
        match self {
            Tab::Overview => 0,
            Tab::Detail => 1,
            Tab::Errors => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    RateIn,
    TotalIn,
    Name,
    Conns,
}

impl SortKey {
    #[must_use]
    pub fn cycle(self) -> Self {
        match self {
            SortKey::RateIn => SortKey::TotalIn,
            SortKey::TotalIn => SortKey::Name,
            SortKey::Name => SortKey::Conns,
            SortKey::Conns => SortKey::RateIn,
        }
    }
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            SortKey::RateIn => "rate",
            SortKey::TotalIn => "total",
            SortKey::Name => "name",
            SortKey::Conns => "conns",
        }
    }
}

#[derive(Debug)]
pub struct AppState {
    pub tab: Tab,
    pub selected: usize,
    pub sort: SortKey,
    pub sort_desc: bool,
    pub paused: bool,
    pub show_help: bool,
    /// Captured cumulative values for client-side "session reset".
    pub baseline: HashMap<String, BaselineEntry>,
    /// Latest probe sample per rule id (active target only).
    pub probes: HashMap<String, ProbeSample>,
    /// When the last probe was issued; `None` until the first probe.
    pub last_probe_at: Option<Instant>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BaselineEntry {
    pub bytes_in: u64,
    pub out: u64,
    pub conns_total: u64,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            tab: Tab::Overview,
            selected: 0,
            sort: SortKey::RateIn,
            sort_desc: true,
            paused: false,
            show_help: false,
            baseline: HashMap::new(),
            probes: HashMap::new(),
            last_probe_at: None,
        }
    }

    pub fn reset_baseline(&mut self, snap: &Snapshot) {
        self.baseline.clear();
        for r in &snap.r {
            self.baseline.insert(
                r.id.clone(),
                BaselineEntry {
                    bytes_in: r.bytes_in,
                    out: r.out,
                    conns_total: r.conns_total,
                },
            );
        }
    }

    /// Display value: cumulative minus baseline (if set), saturating.
    #[must_use]
    pub fn displayed_in(&self, rule: &RuleSnap) -> u64 {
        self.baseline
            .get(&rule.id)
            .map_or(rule.bytes_in, |b| rule.bytes_in.saturating_sub(b.bytes_in))
    }
    #[must_use]
    pub fn displayed_out(&self, rule: &RuleSnap) -> u64 {
        self.baseline
            .get(&rule.id)
            .map_or(rule.out, |b| rule.out.saturating_sub(b.out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::{ErrorSnap, ProcessSnap};

    fn rule(id: &str, in_b: u64) -> RuleSnap {
        RuleSnap {
            id: id.into(),
            bytes_in: in_b,
            out: 0,
            conns_active: 0,
            conns_total: 0,
            datagrams_in: 0,
            datagrams_out: 0,
            flows_active: 0,
            target_failovers_total: 0,
            err: ErrorSnap::default(),
        }
    }

    #[test]
    fn baseline_reset_subtracts() {
        let mut s = AppState::new();
        let snap = Snapshot {
            t_ms: 0,
            uptime_ms: 0,
            seq: 0,
            process: ProcessSnap::default(),
            r: vec![rule("x", 1000)],
        };
        s.reset_baseline(&snap);
        let later = rule("x", 1500);
        assert_eq!(s.displayed_in(&later), 500);
    }

    #[test]
    fn tab_cycle() {
        assert_eq!(Tab::Overview.next(), Tab::Detail);
        assert_eq!(Tab::Errors.next(), Tab::Overview);
        assert_eq!(Tab::Overview.prev(), Tab::Errors);
    }

    #[test]
    fn tab_next_covers_all_arms() {
        assert_eq!(Tab::Overview.next(), Tab::Detail);
        assert_eq!(Tab::Detail.next(), Tab::Errors);
        assert_eq!(Tab::Errors.next(), Tab::Overview);
    }

    #[test]
    fn tab_prev_covers_all_arms() {
        assert_eq!(Tab::Overview.prev(), Tab::Errors);
        assert_eq!(Tab::Detail.prev(), Tab::Overview);
        assert_eq!(Tab::Errors.prev(), Tab::Detail);
    }

    #[test]
    fn tab_index_covers_all_arms() {
        assert_eq!(Tab::Overview.index(), 0);
        assert_eq!(Tab::Detail.index(), 1);
        assert_eq!(Tab::Errors.index(), 2);
    }

    #[test]
    fn sort_key_cycle_covers_all_arms() {
        assert_eq!(SortKey::RateIn.cycle(), SortKey::TotalIn);
        assert_eq!(SortKey::TotalIn.cycle(), SortKey::Name);
        assert_eq!(SortKey::Name.cycle(), SortKey::Conns);
        assert_eq!(SortKey::Conns.cycle(), SortKey::RateIn);
    }

    #[test]
    fn sort_key_label_covers_all_arms() {
        assert_eq!(SortKey::RateIn.label(), "rate");
        assert_eq!(SortKey::TotalIn.label(), "total");
        assert_eq!(SortKey::Name.label(), "name");
        assert_eq!(SortKey::Conns.label(), "conns");
    }

    #[test]
    fn app_state_default_matches_new() {
        let d = AppState::default();
        let n = AppState::new();
        assert_eq!(d.tab, n.tab);
        assert_eq!(d.selected, n.selected);
        assert_eq!(d.sort, n.sort);
        assert_eq!(d.sort_desc, n.sort_desc);
        assert_eq!(d.paused, n.paused);
        assert_eq!(d.show_help, n.show_help);
        assert!(d.baseline.is_empty());
        assert!(d.probes.is_empty());
        assert!(d.last_probe_at.is_none());
    }

    #[test]
    fn displayed_out_subtracts_baseline() {
        let mut s = AppState::new();
        let snap = Snapshot {
            t_ms: 0,
            uptime_ms: 0,
            seq: 0,
            process: ProcessSnap::default(),
            r: vec![{
                let mut r = rule("y", 0);
                r.out = 200;
                r
            }],
        };
        s.reset_baseline(&snap);
        let mut later = rule("y", 0);
        later.out = 350;
        assert_eq!(s.displayed_out(&later), 150);
    }

    #[test]
    fn displayed_out_without_baseline_returns_cumulative() {
        let s = AppState::new();
        let mut r = rule("z", 0);
        r.out = 42;
        assert_eq!(s.displayed_out(&r), 42);
    }

    #[test]
    fn displayed_in_without_baseline_returns_cumulative() {
        let s = AppState::new();
        let r = rule("z", 77);
        assert_eq!(s.displayed_in(&r), 77);
    }

    #[test]
    fn displayed_values_saturate_when_cumulative_drops() {
        let mut s = AppState::new();
        let snap = Snapshot {
            t_ms: 0,
            uptime_ms: 0,
            seq: 0,
            process: ProcessSnap::default(),
            r: vec![{
                let mut r = rule("w", 1000);
                r.out = 1000;
                r
            }],
        };
        s.reset_baseline(&snap);
        // A lower cumulative (e.g. process restart) saturates to zero.
        let mut later = rule("w", 500);
        later.out = 400;
        assert_eq!(s.displayed_in(&later), 0);
        assert_eq!(s.displayed_out(&later), 0);
    }
}

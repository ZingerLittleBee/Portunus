//! TUI application state. Holds selection, sort, filter, pause,
//! and an optional client-side baseline for "session reset".

use std::collections::HashMap;

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
    pub filter: String,
    pub paused: bool,
    pub show_help: bool,
    /// Captured cumulative values for client-side "session reset".
    pub baseline: HashMap<String, BaselineEntry>,
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
            filter: String::new(),
            paused: false,
            show_help: false,
            baseline: HashMap::new(),
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
}

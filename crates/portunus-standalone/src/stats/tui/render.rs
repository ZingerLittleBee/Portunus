//! Render functions for the three TUI tabs.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{
    Block, Borders, Cell, Gauge, Paragraph, Row, Sparkline, Table, TableState, Tabs,
};

use super::format::{fmt_bytes, fmt_rate};
use super::state::{AppState, Tab};
use crate::stats::client::Client;

pub fn render(frame: &mut Frame, area: Rect, client: &Client, state: &mut AppState) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // header
            Constraint::Length(1), // tabs
            Constraint::Min(5),    // body
            Constraint::Length(1), // footer
        ])
        .split(area);

    render_header(frame, layout[0], client);
    render_tabs(frame, layout[1], state.tab);
    match state.tab {
        Tab::Overview => render_overview(frame, layout[2], client, state),
        Tab::Detail   => render_detail(frame, layout[2], client, state),
        Tab::Errors   => render_errors(frame, layout[2], client, state),
    }
    render_footer(frame, layout[3], client);

    if state.show_help {
        render_help_overlay(frame, area);
    }
}

fn render_header(frame: &mut Frame, area: Rect, client: &Client) {
    let title = format!(" portunus-standalone stats — daemon v{} ",
                        client.hello.daemon_version);
    let p = Paragraph::new(title).style(Style::default().add_modifier(Modifier::BOLD));
    frame.render_widget(p, area);
}

fn render_tabs(frame: &mut Frame, area: Rect, current: Tab) {
    let titles = vec!["Overview", "Detail", "Errors"];
    let t = Tabs::new(titles)
        .select(current.index())
        .style(Style::default())
        .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
    frame.render_widget(t, area);
}

fn render_overview(frame: &mut Frame, area: Rect, client: &Client, state: &mut AppState) {
    let rows: Vec<Row> = client.hello.rules.iter().map(|meta| {
        let last = client.ring.back();
        let snap_row = last.and_then(|s| s.r.iter().find(|r| r.id == meta.id));
        let in_rate = client.in_rate(&meta.id);
        let out_rate = client.out_rate(&meta.id);
        let (conns, conns_total) = snap_row
            .map_or((0, 0), |r| (r.conns_active, r.conns_total));
        Row::new(vec![
            Cell::from(meta.name.clone()),
            Cell::from(meta.proto.clone()),
            Cell::from(meta.listen.clone()),
            Cell::from(fmt_rate(in_rate)),
            Cell::from(fmt_rate(out_rate)),
            Cell::from(format!("{conns}/{conns_total}")),
        ])
    }).collect();

    let header = Row::new(vec!["name", "proto", "listen", "in rate", "out rate", "conns"])
        .style(Style::default().add_modifier(Modifier::BOLD));

    let table = Table::new(rows, [
        Constraint::Length(18),
        Constraint::Length(5),
        Constraint::Length(12),
        Constraint::Length(13),
        Constraint::Length(13),
        Constraint::Length(12),
    ])
    .header(header)
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .highlight_symbol("▶ ");

    let mut ts = TableState::default();
    ts.select(Some(state.selected.min(client.hello.rules.len().saturating_sub(1))));
    frame.render_stateful_widget(table, area, &mut ts);
}

fn render_detail(frame: &mut Frame, area: Rect, client: &Client, state: &AppState) {
    if client.hello.rules.is_empty() {
        let p = Paragraph::new("no rules").block(Block::default().borders(Borders::ALL));
        frame.render_widget(p, area);
        return;
    }
    let idx = state.selected.min(client.hello.rules.len() - 1);
    let meta = &client.hello.rules[idx];
    let last = client.ring.back();
    let snap = last.and_then(|s| s.r.iter().find(|r| r.id == meta.id));

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Detail · {} ({} {}) ",
                       meta.name, meta.proto, meta.listen));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let vchunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // in spark
            Constraint::Length(2), // out spark
            Constraint::Length(2), // gauge
            Constraint::Min(1),    // text
        ])
        .split(inner);

    let in_series: Vec<u64> = client.ring.iter()
        .filter_map(|s| s.r.iter().find(|r| r.id == meta.id))
        .map(|r| r.bytes_in)
        .collect();
    let uptime_series: Vec<u64> = client.ring.iter().map(|s| s.uptime_ms).collect();
    let in_rates = pairwise_rates(&in_series, &uptime_series);
    let sp_in = Sparkline::default()
        .block(Block::default().title(format!("in  {}",  fmt_rate(client.in_rate(&meta.id)))))
        .data(&in_rates);
    frame.render_widget(sp_in, vchunks[0]);

    let out_series: Vec<u64> = client.ring.iter()
        .filter_map(|s| s.r.iter().find(|r| r.id == meta.id))
        .map(|r| r.out)
        .collect();
    let out_rates = pairwise_rates(&out_series, &uptime_series);
    let sp_out = Sparkline::default()
        .block(Block::default().title(format!("out {}", fmt_rate(client.out_rate(&meta.id)))))
        .data(&out_rates);
    frame.render_widget(sp_out, vchunks[1]);

    if let (Some(s), Some(max)) = (snap, meta.udp_max_flows) {
        let ratio = (f64::from(s.flows_active) / f64::from(max)).min(1.0);
        let g = Gauge::default()
            .block(Block::default().title(format!("flows {}/{max}", s.flows_active)))
            .ratio(ratio);
        frame.render_widget(g, vchunks[2]);
    } else if let Some(s) = snap {
        let p = Paragraph::new(format!("conns active {} / total {}", s.conns_active, s.conns_total));
        frame.render_widget(p, vchunks[2]);
    }

    if let Some(s) = snap {
        let lines = vec![
            Line::from(format!("total in  {}", fmt_bytes(state.displayed_in(s)))),
            Line::from(format!("total out {}", fmt_bytes(state.displayed_out(s)))),
            Line::from(format!("target_failovers_total {}", s.target_failovers_total)),
        ];
        frame.render_widget(Paragraph::new(lines), vchunks[3]);
    }
}

fn render_errors(frame: &mut Frame, area: Rect, client: &Client, _state: &AppState) {
    let last = client.ring.back();
    let mut rows: Vec<Row> = Vec::new();
    if let Some(snap) = last {
        for meta in &client.hello.rules {
            if let Some(r) = snap.r.iter().find(|r| r.id == meta.id) {
                push_err_row(&mut rows, &meta.name, "port_in_use",             r.err.port_in_use);
                push_err_row(&mut rows, &meta.name, "upstream_connect_failed", r.err.upstream_connect_failed);
                push_err_row(&mut rows, &meta.name, "icmp_evict",              r.err.icmp_evict);
                push_err_row(&mut rows, &meta.name, "emsgsize",                r.err.emsgsize);
                push_err_row(&mut rows, &meta.name, "wouldblock",              r.err.wouldblock);
                push_err_row(&mut rows, &meta.name, "addflow_dropped",         r.err.addflow_dropped);
                push_err_row(&mut rows, &meta.name, "dns_failures",            r.err.dns_failures);
                push_err_row(&mut rows, &meta.name, "flows_dropped_overflow",  r.err.flows_dropped_overflow);
            }
        }
    }
    let header = Row::new(vec!["rule", "event", "count"])
        .style(Style::default().add_modifier(Modifier::BOLD));
    let table = Table::new(rows, [
        Constraint::Length(20),
        Constraint::Length(28),
        Constraint::Length(10),
    ])
    .header(header)
    .block(Block::default().title(" Errors (cumulative) ").borders(Borders::ALL));
    frame.render_widget(table, area);
}

fn push_err_row(rows: &mut Vec<Row>, rule: &str, name: &str, count: u64) {
    if count == 0 { return; }
    let style = Style::default().fg(Color::Yellow);
    rows.push(Row::new(vec![
        Cell::from(rule.to_string()),
        Cell::from(name.to_string()),
        Cell::from(count.to_string()),
    ]).style(style));
}

fn render_footer(frame: &mut Frame, area: Rect, client: &Client) {
    let total_in: u64 = client.hello.rules.iter()
        .map(|m| client.in_rate(&m.id))
        .sum();
    let total_out: u64 = client.hello.rules.iter()
        .map(|m| client.out_rate(&m.id))
        .sum();
    let fd = client.ring.back()
        .and_then(|s| Some((s.process.fd_open?, s.process.fd_limit?)))
        .map_or_else(|| "fd \u{2014}".into(), |(o, l)| format!("fd {o}/{l}"));
    let txt = format!(" rules {}  total in {}  out {}  {fd}  [q]uit [?]help ",
                      client.hello.rules.len(),
                      fmt_rate(total_in), fmt_rate(total_out));
    frame.render_widget(Paragraph::new(txt), area);
}

fn render_help_overlay(frame: &mut Frame, area: Rect) {
    let popup = centered_rect(60, 60, area);
    let text = vec![
        Line::from("Keys:"),
        Line::from("  \u{2191} \u{2193} j k        select rule"),
        Line::from("  \u{2190} \u{2192} h l Tab    cycle tab"),
        Line::from("  Enter          jump to Detail"),
        Line::from("  s              cycle sort"),
        Line::from("  r              reverse sort"),
        Line::from("  /              filter (Esc clears)"),
        Line::from("  p              pause"),
        Line::from("  c              session reset"),
        Line::from("  q / Ctrl-C     quit"),
        Line::from("  ?              toggle help"),
    ];
    let block = Block::default().title(" Help ").borders(Borders::ALL);
    let p = Paragraph::new(text).block(block);
    frame.render_widget(p, popup);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup[1])[1]
}

fn pairwise_rates(field_series: &[u64], uptime_series: &[u64]) -> Vec<u64> {
    if field_series.len() < 2 { return vec![]; }
    field_series.windows(2).zip(uptime_series.windows(2))
        .map(|(fs, us)| {
            let dt_ms = us[1].saturating_sub(us[0]);
            if dt_ms == 0 { return 0; }
            fs[1].saturating_sub(fs[0]).saturating_mul(1000) / dt_ms
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use std::collections::VecDeque;
    use crate::stats::{ErrorSnap, Hello, ProcessSnap, RuleMeta, RuleSnap, Snapshot, TargetMeta};

    fn fake_client() -> Client {
        let hello = Hello {
            v: 1, daemon_version: "1.6.0".into(),
            daemon_started_at_ms: 0, refresh_ms: 1000,
            rules: vec![RuleMeta {
                id: "abc".into(), name: "smoke".into(),
                proto: "tcp".into(), listen: "2222".into(),
                targets: vec![TargetMeta {
                    host: "1.1.1.1".into(), port: 22, priority: 0, proxy_protocol: None,
                }],
                splice_capable: true, udp_max_flows: None,
            }],
        };
        let mut ring: VecDeque<Snapshot> = VecDeque::new();
        ring.push_back(Snapshot {
            t_ms: 0, uptime_ms: 1000, seq: 1,
            process: ProcessSnap { fd_open: Some(10), fd_limit: Some(1024), rss_bytes: None },
            r: vec![RuleSnap {
                id: "abc".into(), bytes_in: 0, out: 0,
                conns_active: 0, conns_total: 0,
                datagrams_in: 0, datagrams_out: 0,
                flows_active: 0, target_failovers_total: 0,
                err: ErrorSnap::default(),
            }],
        });
        ring.push_back(Snapshot {
            t_ms: 0, uptime_ms: 2000, seq: 2,
            process: ProcessSnap { fd_open: Some(10), fd_limit: Some(1024), rss_bytes: None },
            r: vec![RuleSnap {
                id: "abc".into(), bytes_in: 1024, out: 2048,
                conns_active: 1, conns_total: 5,
                datagrams_in: 0, datagrams_out: 0,
                flows_active: 0, target_failovers_total: 0,
                err: ErrorSnap::default(),
            }],
        });
        Client { hello, ring, capacity: 60 }
    }

    #[test]
    fn overview_renders_rule_row() {
        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = super::super::state::AppState::new();
        let client = fake_client();
        terminal.draw(|f| render(f, f.area(), &client, &mut state)).unwrap();
        let buf = terminal.backend().buffer();
        let s = buffer_to_string(buf);
        assert!(s.contains("smoke"), "buffer:\n{s}");
        assert!(s.contains("tcp"));
    }

    #[test]
    fn footer_shows_rule_count() {
        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = super::super::state::AppState::new();
        let client = fake_client();
        terminal.draw(|f| render(f, f.area(), &client, &mut state)).unwrap();
        let s = buffer_to_string(terminal.backend().buffer());
        assert!(s.contains("rules 1"), "footer missing rules count:\n{s}");
    }

    fn buffer_to_string(buf: &ratatui::buffer::Buffer) -> String {
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }
}

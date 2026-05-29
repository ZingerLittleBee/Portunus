//! Render functions for the three TUI tabs.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Axis, Block, Borders, Cell, Chart, Dataset, GraphType, List, ListItem, Paragraph, Row, Table,
    TableState, Tabs,
};

use super::format::{fmt_bytes, fmt_rate};
use super::state::{AppState, Tab};
use crate::stats::client::Client;
use crate::stats::{RuleMeta, RuleSnap};

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
        Tab::Detail => render_detail(frame, layout[2], client, state),
        Tab::Errors => render_errors(frame, layout[2], client, state),
    }
    render_footer(frame, layout[3], client);

    if state.show_help {
        render_help_overlay(frame, area);
    }
}

fn render_header(frame: &mut Frame, area: Rect, client: &Client) {
    let title = format!(
        " portunus-standalone stats — daemon v{} ",
        client.hello.daemon_version
    );
    let p = Paragraph::new(title).style(Style::default().add_modifier(Modifier::BOLD));
    frame.render_widget(p, area);
}

fn render_tabs(frame: &mut Frame, area: Rect, current: Tab) {
    let titles = vec!["Overview", "Detail", "Errors"];
    let t = Tabs::new(titles)
        .select(current.index())
        .style(Style::default())
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(t, area);
}

fn render_overview(frame: &mut Frame, area: Rect, client: &Client, state: &mut AppState) {
    let rows: Vec<Row> = client
        .hello
        .rules
        .iter()
        .map(|meta| {
            let last = client.ring.back();
            let snap_row = last.and_then(|s| s.r.iter().find(|r| r.id == meta.id));
            let in_rate = client.in_rate(&meta.id);
            let out_rate = client.out_rate(&meta.id);
            let (conns, conns_total) = snap_row.map_or((0, 0), |r| (r.conns_active, r.conns_total));
            Row::new(vec![
                Cell::from(meta.name.clone()),
                Cell::from(meta.proto.clone()),
                Cell::from(meta.listen.clone()),
                Cell::from(fmt_rate(in_rate)),
                Cell::from(fmt_rate(out_rate)),
                Cell::from(format!("{conns}/{conns_total}")),
            ])
        })
        .collect();

    let header = Row::new(vec![
        "name", "proto", "listen", "in rate", "out rate", "conns",
    ])
    .style(Style::default().add_modifier(Modifier::BOLD));

    let table = Table::new(
        rows,
        [
            Constraint::Length(18),
            Constraint::Length(5),
            Constraint::Length(12),
            Constraint::Length(13),
            Constraint::Length(13),
            Constraint::Length(12),
        ],
    )
    .header(header)
    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
    .highlight_symbol("▶ ");

    let mut ts = TableState::default();
    ts.select(Some(
        state
            .selected
            .min(client.hello.rules.len().saturating_sub(1)),
    ));
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
    let snap = client
        .ring
        .back()
        .and_then(|s| s.r.iter().find(|r| r.id == meta.id));

    let block = Block::default().borders(Borders::ALL).title(format!(
        " Detail · {} ({} {}) ",
        meta.name, meta.proto, meta.listen
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Top: throughput chart. Bottom: structured per-rule panels.
    let vchunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Min(6)])
        .split(inner);

    render_detail_chart(frame, vchunks[0], client, meta);
    render_detail_panels(frame, vchunks[1], meta, snap, state);
}

/// Top throughput chart: in/out byte-rate lines over the 60 s window,
/// with scaled axes and exact peak/avg annotations in the title.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn render_detail_chart(frame: &mut Frame, area: Rect, client: &Client, meta: &RuleMeta) {
    // Single pass over the ring, pushing all three series together only
    // for snapshots that actually contain this rule. Collecting them in
    // lockstep keeps the byte and uptime series the same length, so
    // `pairwise_rates` always pairs a byte delta with its own time delta
    // (a separate uptime pass would misalign if the rule were ever
    // absent from a snapshot).
    let cap = client.ring.len();
    let mut uptime_series: Vec<u64> = Vec::with_capacity(cap);
    let mut in_series: Vec<u64> = Vec::with_capacity(cap);
    let mut out_series: Vec<u64> = Vec::with_capacity(cap);
    for s in &client.ring {
        if let Some(r) = s.r.iter().find(|r| r.id == meta.id) {
            uptime_series.push(s.uptime_ms);
            in_series.push(r.bytes_in);
            out_series.push(r.out);
        }
    }
    let in_rates = pairwise_rates(&in_series, &uptime_series);
    let out_rates = pairwise_rates(&out_series, &uptime_series);

    // pairwise_rates yields one fewer point than snapshots; an empty
    // series means fewer than two snapshots have arrived yet.
    if in_rates.is_empty() {
        let p = Paragraph::new("collecting…")
            .block(Block::default().title(" throughput · 60s window "));
        frame.render_widget(p, area);
        return;
    }

    let in_pts: Vec<(f64, f64)> = in_rates
        .iter()
        .enumerate()
        .map(|(i, &v)| (i as f64, v as f64))
        .collect();
    let out_pts: Vec<(f64, f64)> = out_rates
        .iter()
        .enumerate()
        .map(|(i, &v)| (i as f64, v as f64))
        .collect();

    let peak = in_rates
        .iter()
        .chain(out_rates.iter())
        .copied()
        .max()
        .unwrap_or(0);
    let avg_in = in_rates.iter().sum::<u64>() / in_rates.len() as u64;
    let y_max = (peak as f64 * 1.15).max(1.0);
    let x_max = in_rates.len().saturating_sub(1).max(1) as f64;

    let datasets = vec![
        Dataset::default()
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::Green))
            .data(&in_pts),
        Dataset::default()
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(Color::Cyan))
            .data(&out_pts),
    ];

    // Custom title doubles as the legend (colored in/out) plus exact
    // current/peak/avg figures the axis ticks can only approximate. The
    // "now" figures are the rightmost plotted points (last() is safe —
    // the empty-series case returned above), so the title and the line
    // ends can never disagree.
    let now_in = *in_rates.last().unwrap();
    let now_out = *out_rates.last().unwrap_or(&0);
    let title = Line::from(vec![
        Span::raw(" throughput · 60s   "),
        Span::styled("in ", Style::default().fg(Color::Green)),
        Span::raw(format!("{}  ", fmt_rate(now_in))),
        Span::styled("out ", Style::default().fg(Color::Cyan)),
        Span::raw(format!(
            "{}   peak {}  avg in {} ",
            fmt_rate(now_out),
            fmt_rate(peak),
            fmt_rate(avg_in),
        )),
    ]);

    let x_axis = Axis::default()
        .style(Style::default().fg(Color::DarkGray))
        .bounds([0.0, x_max])
        .labels(vec![Line::from("-60s"), Line::from("now")]);
    let y_axis = Axis::default()
        .style(Style::default().fg(Color::DarkGray))
        .bounds([0.0, y_max])
        .labels(vec![
            Line::from("0"),
            Line::from(fmt_rate((y_max / 2.0) as u64)),
            Line::from(fmt_rate(y_max as u64)),
        ]);

    let chart = Chart::new(datasets)
        .block(Block::default().title(title))
        .x_axis(x_axis)
        .y_axis(y_axis);
    frame.render_widget(chart, area);
}

/// Minimum body width (columns) at which the four detail panels fit
/// side by side as three columns. Roughly the sum of the narrowest
/// legible widths of Targets + Counters + Capabilities; below it the
/// panels stack vertically instead.
const WIDE_PANELS_MIN_WIDTH: u16 = 78;

/// Bottom panels: Targets + per-rule Errors on the left, Counters in
/// the middle, Capabilities on the right. Collapses to a single stacked
/// column on narrow terminals. The wide/narrow branch only chooses the
/// four panel rects; the render calls below happen once so the panel set
/// and order can never drift between layouts.
fn render_detail_panels(
    frame: &mut Frame,
    area: Rect,
    meta: &RuleMeta,
    snap: Option<&RuleSnap>,
    state: &AppState,
) {
    let (targets_area, errors_area, counters_area, caps_area) =
        if area.width < WIDE_PANELS_MIN_WIDTH {
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Ratio(1, 4); 4])
                .split(area);
            (rows[0], rows[3], rows[1], rows[2])
        } else {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Ratio(1, 3); 3])
                .split(area);
            let left = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
                .split(cols[0]);
            (left[0], left[1], cols[1], cols[2])
        };

    render_targets(frame, targets_area, meta);
    render_counters(frame, counters_area, meta, snap, state);
    render_capabilities(frame, caps_area, meta, snap);
    render_rule_errors(frame, errors_area, snap);
}

fn render_targets(frame: &mut Frame, area: Rect, meta: &RuleMeta) {
    let block = Block::default().borders(Borders::ALL).title(" Targets ");
    // Lowest-priority target is the active/primary one.
    let min_prio = meta.targets.iter().map(|t| t.priority).min();
    let items: Vec<ListItem> = meta
        .targets
        .iter()
        .map(|t| {
            let active = Some(t.priority) == min_prio;
            let marker = if active { "▶ " } else { "  " };
            let proxy = if t.proxy_protocol.is_some() {
                "  proxy"
            } else {
                ""
            };
            let text = format!("{marker}{}:{}  prio {}{proxy}", t.host, t.port, t.priority);
            let style = if active {
                Style::default().fg(Color::Green)
            } else {
                Style::default()
            };
            ListItem::new(text).style(style)
        })
        .collect();
    frame.render_widget(List::new(items).block(block), area);
}

fn render_counters(
    frame: &mut Frame,
    area: Rect,
    meta: &RuleMeta,
    snap: Option<&RuleSnap>,
    state: &AppState,
) {
    let block = Block::default().borders(Borders::ALL).title(" Counters ");
    let lines = if let Some(s) = snap {
        let mut lines = vec![
            Line::from(format!("total in   {}", fmt_bytes(state.displayed_in(s)))),
            Line::from(format!("total out  {}", fmt_bytes(state.displayed_out(s)))),
        ];
        if let Some(max) = meta.udp_max_flows {
            lines.push(Line::from(format!("flows      {}/{max}", s.flows_active)));
            lines.push(Line::from(format!(
                "datagrams  {} / {}",
                s.datagrams_in, s.datagrams_out
            )));
        } else {
            lines.push(Line::from(format!(
                "conns      {}/{}",
                s.conns_active, s.conns_total
            )));
            lines.push(Line::from("datagrams  —".to_string()));
        }
        lines.push(Line::from(format!(
            "failovers  {}",
            s.target_failovers_total
        )));
        lines
    } else {
        vec![Line::from("—")]
    };
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_capabilities(frame: &mut Frame, area: Rect, meta: &RuleMeta, snap: Option<&RuleSnap>) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Capabilities ");
    let badge = |on: bool| if on { "● on" } else { "○ off" };
    let proxy_on = meta.targets.iter().any(|t| t.proxy_protocol.is_some());
    let mut lines = vec![
        Line::from(format!("splice     {}", badge(meta.splice_capable))),
        Line::from(format!("proxy      {}", badge(proxy_on))),
    ];
    if let Some(max) = meta.udp_max_flows {
        let active = snap.map_or(0, |s| s.flows_active);
        lines.push(Line::from(format!("udp flows  {active}/{max}")));
    } else {
        lines.push(Line::from("udp flows  —".to_string()));
    }
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_rule_errors(frame: &mut Frame, area: Rect, snap: Option<&RuleSnap>) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Errors (this rule) ");
    let lines = if let Some(s) = snap {
        let mut v: Vec<Line> = s
            .err
            .labeled()
            .into_iter()
            .filter(|(_, c)| *c > 0)
            .map(|(n, c)| {
                Line::from(Span::styled(
                    format!("{n} {c}"),
                    Style::default().fg(Color::Yellow),
                ))
            })
            .collect();
        if v.is_empty() {
            v.push(Line::from(Span::styled(
                "✓ none",
                Style::default().fg(Color::Green),
            )));
        }
        v
    } else {
        vec![Line::from("—")]
    };
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn render_errors(frame: &mut Frame, area: Rect, client: &Client, _state: &AppState) {
    let last = client.ring.back();
    let mut rows: Vec<Row> = Vec::new();
    if let Some(snap) = last {
        for meta in &client.hello.rules {
            if let Some(r) = snap.r.iter().find(|r| r.id == meta.id) {
                for (name, count) in r.err.labeled() {
                    push_err_row(&mut rows, &meta.name, name, count);
                }
            }
        }
    }
    let header = Row::new(vec!["rule", "event", "count"])
        .style(Style::default().add_modifier(Modifier::BOLD));
    let table = Table::new(
        rows,
        [
            Constraint::Length(20),
            Constraint::Length(28),
            Constraint::Length(10),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .title(" Errors (cumulative) ")
            .borders(Borders::ALL),
    );
    frame.render_widget(table, area);
}

fn push_err_row(rows: &mut Vec<Row>, rule: &str, name: &str, count: u64) {
    if count == 0 {
        return;
    }
    let style = Style::default().fg(Color::Yellow);
    rows.push(
        Row::new(vec![
            Cell::from(rule.to_string()),
            Cell::from(name.to_string()),
            Cell::from(count.to_string()),
        ])
        .style(style),
    );
}

fn render_footer(frame: &mut Frame, area: Rect, client: &Client) {
    let total_in: u64 = client
        .hello
        .rules
        .iter()
        .map(|m| client.in_rate(&m.id))
        .sum();
    let total_out: u64 = client
        .hello
        .rules
        .iter()
        .map(|m| client.out_rate(&m.id))
        .sum();
    let fd = client
        .ring
        .back()
        .and_then(|s| Some((s.process.fd_open?, s.process.fd_limit?)))
        .map_or_else(|| "fd \u{2014}".into(), |(o, l)| format!("fd {o}/{l}"));
    let txt = format!(
        " rules {}  total in {}  out {}  {fd}  [q]uit [?]help ",
        client.hello.rules.len(),
        fmt_rate(total_in),
        fmt_rate(total_out)
    );
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
    if field_series.len() < 2 {
        return vec![];
    }
    field_series
        .windows(2)
        .zip(uptime_series.windows(2))
        .map(|(fs, us)| {
            let dt_ms = us[1].saturating_sub(us[0]);
            if dt_ms == 0 {
                return 0;
            }
            fs[1].saturating_sub(fs[0]).saturating_mul(1000) / dt_ms
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::{ErrorSnap, Hello, ProcessSnap, RuleMeta, RuleSnap, Snapshot, TargetMeta};
    use ratatui::{Terminal, backend::TestBackend};
    use std::collections::VecDeque;

    fn fake_client() -> Client {
        let hello = Hello {
            v: 1,
            daemon_version: "1.6.0".into(),
            daemon_started_at_ms: 0,
            refresh_ms: 1000,
            rules: vec![RuleMeta {
                id: "abc".into(),
                name: "smoke".into(),
                proto: "tcp".into(),
                listen: "2222".into(),
                targets: vec![TargetMeta {
                    host: "1.1.1.1".into(),
                    port: 22,
                    priority: 0,
                    proxy_protocol: None,
                }],
                splice_capable: true,
                udp_max_flows: None,
            }],
        };
        let mut ring: VecDeque<Snapshot> = VecDeque::new();
        ring.push_back(Snapshot {
            t_ms: 0,
            uptime_ms: 1000,
            seq: 1,
            process: ProcessSnap {
                fd_open: Some(10),
                fd_limit: Some(1024),
                rss_bytes: None,
            },
            r: vec![RuleSnap {
                id: "abc".into(),
                bytes_in: 0,
                out: 0,
                conns_active: 0,
                conns_total: 0,
                datagrams_in: 0,
                datagrams_out: 0,
                flows_active: 0,
                target_failovers_total: 0,
                err: ErrorSnap::default(),
            }],
        });
        ring.push_back(Snapshot {
            t_ms: 0,
            uptime_ms: 2000,
            seq: 2,
            process: ProcessSnap {
                fd_open: Some(10),
                fd_limit: Some(1024),
                rss_bytes: None,
            },
            r: vec![RuleSnap {
                id: "abc".into(),
                bytes_in: 1024,
                out: 2048,
                conns_active: 1,
                conns_total: 5,
                datagrams_in: 0,
                datagrams_out: 0,
                flows_active: 0,
                target_failovers_total: 0,
                err: ErrorSnap::default(),
            }],
        });
        Client {
            hello,
            ring,
            capacity: 60,
        }
    }

    #[test]
    fn overview_renders_rule_row() {
        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = super::super::state::AppState::new();
        let client = fake_client();
        terminal
            .draw(|f| render(f, f.area(), &client, &mut state))
            .unwrap();
        let buf = terminal.backend().buffer();
        let s = buffer_to_string(buf);
        assert!(s.contains("smoke"), "buffer:\n{s}");
        assert!(s.contains("tcp"));
    }

    fn draw_detail(client: &Client, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = super::super::state::AppState::new();
        state.tab = Tab::Detail;
        terminal
            .draw(|f| render(f, f.area(), client, &mut state))
            .unwrap();
        buffer_to_string(terminal.backend().buffer())
    }

    #[test]
    fn detail_tcp_renders_chart_and_panels() {
        let s = draw_detail(&fake_client(), 100, 30);
        assert!(s.contains("throughput"), "missing chart title:\n{s}");
        assert!(s.contains("Targets"), "missing targets panel:\n{s}");
        assert!(s.contains("Counters"), "missing counters panel:\n{s}");
        assert!(s.contains("splice"), "missing capabilities:\n{s}");
        assert!(s.contains("1.1.1.1"), "missing target host:\n{s}");
    }

    #[test]
    fn detail_narrow_terminal_still_shows_all_panels() {
        // Below WIDE_PANELS_MIN_WIDTH the panels stack vertically; the
        // same four panels must still render (no drift between layouts).
        let s = draw_detail(&fake_client(), 70, 30);
        assert!(
            s.contains("Targets"),
            "missing targets panel (narrow):\n{s}"
        );
        assert!(
            s.contains("Counters"),
            "missing counters panel (narrow):\n{s}"
        );
        assert!(
            s.contains("Capabilities"),
            "missing capabilities panel (narrow):\n{s}"
        );
        assert!(s.contains("Errors"), "missing errors panel (narrow):\n{s}");
    }

    #[test]
    fn detail_collecting_with_single_snapshot() {
        let mut client = fake_client();
        client.ring.pop_back(); // leave a single snapshot in the ring
        let s = draw_detail(&client, 100, 30);
        assert!(s.contains("collecting"), "expected collecting state:\n{s}");
    }

    #[test]
    fn detail_udp_shows_flows() {
        let mut client = fake_client();
        client.hello.rules[0].proto = "udp".into();
        client.hello.rules[0].udp_max_flows = Some(128);
        for snap in &mut client.ring {
            snap.r[0].flows_active = 7;
        }
        let s = draw_detail(&client, 100, 30);
        assert!(s.contains("flows"), "missing flows row:\n{s}");
        assert!(s.contains("7/128"), "missing flow saturation:\n{s}");
    }

    #[test]
    fn detail_errors_panel_lists_nonzero() {
        let mut client = fake_client();
        if let Some(last) = client.ring.back_mut() {
            last.r[0].err.upstream_connect_failed = 3;
        }
        let s = draw_detail(&client, 100, 30);
        assert!(s.contains("connect_failed"), "missing error counter:\n{s}");
        assert!(s.contains('3'), "missing error count:\n{s}");
    }

    #[test]
    fn detail_errors_panel_none_when_clean() {
        let s = draw_detail(&fake_client(), 100, 30);
        assert!(s.contains("none"), "expected clean errors marker:\n{s}");
    }

    #[test]
    fn footer_shows_rule_count() {
        let backend = TestBackend::new(80, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = super::super::state::AppState::new();
        let client = fake_client();
        terminal
            .draw(|f| render(f, f.area(), &client, &mut state))
            .unwrap();
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

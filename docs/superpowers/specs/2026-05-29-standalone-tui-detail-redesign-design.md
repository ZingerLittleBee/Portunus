# Standalone stats TUI — Detail page redesign

Date: 2026-05-29
Crate: `portunus-standalone` (`src/stats/tui/render.rs`)

## Problem

The current Detail page (`render_detail`) stacks two short
`Sparkline` widgets (in / out), a gauge-or-text line, and three text
lines. Issues observed in the field:

1. **Sparklines are unreadable.** Each sample is an isolated vertical
   bar with no Y-axis scale and no time axis. Gaps between bars read as
   noise; the viewer cannot tell the current value, the peak, or the
   trend.
2. **Most of the screen is empty.** The body uses ~8 rows and leaves the
   rest of the terminal blank — very low information density.
3. **No context.** No upstream/target info, no capability badges, no
   per-rule error breakdown, no peak/avg annotations.

## Goal

Redesign the Detail body to serve **both** real-time trend monitoring
(top) **and** structured per-rule diagnostics (bottom), filling the
available space.

No change to the stats wire protocol, to other tabs, or to the
header/footer. This is a pure presentation refactor of `render_detail`.

## Layout

```
┌ Detail · fwd-40574 (tcp 40574) ──────────────────────────────────────────────┐
│ throughput · 60s window            ── in 362.2 KB/s    ·· out 717 B/s         │
│ 1.2M┤        ╭─╮                                          peak in  1.2 MB/s   │
│     │     ╭──╯ ╰╮      ╭──╮                               peak out 2.1 KB/s   │
│ 600K┤  ╭──╯     ╰──────╯  ╰─╮                            avg in   410 KB/s    │
│     │╭─╯                    ╰────╮                                            │
│   0 ┼┴····························╰·····································          │
│     -60s                                                              now     │
│ ┌ Targets ───────────────────┐ ┌ Counters ──────────┐ ┌ Capabilities ──────┐ │
│ │▶ 10.0.0.1:22   prio 0       │ │ total in   53.3 MB │ │ splice    ● on     │ │
│ │  10.0.0.2:22   prio 1  proxy│ │ total out  323.4MB │ │ proxy     ○ off    │ │
│ │                             │ │ conns      60/151  │ │ udp flows  —       │ │
│ └─────────────────────────────┘ │ datagrams  —       │ └────────────────────┘ │
│ ┌ Errors (this rule) ─────────┐ │ failovers  0       │                        │
│ │ ✓ none                      │ └────────────────────┘                        │
│ └─────────────────────────────┘                                               │
└───────────────────────────────────────────────────────────────────────────────┘
```

Body split vertically: top **chart** region (~55%, min ~9 rows), bottom
**panels** region (rest).

## Components

### 1. Throughput chart (top)

- `ratatui::widgets::Chart` with two `Dataset`s:
  - `in` — solid line, `GraphType::Line`, braille marker, green.
  - `out` — dotted/secondary line, distinct color (cyan), so the two are
    distinguishable in monochrome and color terminals.
- **Y axis**: bounds `0..=peak`, where `peak = max(in, out) over window *
  1.15` (headroom), with a sane floor so a flat-zero series still renders
  an axis. Labels: `0`, mid, `peak`, formatted with `fmt_rate`.
- **X axis**: bounds cover the 60s window; labels `-60s` … `now`.
- Data: reuse `pairwise_rates()` to get per-interval in/out rate series,
  map to `Vec<(f64, f64)>` where `x` = seconds-ago (negative → 0) and
  `y` = rate (bytes/s as f64).
- Legend + right-aligned annotations: `peak in`, `peak out`, `avg in`
  (computed from the rate series). These give exact numbers the axis
  ticks only approximate.
- Title line above the chart: `throughput · 60s window`.

### 2. Detail panels (bottom)

Horizontal split into up to three columns; left column stacks Targets
over Errors.

- **Targets** (`Table` or bordered `List`): one row per `TargetMeta` —
  `host:port`, `prio N`, `proxy` marker when `proxy_protocol` is set.
  The active/primary target (lowest priority) is marked with `▶`.
- **Counters** (text grid): `total in` / `total out` (via
  `state.displayed_in/out` so session-reset baseline still applies),
  `conns active/total` (TCP) or `flows active/max` (UDP), `datagrams
  in/out` (UDP; `—` for TCP), `failovers`.
- **Capabilities** (badges): `splice ● on / ○ off`, `proxy ● / ○`
  (any target with proxy_protocol), `udp flows N/max` or `—`.
- **Errors (this rule)**: list only the non-zero error counters from
  `RuleSnap.err` for the selected rule. When all zero, show `✓ none`
  (green). Complements the Errors tab but scoped to the current rule.

## Responsiveness & edge cases

- **Wide (≥ ~90 cols)**: three columns as drawn.
- **Narrow**: degrade to fewer columns / vertical stacking; the chart
  always spans full width. Use `Layout` with percentage/min constraints
  that collapse gracefully (no panic on tiny areas — guard against
  zero-height/width rects).
- **< 2 snapshots in ring**: chart region shows `collecting…` instead of
  an empty/degenerate plot.
- **UDP rule** (`udp_max_flows` set): Counters shows flows row; optional
  `LineGauge` for flow saturation; datagrams populated.
- **TCP rule**: conns row; datagrams `—`.
- **No rules**: existing `no rules` paragraph (unchanged).

## Testing

Reuse the `TestBackend` snapshot-render pattern already in
`render.rs::tests`:

- TCP rule (multi-target): buffer contains `throughput`, a target host,
  `total in`, `splice`.
- UDP rule (`udp_max_flows = Some(..)`): buffer contains `flows` /
  saturation.
- Single-snapshot ring: buffer contains `collecting`.
- Errors present: a non-zero counter name appears in the Errors panel;
  errors absent: `none` appears.

All existing tests for Overview/Errors/footer remain green.
`cargo test -p portunus-standalone`, `cargo clippy -p
portunus-standalone --all-targets -- -D warnings`, `cargo fmt` must pass.

## Non-goals

- No stats protocol / wire-format change.
- No new dependency (Chart/Canvas are in ratatui core).
- No change to Overview, Errors, header, footer, or key handling.

# Standalone TUI Upstream Latency Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Show TCP connect-time latency to each rule's active upstream target in the `portunus-standalone stats` TUI Detail page.

**Architecture:** Entirely client-side. The stats TUI process probes the selected rule's active target with a timed `TcpStream::connect` every ~2s while the Detail tab is visible, off the render path via `tokio::spawn` + an `mpsc` channel. No daemon changes, no `Snapshot`/`Hello` wire-field changes, no new config or CLI flag. Target `host:port` already arrive in `Hello`.

**Tech Stack:** Rust 2024, tokio (`net`, `time`, `sync`, `macros`), ratatui/crossterm (behind the `stats-tui` feature). All probing lives under `crates/portunus-standalone/src/stats/tui/`.

---

## File Structure

| File | Responsibility |
|------|----------------|
| `crates/portunus-standalone/src/stats/tui/probe.rs` (new) | `ProbeSample` enum, probe cadence/timeout constants, `active_target_index`/`active_target` helpers, `probe_tcp`. |
| `crates/portunus-standalone/src/stats/tui/format.rs` | add `fmt_rtt` (text + colour). |
| `crates/portunus-standalone/src/stats/tui/state.rs` | `AppState` gains `probes` cache + `last_probe_at`. |
| `crates/portunus-standalone/src/stats/tui/render.rs` | `render_targets` appends RTT to the active target row. |
| `crates/portunus-standalone/src/stats/tui/mod.rs` | register `probe` module; `run_loop` issues probes + drains results. |

All commands run from the repo root `/Users/zingerbee/Bee/Portunus`. The `tui` module is gated behind the default `stats-tui` feature, so plain `cargo test -p portunus-standalone` exercises it.

---

### Task 1: Probe primitives (`probe.rs`)

**Files:**
- Create: `crates/portunus-standalone/src/stats/tui/probe.rs`
- Modify: `crates/portunus-standalone/src/stats/tui/mod.rs` (register module)

- [ ] **Step 1: Register the new module**

In `crates/portunus-standalone/src/stats/tui/mod.rs`, add `pub mod probe;` next to the existing module declarations (after `pub mod format;`):

```rust
pub mod format;
pub mod probe;
pub mod render;
pub mod state;
```

- [ ] **Step 2: Write `probe.rs` with the type, helpers, probe fn, and tests**

Create `crates/portunus-standalone/src/stats/tui/probe.rs`:

```rust
//! Client-side TCP-connect latency probing for the stats TUI.
//!
//! Probes are issued only for the selected rule's active target while the
//! Detail tab is visible (see `tui::run_loop`). Nothing here touches the
//! daemon or the UDS wire protocol — the target `host:port` already arrive
//! in `Hello`.

use std::time::{Duration, Instant};

use tokio::net::TcpStream;

use crate::stats::{RuleMeta, TargetMeta};

/// How long a single connect probe may take before it is reported as
/// `Timeout`.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// Minimum spacing between probes. Decoupled from the snapshot refresh so
/// one probe per interval stays negligible load regardless of `refresh_ms`.
pub const PROBE_INTERVAL: Duration = Duration::from_secs(2);

/// Outcome of one TCP connect probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeSample {
    /// Connect succeeded; the value is the measured connect time.
    Ok(Duration),
    /// Connect did not complete within `PROBE_TIMEOUT`.
    Timeout,
    /// Connect failed (refused, unreachable, or DNS error).
    Failed,
}

/// Index of the active (lowest-priority) target, or `None` if the rule has
/// no targets. `min_by_key` returns the first minimum on ties, so the
/// prober and the renderer always agree on the single active row.
#[must_use]
pub fn active_target_index(meta: &RuleMeta) -> Option<usize> {
    meta.targets
        .iter()
        .enumerate()
        .min_by_key(|(_, t)| t.priority)
        .map(|(i, _)| i)
}

/// The active target of a rule, or `None` if it has no targets.
#[must_use]
pub fn active_target(meta: &RuleMeta) -> Option<&TargetMeta> {
    active_target_index(meta).map(|i| &meta.targets[i])
}

/// Measure TCP connect time to `host:port`. `TcpStream::connect` performs
/// DNS resolution internally, so `host` may be a domain or an IP literal;
/// the `(host, port)` tuple form also handles IPv6 literals correctly. The
/// connection is dropped immediately, so the probe leaves no lingering
/// socket.
pub async fn probe_tcp(host: &str, port: u16) -> ProbeSample {
    let start = Instant::now();
    match tokio::time::timeout(PROBE_TIMEOUT, TcpStream::connect((host, port))).await {
        Ok(Ok(_stream)) => ProbeSample::Ok(start.elapsed()),
        Ok(Err(_)) => ProbeSample::Failed,
        Err(_) => ProbeSample::Timeout,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::TargetMeta;

    fn target(host: &str, port: u16, priority: u32) -> TargetMeta {
        TargetMeta {
            host: host.into(),
            port,
            priority,
            proxy_protocol: None,
        }
    }

    fn meta_with(targets: Vec<TargetMeta>) -> RuleMeta {
        RuleMeta {
            id: "r".into(),
            name: "r".into(),
            proto: "tcp".into(),
            listen: "1".into(),
            targets,
            splice_capable: true,
            udp_max_flows: None,
        }
    }

    #[test]
    fn active_target_picks_lowest_priority() {
        let m = meta_with(vec![target("a", 1, 5), target("b", 1, 0), target("c", 1, 3)]);
        assert_eq!(active_target_index(&m), Some(1));
        assert_eq!(active_target(&m).unwrap().host, "b");
    }

    #[test]
    fn active_target_none_when_empty() {
        let m = meta_with(vec![]);
        assert_eq!(active_target_index(&m), None);
        assert!(active_target(&m).is_none());
    }

    #[tokio::test]
    async fn probe_ok_against_loopback_listener() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        // Accept in the background so the connect completes.
        tokio::spawn(async move {
            let _ = listener.accept().await;
        });
        let s = probe_tcp(&addr.ip().to_string(), addr.port()).await;
        assert!(matches!(s, ProbeSample::Ok(_)), "got {s:?}");
    }

    #[tokio::test]
    async fn probe_failed_against_closed_port() {
        // Bind then drop to obtain a port nothing listens on.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let s = probe_tcp(&addr.ip().to_string(), addr.port()).await;
        assert!(
            matches!(s, ProbeSample::Failed | ProbeSample::Timeout),
            "got {s:?}"
        );
    }
}
```

- [ ] **Step 3: Run the probe tests, expect PASS**

Run: `cargo test -p portunus-standalone --lib stats::tui::probe -- --nocapture`
Expected: PASS — 4 tests (`active_target_picks_lowest_priority`, `active_target_none_when_empty`, `probe_ok_against_loopback_listener`, `probe_failed_against_closed_port`).

- [ ] **Step 4: Commit**

```bash
git add crates/portunus-standalone/src/stats/tui/probe.rs crates/portunus-standalone/src/stats/tui/mod.rs
git commit -m "feat(standalone/tui): add client-side TCP latency probe primitives"
```

---

### Task 2: RTT formatter (`format.rs`)

**Files:**
- Modify: `crates/portunus-standalone/src/stats/tui/format.rs`

- [ ] **Step 1: Add `fmt_rtt` and its test**

Append to `crates/portunus-standalone/src/stats/tui/format.rs`, after `fmt_rate` (before the `#[cfg(test)]` module). First add the imports at the top of the file:

```rust
use ratatui::style::Color;

use super::probe::ProbeSample;
```

Then the function:

```rust
/// Format a probe sample for the Targets panel: the text to display and
/// the colour to display it in. Green `< 50 ms`, yellow `< 200 ms`, red
/// otherwise; timeouts and failures are red.
#[must_use]
pub fn fmt_rtt(sample: ProbeSample) -> (String, Color) {
    match sample {
        ProbeSample::Ok(d) => {
            let ms = d.as_millis();
            let color = if ms < 50 {
                Color::Green
            } else if ms < 200 {
                Color::Yellow
            } else {
                Color::Red
            };
            (format!("{ms}ms"), color)
        }
        ProbeSample::Timeout => ("timeout".to_string(), Color::Red),
        ProbeSample::Failed => ("down".to_string(), Color::Red),
    }
}
```

Add these tests inside the existing `#[cfg(test)] mod tests`:

```rust
    #[test]
    fn rtt_ok_thresholds() {
        use std::time::Duration;
        assert_eq!(
            fmt_rtt(ProbeSample::Ok(Duration::from_millis(10))),
            ("10ms".to_string(), Color::Green)
        );
        assert_eq!(
            fmt_rtt(ProbeSample::Ok(Duration::from_millis(120))).1,
            Color::Yellow
        );
        assert_eq!(
            fmt_rtt(ProbeSample::Ok(Duration::from_millis(500))).1,
            Color::Red
        );
    }

    #[test]
    fn rtt_timeout_and_failed_are_red() {
        assert_eq!(fmt_rtt(ProbeSample::Timeout), ("timeout".to_string(), Color::Red));
        assert_eq!(fmt_rtt(ProbeSample::Failed), ("down".to_string(), Color::Red));
    }
```

- [ ] **Step 2: Run the formatter tests, expect PASS**

Run: `cargo test -p portunus-standalone --lib stats::tui::format`
Expected: PASS — existing 3 byte/rate tests plus `rtt_ok_thresholds` and `rtt_timeout_and_failed_are_red`.

- [ ] **Step 3: Commit**

```bash
git add crates/portunus-standalone/src/stats/tui/format.rs
git commit -m "feat(standalone/tui): add fmt_rtt latency formatter"
```

---

### Task 3: AppState probe cache (`state.rs`)

**Files:**
- Modify: `crates/portunus-standalone/src/stats/tui/state.rs`

- [ ] **Step 1: Add the cache fields**

In `crates/portunus-standalone/src/stats/tui/state.rs`, extend the imports:

```rust
use std::collections::HashMap;
use std::time::Instant;

use crate::stats::tui::probe::ProbeSample;
use crate::stats::{RuleSnap, Snapshot};
```

Add two fields to `AppState` (after `baseline`):

```rust
    /// Latest probe sample per rule id (active target only).
    pub probes: HashMap<String, ProbeSample>,
    /// When the last probe was issued; `None` until the first probe.
    pub last_probe_at: Option<Instant>,
```

Initialise them in `AppState::new()` (after `baseline: HashMap::new(),`):

```rust
            probes: HashMap::new(),
            last_probe_at: None,
```

- [ ] **Step 2: Build to verify the struct compiles**

Run: `cargo build -p portunus-standalone`
Expected: builds (the existing `AppState::new` and `Default` still satisfy the struct).

- [ ] **Step 3: Run state tests, expect PASS**

Run: `cargo test -p portunus-standalone --lib stats::tui::state`
Expected: PASS — `baseline_reset_subtracts`, `tab_cycle` unaffected.

- [ ] **Step 4: Commit**

```bash
git add crates/portunus-standalone/src/stats/tui/state.rs
git commit -m "feat(standalone/tui): add per-rule probe cache to AppState"
```

---

### Task 4: Render RTT on the active target row (`render.rs`)

**Files:**
- Modify: `crates/portunus-standalone/src/stats/tui/render.rs`

- [ ] **Step 1: Wire the imports**

In `crates/portunus-standalone/src/stats/tui/render.rs`, update the format import and add the probe helper:

```rust
use super::format::{fmt_bytes, fmt_rate, fmt_rtt};
use super::probe::active_target_index;
use super::state::{AppState, Tab};
```

- [ ] **Step 2: Replace `render_targets` to mark one active row and append RTT**

Replace the whole existing `render_targets` function with:

```rust
fn render_targets(frame: &mut Frame, area: Rect, meta: &RuleMeta, state: &AppState) {
    let block = Block::default().borders(Borders::ALL).title(" Targets ");
    // Single active target (first lowest-priority) — same rule the prober
    // uses, so the `▶` mark and the RTT always describe the same row.
    let active_idx = active_target_index(meta);
    let is_udp = meta.proto == "udp";
    let items: Vec<ListItem> = meta
        .targets
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let active = Some(i) == active_idx;
            let marker = if active { "▶ " } else { "  " };
            let proxy = if t.proxy_protocol.is_some() {
                "  proxy"
            } else {
                ""
            };
            let base = format!("{marker}{}:{}  prio {}{proxy}", t.host, t.port, t.priority);
            if active {
                // UDP: TCP probe is meaningless, show "—". TCP: cached
                // sample, or "…" until the first probe lands.
                let (rtt_text, rtt_color) = if is_udp {
                    ("\u{2014}".to_string(), Color::DarkGray)
                } else {
                    state
                        .probes
                        .get(&meta.id)
                        .map_or_else(|| ("\u{2026}".to_string(), Color::DarkGray), |s| fmt_rtt(*s))
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{base}   "), Style::default().fg(Color::Green)),
                    Span::styled(rtt_text, Style::default().fg(rtt_color)),
                ]))
            } else {
                ListItem::new(base)
            }
        })
        .collect();
    frame.render_widget(List::new(items).block(block), area);
}
```

- [ ] **Step 3: Pass `state` from the panel layout**

In `render_detail_panels`, update the call (it already receives `state: &AppState`):

```rust
    render_targets(frame, targets_area, meta, state);
```

- [ ] **Step 4: Add render tests for RTT and UDP "—"**

Add these tests inside the existing `#[cfg(test)] mod tests` in `render.rs`:

```rust
    #[test]
    fn detail_active_target_shows_rtt() {
        use crate::stats::tui::probe::ProbeSample;
        use std::time::Duration;
        let client = fake_client();
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = super::super::state::AppState::new();
        state.tab = Tab::Detail;
        state
            .probes
            .insert("abc".to_string(), ProbeSample::Ok(Duration::from_millis(12)));
        terminal
            .draw(|f| render(f, f.area(), &client, &mut state))
            .unwrap();
        let s = buffer_to_string(terminal.backend().buffer());
        assert!(s.contains("12ms"), "missing rtt on active target:\n{s}");
    }

    #[test]
    fn detail_udp_active_target_shows_dash() {
        let mut client = fake_client();
        client.hello.rules[0].proto = "udp".into();
        client.hello.rules[0].udp_max_flows = Some(128);
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut state = super::super::state::AppState::new();
        state.tab = Tab::Detail;
        terminal
            .draw(|f| render(f, f.area(), &client, &mut state))
            .unwrap();
        let s = buffer_to_string(terminal.backend().buffer());
        // The em-dash marks "not applicable" for UDP.
        assert!(s.contains('\u{2014}'), "missing UDP dash marker:\n{s}");
    }
```

- [ ] **Step 5: Run render tests, expect PASS**

Run: `cargo test -p portunus-standalone --lib stats::tui::render`
Expected: PASS — existing detail/overview/footer tests plus `detail_active_target_shows_rtt` and `detail_udp_active_target_shows_dash`. (Existing `detail_tcp_renders_chart_and_panels` still passes: with no probe sample the active row shows `…`, and `1.1.1.1` is still present.)

- [ ] **Step 6: Commit**

```bash
git add crates/portunus-standalone/src/stats/tui/render.rs
git commit -m "feat(standalone/tui): show active-target RTT in Targets panel"
```

---

### Task 5: Probe orchestration in the event loop (`mod.rs`)

**Files:**
- Modify: `crates/portunus-standalone/src/stats/tui/mod.rs`

- [ ] **Step 1: Add imports**

In `crates/portunus-standalone/src/stats/tui/mod.rs`, add to the imports near the top:

```rust
use std::time::Instant;

use crate::stats::tui::probe::{self, ProbeSample, active_target};
```

(Keep the existing `use state::{AppState, Tab};` line.)

- [ ] **Step 2: Create the result channel in `run_loop`**

At the start of `run_loop`, just after `let mut line_buf = String::new();`, add the channel:

```rust
    // Results from spawned probe tasks. Bounded; a full channel just drops
    // a probe result, which self-heals on the next tick.
    let (probe_tx, mut probe_rx) =
        tokio::sync::mpsc::channel::<(String, ProbeSample)>(8);
```

- [ ] **Step 3: Drain results and trigger probes each iteration**

Inside the `loop`, immediately after the `match read { ... }` block (before the key-event poll), add:

```rust
        // Drain any probe results that arrived since the last iteration.
        while let Ok((id, sample)) = probe_rx.try_recv() {
            state.probes.insert(id, sample);
        }

        // Issue a probe for the selected rule's active TCP target while the
        // Detail tab is visible. Spawned off the render path so the connect
        // timeout never blocks key handling. Probing continues while paused
        // (RTT is live, not part of the throughput ring).
        if state.tab == Tab::Detail {
            let due = state
                .last_probe_at
                .is_none_or(|t| t.elapsed() >= probe::PROBE_INTERVAL);
            if due && !client.hello.rules.is_empty() {
                let idx = state.selected.min(client.hello.rules.len() - 1);
                let meta = &client.hello.rules[idx];
                if meta.proto == "tcp"
                    && let Some(target) = active_target(meta)
                {
                    let tx = probe_tx.clone();
                    let id = meta.id.clone();
                    let host = target.host.clone();
                    let port = target.port;
                    tokio::spawn(async move {
                        let sample = probe::probe_tcp(&host, port).await;
                        let _ = tx.send((id, sample)).await;
                    });
                    state.last_probe_at = Some(Instant::now());
                }
            }
        }
```

- [ ] **Step 4: Build the whole crate**

Run: `cargo build -p portunus-standalone`
Expected: builds clean. (`if let` chains with `&&` are stable on the 2024 edition / MSRV 1.88.)

- [ ] **Step 5: Run the full standalone test suite**

Run: `cargo test -p portunus-standalone`
Expected: PASS — all lib tests (probe, format, state, render) plus the existing `tests/` integration tests.

- [ ] **Step 6: Commit**

```bash
git add crates/portunus-standalone/src/stats/tui/mod.rs
git commit -m "feat(standalone/tui): probe active target latency in the event loop"
```

---

### Task 6: Workspace gates + manual smoke

**Files:** none (verification only)

- [ ] **Step 1: Format**

Run: `cargo fmt --all`
Expected: no diff (or only this feature's files, already formatted).

- [ ] **Step 2: Clippy with the project's strict gate**

Run: `cargo clippy -p portunus-standalone --all-targets -- -D warnings`
Expected: no warnings. Watch for `clippy::pedantic` hits (e.g. prefer `is_none_or`/`map_or_else`, which this plan already uses).

- [ ] **Step 3: Full workspace test**

Run: `cargo test --workspace`
Expected: green on macOS.

- [ ] **Step 4: Manual TUI smoke (real probe path)**

This exercises the live connect probe end-to-end, which the unit tests cannot.

```bash
# Terminal A: a throwaway upstream to forward to.
python3 -m http.server 9099

# Terminal B: minimal config, then run the daemon (daemon mode is the
# default command — there is no `run` subcommand; the rule field is
# `listen_port`, not `listen`).
cat > /tmp/lat-smoke.toml <<'EOF'
[stats]
enabled = true
socket_path = "/tmp/portunus-lat.sock"

[[rule]]
name = "smoke"
protocol = "tcp"
listen_port = 18080
target = "127.0.0.1:9099"
EOF
cargo run -p portunus-standalone -- --config /tmp/lat-smoke.toml

# Terminal C: attach the TUI, press Enter to reach Detail, watch the
# Targets panel. Expect "127.0.0.1:9099  prio 0   <N>ms" in green within ~2s.
cargo run -p portunus-standalone -- stats --socket /tmp/portunus-lat.sock
```

Then stop the upstream (Ctrl-C in Terminal A) and confirm the RTT turns red and shows `down`/`timeout` within a couple of seconds.

- [ ] **Step 5: No commit**

Verification only — nothing to commit. Do **not** push (per session goal).

---

## Self-Review

**Spec coverage:**
- TCP connect probe → Task 1 `probe_tcp`. ✓
- Client-side only, no wire change → no `stats/mod.rs`/`server.rs` edits anywhere in the plan. ✓
- Active target only → `active_target`/`active_target_index` (Task 1), used by both prober (Task 5) and renderer (Task 4). ✓
- Selected rule + Detail tab only → Task 5 trigger guard. ✓
- UDP shows `—` → Task 4 `is_udp` branch + `detail_udp_active_target_shows_dash`. ✓
- ~2s cadence, 1s timeout → `PROBE_INTERVAL`/`PROBE_TIMEOUT` (Task 1), enforced in Task 5. ✓
- Current value + colour, green/yellow/red thresholds → `fmt_rtt` (Task 2) + Task 4 rendering. ✓
- Pause keeps probing → Task 5 guard does not check `state.paused`; comment documents it. ✓
- Tests on real loopback sockets → Task 1 `probe_ok_*`/`probe_failed_*`. ✓

**Placeholder scan:** none — every step has concrete code/commands.

**Type consistency:** `ProbeSample` (Task 1) used identically in `fmt_rtt` (Task 2), `AppState.probes: HashMap<String, ProbeSample>` (Task 3), render (Task 4), channel `(String, ProbeSample)` (Task 5). `active_target_index`/`active_target` signatures match between Task 1, Task 4, and Task 5. `render_targets` gains a `state: &AppState` param (Task 4) and its sole caller is updated in the same task.

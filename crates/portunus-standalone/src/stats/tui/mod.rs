//! Ratatui TUI for `portunus-standalone stats`.

pub mod format;
pub mod probe;
pub mod render;
pub mod state;

use std::io;
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::stats::Snapshot;
use crate::stats::client::Client;
use probe::{ProbeSample, active_target};
use state::{AppState, Tab};

/// RAII guard for terminal state. Entering raw mode + the alternate screen
/// is undone on `Drop`, so an early `?` return or a panic that unwinds
/// through `run_inner` can never leave the user's terminal corrupted.
struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        Ok(Self)
    }

    /// Best-effort restore. Shared by `Drop` and the panic hook so both the
    /// unwind path (`panic = "unwind"`) and the abort path
    /// (`panic = "abort"`, the workspace release profile) leave a clean
    /// terminal.
    fn restore() {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        Self::restore();
    }
}

/// Install a panic hook that restores the terminal before delegating to the
/// previous hook. Necessary because the release profile sets
/// `panic = "abort"`, which skips `Drop` — without this a panic would abort
/// with raw mode still enabled.
fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        TerminalGuard::restore();
        prev(info);
    }));
}

/// Entry point for the TUI; manages raw-mode setup/teardown around
/// `run_inner`. Always disables raw mode and leaves the alternate
/// screen, even when `run_inner` returns an error or panics.
pub async fn run(socket: &Path) -> ExitCode {
    install_panic_hook();
    match run_inner(socket).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: stats TUI: {e}");
            ExitCode::from(2)
        }
    }
}

async fn run_inner(socket: &Path) -> io::Result<()> {
    // Connect before entering raw mode so a connection error prints normally.
    let (mut client, mut reader) = Client::connect(socket).await?;

    // From here the guard restores the terminal on any exit path (Ok, Err,
    // or panic unwind); the panic hook covers the abort path.
    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut state = AppState::new();
    run_loop(&mut client, &mut reader, &mut terminal, &mut state).await
}

async fn run_loop(
    client: &mut Client,
    reader: &mut tokio::io::BufReader<tokio::net::UnixStream>,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut AppState,
) -> io::Result<()> {
    let mut line_buf = String::new();
    // Render only when something visible changed (a new snapshot, a probe
    // result, or a key press). Idle/paused frames are skipped so we don't
    // rebuild the whole widget tree every 50 ms for no reason. Start dirty
    // so the initial frame paints.
    let mut dirty = true;
    // Results from spawned probe tasks. Bounded; a full channel just drops
    // a probe result, which self-heals on the next tick.
    let (probe_tx, mut probe_rx) = tokio::sync::mpsc::channel::<(String, ProbeSample)>(8);
    loop {
        // Try to receive a snapshot with a short timeout so we can poll key events.
        line_buf.clear();
        let read = tokio::time::timeout(
            Duration::from_millis(50),
            crate::stats::client::read_line_bounded(reader, &mut line_buf),
        )
        .await;
        match read {
            Ok(Ok(0)) => {
                // EOF — daemon closed the socket.
                return Ok(());
            }
            Ok(Ok(_)) => {
                if let Ok(snap) = serde_json::from_str::<Snapshot>(line_buf.trim_end())
                    && !state.paused
                {
                    client.push(snap);
                    dirty = true;
                }
            }
            Ok(Err(e)) => return Err(e),
            Err(_) => { /* timeout — fall through to event poll + render */ }
        }

        // Drain any probe results that arrived since the last iteration.
        while let Ok((id, sample)) = probe_rx.try_recv() {
            state.probes.insert(id, sample);
            dirty = true;
        }

        // Probe every rule's active TCP target so both the Overview list and
        // the Detail panel can show live RTT. One connect per rule per
        // interval; spawned off the render path so the connect timeout never
        // blocks key handling. Probing continues while paused (RTT is live,
        // not part of the throughput ring).
        let due = state
            .last_probe_at
            .is_none_or(|t| t.elapsed() >= probe::PROBE_INTERVAL);
        if due {
            for meta in &client.hello.rules {
                if meta.proto == "tcp"
                    && let Some(target) = active_target(meta)
                {
                    let tx = probe_tx.clone();
                    let id = meta.id.clone();
                    let host = target.host.clone();
                    let port = target.port;
                    tokio::spawn(async move {
                        let sample = probe::probe_tcp(&host, port).await;
                        // Non-blocking: a full channel drops this sample,
                        // which self-heals on the next probe interval.
                        let _ = tx.try_send((id, sample));
                    });
                }
            }
            state.last_probe_at = Some(Instant::now());
        }

        // Non-blocking event poll (10 ms budget).
        if event::poll(Duration::from_millis(10))? {
            let ev = event::read()?;
            // A terminal resize must force a repaint: with change-driven
            // rendering it would otherwise stay stale (mis-laid-out / blank)
            // until the next snapshot/probe/key — up to refresh_ms.
            if matches!(ev, Event::Resize(_, _)) {
                dirty = true;
            }
            if let Event::Key(k) = ev {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                // Any handled key press may change what's displayed.
                dirty = true;
                // IMPORTANT: match Ctrl-C BEFORE plain 'c' (session-reset).
                match (k.code, k.modifiers) {
                    (KeyCode::Char('q'), _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                        return Ok(());
                    }

                    (KeyCode::Char('?'), _) => state.show_help = !state.show_help,

                    (KeyCode::Esc, _) => {
                        // Esc closes the help overlay; otherwise no-op (never quits).
                        state.show_help = false;
                    }

                    (KeyCode::Char('p'), _) => state.paused = !state.paused,

                    (KeyCode::Char('s'), _) => state.sort = state.sort.cycle(),

                    (KeyCode::Char('r'), _) => state.sort_desc = !state.sort_desc,

                    // Session reset — capture current cumulative values as new zero baseline.
                    (KeyCode::Char('c'), _) => {
                        if let Some(snap) = client.ring.back() {
                            // Clone is needed because `reset_baseline` takes &Snapshot
                            // and we hold &client.ring simultaneously.
                            let snap = snap.clone();
                            state.reset_baseline(&snap);
                        }
                    }

                    (KeyCode::Up | KeyCode::Char('k'), _) => {
                        state.selected = state.selected.saturating_sub(1);
                    }
                    (KeyCode::Down | KeyCode::Char('j'), _) => {
                        let n = client.hello.rules.len().saturating_sub(1);
                        if state.selected < n {
                            state.selected += 1;
                        }
                    }

                    // Tab / right / l → next tab.
                    (KeyCode::Tab | KeyCode::Right | KeyCode::Char('l'), _) => {
                        state.tab = state.tab.next();
                    }
                    // Shift-Tab / left / h → previous tab.
                    (KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h'), _) => {
                        state.tab = state.tab.prev();
                    }

                    // Enter → jump to Detail tab.
                    (KeyCode::Enter, _) => state.tab = Tab::Detail,

                    _ => {}
                }
            }
        }

        // Repaint only when state changed; skip idle/paused frames.
        if dirty {
            terminal.draw(|f| render::render(f, f.area(), client, state))?;
            dirty = false;
        }
    }
}

//! Ratatui TUI for `portunus-standalone stats`.

pub mod format;
pub mod render;
pub mod state;

use std::io;
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::io::AsyncBufReadExt;

use crate::stats::client::Client;
use crate::stats::Snapshot;
use state::{AppState, Tab};

/// Entry point for the TUI; manages raw-mode setup/teardown around
/// `run_inner`. Always disables raw mode and leaves the alternate
/// screen, even when `run_inner` returns an error.
pub async fn run(socket: &Path) -> ExitCode {
    match run_inner(socket).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: stats TUI: {e}");
            ExitCode::from(2)
        }
    }
}

async fn run_inner(socket: &Path) -> io::Result<()> {
    let (mut client, mut reader) = Client::connect(socket).await?;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut state = AppState::new();
    let r = run_loop(&mut client, &mut reader, &mut terminal, &mut state).await;

    // Always restore the terminal, even if the loop returned an error.
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    r
}

async fn run_loop(
    client: &mut Client,
    reader: &mut tokio::io::BufReader<tokio::net::UnixStream>,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut AppState,
) -> io::Result<()> {
    let mut line_buf = String::new();
    loop {
        // Try to receive a snapshot with a short timeout so we can poll key events.
        line_buf.clear();
        let read =
            tokio::time::timeout(Duration::from_millis(50), reader.read_line(&mut line_buf)).await;
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
                }
            }
            Ok(Err(e)) => return Err(e),
            Err(_) => { /* timeout — fall through to event poll + render */ }
        }

        // Non-blocking key event poll (10 ms budget).
        if event::poll(Duration::from_millis(10))?
            && let Event::Key(k) = event::read()?
        {
            if k.kind != KeyEventKind::Press {
                continue;
            }
            // IMPORTANT: match Ctrl-C BEFORE plain 'c' (session-reset).
            match (k.code, k.modifiers) {
                (KeyCode::Char('q'), _)
                | (KeyCode::Char('c'), KeyModifiers::CONTROL) => return Ok(()),

                (KeyCode::Char('?'), _) => state.show_help = !state.show_help,

                (KeyCode::Esc, _) => {
                    if state.show_help {
                        state.show_help = false;
                    } else if !state.filter.is_empty() {
                        state.filter.clear();
                    }
                    // In normal mode with no filter: no-op (no quit).
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

        terminal.draw(|f| render::render(f, f.area(), client, state))?;
    }
}

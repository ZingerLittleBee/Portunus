//! Ratatui-based TUI for `portunus-standalone stats`.
//! Task 16-18 implement the real event loop; this stub returns an
//! error exit code so the binary compiles and dispatches correctly.

use std::path::Path;
use std::process::ExitCode;

/// Stub entry point — Tasks 16-18 replace this with the real TUI loop.
#[allow(clippy::unused_async)]
pub async fn run(_socket: &Path) -> ExitCode {
    eprintln!("error: TUI not yet implemented");
    ExitCode::from(2)
}

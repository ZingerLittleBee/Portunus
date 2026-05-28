//! Ratatui-based TUI for `portunus-standalone stats`.

pub mod format;
pub mod render;
pub mod state;

use std::path::Path;
use std::process::ExitCode;

/// Stub — Task 18 implements the real event loop.
#[allow(clippy::unused_async)]
pub async fn run(_socket: &Path) -> ExitCode {
    eprintln!("error: TUI not yet implemented");
    ExitCode::from(2)
}

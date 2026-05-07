//! 006-management-web-ui T005: ensure `webui/dist/` exists before
//! `rust-embed` tries to compile-time-embed it.
//!
//! Two modes:
//! - Default (release / CI): the SPA must be built first
//!   (`cd webui && pnpm build`). Otherwise the build fails with a
//!   clear, actionable message.
//! - Backend-only iteration: set `FORWARD_SKIP_WEBUI=1` to compile a
//!   UI-less binary. Useful during pure-Rust work; release pipelines
//!   never set this env var.

use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dist_dir = manifest_dir.join("../../webui/dist");
    let index_html = dist_dir.join("index.html");

    println!("cargo:rerun-if-env-changed=FORWARD_SKIP_WEBUI");
    println!("cargo:rerun-if-changed=../../webui/dist");

    if std::env::var_os("FORWARD_SKIP_WEBUI").is_some() {
        // Compile a stub index.html so rust-embed has something to embed.
        std::fs::create_dir_all(&dist_dir).ok();
        if !index_html.exists() {
            let _ = std::fs::write(
                &index_html,
                "<!doctype html><meta charset=utf-8><title>forward-server (UI skipped)</title>\
                 <p>This forward-server was compiled with FORWARD_SKIP_WEBUI=1. \
                 Rebuild without that env var to bundle the operator Web UI.</p>",
            );
        }
        println!("cargo:warning=FORWARD_SKIP_WEBUI=1 — embedding stub Web UI");
        return;
    }

    if !index_html.exists() {
        eprintln!(
            "\n  forward-server build error: webui/dist/index.html is missing.\n\
             \n  Build the operator Web UI first:\n    cd webui && pnpm install && pnpm build\n\
             \n  Or skip the UI for backend-only iteration:\n    FORWARD_SKIP_WEBUI=1 cargo build -p forward-server\n",
        );
        std::process::exit(1);
    }
}

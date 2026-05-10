//! 006-management-web-ui T005: ensure `webui/dist/` exists before
//! `rust-embed` tries to compile-time-embed it.
//!
//! Two modes:
//! - Default (release / CI): the SPA must be built first
//!   (`cd webui && pnpm build`). Otherwise the build fails with a
//!   clear, actionable message.
//! - Backend-only iteration: set `PORTUNUS_SKIP_WEBUI=1` to compile a
//!   UI-less binary. Useful during pure-Rust work; release pipelines
//!   never set this env var.

use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dist_dir = manifest_dir.join("../../webui/dist");
    let index_html = dist_dir.join("index.html");

    println!("cargo:rerun-if-env-changed=PORTUNUS_SKIP_WEBUI");
    println!("cargo:rerun-if-changed=../../webui/dist");

    let skip_webui_env = if std::env::var_os("PORTUNUS_SKIP_WEBUI").is_some() {
        Some("PORTUNUS_SKIP_WEBUI")
    } else {
        None
    };

    if let Some(env_name) = skip_webui_env {
        // Compile a stub index.html so rust-embed has something to embed.
        std::fs::create_dir_all(&dist_dir).ok();
        if !index_html.exists() {
            let _ = std::fs::write(
                &index_html,
                "<!doctype html><meta charset=utf-8><title>portunus-server (UI skipped)</title>\
                 <p>This portunus-server was compiled with a Web UI skip env var. \
                 Rebuild without that env var to bundle the operator Web UI.</p>",
            );
        }
        println!("cargo:warning={env_name}=1 - embedding stub Web UI");
        return;
    }

    if !index_html.exists() {
        eprintln!(
            "\n  portunus-server build error: webui/dist/index.html is missing.\n\
             \n  Build the operator Web UI first:\n    cd webui && pnpm install && pnpm build\n\
             \n  Or skip the UI for backend-only iteration:\n    PORTUNUS_SKIP_WEBUI=1 cargo build -p portunus-server\n",
        );
        std::process::exit(1);
    }
}

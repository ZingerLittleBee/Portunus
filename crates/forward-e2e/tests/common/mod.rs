//! Common helpers shared across e2e test files.
//!
//! - [`spawn_server`] launches the `forward-server` binary against a fresh
//!   temp config dir.
//! - [`spawn_client`] launches the `forward-client` binary against a bundle
//!   path produced via `provision-client`.
//!
//! Both helpers return a handle that kills the child on drop, so tests can
//! assert on side-effects without leaking processes.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

fn fresh_tempdir(label: &str) -> TempDir {
    TempDir::new().unwrap_or_else(|e| panic!("tempdir for {label}: {e}"))
}

/// Locate a workspace binary built by `cargo test` without relying on
/// `CARGO_BIN_EXE_*` env vars (which Cargo only injects for binaries that
/// belong to the *current* package, not workspace siblings).
pub fn workspace_bin(name: &str) -> PathBuf {
    // CARGO_MANIFEST_DIR points to forward-e2e/. Walk up to the workspace
    // root, then descend into target/<profile>/<name>.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root resolvable")
        .to_path_buf();
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map_or_else(|_| workspace_root.join("target"), PathBuf::from);
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    // Tests run under the same profile that built them; check both.
    for profile in ["debug", "release"] {
        let candidate = target_dir.join(profile).join(&exe);
        if candidate.exists() {
            return candidate;
        }
    }
    // Last resort: the binary may not have been built yet (e.g., cross-crate
    // dev-dep on a `[[bin]]` target doesn't trigger a build). Build it now.
    let status = Command::new(env!("CARGO"))
        .arg("build")
        .arg("--quiet")
        .arg("-p")
        .arg(name)
        .arg("--bin")
        .arg(name)
        .status()
        .unwrap_or_else(|e| panic!("cargo build --bin {name} failed to launch: {e}"));
    assert!(status.success(), "cargo build --bin {name} failed");
    let candidate = target_dir.join("debug").join(&exe);
    assert!(
        candidate.exists(),
        "binary still missing after build: {}",
        candidate.display()
    );
    candidate
}

fn cmd_for(name: &str) -> Command {
    Command::new(workspace_bin(name))
}

pub struct ServerHandle {
    pub child: Child,
    pub config_dir: TempDir,
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct ClientHandle {
    pub child: Child,
}

impl Drop for ClientHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Launch `forward-server serve` against a fresh temp config dir.
/// Caller is responsible for waiting for readiness (e.g., by polling the
/// operator HTTP listener).
pub fn spawn_server(extra_args: &[&str]) -> ServerHandle {
    let config_dir = fresh_tempdir("server config");
    let mut cmd = cmd_for("forward-server");
    cmd.arg("--config-dir")
        .arg(config_dir.path())
        .arg("serve")
        .args(extra_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().expect("spawn forward-server");
    ServerHandle { child, config_dir }
}

/// Launch `forward-client --bundle <path>`.
pub fn spawn_client(bundle_path: &Path, extra_args: &[&str]) -> ClientHandle {
    let mut cmd = cmd_for("forward-client");
    cmd.arg("--bundle")
        .arg(bundle_path)
        .args(extra_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn().expect("spawn forward-client");
    ClientHandle { child }
}

/// Run `forward-server provision-client <name>` synchronously; return the
/// path to the generated bundle file.
pub fn provision_client(config_dir: &Path, name: &str) -> PathBuf {
    let out = fresh_tempdir("bundle out").keep();
    let bundle = out.join(format!("{name}.bundle.json"));
    let status = cmd_for("forward-server")
        .arg("--config-dir")
        .arg(config_dir)
        .arg("provision-client")
        .arg(name)
        .arg("--out")
        .arg(&bundle)
        .status()
        .expect("run provision-client");
    assert!(status.success(), "provision-client failed: {status:?}");
    bundle
}

/// Block (with a timeout) until `predicate` returns `Some(value)`.
pub fn wait_for<T>(timeout: Duration, mut predicate: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(v) = predicate() {
            return Some(v);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

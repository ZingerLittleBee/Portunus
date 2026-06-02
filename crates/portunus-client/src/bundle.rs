//! `CredentialBundle` reader/writer for the local client bundle.
//!
//! The enrollment RPC returns this schema. We do NOT pull `portunus-server`
//! into the client's compile graph; duplicating the small struct keeps the
//! two binaries decoupled.

use std::path::{Path, PathBuf};

use portunus_core::{ClientId, ClientName};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CredentialBundle {
    #[serde(default = "default_version")]
    pub version: u32,
    pub client_name: ClientName,
    /// 015-client-stable-id: stable opaque identity. Absent in a pre-upgrade
    /// bundle (legacy-tolerant) — the client still authenticates via `token`,
    /// and the server resolves the id from that token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<ClientId>,
    pub server_endpoint: String,
    pub server_cert_sha256: String,
    pub server_cert_pem: String,
    pub token: String,
}

fn default_version() -> u32 {
    1
}

impl CredentialBundle {
    pub fn read_from(path: &Path) -> std::io::Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        let bundle: Self = serde_json::from_str(&raw).map_err(std::io::Error::other)?;
        if bundle.version != 1 {
            return Err(std::io::Error::other(format!(
                "unsupported bundle version: {}",
                bundle.version
            )));
        }
        bundle.verify_pin_consistency()?;
        Ok(bundle)
    }

    pub fn from_enrollment(
        version: u32,
        client_name: ClientName,
        client_id: Option<ClientId>,
        server_endpoint: String,
        server_cert_sha256: String,
        server_cert_pem: String,
        token: String,
    ) -> std::io::Result<Self> {
        if version != 1 {
            return Err(std::io::Error::other(format!(
                "unsupported bundle version: {version}"
            )));
        }
        let bundle = Self {
            version,
            client_name,
            client_id,
            server_endpoint,
            server_cert_sha256,
            server_cert_pem,
            token,
        };
        bundle.verify_pin_consistency()?;
        Ok(bundle)
    }

    pub fn write_to(&self, path: &Path) -> std::io::Result<()> {
        let body = serde_json::to_vec_pretty(self)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_secret(path, &body)
    }

    /// Confirm `sha256(DER(server_cert_pem)) == server_cert_sha256`. A bundle
    /// that fails this check is corrupt or maliciously assembled — fail
    /// loudly rather than dialling out under a forged pin.
    fn verify_pin_consistency(&self) -> std::io::Result<()> {
        let der = leaf_der_from_pem(&self.server_cert_pem)?;
        let computed = portunus_core::fingerprint::sha256_hex(&der);
        if !computed.eq_ignore_ascii_case(&self.server_cert_sha256) {
            return Err(std::io::Error::other(format!(
                "bundle pin mismatch: cert_pem hashes to {computed}, bundle says {}",
                self.server_cert_sha256
            )));
        }
        Ok(())
    }
}

#[cfg(unix)]
fn write_secret(path: &Path, body: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(body)?;
    file.sync_all()
}

#[cfg(not(unix))]
fn write_secret(path: &Path, body: &[u8]) -> std::io::Result<()> {
    std::fs::write(path, body)
}

/// 008-sqlite-storage T078 — bundle path resolution per FR-020.
///
/// Resolution order (first existing file wins):
/// 1. Explicit `--bundle <PATH>` argument (`override_path`).
/// 2. `$PORTUNUS_CLIENT_BUNDLE` environment variable.
/// 3. `$XDG_CONFIG_HOME/portunus/client.bundle.json`.
/// 4. `$HOME/.config/portunus/client.bundle.json`.
/// 5. `./client.bundle.json` (current working directory).
///
/// Returns the resolved path on success. On failure, returns the list of
/// every candidate path that was attempted so the operator can see which
/// locations were considered.
#[derive(Debug)]
pub struct BundleSearchError {
    pub attempted: Vec<PathBuf>,
}

impl std::fmt::Display for BundleSearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "no client bundle found; attempted paths:")?;
        for p in &self.attempted {
            writeln!(f, "  - {}", p.display())?;
        }
        Ok(())
    }
}

impl std::error::Error for BundleSearchError {}

pub fn resolve_bundle_path(override_path: Option<&Path>) -> Result<PathBuf, BundleSearchError> {
    resolve_bundle_path_with(override_path, |k| std::env::var(k).ok())
}

/// Deterministic search-path resolver: identical semantics to
/// [`resolve_bundle_path`], but takes an explicit env getter so the
/// search-path contract can be exercised in tests without mutating
/// the process-global environment (workspace lint forbids `unsafe`).
pub fn resolve_bundle_path_with<F>(
    override_path: Option<&Path>,
    env: F,
) -> Result<PathBuf, BundleSearchError>
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(p) = override_path {
        // Honour the operator's explicit choice even if the path doesn't
        // exist — the load step will surface the error with a clear
        // message and the path actually requested. Mirroring v0.7
        // behaviour avoids "silently fell through to a different bundle"
        // surprises.
        return Ok(p.to_path_buf());
    }
    let mut attempted: Vec<PathBuf> = Vec::with_capacity(4);

    if let Some(val) = env("PORTUNUS_CLIENT_BUNDLE") {
        let p = PathBuf::from(val);
        if p.is_file() {
            return Ok(p);
        }
        attempted.push(p);
    }
    if let Some(xdg) = env("XDG_CONFIG_HOME") {
        let p = PathBuf::from(xdg)
            .join("portunus")
            .join("client.bundle.json");
        if p.is_file() {
            return Ok(p);
        }
        attempted.push(p);
    }
    if let Some(home) = env("HOME") {
        let p = PathBuf::from(home)
            .join(".config")
            .join("portunus")
            .join("client.bundle.json");
        if p.is_file() {
            return Ok(p);
        }
        attempted.push(p);
    }
    let cwd_path = PathBuf::from("./client.bundle.json");
    if cwd_path.is_file() {
        return Ok(cwd_path);
    }
    attempted.push(cwd_path);

    Err(BundleSearchError { attempted })
}

fn leaf_der_from_pem(pem: &str) -> std::io::Result<Vec<u8>> {
    use base64::Engine as _;
    let mut in_block = false;
    let mut buf = String::new();
    for line in pem.lines() {
        let line = line.trim();
        if line == "-----BEGIN CERTIFICATE-----" {
            in_block = true;
            buf.clear();
            continue;
        }
        if line == "-----END CERTIFICATE-----" {
            return base64::engine::general_purpose::STANDARD
                .decode(buf.trim())
                .map_err(std::io::Error::other);
        }
        if in_block {
            buf.push_str(line);
        }
    }
    Err(std::io::Error::other("no CERTIFICATE block in PEM"))
}

#[cfg(test)]
mod search_tests {
    //! 008-sqlite-storage T079 — bundle search-path contract tests.
    //!
    //! Workspace lint forbids `unsafe`, which rules out
    //! `std::env::set_var` (now `unsafe` since Rust 1.85). The resolver
    //! exposes `resolve_bundle_path_with`, taking an env getter, so we
    //! drive the contract from a `HashMap` per test instead of mutating
    //! the process environment.
    use super::*;
    use std::collections::HashMap;

    fn env_from<'a>(
        map: &'a HashMap<&'static str, String>,
    ) -> impl Fn(&str) -> Option<String> + 'a {
        move |k: &str| map.get(k).cloned()
    }

    #[test]
    fn explicit_override_wins_even_if_path_does_not_exist() {
        let env = HashMap::<&'static str, String>::new();
        let nonexistent = PathBuf::from("/tmp/portunus-client-does-not-exist.json");
        let resolved = resolve_bundle_path_with(Some(&nonexistent), env_from(&env))
            .expect("override must short-circuit search");
        assert_eq!(resolved, nonexistent);
    }

    #[test]
    fn portunus_env_var_resolves_when_path_exists() {
        let dir = tempfile::TempDir::new().unwrap();
        let p = dir.path().join("custom.bundle.json");
        std::fs::write(&p, "{}").unwrap();
        let mut env = HashMap::new();
        env.insert("PORTUNUS_CLIENT_BUNDLE", p.to_string_lossy().into_owned());
        let resolved =
            resolve_bundle_path_with(None, env_from(&env)).expect("env var path resolves");
        assert_eq!(resolved, p);
    }

    #[test]
    fn xdg_config_home_resolves_when_present() {
        let dir = tempfile::TempDir::new().unwrap();
        let xdg = dir.path().to_path_buf();
        let target = xdg.join("portunus").join("client.bundle.json");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "{}").unwrap();
        let mut env = HashMap::new();
        env.insert("XDG_CONFIG_HOME", xdg.to_string_lossy().into_owned());
        let resolved = resolve_bundle_path_with(None, env_from(&env)).expect("XDG path resolves");
        assert_eq!(resolved, target);
    }

    #[test]
    fn home_dotconfig_resolves_when_xdg_unset() {
        let dir = tempfile::TempDir::new().unwrap();
        let home = dir.path().to_path_buf();
        let target = home
            .join(".config")
            .join("portunus")
            .join("client.bundle.json");
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, "{}").unwrap();
        let mut env = HashMap::new();
        env.insert("HOME", home.to_string_lossy().into_owned());
        let resolved = resolve_bundle_path_with(None, env_from(&env)).expect("HOME path resolves");
        assert_eq!(resolved, target);
    }

    #[test]
    fn env_precedes_xdg_precedes_home() {
        // All three set; the env var must win.
        let env_dir = tempfile::TempDir::new().unwrap();
        let env_target = env_dir.path().join("env.bundle.json");
        std::fs::write(&env_target, "{}").unwrap();
        let xdg_dir = tempfile::TempDir::new().unwrap();
        let xdg_target = xdg_dir.path().join("portunus").join("client.bundle.json");
        std::fs::create_dir_all(xdg_target.parent().unwrap()).unwrap();
        std::fs::write(&xdg_target, "{}").unwrap();
        let home_dir = tempfile::TempDir::new().unwrap();
        let home_target = home_dir
            .path()
            .join(".config")
            .join("portunus")
            .join("client.bundle.json");
        std::fs::create_dir_all(home_target.parent().unwrap()).unwrap();
        std::fs::write(&home_target, "{}").unwrap();
        let mut env = HashMap::new();
        env.insert(
            "PORTUNUS_CLIENT_BUNDLE",
            env_target.to_string_lossy().into_owned(),
        );
        env.insert(
            "XDG_CONFIG_HOME",
            xdg_dir.path().to_string_lossy().into_owned(),
        );
        env.insert("HOME", home_dir.path().to_string_lossy().into_owned());
        let resolved = resolve_bundle_path_with(None, env_from(&env)).unwrap();
        assert_eq!(resolved, env_target);
    }

    #[test]
    fn not_found_lists_every_attempted_path() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut env = HashMap::new();
        env.insert(
            "PORTUNUS_CLIENT_BUNDLE",
            dir.path()
                .join("env-missing.json")
                .to_string_lossy()
                .into_owned(),
        );
        env.insert(
            "XDG_CONFIG_HOME",
            dir.path()
                .join("xdg-missing")
                .to_string_lossy()
                .into_owned(),
        );
        env.insert(
            "HOME",
            dir.path()
                .join("home-missing")
                .to_string_lossy()
                .into_owned(),
        );
        let err = resolve_bundle_path_with(None, env_from(&env))
            .expect_err("must fail when nothing resolves");
        // 4 candidates: env, xdg, home, cwd.
        assert_eq!(err.attempted.len(), 4, "attempted: {:?}", err.attempted);
        assert!(
            err.attempted
                .iter()
                .any(|p| p.ends_with("env-missing.json")),
            "should record env-var attempt: {:?}",
            err.attempted
        );
        assert!(
            err.attempted
                .iter()
                .any(|p| p.to_string_lossy().contains("xdg-missing")),
            "should record XDG attempt: {:?}",
            err.attempted
        );
        assert!(
            err.attempted
                .iter()
                .any(|p| p.to_string_lossy().contains("home-missing")),
            "should record HOME attempt: {:?}",
            err.attempted
        );
        assert!(
            err.attempted
                .iter()
                .any(|p| p.ends_with("client.bundle.json")),
            "should record cwd attempt: {:?}",
            err.attempted
        );
        // Display message lists every attempted path on its own line.
        let s = err.to_string();
        assert!(s.contains("no client bundle found"), "msg: {s}");
        assert_eq!(
            s.matches("  - ").count(),
            err.attempted.len(),
            "one bullet per attempted path: msg={s}"
        );
    }
}

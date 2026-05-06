//! Atomic-write JSON token store.
//!
//! On-disk schema and write protocol are governed by `contracts/persistence.md`.
//! In short:
//!   - Schema is `{ "version": 1, "tokens": [...] }`. Loader refuses unknown
//!     versions (forward-compatible).
//!   - Mutations write to a temp file in the same directory, fsync, then
//!     rename(2) over the target, then fsync the parent directory.
//!   - Revoked records are kept (audit trail), not deleted.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use chrono::{DateTime, Utc};
use forward_core::{ClientName, fingerprint};
use serde::{Deserialize, Serialize};

use crate::{AuthError, AuthFailureReason, Authenticator, ClientIdentity, token};

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct StoreFile {
    version: u32,
    tokens: Vec<TokenRecordWire>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TokenRecordWire {
    client_name: ClientName,
    /// Hex-encoded blake3 hash. 64 lowercase hex chars.
    token_hash: String,
    issued_at: DateTime<Utc>,
    revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
struct TokenRecord {
    client_name: ClientName,
    token_hash: [u8; 32],
    issued_at: DateTime<Utc>,
    revoked_at: Option<DateTime<Utc>>,
}

impl TokenRecord {
    fn from_wire(w: TokenRecordWire) -> Result<Self, AuthError> {
        let raw = hex_decode(&w.token_hash)
            .ok_or_else(|| AuthError::StoreCorrupt(format!("bad hash for {}", w.client_name)))?;
        if raw.len() != 32 {
            return Err(AuthError::StoreCorrupt(format!(
                "hash for {} has length {}",
                w.client_name,
                raw.len()
            )));
        }
        let mut hash = [0u8; 32];
        hash.copy_from_slice(&raw);
        Ok(Self {
            client_name: w.client_name,
            token_hash: hash,
            issued_at: w.issued_at,
            revoked_at: w.revoked_at,
        })
    }

    fn to_wire(&self) -> TokenRecordWire {
        TokenRecordWire {
            client_name: self.client_name.clone(),
            token_hash: fingerprint::hex(&self.token_hash),
            issued_at: self.issued_at,
            revoked_at: self.revoked_at,
        }
    }
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() / 2);
    let bytes = s.as_bytes();
    for chunk in bytes.chunks_exact(2) {
        let hi = hex_nibble(chunk[0])?;
        let lo = hex_nibble(chunk[1])?;
        out.push((hi << 4) | lo);
    }
    Some(out)
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// On-disk token store. Cheap to clone (`Arc` internally); thread-safe.
#[derive(Debug)]
pub struct FileTokenStore {
    path: PathBuf,
    state: RwLock<HashMap<ClientName, TokenRecord>>,
}

impl FileTokenStore {
    /// Open an existing store, or initialise an empty one if absent.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, AuthError> {
        let path = path.into();
        let state = if path.exists() {
            Self::load_from(&path)?
        } else {
            HashMap::new()
        };
        Ok(Self {
            path,
            state: RwLock::new(state),
        })
    }

    fn load_from(path: &Path) -> Result<HashMap<ClientName, TokenRecord>, AuthError> {
        let raw = fs::read_to_string(path)?;
        let file: StoreFile = serde_json::from_str(&raw)
            .map_err(|e| AuthError::StoreCorrupt(format!("invalid json: {e}")))?;
        if file.version != SCHEMA_VERSION {
            return Err(AuthError::StoreCorrupt(format!(
                "unsupported schema version {} (this binary supports {})",
                file.version, SCHEMA_VERSION
            )));
        }
        let mut map: HashMap<ClientName, TokenRecord> = HashMap::with_capacity(file.tokens.len());
        for entry in file.tokens {
            let rec = TokenRecord::from_wire(entry)?;
            if map.contains_key(&rec.client_name) {
                return Err(AuthError::StoreCorrupt(format!(
                    "duplicate client_name {}",
                    rec.client_name
                )));
            }
            map.insert(rec.client_name.clone(), rec);
        }
        Ok(map)
    }

    fn snapshot(&self) -> StoreFile {
        let state = self.state.read().expect("poisoned");
        let mut tokens: Vec<TokenRecordWire> = state.values().map(TokenRecord::to_wire).collect();
        tokens.sort_by(|a, b| a.client_name.as_str().cmp(b.client_name.as_str()));
        StoreFile {
            version: SCHEMA_VERSION,
            tokens,
        }
    }

    /// Atomic write: tmp + fsync + rename + parent fsync. Mode 0600.
    fn persist(&self) -> Result<(), AuthError> {
        let snapshot = self.snapshot();
        let body = serde_json::to_vec_pretty(&snapshot)
            .map_err(|e| AuthError::StoreCorrupt(format!("serialize: {e}")))?;
        let parent = self.path.parent().ok_or_else(|| {
            AuthError::StoreCorrupt(format!("path has no parent: {}", self.path.display()))
        })?;
        fs::create_dir_all(parent)?;
        let pid = std::process::id();
        let tag = nano_random_tag();
        let file_name = self.path.file_name().map_or_else(
            || "tokens.json".to_string(),
            |s| s.to_string_lossy().into_owned(),
        );
        let tmp = parent.join(format!("{file_name}.tmp.{pid}.{tag}"));

        write_tmp_then_rename(&tmp, &self.path, parent, &body)?;
        Ok(())
    }

    /// Snapshot of provisioned clients (for `list-clients`). The token hash
    /// is intentionally NOT exposed.
    pub fn list(&self) -> Vec<ProvisionedClient> {
        let state = self.state.read().expect("poisoned");
        let mut out: Vec<ProvisionedClient> = state
            .values()
            .map(|r| ProvisionedClient {
                client_name: r.client_name.clone(),
                issued_at: r.issued_at,
                revoked_at: r.revoked_at,
            })
            .collect();
        out.sort_by(|a, b| a.client_name.as_str().cmp(b.client_name.as_str()));
        out
    }
}

#[derive(Debug, Clone)]
pub struct ProvisionedClient {
    pub client_name: ClientName,
    pub issued_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

fn nano_random_tag() -> u64 {
    use rand::RngCore;
    rand::rngs::OsRng.next_u64()
}

#[cfg(unix)]
fn write_tmp_then_rename(
    tmp: &Path,
    dest: &Path,
    parent: &Path,
    body: &[u8],
) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(tmp)?;
    f.write_all(body)?;
    f.sync_all()?;
    drop(f);
    fs::rename(tmp, dest)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn write_tmp_then_rename(
    tmp: &Path,
    dest: &Path,
    _parent: &Path,
    body: &[u8],
) -> std::io::Result<()> {
    let mut f = OpenOptions::new().write(true).create_new(true).open(tmp)?;
    f.write_all(body)?;
    f.sync_all()?;
    drop(f);
    fs::rename(tmp, dest)?;
    Ok(())
}

impl Authenticator for FileTokenStore {
    fn verify(&self, token: &str) -> Result<ClientIdentity, AuthError> {
        if token.is_empty() {
            return Err(AuthError::Failed(AuthFailureReason::Missing));
        }
        if token.len() > 256 {
            return Err(AuthError::Failed(AuthFailureReason::Malformed));
        }
        let presented = token::hash_token(token);
        let state = self.state.read().expect("poisoned");
        // Linear scan over ≤100 entries — cache-friendly, no early exit
        // beyond the necessary `match` per Constitution V (no leaky timing).
        let mut matched: Option<&TokenRecord> = None;
        for record in state.values() {
            if fingerprint::ct_eq(&presented, &record.token_hash) {
                matched = Some(record);
            }
        }
        match matched {
            None => Err(AuthError::Failed(AuthFailureReason::NotFound)),
            Some(rec) if rec.revoked_at.is_some() => {
                Err(AuthError::Failed(AuthFailureReason::Revoked))
            }
            Some(rec) => Ok(ClientIdentity {
                client_name: rec.client_name.clone(),
            }),
        }
    }

    fn issue(&self, name: ClientName) -> Result<String, AuthError> {
        {
            let state = self.state.read().expect("poisoned");
            if state.contains_key(&name) {
                return Err(AuthError::ClientAlreadyExists(name));
            }
        }
        let token = token::generate_token();
        let hash = token::hash_token(&token);
        let record = TokenRecord {
            client_name: name.clone(),
            token_hash: hash,
            issued_at: Utc::now(),
            revoked_at: None,
        };
        {
            let mut state = self.state.write().expect("poisoned");
            // Re-check under write lock (TOCTOU).
            if state.contains_key(&name) {
                return Err(AuthError::ClientAlreadyExists(name));
            }
            state.insert(name, record);
        }
        self.persist()?;
        Ok(token)
    }

    fn revoke(&self, name: &ClientName) -> Result<(), AuthError> {
        let mut changed = false;
        {
            let mut state = self.state.write().expect("poisoned");
            if let Some(rec) = state.get_mut(name)
                && rec.revoked_at.is_none()
            {
                rec.revoked_at = Some(Utc::now());
                changed = true;
            }
        }
        if changed {
            self.persist()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cn(s: &str) -> ClientName {
        ClientName::new(s).unwrap()
    }

    #[test]
    fn issue_returns_unique_token_each_call() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileTokenStore::open(dir.path().join("tokens.json")).unwrap();
        let t1 = store.issue(cn("edge-01")).unwrap();
        let t2 = store.issue(cn("edge-02")).unwrap();
        assert_ne!(t1, t2);
        assert_eq!(t1.len(), 43);
    }

    #[test]
    fn issue_rejects_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileTokenStore::open(dir.path().join("tokens.json")).unwrap();
        store.issue(cn("edge-01")).unwrap();
        let err = store.issue(cn("edge-01")).unwrap_err();
        match err {
            AuthError::ClientAlreadyExists(n) => assert_eq!(n.as_str(), "edge-01"),
            other => panic!("expected ClientAlreadyExists, got {other:?}"),
        }
    }

    #[test]
    fn verify_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileTokenStore::open(dir.path().join("tokens.json")).unwrap();
        let token = store.issue(cn("edge-01")).unwrap();
        let id = store.verify(&token).unwrap();
        assert_eq!(id.client_name.as_str(), "edge-01");
    }

    #[test]
    fn verify_rejects_wrong_token() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileTokenStore::open(dir.path().join("tokens.json")).unwrap();
        store.issue(cn("edge-01")).unwrap();
        let err = store
            .verify("not-a-real-token-not-a-real-token-not-a-rea")
            .unwrap_err();
        assert!(matches!(
            err,
            AuthError::Failed(AuthFailureReason::NotFound)
        ));
    }

    #[test]
    fn verify_missing_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileTokenStore::open(dir.path().join("tokens.json")).unwrap();
        let err = store.verify("").unwrap_err();
        assert!(matches!(err, AuthError::Failed(AuthFailureReason::Missing)));
    }

    #[test]
    fn revoke_blocks_verify() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let store = FileTokenStore::open(&path).unwrap();
        let token = store.issue(cn("edge-01")).unwrap();
        store.revoke(&cn("edge-01")).unwrap();
        let err = store.verify(&token).unwrap_err();
        assert!(matches!(err, AuthError::Failed(AuthFailureReason::Revoked)));
    }

    #[test]
    fn revoke_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileTokenStore::open(dir.path().join("tokens.json")).unwrap();
        store.issue(cn("edge-01")).unwrap();
        store.revoke(&cn("edge-01")).unwrap();
        store.revoke(&cn("edge-01")).unwrap(); // second call no-op
        store.revoke(&cn("nonexistent")).unwrap(); // also no-op
    }

    #[test]
    fn persists_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let token = {
            let store = FileTokenStore::open(&path).unwrap();
            let t = store.issue(cn("edge-01")).unwrap();
            store.issue(cn("edge-02")).unwrap();
            store.revoke(&cn("edge-02")).unwrap();
            t
        };
        let reopened = FileTokenStore::open(&path).unwrap();
        let id = reopened.verify(&token).unwrap();
        assert_eq!(id.client_name.as_str(), "edge-01");
    }

    #[test]
    fn rejects_unknown_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        std::fs::write(&path, r#"{"version": 99, "tokens": []}"#).unwrap();
        let err = FileTokenStore::open(&path).unwrap_err();
        assert!(matches!(err, AuthError::StoreCorrupt(_)));
    }

    #[test]
    fn rejects_duplicate_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let body = r#"{
          "version": 1,
          "tokens": [
            {"client_name": "edge-01", "token_hash": "00", "issued_at": "2026-05-06T00:00:00Z", "revoked_at": null},
            {"client_name": "edge-01", "token_hash": "01", "issued_at": "2026-05-06T00:00:00Z", "revoked_at": null}
          ]
        }"#;
        std::fs::write(&path, body).unwrap();
        let err = FileTokenStore::open(&path).unwrap_err();
        assert!(matches!(err, AuthError::StoreCorrupt(_)));
    }

    #[test]
    fn verify_concurrent_readers() {
        use std::sync::Arc;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let store = Arc::new(FileTokenStore::open(&path).unwrap());
        let token = store.issue(cn("edge-01")).unwrap();
        let mut handles = vec![];
        for _ in 0..16 {
            let s = Arc::clone(&store);
            let t = token.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..100 {
                    s.verify(&t).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn persisted_file_is_mode_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens.json");
        let store = FileTokenStore::open(&path).unwrap();
        store.issue(cn("edge-01")).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "got mode {mode:o}");
    }
}

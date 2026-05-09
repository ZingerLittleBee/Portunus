//! 008-sqlite-storage T004 / T013 — data-dir resolution + filesystem-class probe.
//!
//! Spec: `specs/008-sqlite-storage/spec.md` FR-019.
//! Research: `specs/008-sqlite-storage/research.md` R-008.
//!
//! Two responsibilities:
//!
//! 1. `resolve(opt)` — turn an optional `--data-dir <PATH>` into a
//!    concrete `PathBuf`, honouring `$STATE_DIRECTORY` (systemd) →
//!    `$XDG_STATE_HOME/forward-rs` → `$HOME/.local/state/forward-rs` →
//!    `./forward-rs.state`.
//!
//! 2. `probe_fs_class(path)` — refuse NFS / TMPFS / RAMFS so SQLite
//!    never lands on a filesystem that cannot honour POSIX advisory
//!    locking + `fsync`.

use std::path::{Path, PathBuf};

/// Application name used in path-resolution defaults.
const APP_NAME: &str = "forward-rs";

/// Filesystem classes the data-dir is permitted to live on.
///
/// `Supported` covers ext4, xfs, btrfs, zfs, f2fs (Linux); apfs, hfs
/// (macOS). `Unsupported(reason)` covers NFS / TMPFS / RAMFS / SMBFS /
/// WebDAV — the boot path refuses to start on these. `Unknown` is
/// treated as supported with a warning so a new local filesystem on a
/// developer's machine does not block boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsClass {
    Supported,
    Unsupported(&'static str),
    Unknown,
}

/// Resolve the data-dir per FR-019. Resolution order, first hit wins:
///
/// 1. The explicit CLI override (`--data-dir <PATH>`).
/// 2. `$STATE_DIRECTORY` set by systemd's `StateDirectory=forward-rs`.
/// 3. `$XDG_STATE_HOME/forward-rs`.
/// 4. `$HOME/.local/state/forward-rs`.
/// 5. `./forward-rs.state` (cwd fallback).
#[must_use]
pub fn resolve(opt: Option<PathBuf>) -> PathBuf {
    resolve_with_env(opt, |k| std::env::var(k).ok())
}

/// Pure variant of `resolve` that takes an env-var lookup closure.
/// Tests pass a hashmap-backed closure so they do not need to mutate
/// the process-wide environment (which requires `unsafe` in Rust
/// edition 2024 / MSRV 1.88+).
fn resolve_with_env<F>(opt: Option<PathBuf>, get: F) -> PathBuf
where
    F: Fn(&str) -> Option<String>,
{
    if let Some(p) = opt {
        return p;
    }
    if let Some(state_dir) = get("STATE_DIRECTORY")
        && !state_dir.is_empty()
    {
        return PathBuf::from(state_dir);
    }
    if let Some(xdg) = get("XDG_STATE_HOME")
        && !xdg.is_empty()
    {
        return PathBuf::from(xdg).join(APP_NAME);
    }
    if let Some(home) = get("HOME")
        && !home.is_empty()
    {
        return PathBuf::from(home).join(".local/state").join(APP_NAME);
    }
    PathBuf::from("./forward-rs.state")
}

/// Probe the filesystem class hosting `path`. Per R-008 the probe is
/// a `statfs(2)` call; on Linux we match `f_type` against well-known
/// magic numbers, on macOS we match `f_fstypename`.
///
/// `path` need not exist; we walk up the parent chain until we find
/// an extant ancestor (typically the data-dir itself or its parent).
#[cfg(target_os = "linux")]
#[must_use]
pub fn probe_fs_class(path: &Path) -> FsClass {
    use nix::sys::statfs::statfs;

    let probe_target = nearest_existing_ancestor(path);
    let Ok(stat) = statfs(probe_target.as_path()) else {
        return FsClass::Unknown;
    };

    // Magic numbers from `man 2 statfs`. `f_type` returns `FsType` in
    // nix 0.29; we compare via its inner representation.
    let magic = stat.filesystem_type().0;
    match magic {
        // Network / pseudo / volatile filesystems we refuse.
        0x6969 => FsClass::Unsupported("nfs"),               // NFS_SUPER_MAGIC
        0x517B => FsClass::Unsupported("smbfs"),             // SMB_SUPER_MAGIC
        0xFE53_4D42 => FsClass::Unsupported("smb2"),         // SMB2_MAGIC_NUMBER
        0x0102_1994 => FsClass::Unsupported("tmpfs"),        // TMPFS_MAGIC
        0x8584_58F6 => FsClass::Unsupported("ramfs"),        // RAMFS_MAGIC
        0x6573_5546 => FsClass::Unsupported("fuse"),         // FUSE_SUPER_MAGIC
        0x7375_7245 => FsClass::Unsupported("ceph"),         // CEPH_SUPER_MAGIC
        // Local supported filesystems.
        0xEF53        // EXT2/3/4
        | 0x5846_5342  // XFS
        | 0x9123_683E  // BTRFS
        | 0x2FC1_2FC1  // ZFS
        | 0xF2F5_2010  // F2FS
        | 0x4D44      // VFAT (msdos) — host iteration only
        | 0x5346_544E  // NTFS
        | 0x5265_4973  // REISERFS
        => FsClass::Supported,
        _ => FsClass::Unknown,
    }
}

#[cfg(target_os = "macos")]
#[must_use]
pub fn probe_fs_class(path: &Path) -> FsClass {
    use nix::sys::statfs::statfs;

    let probe_target = nearest_existing_ancestor(path);
    let Ok(stat) = statfs(probe_target.as_path()) else {
        return FsClass::Unknown;
    };
    let name = stat.filesystem_type_name().to_ascii_lowercase();
    match name.as_str() {
        "nfs" | "smbfs" | "webdav" | "afpfs" => FsClass::Unsupported("nfs/smb/webdav"),
        "apfs" | "hfs" | "exfat" | "msdos" => FsClass::Supported,
        _ => FsClass::Unknown,
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn probe_fs_class(_path: &Path) -> FsClass {
    // Other platforms (Windows etc.) are out of scope per Constitution.
    FsClass::Unknown
}

fn nearest_existing_ancestor(path: &Path) -> PathBuf {
    let mut cur = path.to_path_buf();
    loop {
        if cur.exists() {
            return cur;
        }
        match cur.parent() {
            Some(p) if p != cur.as_path() => cur = p.to_path_buf(),
            _ => return PathBuf::from("/"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::tempdir;

    fn env(map: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: HashMap<String, String> = map
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k: &str| owned.get(k).cloned()
    }

    #[test]
    fn explicit_override_wins() {
        let dir = tempdir().unwrap();
        let p = dir.path().to_path_buf();
        assert_eq!(resolve_with_env(Some(p.clone()), env(&[])), p);
    }

    #[test]
    fn state_directory_env_used_when_no_override() {
        assert_eq!(
            resolve_with_env(None, env(&[("STATE_DIRECTORY", "/var/lib/forward-rs")])),
            PathBuf::from("/var/lib/forward-rs")
        );
    }

    #[test]
    fn xdg_state_home_used_when_no_state_directory() {
        assert_eq!(
            resolve_with_env(None, env(&[("XDG_STATE_HOME", "/tmp/xdg")])),
            PathBuf::from("/tmp/xdg/forward-rs")
        );
    }

    #[test]
    fn home_local_state_used_when_no_xdg() {
        assert_eq!(
            resolve_with_env(None, env(&[("HOME", "/tmp/home")])),
            PathBuf::from("/tmp/home/.local/state/forward-rs")
        );
    }

    #[test]
    fn cwd_fallback_when_no_env() {
        assert_eq!(
            resolve_with_env(None, env(&[])),
            PathBuf::from("./forward-rs.state")
        );
    }

    #[test]
    fn explicit_override_beats_state_directory() {
        let dir = tempdir().unwrap();
        let p = dir.path().to_path_buf();
        assert_eq!(
            resolve_with_env(
                Some(p.clone()),
                env(&[("STATE_DIRECTORY", "/var/lib/forward-rs")])
            ),
            p
        );
    }

    #[test]
    fn state_directory_beats_xdg() {
        assert_eq!(
            resolve_with_env(
                None,
                env(&[
                    ("STATE_DIRECTORY", "/var/lib/forward-rs"),
                    ("XDG_STATE_HOME", "/tmp/xdg"),
                ])
            ),
            PathBuf::from("/var/lib/forward-rs")
        );
    }

    #[test]
    fn empty_env_var_is_skipped() {
        assert_eq!(
            resolve_with_env(
                None,
                env(&[("STATE_DIRECTORY", ""), ("XDG_STATE_HOME", "/tmp/xdg")])
            ),
            PathBuf::from("/tmp/xdg/forward-rs")
        );
    }

    #[test]
    fn fs_class_supported_on_local_dir() {
        let dir = tempdir().unwrap();
        let class = probe_fs_class(dir.path());
        // tempdir is on the host's local fs; either Supported or
        // Unknown (e.g., a less common fs the magic-number table
        // does not list). NEVER Unsupported on a normal CI host.
        assert!(
            matches!(class, FsClass::Supported | FsClass::Unknown),
            "got {class:?}"
        );
    }

    #[test]
    fn fs_class_handles_missing_path() {
        // Pass a path that does not exist; the probe should walk up
        // and either return the parent's class or Unknown — never
        // panic.
        let dir = tempdir().unwrap();
        let missing = dir.path().join("does/not/exist");
        let class = probe_fs_class(&missing);
        assert!(
            !matches!(class, FsClass::Unsupported(_)),
            "got unexpected unsupported: {class:?}"
        );
    }
}

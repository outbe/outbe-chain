//! Locked private staging directories and stale staging cleanup.

use std::{
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use eyre::{Context, Result};
use rustix::{
    fd::OwnedFd,
    fs::{flock, fstat, openat, FileType, FlockOperation, Mode, OFlags, CWD},
    io::Errno,
    process::getuid,
};

const STALE_STAGING_PREFIXES: &[&str] = &[".outbe-prepare-", ".outbe-disk-", ".outbe-vm-disk-"];

pub(super) struct LockedTempDir {
    // Field order matters: `dir` drops (`remove_dir_all`) BEFORE `_lock` releases the exclusive
    // directory `flock`, preventing `--cleanup-stale` from racing with teardown.
    dir: tempfile::TempDir,
    _lock: OwnedFd,
}

impl LockedTempDir {
    pub(super) fn path(&self) -> &Path {
        self.dir.path()
    }
}

/// Remove stale `.outbe-prepare-*`, `.outbe-disk-*`, and `.outbe-vm-disk-*` staging directories
/// inside `staging_parent` that are real directories (not symlinks) owned by the current UID and
/// NOT currently locked by a live `outbe-up` process.
///
/// Acquires an exclusive `flock` on `staging_parent` during the scan (serialized against the brief
/// shared `flock` held while live processes create and lock new staging dirs) and probes each
/// candidate directory with `NonBlockingLockExclusive` so live staging directories are never
/// deleted.
///
/// Only intended to be called when the operator explicitly passes `--cleanup-stale` (for example
/// after an uncatchable `SIGKILL` or host power loss).
pub fn cleanup_stale_staging_dirs(staging_parent: &Path) -> Result<Vec<PathBuf>> {
    let parent_fd = match openat(
        CWD,
        staging_parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => fd,
        Err(Errno::NOENT) => return Ok(Vec::new()),
        Err(err) => {
            return Err(io::Error::from(err)).wrap_err_with(|| {
                format!(
                    "failed to open staging directory parent {}",
                    staging_parent.display()
                )
            });
        }
    };
    flock(&parent_fd, FlockOperation::LockExclusive)
        .map_err(io::Error::from)
        .wrap_err_with(|| {
            format!(
                "failed to lock staging directory parent {}",
                staging_parent.display()
            )
        })?;

    let entries = fs::read_dir(staging_parent).wrap_err_with(|| {
        format!(
            "failed to read staging directory parent {}",
            staging_parent.display()
        )
    })?;

    let my_uid = getuid().as_raw();
    let mut removed = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        if !STALE_STAGING_PREFIXES
            .iter()
            .any(|prefix| name_str.starts_with(prefix))
        {
            continue;
        }
        let path = entry.path();
        let cand_fd = match openat(
            CWD,
            &path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(Errno::NOENT | Errno::NOTDIR | Errno::LOOP) => continue,
            Err(err) => {
                return Err(io::Error::from(err)).wrap_err_with(|| {
                    format!("failed to open staging candidate {}", path.display())
                });
            }
        };
        let st = fstat(&cand_fd)
            .map_err(io::Error::from)
            .wrap_err_with(|| format!("failed to stat {}", path.display()))?;
        if FileType::from_raw_mode(st.st_mode) != FileType::Directory || st.st_uid != my_uid {
            continue;
        }
        match flock(&cand_fd, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(Errno::WOULDBLOCK) => {
                // Held exclusively by a live `outbe-up` process; never touch it.
                continue;
            }
            Err(err) => {
                return Err(io::Error::from(err))
                    .wrap_err_with(|| format!("failed to probe lock on {}", path.display()));
            }
        }
        fs::remove_dir_all(&path)
            .wrap_err_with(|| format!("failed to remove stale staging dir {}", path.display()))?;
        drop(cand_fd);
        removed.push(path);
    }
    drop(parent_fd);
    Ok(removed)
}

pub(super) fn create_locked_staging_dir(parent: &Path, prefix: &str) -> Result<LockedTempDir> {
    fs::create_dir_all(parent)
        .wrap_err_with(|| format!("failed to create staging parent {}", parent.display()))?;
    let parent_fd = openat(
        CWD,
        parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)
    .wrap_err_with(|| format!("failed to open staging parent {}", parent.display()))?;
    flock(&parent_fd, FlockOperation::LockShared)
        .map_err(io::Error::from)
        .wrap_err_with(|| format!("failed to lock staging parent {}", parent.display()))?;

    let dir = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(parent)
        .wrap_err_with(|| format!("failed to create staging tempdir in {}", parent.display()))?;
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700))?;
    let lock = openat(
        CWD,
        dir.path(),
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)
    .wrap_err_with(|| {
        format!(
            "failed to open staging dir {} for flock",
            dir.path().display()
        )
    })?;
    flock(&lock, FlockOperation::LockExclusive)
        .map_err(io::Error::from)
        .wrap_err_with(|| format!("failed to flock staging dir {}", dir.path().display()))?;
    drop(parent_fd);
    Ok(LockedTempDir { dir, _lock: lock })
}

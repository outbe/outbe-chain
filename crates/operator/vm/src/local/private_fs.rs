//! Ownership and permission checks for private state paths (`0700` dirs, `0600` files).
//!
//! Every check refuses symlinks and paths that another uid owns. Unsafe group/other mode bits are
//! fixed only when the caller passes `fix_permissions`.

use std::{
    fs,
    io::ErrorKind,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::Path,
};

use eyre::{bail, ensure, Context, Result};

const PRIVATE_DIR_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;

pub(super) fn current_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// Return `lstat` metadata, or `None` when the path does not exist.
pub(super) fn optional_symlink_metadata(path: &Path, label: &str) -> Result<Option<fs::Metadata>> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(Some(meta)),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).wrap_err_with(|| format!("stat {label} {}", path.display())),
    }
}

/// Create a private state directory, or check an existing one.
pub(super) fn ensure_private_dir(path: &Path, fix_permissions: bool) -> Result<()> {
    if let Some(meta) = optional_symlink_metadata(path, "state directory")? {
        return check_private_dir(path, &meta, fix_permissions, "state directory");
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(PRIVATE_DIR_MODE)
        .create(path)
        .wrap_err_with(|| format!("create state directory {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(PRIVATE_DIR_MODE))
        .wrap_err_with(|| format!("set 0700 mode on {}", path.display()))
}

/// Check a private directory that must already exist.
pub(super) fn verify_existing_private_dir(
    path: &Path,
    fix_permissions: bool,
    label: &str,
) -> Result<()> {
    let meta = fs::symlink_metadata(path)
        .wrap_err_with(|| format!("{label} does not exist: {}", path.display()))?;
    check_private_dir(path, &meta, fix_permissions, label)
}

pub(super) fn ensure_private_regular_file(
    path: &Path,
    meta: &fs::Metadata,
    current_uid: u32,
    fix_permissions: bool,
    label: &str,
) -> Result<()> {
    ensure!(
        meta.file_type().is_file(),
        "refusing non-regular {label}: {}",
        path.display()
    );
    ensure_owned_by(path, meta, current_uid, label)?;
    enforce_private_mode(path, meta, PRIVATE_FILE_MODE, fix_permissions, label)
}

fn check_private_dir(
    path: &Path,
    meta: &fs::Metadata,
    fix_permissions: bool,
    label: &str,
) -> Result<()> {
    ensure!(
        meta.file_type().is_dir(),
        "{label} path is not a real directory: {}",
        path.display()
    );
    ensure_owned_by(path, meta, current_uid(), label)?;
    enforce_private_mode(path, meta, PRIVATE_DIR_MODE, fix_permissions, label)
}

fn ensure_owned_by(path: &Path, meta: &fs::Metadata, current_uid: u32, label: &str) -> Result<()> {
    ensure!(
        meta.uid() == current_uid,
        "{label} {} is owned by uid {}, expected current uid {current_uid}",
        path.display(),
        meta.uid()
    );
    Ok(())
}

/// Refuse group/other or special mode bits, or reset them to `wanted` when `fix_permissions`.
fn enforce_private_mode(
    path: &Path,
    meta: &fs::Metadata,
    wanted: u32,
    fix_permissions: bool,
    label: &str,
) -> Result<()> {
    let mode = meta.permissions().mode() & 0o7777;
    if mode & 0o7077 == 0 {
        return Ok(());
    }
    if !fix_permissions {
        bail!(
            "refusing {label} {} with unsafe mode {:04o} (expected {wanted:04o}, no group/other access); run `chmod {wanted:04o} {}` or pass --fix-permissions",
            path.display(),
            mode,
            path.display()
        );
    }
    fs::set_permissions(path, fs::Permissions::from_mode(wanted))
        .wrap_err_with(|| format!("chmod {wanted:04o} {}", path.display()))
}

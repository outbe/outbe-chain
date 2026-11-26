//! Static scan of the data tree: no links, devices, or executable static files.

use std::{
    fs::{self, FileType, Metadata},
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::Path,
};

use eyre::{bail, ensure, Context, Result};
use rustix::fs::CWD;
use walkdir::WalkDir;

use super::fd_ops::{open_dir_nofollow, remove_stale_socket};

const STALE_SOCKETS: &[&str] = &[
    "keys/radicle/node/outbe-control.sock",
    "validator-0/data/reth.ipc",
];

/// Large service datadir skipped during recursive walk; kernel mount flags enforce safety inside.
const SKIPPED_STATE_SUBTREE: &str = "validator-0/data";

/// Service-state subtrees where daemons write runtime files (sockets, fifos, exec bits allowed).
const SERVICE_STATE_ROOTS: &[&str] = &[
    "validator-0",
    "state",
    "keys/radicle/storage",
    "keys/radicle/node",
    "keys/radicle/cobs",
];

pub(super) fn verify_tree(root: &Path) -> Result<()> {
    let root_fd = open_dir_nofollow(CWD, root)?;
    let skipped = root.join(SKIPPED_STATE_SUBTREE);

    // Check the skipped datadir itself is not a symlink or device node, then clean stale sockets.
    verify_skipped_subtree_root(&skipped)?;
    for relative in STALE_SOCKETS {
        remove_stale_socket(&root_fd, root, Path::new(relative))?;
    }

    let mut iter = WalkDir::new(root).follow_links(false).into_iter();
    while let Some(entry) = iter.next() {
        let entry =
            entry.wrap_err_with(|| format!("failed to scan data tree under {}", root.display()))?;
        let path = entry.path();
        if path == root {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .wrap_err_with(|| format!("path escaped data root: {}", path.display()))?;
        let metadata = fs::symlink_metadata(path)
            .wrap_err_with(|| format!("failed to stat {}", path.display()))?;
        reject_link_or_device(path, metadata.file_type())?;

        if path == skipped {
            // Do not descend into the large reth datadir.
            iter.skip_current_dir();
            continue;
        }
        if !is_service_state(relative) {
            verify_static_entry(path, &metadata)?;
        }
    }
    Ok(())
}

fn verify_skipped_subtree_root(skipped: &Path) -> Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(skipped) {
        let ft = metadata.file_type();
        ensure!(
            ft.is_dir() && !ft.is_symlink(),
            "refusing non-directory service state root: {}",
            skipped.display()
        );
    }
    Ok(())
}

/// Refuse symlinks and device nodes anywhere in the data tree.
fn reject_link_or_device(path: &Path, file_type: FileType) -> Result<()> {
    if file_type.is_symlink() {
        bail!("refusing symlink in data tree: {}", path.display());
    }
    if file_type.is_block_device() {
        bail!("refusing block device in data tree: {}", path.display());
    }
    if file_type.is_char_device() {
        bail!("refusing character device in data tree: {}", path.display());
    }
    Ok(())
}

/// Static inputs must contain only non-executable regular files and directories.
fn verify_static_entry(path: &Path, metadata: &Metadata) -> Result<()> {
    let file_type = metadata.file_type();
    if file_type.is_dir() {
        return Ok(());
    }
    if file_type.is_socket() {
        bail!("refusing socket in static data tree: {}", path.display());
    }
    if file_type.is_fifo() {
        bail!("refusing fifo in static data tree: {}", path.display());
    }
    if !file_type.is_file() {
        bail!(
            "refusing unsupported file type in data tree: {}",
            path.display()
        );
    }
    let mode = metadata.mode() & 0o7777;
    ensure!(
        mode & 0o111 == 0,
        "refusing executable file on data disk (mode {mode:04o}): {}",
        path.display()
    );
    ensure!(
        mode & 0o6000 == 0,
        "refusing setuid/setgid file on data disk (mode {mode:04o}): {}",
        path.display()
    );
    Ok(())
}

fn is_service_state(relative: &Path) -> bool {
    SERVICE_STATE_ROOTS
        .iter()
        .any(|prefix| relative.starts_with(prefix))
}

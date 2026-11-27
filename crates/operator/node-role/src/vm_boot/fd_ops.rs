//! Symlink-safe directory and file operations relative to an open data-root descriptor.

use std::{
    io,
    path::{Component, Path},
};

use eyre::{bail, ensure, Context, Result};
use rustix::{
    fd::{AsFd, OwnedFd},
    fs::{
        fchmod, fchown, fstat, mkdirat, openat, statat, unlinkat, AtFlags, FileType, Mode, OFlags,
    },
    io::Errno,
    process::{Gid, Uid},
};

pub(super) fn open_dir_nofollow(parent: impl AsFd, path: &Path) -> Result<OwnedFd> {
    openat(
        parent,
        path,
        OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)
    .wrap_err_with(|| format!("refusing symlink or non-directory: {}", path.display()))
}

pub(super) fn ensure_dir_nofollow(
    root_fd: &OwnedFd,
    root: &Path,
    relative: &Path,
    mode: u32,
    ownership: Option<(u32, u32)>,
) -> Result<()> {
    let mut current_fd = open_dir_nofollow(root_fd, Path::new("."))?;
    let mut current_path = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            bail!("invalid relative directory path: {}", relative.display());
        };
        current_path.push(name);
        match mkdirat(&current_fd, name, Mode::from_raw_mode(mode as _)) {
            Ok(()) | Err(Errno::EXIST) => {}
            Err(error) => {
                return Err(io::Error::from(error)).wrap_err_with(|| {
                    format!("failed to create directory: {}", current_path.display())
                });
            }
        }
        current_fd = open_dir_nofollow(&current_fd, Path::new(name)).wrap_err_with(|| {
            format!(
                "refusing symlink or non-directory at {}",
                current_path.display()
            )
        })?;
    }
    apply_fd_attrs(&current_fd, &current_path, mode, ownership)
}

pub(super) fn normalize_file_nofollow(
    root_fd: &OwnedFd,
    root: &Path,
    relative: &Path,
    mode: u32,
    ownership: Option<(u32, u32)>,
) -> Result<()> {
    let full_path = root.join(relative);
    let parent_rel = relative.parent().unwrap_or_else(|| Path::new(""));
    let file_name = relative
        .file_name()
        .ok_or_else(|| eyre::eyre!("invalid file path: {}", full_path.display()))?;
    let parent_fd = open_relative_dir_nofollow(root_fd, root, parent_rel)?;
    let fd = openat(
        &parent_fd,
        file_name,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(io::Error::from)
    .wrap_err_with(|| {
        format!(
            "refusing symlink or unreadable file: {}",
            full_path.display()
        )
    })?;
    let stat = fstat(&fd)
        .map_err(io::Error::from)
        .wrap_err_with(|| format!("failed to stat {}", full_path.display()))?;
    ensure!(
        FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile,
        "expected regular file: {}",
        full_path.display()
    );
    apply_fd_attrs(&fd, &full_path, mode, ownership)
}

fn open_relative_dir_nofollow(root_fd: &OwnedFd, root: &Path, relative: &Path) -> Result<OwnedFd> {
    let mut current_fd = open_dir_nofollow(root_fd, Path::new("."))?;
    let mut current_path = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            continue;
        };
        current_path.push(name);
        current_fd = open_dir_nofollow(&current_fd, Path::new(name)).wrap_err_with(|| {
            format!(
                "refusing symlink or non-directory at {}",
                current_path.display()
            )
        })?;
    }
    Ok(current_fd)
}

pub(super) fn remove_stale_socket(root_fd: &OwnedFd, root: &Path, relative: &Path) -> Result<()> {
    let full_path = root.join(relative);
    let parent_rel = relative.parent().unwrap_or_else(|| Path::new(""));
    let file_name = relative
        .file_name()
        .ok_or_else(|| eyre::eyre!("invalid socket path: {}", full_path.display()))?;
    let Ok(parent_fd) = open_relative_dir_nofollow(root_fd, root, parent_rel) else {
        return Ok(());
    };
    let stat = match statat(&parent_fd, file_name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => stat,
        Err(Errno::NOENT) => return Ok(()),
        Err(error) => {
            return Err(io::Error::from(error))
                .wrap_err_with(|| format!("failed to stat {}", full_path.display()));
        }
    };
    if FileType::from_raw_mode(stat.st_mode) == FileType::Socket {
        unlinkat(&parent_fd, file_name, AtFlags::empty())
            .map_err(io::Error::from)
            .wrap_err_with(|| format!("failed to remove stale socket: {}", full_path.display()))?;
    }
    Ok(())
}

fn apply_fd_attrs(
    fd: &OwnedFd,
    path: &Path,
    mode: u32,
    ownership: Option<(u32, u32)>,
) -> Result<()> {
    fchmod(fd, Mode::from_raw_mode(mode as _))
        .map_err(io::Error::from)
        .wrap_err_with(|| format!("failed to chmod {}", path.display()))?;
    if let Some((uid, gid)) = ownership {
        fchown(fd, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid)))
            .map_err(io::Error::from)
            .wrap_err_with(|| format!("failed to chown {}", path.display()))?;
    }
    Ok(())
}

//! Cancellable copy of a validated guest image tree into a staging directory.

use std::{
    fs,
    io::{Read, Write},
    path::Path,
    sync::atomic::AtomicBool,
};

use eyre::{bail, ensure, Context, Result};
use walkdir::WalkDir;

use super::lock::check_image_canceled;

const COPY_BUFFER_BYTES: usize = 64 * 1024;

/// Copy `src_root` into `dst_root`, keep permissions, and refuse symlinks and special files.
pub(super) fn copy_image_tree_cancellable(
    src_root: &Path,
    dst_root: &Path,
    cancel: Option<&AtomicBool>,
) -> Result<()> {
    let mut buf = [0_u8; COPY_BUFFER_BYTES];
    for entry in WalkDir::new(src_root).follow_links(false) {
        check_image_canceled(cancel, "staging image tree")?;
        let entry = entry?;
        let path = entry.path();
        if path == src_root {
            continue;
        }
        let dst = dst_root.join(path.strip_prefix(src_root)?);
        copy_image_entry(path, &dst, &mut buf, cancel)?;
    }
    Ok(())
}

fn copy_image_entry(
    path: &Path,
    dst: &Path,
    buf: &mut [u8],
    cancel: Option<&AtomicBool>,
) -> Result<()> {
    let meta = fs::symlink_metadata(path)
        .wrap_err_with(|| format!("stat guest image entry {}", path.display()))?;
    let ft = meta.file_type();
    ensure!(
        !ft.is_symlink(),
        "refusing symlink in guest image directory: {}",
        path.display()
    );
    if ft.is_dir() {
        fs::create_dir_all(dst).wrap_err_with(|| format!("create directory {}", dst.display()))?;
    } else if ft.is_file() {
        copy_file_cancellable(path, dst, buf, cancel)?;
    } else {
        bail!(
            "refusing non-regular entry in guest image directory: {}",
            path.display()
        );
    }
    fs::set_permissions(dst, meta.permissions())?;
    Ok(())
}

fn copy_file_cancellable(
    path: &Path,
    dst: &Path,
    buf: &mut [u8],
    cancel: Option<&AtomicBool>,
) -> Result<()> {
    let mut reader = fs::File::open(path)
        .wrap_err_with(|| format!("open {} for staging copy", path.display()))?;
    let mut writer = fs::File::create(dst)
        .wrap_err_with(|| format!("create {} for staging copy", dst.display()))?;
    loop {
        check_image_canceled(cancel, "copying staged image file")?;
        let n = reader
            .read(buf)
            .wrap_err_with(|| format!("read {}", path.display()))?;
        if n == 0 {
            break;
        }
        writer
            .write_all(&buf[..n])
            .wrap_err_with(|| format!("write {}", dst.display()))?;
    }
    writer.flush()?;
    Ok(())
}

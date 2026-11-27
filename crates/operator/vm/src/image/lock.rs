//! Exclusive per-image `flock` on `<vmm_home>/image/.<name>.lock` and cooperative cancellation.

use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

use eyre::{bail, ensure, Context, Result};
use rustix::{
    fs::{flock, openat, FlockOperation, Mode, OFlags, CWD},
    io::Errno,
};

const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// Exclusive `flock` guard for `<vmm_home>/image/.<name>.lock`.
///
/// Released automatically when dropped.
#[derive(Debug)]
pub struct ImageLockGuard {
    _file: fs::File,
}

pub(super) fn check_image_canceled(cancel: Option<&AtomicBool>, stage: &str) -> Result<()> {
    if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        bail!("image operation canceled by signal while {stage}");
    }
    Ok(())
}

pub(super) fn acquire_image_lock(
    image_root: &Path,
    name: &str,
    deadline: Option<Instant>,
    cancel: Option<&AtomicBool>,
) -> Result<ImageLockGuard> {
    check_image_canceled(cancel, "acquiring image lock")?;
    let lock_path = image_root.join(format!(".{name}.lock"));
    let lock_file = open_owned_lock_file(&lock_path)?;

    if deadline.is_none() && cancel.is_none() {
        flock(&lock_file, FlockOperation::LockExclusive)
            .wrap_err_with(|| format!("lock {}", lock_path.display()))?;
    } else {
        poll_exclusive_lock(&lock_file, &lock_path, deadline, cancel)?;
    }
    Ok(ImageLockGuard { _file: lock_file })
}

/// Open (or create) the lock file without following symlinks and check that the current uid owns
/// a regular file both before and after the open.
fn open_owned_lock_file(lock_path: &Path) -> Result<fs::File> {
    let current_uid = rustix::process::getuid().as_raw();
    if let Ok(meta) = fs::symlink_metadata(lock_path) {
        ensure_owned_regular_lock(lock_path, &meta, current_uid)?;
    }
    let lock_fd = openat(
        CWD,
        lock_path,
        OFlags::CREATE | OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )
    .wrap_err_with(|| format!("open image lock {}", lock_path.display()))?;
    let lock_file = fs::File::from(lock_fd);
    let lock_meta = lock_file
        .metadata()
        .wrap_err_with(|| format!("stat image lock {}", lock_path.display()))?;
    ensure_owned_regular_lock(lock_path, &lock_meta, current_uid)?;
    Ok(lock_file)
}

fn ensure_owned_regular_lock(
    lock_path: &Path,
    meta: &fs::Metadata,
    current_uid: u32,
) -> Result<()> {
    ensure!(
        meta.file_type().is_file() && meta.uid() == current_uid,
        "image lock must be a regular file owned by current uid: {}",
        lock_path.display()
    );
    Ok(())
}

/// Try the non-blocking lock until it succeeds, the deadline passes, or `cancel` is set.
fn poll_exclusive_lock(
    lock_file: &fs::File,
    lock_path: &Path,
    deadline: Option<Instant>,
    cancel: Option<&AtomicBool>,
) -> Result<()> {
    loop {
        check_image_canceled(cancel, "waiting for image lock")?;
        match flock(lock_file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(()),
            Err(err) if err == Errno::WOULDBLOCK || err == Errno::AGAIN => {
                sleep_until_next_try(lock_path, deadline)?;
            }
            Err(err) => {
                return Err(err).wrap_err_with(|| format!("lock {}", lock_path.display()));
            }
        }
    }
}

/// Sleep one poll interval, clipped to the deadline, or fail when the deadline has passed.
fn sleep_until_next_try(lock_path: &Path, deadline: Option<Instant>) -> Result<()> {
    let Some(dl) = deadline else {
        thread::sleep(LOCK_POLL_INTERVAL);
        return Ok(());
    };
    let now = Instant::now();
    if now >= dl {
        bail!(
            "timed out waiting for exclusive image lock {}",
            lock_path.display()
        );
    }
    thread::sleep(LOCK_POLL_INTERVAL.min(dl.saturating_duration_since(now)));
    Ok(())
}

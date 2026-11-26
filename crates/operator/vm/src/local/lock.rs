//! Exclusive `O_CLOEXEC` `flock` guards for per-VM state and per-name disk staging.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    thread,
    time::Instant,
};

use eyre::{bail, Context, Result};
use rustix::{
    fs::{flock, openat, FlockOperation, Mode, OFlags, CWD},
    io::Errno,
};

use super::{
    check_canceled,
    private_fs::{current_uid, ensure_private_regular_file},
    validate_vm_name, LocalVmStore, LOCK_POLL_INTERVAL,
};

/// Exclusive per-VM file lock held for the lifetime of one state transition.
///
/// Opened with `O_CLOEXEC` so child subprocesses (such as `qemu-img`) never inherit the lock
/// descriptor.
pub struct VmLockGuard {
    _file: fs::File,
}

/// Exclusive per-staging-name file lock (`O_CLOEXEC`) serializing `disk prepare --staging-name`,
/// `vm create --disk-staging --consume-staging`, and `disk clean --staging-name`.
pub struct StagingLockGuard {
    _file: fs::File,
}

/// Which lock family a named lock belongs to. It selects the lock directory and error texts.
#[derive(Clone, Copy)]
enum LockKind {
    /// `<state-dir>/vms/.<name>.lock`.
    Vm,
    /// `<state-dir>/staging/.<staging_name>.lock`.
    Staging,
}

impl LockKind {
    /// Label for the ownership and mode checks of the lock file.
    fn file_label(self) -> &'static str {
        match self {
            Self::Vm => "VM state lock",
            Self::Staging => "disk staging lock",
        }
    }

    /// Label for the open and stat errors of the lock file.
    fn handle_label(self) -> &'static str {
        match self {
            Self::Vm => "VM lock",
            Self::Staging => "disk staging lock",
        }
    }

    fn acquiring_step(self, name: &str) -> String {
        match self {
            Self::Vm => format!("acquiring lock for VM `{name}`"),
            Self::Staging => format!("acquiring staging lock for `{name}`"),
        }
    }

    fn waiting_step(self, name: &str) -> String {
        match self {
            Self::Vm => format!("waiting for VM `{name}` lock"),
            Self::Staging => format!("waiting for staging lock `{name}`"),
        }
    }

    fn timeout_message(self, path: &Path, name: &str) -> String {
        match self {
            Self::Vm => format!(
                "timed out waiting for exclusive lock on {} for VM `{name}`",
                path.display()
            ),
            Self::Staging => format!(
                "timed out waiting for exclusive staging lock on {} for `{name}`",
                path.display()
            ),
        }
    }
}

/// One named lock to take, with its wait limits.
struct LockRequest<'a> {
    kind: LockKind,
    name: &'a str,
    path: PathBuf,
    deadline: Option<Instant>,
    cancel: Option<&'a AtomicBool>,
}

impl LocalVmStore {
    /// Acquire an exclusive `O_CLOEXEC` `flock` on `<state-dir>/vms/.<name>.lock`.
    pub fn lock_vm(&self, name: &str) -> Result<VmLockGuard> {
        self.lock_vm_with_options(name, None, None)
    }

    /// Acquire an exclusive `O_CLOEXEC` `flock` on `<state-dir>/vms/.<name>.lock` using a
    /// non-blocking try-lock loop that honors both `deadline` and `cancel`.
    pub fn lock_vm_with_options(
        &self,
        name: &str,
        deadline: Option<Instant>,
        cancel: Option<&AtomicBool>,
    ) -> Result<VmLockGuard> {
        let file = self.lock_named(LockKind::Vm, name, deadline, cancel)?;
        Ok(VmLockGuard { _file: file })
    }

    /// Acquire an exclusive `O_CLOEXEC` `flock` on `<state-dir>/staging/.<staging_name>.lock`.
    pub fn lock_staging(&self, staging_name: &str) -> Result<StagingLockGuard> {
        self.lock_staging_with_options(staging_name, None, None)
    }

    /// Acquire an exclusive `O_CLOEXEC` `flock` on `<state-dir>/staging/.<staging_name>.lock`
    /// using a non-blocking try-lock loop that honors both `deadline` and `cancel`.
    pub fn lock_staging_with_options(
        &self,
        staging_name: &str,
        deadline: Option<Instant>,
        cancel: Option<&AtomicBool>,
    ) -> Result<StagingLockGuard> {
        let file = self.lock_named(LockKind::Staging, staging_name, deadline, cancel)?;
        Ok(StagingLockGuard { _file: file })
    }

    /// Create the private lock directory of `kind`, then open, check, and lock `.<name>.lock`.
    fn lock_named(
        &self,
        kind: LockKind,
        name: &str,
        deadline: Option<Instant>,
        cancel: Option<&AtomicBool>,
    ) -> Result<fs::File> {
        validate_vm_name(name)?;
        check_canceled(cancel, &kind.acquiring_step(name))?;
        let lock_dir = match kind {
            LockKind::Vm => {
                self.ensure_dirs()?;
                self.vms_dir()
            }
            LockKind::Staging => self.ensure_staging_root()?,
        };
        let request = LockRequest {
            kind,
            name,
            path: lock_dir.join(format!(".{name}.lock")),
            deadline,
            cancel,
        };
        let file = self.open_private_lock_file(&request)?;
        acquire_exclusive(&file, &request)?;
        Ok(file)
    }

    /// Open (or create) the lock file without following symlinks and check it before and after
    /// the open.
    fn open_private_lock_file(&self, request: &LockRequest<'_>) -> Result<fs::File> {
        let path = request.path.as_path();
        let label = request.kind.file_label();
        let uid = current_uid();
        if let Ok(meta) = fs::symlink_metadata(path) {
            ensure_private_regular_file(path, &meta, uid, self.fix_permissions, label)?;
        }
        let handle_label = request.kind.handle_label();
        let fd = openat(
            CWD,
            path,
            OFlags::CREATE | OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .wrap_err_with(|| format!("open {handle_label} {}", path.display()))?;
        let file = fs::File::from(fd);
        let meta = file
            .metadata()
            .wrap_err_with(|| format!("stat {handle_label} {}", path.display()))?;
        ensure_private_regular_file(path, &meta, uid, self.fix_permissions, label)?;
        Ok(file)
    }
}

/// Take the exclusive lock: block when neither a deadline nor a cancel flag is given, else try
/// without blocking until the lock is free, the deadline passes, or `cancel` is set.
fn acquire_exclusive(file: &fs::File, request: &LockRequest<'_>) -> Result<()> {
    let lock_error = || format!("acquire exclusive lock on {}", request.path.display());
    if request.deadline.is_none() && request.cancel.is_none() {
        return flock(file, FlockOperation::LockExclusive).wrap_err_with(lock_error);
    }
    let waiting_step = request.kind.waiting_step(request.name);
    loop {
        check_canceled(request.cancel, &waiting_step)?;
        match flock(file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => return Ok(()),
            Err(Errno::WOULDBLOCK) => sleep_until_next_try(request)?,
            Err(err) => return Err(err).wrap_err_with(lock_error),
        }
    }
}

/// Sleep one poll interval, clipped to the deadline, or fail when the deadline has passed.
fn sleep_until_next_try(request: &LockRequest<'_>) -> Result<()> {
    let Some(deadline) = request.deadline else {
        thread::sleep(LOCK_POLL_INTERVAL);
        return Ok(());
    };
    let now = Instant::now();
    if now >= deadline {
        bail!(
            "{}",
            request.kind.timeout_message(&request.path, request.name)
        );
    }
    thread::sleep(
        deadline
            .saturating_duration_since(now)
            .min(LOCK_POLL_INTERVAL),
    );
    Ok(())
}

//! Single-VM local lifecycle management over the local dstack VMM Unix socket.
//!
//! Stores one versioned state file per VM under `${XDG_STATE_HOME:-$HOME/.local/state}/outbe-up/vms/`
//! (`0700` dirs, `0600` files), protected by an `O_CLOEXEC` per-VM exclusive `flock` and atomic
//! fsync+rename journaling before every fallible step (including a durable `Creating` intent with a
//! client-chosen VMM annotation token written before `CreateVm`). Only VMs recorded in our state
//! store are ever started, stopped, inspected, or removed.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    net::Ipv4Addr,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use eyre::{bail, ensure, Context, Result};
use rustix::fs::{flock, openat, FlockOperation, Mode, OFlags, CWD};
use serde::{Deserialize, Serialize};

use crate::{
    disk::{sha256_sparse_file, stage_local_disk_cancellable},
    dnsmasq::lookup_lease_ip_by_mac,
    image::publish_local_image_locked_cancellable,
    nat::validate_iface_name,
    vmm::{
        calc_app_id, extract_primary_mac, validate_vm_id, CreateVmRequest, NetworkingConfig,
        StatusRequest, VmInfo, VmmClient, VmmTransport, APP_COMPOSE_JSON,
    },
};

pub const LOCAL_VM_STATE_VERSION: u32 = 1;
pub const DEFAULT_LOCAL_VCPU: u32 = 3;
pub const DEFAULT_LOCAL_MEMORY_GIB: u32 = 5;
pub const DEFAULT_LOCAL_DISK_GIB: u32 = 40;
pub const DEFAULT_LOCAL_FLAVOR: &str = "dev";
pub const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// Default grace window when polling `Status` for an in-flight `Creating` VM before refusing.
pub const DEFAULT_CREATE_RECOVERY_GRACE: Duration = Duration::from_secs(30);

const STATUS_DISCOVERY_PAGE_SIZE: u32 = 100;
const MAX_STATUS_DISCOVERY_PAGES: u32 = 100;
const LOCK_POLL_INTERVAL: Duration = Duration::from_millis(25);

/// VMM manifest annotation key carrying our unique client-chosen recovery marker.
///
/// Security note: this token is a bearer marker stored in the VMM manifest and readable via
/// `GetInfo`/`Status` by any client authorized to access the VMM Unix socket — it carries the
/// same trust level as the VMM socket itself, not cryptographic proof of origin against another
/// VMM-authorized peer. Adoption therefore also verifies `name`, `image`, `vcpu`, `memory`, and
/// `app_id` against our local journal.
pub const CREATE_TOKEN_ANNOTATION: &str = "outbe-up.create-token";

/// VMM manifest annotation key carrying an optional caller-assigned ownership tag
/// (e.g. `outbe-net:<network>:<genesis8>:<member>`).
pub const OWNER_TAG_ANNOTATION: &str = "outbe-up.owner-tag";

/// Warning displayed when `--force` bypasses graceful guest shutdown (`ShutdownVm`).
pub const FORCE_STOP_DKG_WARNING: &str = "WARNING: forced VM stop (--force) bypasses graceful guest shutdown and can lose a pending DKG boundary snapshot (see /home/ubuntu/tdx/12-defect-pending-dkg-snapshot.md).";

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LocalVmPhase {
    /// Durable create intent written BEFORE calling `CreateVm`; carries `create_token` for
    /// ownership-safe recovery if the process crashes or the `CreateVm` response is lost.
    Creating,
    /// `CreateVm` succeeded and `vm_id` is journaled; MAC/disk not yet staged.
    Created,
    /// `GetInfo` MAC recorded; data disk substitution pending.
    MacRecorded,
    /// Data disk converted and staged to `hda.img`.
    DiskStaged,
    /// Provisioning completed; VM is ready to start.
    #[default]
    Ready,
    /// Removal requested; waiting for asynchronous VMM removal before deleting state.
    Removing,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JournalEntry {
    pub timestamp_unix_secs: u64,
    pub step: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LocalVmState {
    pub version: u32,
    pub name: String,
    #[serde(default)]
    pub vm_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_tag: Option<String>,
    pub vmm_socket: PathBuf,
    pub vmm_home: PathBuf,
    pub image_name: String,
    pub os_image_hash: String,
    pub disk_source: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_source_sha256: Option<String>,
    pub vcpu: u32,
    pub memory_gib: u32,
    pub disk_gib: u32,
    pub bridge: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intended_ip: Option<Ipv4Addr>,
    #[serde(default)]
    pub mac: String,
    pub app_id: String,
    pub no_tee: bool,
    pub phase: LocalVmPhase,
    #[serde(default)]
    pub journal: Vec<JournalEntry>,
}

impl LocalVmState {
    pub(crate) fn record_step(&mut self, step: impl Into<String>, detail: Option<String>) {
        let timestamp_unix_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.journal.push(JournalEntry {
            timestamp_unix_secs,
            step: step.into(),
            detail,
        });
    }

    fn validate_identity(&self, path: &Path) -> Result<()> {
        validate_vm_name(&self.name)?;
        if let Some(tag) = &self.owner_tag {
            validate_owner_tag(tag).wrap_err_with(|| {
                format!("VM state file {} has invalid owner_tag", path.display())
            })?;
        }
        if self.phase == LocalVmPhase::Creating {
            if !self.vm_id.is_empty() {
                validate_vm_id(&self.vm_id).wrap_err_with(|| {
                    format!("VM state file {} has invalid vm_id", path.display())
                })?;
            }
            ensure!(
                self.create_token
                    .as_deref()
                    .is_some_and(|t| t.len() >= 16 && t.bytes().all(|b| b.is_ascii_hexdigit())),
                "VM state file {} in `creating` phase is missing a valid create_token",
                path.display()
            );
        } else {
            validate_vm_id(&self.vm_id)
                .wrap_err_with(|| format!("VM state file {} has invalid vm_id", path.display()))?;
        }
        Ok(())
    }
}

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

/// Metadata for one entry under `<state-dir>/staging/`.
///
/// Valid entries populate `path`, `sha256`, and `size_bytes` with `error: None`.
/// Malformed or unsafe entries populate `error` so one bad entry never aborts listing valid siblings.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StagedDiskInfo {
    pub staging_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Result of removing one staged disk directory under `<state-dir>/staging/<staging_name>`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CleanStagingOutput {
    pub staging_name: String,
    pub path: PathBuf,
    pub removed: bool,
}

/// Per-VM state store rooted at `${XDG_STATE_HOME:-$HOME/.local/state}/outbe-up`.
#[derive(Clone, Debug)]
pub struct LocalVmStore {
    root: PathBuf,
    fix_permissions: bool,
}

impl LocalVmStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            fix_permissions: false,
        }
    }

    /// Allow tightening unsafe permissions (`0700` on dirs, `0600` on files) on preexisting
    /// state paths owned by the current user. Foreign-owned or symlink paths are always refused.
    pub fn with_fix_permissions(mut self, fix_permissions: bool) -> Self {
        self.fix_permissions = fix_permissions;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn vms_dir(&self) -> PathBuf {
        self.root.join("vms")
    }

    pub fn state_path(&self, name: &str) -> Result<PathBuf> {
        validate_vm_name(name)?;
        Ok(self.vms_dir().join(format!("{name}.json")))
    }

    fn lock_path(&self, name: &str) -> Result<PathBuf> {
        validate_vm_name(name)?;
        Ok(self.vms_dir().join(format!(".{name}.lock")))
    }

    fn ensure_dirs(&self) -> Result<()> {
        ensure_private_dir(&self.root, self.fix_permissions)?;
        ensure_private_dir(&self.vms_dir(), self.fix_permissions)?;
        Ok(())
    }

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
        validate_vm_name(name)?;
        check_canceled(cancel, &format!("acquiring lock for VM `{name}`"))?;
        self.ensure_dirs()?;
        let lock_path = self.lock_path(name)?;
        let current_uid = rustix::process::getuid().as_raw();
        if let Ok(meta) = fs::symlink_metadata(&lock_path) {
            ensure_private_regular_file(
                &lock_path,
                &meta,
                current_uid,
                self.fix_permissions,
                "VM state lock",
            )?;
        }
        let fd = openat(
            CWD,
            &lock_path,
            OFlags::CREATE | OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .wrap_err_with(|| format!("open VM lock {}", lock_path.display()))?;
        let file = fs::File::from(fd);
        let meta = file
            .metadata()
            .wrap_err_with(|| format!("stat VM lock {}", lock_path.display()))?;
        ensure_private_regular_file(
            &lock_path,
            &meta,
            current_uid,
            self.fix_permissions,
            "VM state lock",
        )?;

        if deadline.is_none() && cancel.is_none() {
            flock(&file, FlockOperation::LockExclusive)
                .wrap_err_with(|| format!("acquire exclusive lock on {}", lock_path.display()))?;
            return Ok(VmLockGuard { _file: file });
        }

        loop {
            check_canceled(cancel, &format!("waiting for VM `{name}` lock"))?;
            match flock(&file, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => return Ok(VmLockGuard { _file: file }),
                Err(rustix::io::Errno::WOULDBLOCK) => {
                    if let Some(dl) = deadline {
                        let now = Instant::now();
                        if now >= dl {
                            bail!(
                                "timed out waiting for exclusive lock on {} for VM `{name}`",
                                lock_path.display()
                            );
                        }
                        let wait = dl.saturating_duration_since(now).min(LOCK_POLL_INTERVAL);
                        thread::sleep(wait);
                    } else {
                        thread::sleep(LOCK_POLL_INTERVAL);
                    }
                }
                Err(err) => {
                    return Err(err).wrap_err_with(|| {
                        format!("acquire exclusive lock on {}", lock_path.display())
                    });
                }
            }
        }
    }

    /// Load a VM's state while holding its exclusive lock, failing if not recorded in our store.
    pub fn load_locked(&self, guard: &VmLockGuard, name: &str) -> Result<LocalVmState> {
        self.load_optional_locked(guard, name)?.ok_or_else(|| {
            eyre::eyre!(
                "VM `{name}` is not recorded in {} (only VMs created by outbe-up are managed)",
                self.vms_dir().display()
            )
        })
    }

    /// Load a VM's state if present while holding its exclusive lock.
    pub fn load_optional_locked(
        &self,
        _guard: &VmLockGuard,
        name: &str,
    ) -> Result<Option<LocalVmState>> {
        let path = self.state_path(name)?;
        let current_uid = rustix::process::getuid().as_raw();
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(err).wrap_err_with(|| format!("stat VM state {}", path.display()))
            }
        };
        ensure_private_regular_file(
            &path,
            &meta,
            current_uid,
            self.fix_permissions,
            "VM state file",
        )?;
        let bytes =
            fs::read(&path).wrap_err_with(|| format!("read VM state {}", path.display()))?;
        let state: LocalVmState = serde_json::from_slice(&bytes)
            .wrap_err_with(|| format!("invalid VM state file: {}", path.display()))?;
        ensure!(
            state.version == LOCAL_VM_STATE_VERSION,
            "unsupported local VM state version {} in {}",
            state.version,
            path.display()
        );
        state.validate_identity(&path)?;
        ensure!(
            state.name == name,
            "VM state file {} records name `{}`, expected `{name}`",
            path.display(),
            state.name
        );
        Ok(Some(state))
    }

    /// Atomically persist a VM's state (0600, fsync temp file, rename, fsync parent dir).
    pub fn save_locked(&self, _guard: &VmLockGuard, state: &LocalVmState) -> Result<PathBuf> {
        let path = self.state_path(&state.name)?;
        state.validate_identity(&path)?;
        self.ensure_dirs()?;

        let current_uid = rustix::process::getuid().as_raw();
        let vms_dir = self.vms_dir();
        if let Ok(meta) = fs::symlink_metadata(&path) {
            ensure_private_regular_file(
                &path,
                &meta,
                current_uid,
                self.fix_permissions,
                "VM state file",
            )?;
        }

        let mut bytes = serde_json::to_vec_pretty(state)?;
        bytes.push(b'\n');

        let mut tmp = tempfile::Builder::new()
            .prefix(&format!(".{}.json.tmp.", state.name))
            .tempfile_in(&vms_dir)
            .wrap_err_with(|| format!("create temporary state file in {}", vms_dir.display()))?;
        tmp.as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))
            .wrap_err("set 0600 permissions on temporary VM state file")?;
        tmp.write_all(&bytes)
            .wrap_err("write temporary VM state file")?;
        tmp.as_file()
            .sync_all()
            .wrap_err("fsync temporary VM state file")?;
        tmp.persist(&path)
            .map_err(|err| err.error)
            .wrap_err_with(|| format!("persist VM state to {}", path.display()))?;

        if let Ok(dir) = fs::File::open(&vms_dir) {
            let _ = dir.sync_all();
        }
        Ok(path)
    }

    /// Remove a VM's state file while holding its exclusive lock.
    pub fn delete_locked(&self, _guard: &VmLockGuard, name: &str) -> Result<PathBuf> {
        let path = self.state_path(name)?;
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(err).wrap_err_with(|| format!("remove VM state {}", path.display()))
            }
        }
        if let Ok(dir) = fs::File::open(self.vms_dir()) {
            let _ = dir.sync_all();
        }
        Ok(path)
    }

    /// List sorted VM names currently recorded in `<state-dir>/vms/*.json`.
    pub fn list_vm_names(&self) -> Result<Vec<String>> {
        let vms_dir = self.vms_dir();
        if !self.root.exists() && !vms_dir.exists() {
            return Ok(Vec::new());
        }
        ensure_private_dir(&self.root, self.fix_permissions)?;
        if !vms_dir.exists() {
            return Ok(Vec::new());
        }
        ensure_private_dir(&vms_dir, self.fix_permissions)?;

        let mut names = Vec::new();
        for entry in fs::read_dir(&vms_dir)
            .wrap_err_with(|| format!("read VM state directory {}", vms_dir.display()))?
        {
            let entry = entry?;
            let file_name = entry.file_name();
            let Some(name_str) = file_name.to_str() else {
                continue;
            };
            if name_str.starts_with('.') {
                continue;
            }
            if let Some(stem) = name_str.strip_suffix(".json") {
                if validate_vm_name(stem).is_ok() {
                    names.push(stem.to_owned());
                }
            }
        }
        names.sort();
        Ok(names)
    }

    pub fn staging_root(&self) -> PathBuf {
        self.root.join("staging")
    }

    pub fn staging_dir(&self, staging_name: &str) -> Result<PathBuf> {
        validate_vm_name(staging_name)?;
        Ok(self.staging_root().join(staging_name))
    }

    pub fn staged_disk_path(&self, staging_name: &str) -> Result<PathBuf> {
        Ok(self.staging_dir(staging_name)?.join("data.img"))
    }

    fn staging_lock_path(&self, staging_name: &str) -> Result<PathBuf> {
        validate_vm_name(staging_name)?;
        Ok(self.staging_root().join(format!(".{staging_name}.lock")))
    }

    /// Ensure `<state-dir>` and `<state-dir>/staging` exist as private `0700` real directories
    /// owned by the current UID.
    pub fn ensure_staging_root(&self) -> Result<PathBuf> {
        ensure_private_dir(&self.root, self.fix_permissions)?;
        let root = self.staging_root();
        ensure_private_dir(&root, self.fix_permissions)?;
        Ok(root)
    }

    /// Ensure `<state-dir>/staging/<staging_name>` exists as a private `0700` real directory
    /// owned by the current UID.
    pub fn ensure_staging_dir(&self, staging_name: &str) -> Result<PathBuf> {
        validate_vm_name(staging_name)?;
        self.ensure_staging_root()?;
        let dir = self.staging_dir(staging_name)?;
        ensure_private_dir(&dir, self.fix_permissions)?;
        Ok(dir)
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
        validate_vm_name(staging_name)?;
        check_canceled(
            cancel,
            &format!("acquiring staging lock for `{staging_name}`"),
        )?;
        self.ensure_staging_root()?;
        let lock_path = self.staging_lock_path(staging_name)?;
        let current_uid = rustix::process::getuid().as_raw();
        if let Ok(meta) = fs::symlink_metadata(&lock_path) {
            ensure_private_regular_file(
                &lock_path,
                &meta,
                current_uid,
                self.fix_permissions,
                "disk staging lock",
            )?;
        }
        let fd = openat(
            CWD,
            &lock_path,
            OFlags::CREATE | OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        )
        .wrap_err_with(|| format!("open disk staging lock {}", lock_path.display()))?;
        let file = fs::File::from(fd);
        let meta = file
            .metadata()
            .wrap_err_with(|| format!("stat disk staging lock {}", lock_path.display()))?;
        ensure_private_regular_file(
            &lock_path,
            &meta,
            current_uid,
            self.fix_permissions,
            "disk staging lock",
        )?;

        if deadline.is_none() && cancel.is_none() {
            flock(&file, FlockOperation::LockExclusive)
                .wrap_err_with(|| format!("acquire exclusive lock on {}", lock_path.display()))?;
            return Ok(StagingLockGuard { _file: file });
        }

        loop {
            check_canceled(
                cancel,
                &format!("waiting for staging lock `{staging_name}`"),
            )?;
            match flock(&file, FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => return Ok(StagingLockGuard { _file: file }),
                Err(rustix::io::Errno::WOULDBLOCK) => {
                    if let Some(dl) = deadline {
                        let now = Instant::now();
                        if now >= dl {
                            bail!(
                                "timed out waiting for exclusive staging lock on {} for `{staging_name}`",
                                lock_path.display()
                            );
                        }
                        let wait = dl.saturating_duration_since(now).min(LOCK_POLL_INTERVAL);
                        thread::sleep(wait);
                    } else {
                        thread::sleep(LOCK_POLL_INTERVAL);
                    }
                }
                Err(err) => {
                    return Err(err).wrap_err_with(|| {
                        format!("acquire exclusive lock on {}", lock_path.display())
                    });
                }
            }
        }
    }

    /// Resolve and security-verify `<state-dir>/staging/<staging_name>/data.img` without creating
    /// directories. Refuses symlinks, non-regular files, foreign UIDs, and unsafe permissions.
    pub fn resolve_staged_disk(&self, staging_name: &str) -> Result<PathBuf> {
        validate_vm_name(staging_name)?;
        let staging_root = self.staging_root();
        let staging_dir = self.staging_dir(staging_name)?;
        let disk_path = self.staged_disk_path(staging_name)?;

        verify_existing_private_dir(&self.root, self.fix_permissions, "state directory")?;
        verify_existing_private_dir(
            &staging_root,
            self.fix_permissions,
            "staging root directory",
        )?;
        verify_existing_private_dir(&staging_dir, self.fix_permissions, "staged disk directory")?;

        let current_uid = rustix::process::getuid().as_raw();
        let meta = fs::symlink_metadata(&disk_path).wrap_err_with(|| {
            format!(
                "staged disk image for `{staging_name}` does not exist at {}",
                disk_path.display()
            )
        })?;
        ensure_private_regular_file(
            &disk_path,
            &meta,
            current_uid,
            self.fix_permissions,
            "staged disk image",
        )?;
        Ok(disk_path)
    }

    /// List all staged raw disk images under `<state-dir>/staging/*/data.img`.
    ///
    /// Invalid or unsafe child entries (symlinks, non-directories, unsafe permissions, unreadable
    /// `data.img`) are reported with `error: Some(...)` instead of aborting the whole list.
    pub fn list_staged_disks(&self) -> Result<Vec<StagedDiskInfo>> {
        let staging_root = self.staging_root();
        if fs::symlink_metadata(&self.root).is_err() {
            return Ok(Vec::new());
        }
        verify_existing_private_dir(&self.root, self.fix_permissions, "state directory")?;
        match fs::symlink_metadata(&staging_root) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => {
                return Err(err).wrap_err_with(|| {
                    format!("stat staging root directory {}", staging_root.display())
                });
            }
            Ok(_) => {}
        }
        verify_existing_private_dir(
            &staging_root,
            self.fix_permissions,
            "staging root directory",
        )?;

        let current_uid = rustix::process::getuid().as_raw();
        let mut disks = Vec::new();
        for entry in fs::read_dir(&staging_root)
            .wrap_err_with(|| format!("read staging root {}", staging_root.display()))?
        {
            let entry = entry?;
            let file_name = entry.file_name();
            let Some(name_str) = file_name.to_str() else {
                continue;
            };
            if name_str.starts_with('.') {
                continue;
            }
            let dir_path = entry.path();
            if let Err(err) = validate_vm_name(name_str) {
                disks.push(StagedDiskInfo {
                    staging_name: name_str.to_owned(),
                    path: Some(dir_path),
                    sha256: None,
                    size_bytes: None,
                    error: Some(format!("{err:#}")),
                });
                continue;
            }
            if let Err(err) = verify_existing_private_dir(
                &dir_path,
                self.fix_permissions,
                "staged disk directory",
            ) {
                disks.push(StagedDiskInfo {
                    staging_name: name_str.to_owned(),
                    path: Some(dir_path),
                    sha256: None,
                    size_bytes: None,
                    error: Some(format!("{err:#}")),
                });
                continue;
            }
            let data_img = dir_path.join("data.img");
            let meta = match fs::symlink_metadata(&data_img) {
                Ok(m) => m,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(err) => {
                    disks.push(StagedDiskInfo {
                        staging_name: name_str.to_owned(),
                        path: Some(data_img),
                        sha256: None,
                        size_bytes: None,
                        error: Some(format!("stat staged disk: {err}")),
                    });
                    continue;
                }
            };
            if let Err(err) = ensure_private_regular_file(
                &data_img,
                &meta,
                current_uid,
                self.fix_permissions,
                "staged disk image",
            ) {
                disks.push(StagedDiskInfo {
                    staging_name: name_str.to_owned(),
                    path: Some(data_img),
                    sha256: None,
                    size_bytes: None,
                    error: Some(format!("{err:#}")),
                });
                continue;
            }
            match sha256_sparse_file(&data_img) {
                Ok(sha256) => disks.push(StagedDiskInfo {
                    staging_name: name_str.to_owned(),
                    path: Some(data_img),
                    sha256: Some(sha256),
                    size_bytes: Some(meta.len()),
                    error: None,
                }),
                Err(err) => disks.push(StagedDiskInfo {
                    staging_name: name_str.to_owned(),
                    path: Some(data_img),
                    sha256: None,
                    size_bytes: None,
                    error: Some(format!("hash staged disk: {err:#}")),
                }),
            }
        }
        disks.sort_by(|a, b| a.staging_name.cmp(&b.staging_name));
        Ok(disks)
    }

    /// Safely remove `<state-dir>/staging/<staging_name>` under its exclusive staging lock.
    /// Idempotent (returns `removed: false`) when absent; refuses symlinks or foreign ownership.
    pub fn clean_staging_dir(&self, staging_name: &str) -> Result<CleanStagingOutput> {
        validate_vm_name(staging_name)?;
        let staging_root = self.staging_root();
        let dir_path = self.staging_dir(staging_name)?;
        match fs::symlink_metadata(&self.root) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(CleanStagingOutput {
                    staging_name: staging_name.to_owned(),
                    path: dir_path,
                    removed: false,
                });
            }
            Err(err) => {
                return Err(err)
                    .wrap_err_with(|| format!("stat state directory {}", self.root.display()));
            }
            Ok(_) => {}
        }
        verify_existing_private_dir(&self.root, self.fix_permissions, "state directory")?;
        match fs::symlink_metadata(&staging_root) {
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(CleanStagingOutput {
                    staging_name: staging_name.to_owned(),
                    path: dir_path,
                    removed: false,
                });
            }
            Err(err) => {
                return Err(err).wrap_err_with(|| {
                    format!("stat staging root directory {}", staging_root.display())
                });
            }
            Ok(_) => {}
        }
        verify_existing_private_dir(
            &staging_root,
            self.fix_permissions,
            "staging root directory",
        )?;

        let lock = self.lock_staging(staging_name)?;
        self.clean_staging_dir_locked(&lock, staging_name)
    }

    /// Safely remove `<state-dir>/staging/<staging_name>` while the caller already holds
    /// `lock_staging(staging_name)`.
    pub fn clean_staging_dir_locked(
        &self,
        _lock: &StagingLockGuard,
        staging_name: &str,
    ) -> Result<CleanStagingOutput> {
        validate_vm_name(staging_name)?;
        let staging_root = self.staging_root();
        let dir_path = self.staging_dir(staging_name)?;
        let current_uid = rustix::process::getuid().as_raw();
        let meta = match fs::symlink_metadata(&dir_path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(CleanStagingOutput {
                    staging_name: staging_name.to_owned(),
                    path: dir_path,
                    removed: false,
                });
            }
            Err(err) => {
                return Err(err).wrap_err_with(|| {
                    format!("stat staged disk directory {}", dir_path.display())
                });
            }
        };
        ensure!(
            meta.file_type().is_dir(),
            "refusing to remove non-directory staging path: {}",
            dir_path.display()
        );
        ensure!(
            meta.uid() == current_uid,
            "staging directory {} is owned by uid {}, expected current uid {current_uid}",
            dir_path.display(),
            meta.uid()
        );
        fs::remove_dir_all(&dir_path)
            .wrap_err_with(|| format!("remove staged disk directory {}", dir_path.display()))?;
        if let Ok(root_fd) = fs::File::open(&staging_root) {
            let _ = root_fd.sync_all();
        }
        Ok(CleanStagingOutput {
            staging_name: staging_name.to_owned(),
            path: dir_path,
            removed: true,
        })
    }
}

fn check_canceled(cancel: Option<&AtomicBool>, step: &str) -> Result<()> {
    if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        bail!("operation interrupted by signal while {step}");
    }
    Ok(())
}

fn ensure_private_dir(path: &Path, fix_permissions: bool) -> Result<()> {
    let current_uid = rustix::process::getuid().as_raw();
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            ensure!(
                meta.file_type().is_dir(),
                "state directory path is not a real directory: {}",
                path.display()
            );
            ensure!(
                meta.uid() == current_uid,
                "state directory {} is owned by uid {}, expected current uid {current_uid}",
                path.display(),
                meta.uid()
            );
            let mode = meta.permissions().mode() & 0o7777;
            if mode & 0o7077 != 0 {
                if fix_permissions {
                    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                        .wrap_err_with(|| format!("chmod 0700 {}", path.display()))?;
                } else {
                    bail!(
                        "refusing state directory {} with unsafe mode {:04o} (expected 0700, no group/other access); run `chmod 0700 {}` or pass --fix-permissions",
                        path.display(),
                        mode,
                        path.display()
                    );
                }
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)
                .wrap_err_with(|| format!("create state directory {}", path.display()))?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .wrap_err_with(|| format!("set 0700 mode on {}", path.display()))?;
        }
        Err(err) => {
            return Err(err).wrap_err_with(|| format!("stat state directory {}", path.display()))
        }
    }
    Ok(())
}

fn verify_existing_private_dir(path: &Path, fix_permissions: bool, label: &str) -> Result<()> {
    let current_uid = rustix::process::getuid().as_raw();
    let meta = fs::symlink_metadata(path)
        .wrap_err_with(|| format!("{label} does not exist: {}", path.display()))?;
    ensure!(
        meta.file_type().is_dir(),
        "{label} path is not a real directory: {}",
        path.display()
    );
    ensure!(
        meta.uid() == current_uid,
        "{label} {} is owned by uid {}, expected current uid {current_uid}",
        path.display(),
        meta.uid()
    );
    let mode = meta.permissions().mode() & 0o7777;
    if mode & 0o7077 != 0 {
        if fix_permissions {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .wrap_err_with(|| format!("chmod 0700 {}", path.display()))?;
        } else {
            bail!(
                "refusing {label} {} with unsafe mode {:04o} (expected 0700, no group/other access); run `chmod 0700 {}` or pass --fix-permissions",
                path.display(),
                mode,
                path.display()
            );
        }
    }
    Ok(())
}

fn ensure_private_regular_file(
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
    ensure!(
        meta.uid() == current_uid,
        "{label} {} is owned by uid {}, expected current uid {current_uid}",
        path.display(),
        meta.uid()
    );
    let mode = meta.permissions().mode() & 0o7777;
    if mode & 0o7077 != 0 {
        if fix_permissions {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .wrap_err_with(|| format!("chmod 0600 {}", path.display()))?;
        } else {
            bail!(
                "refusing {label} {} with unsafe mode {:04o} (expected 0600, no group/other access); run `chmod 0600 {}` or pass --fix-permissions",
                path.display(),
                mode,
                path.display()
            );
        }
    }
    Ok(())
}

fn generate_create_token() -> Result<String> {
    let mut bytes = [0u8; 16];
    fs::File::open("/dev/urandom")
        .wrap_err("open /dev/urandom for VM create token")?
        .read_exact(&mut bytes)
        .wrap_err("read random bytes from /dev/urandom")?;
    Ok(hex::encode(bytes))
}

/// Validate a VM name used both as the VMM name and as the `<name>.json` state file stem.
pub fn validate_vm_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && name.len() <= 63,
        "VM name must be 1..=63 characters, got '{name}'"
    );
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    ensure!(
        first.is_ascii_alphanumeric(),
        "VM name must start with an ASCII letter or digit, got '{name}'"
    );
    ensure!(
        chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')),
        "VM name must use only ASCII letters, digits, `-`, or `_`, got '{name}'"
    );
    Ok(())
}

/// Validate an optional VM ownership tag (e.g. `outbe-net:<network>:<genesis8>:<member>`).
pub fn validate_owner_tag(tag: &str) -> Result<()> {
    ensure!(
        !tag.is_empty() && tag.len() <= 128,
        "owner tag must be 1..=128 characters, got '{tag}'"
    );
    let mut chars = tag.chars();
    let first = chars.next().unwrap();
    ensure!(
        first.is_ascii_alphanumeric(),
        "owner tag must start with an ASCII letter or digit, got '{tag}'"
    );
    ensure!(
        chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':' | '.')),
        "owner tag must use only ASCII letters, digits, `-`, `_`, `:`, or `.`, got '{tag}'"
    );
    Ok(())
}

pub fn validate_flavor(flavor: &str) -> Result<()> {
    ensure!(
        matches!(flavor, "dev" | "prod"),
        "VM flavor must be `dev` or `prod`, got `{flavor}`"
    );
    Ok(())
}

/// Expand a leading `~` or `~/` using `$HOME`.
pub fn expand_tilde(path: &Path) -> Result<PathBuf> {
    let raw = path.to_string_lossy();
    if raw == "~" {
        return home_dir();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return Ok(home_dir()?.join(rest));
    }
    Ok(path.to_path_buf())
}

fn home_dir() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| eyre::eyre!("HOME is not set"))?;
    ensure!(!home.is_empty(), "HOME is empty");
    Ok(PathBuf::from(home))
}

/// Default local state directory: `${XDG_STATE_HOME:-$HOME/.local/state}/outbe-up`.
pub fn default_state_dir() -> Result<PathBuf> {
    if let Some(xdg) = std::env::var_os("XDG_STATE_HOME") {
        if !xdg.is_empty() {
            let xdg_path = PathBuf::from(xdg);
            if xdg_path.is_absolute() {
                return Ok(xdg_path.join("outbe-up"));
            }
        }
    }
    Ok(home_dir()?.join(".local/state/outbe-up"))
}

/// Default local dstack VMM home directory (`~/.dstack-vmm`).
pub fn default_vmm_home() -> Result<PathBuf> {
    Ok(home_dir()?.join(".dstack-vmm"))
}

/// Default local dstack VMM Unix socket path (`~/.dstack-vmm/run/vmm.sock`).
pub fn default_vmm_socket() -> Result<PathBuf> {
    Ok(default_vmm_home()?.join("run/vmm.sock"))
}

/// Resolve `(vmm_socket, vmm_home)` from optional CLI overrides.
pub fn resolve_vmm_paths(
    vmm_socket: Option<&Path>,
    vmm_home: Option<&Path>,
) -> Result<(PathBuf, PathBuf)> {
    match (vmm_socket, vmm_home) {
        (Some(sock), Some(home)) => Ok((expand_tilde(sock)?, expand_tilde(home)?)),
        (None, Some(home)) => {
            let home = expand_tilde(home)?;
            Ok((home.join("run/vmm.sock"), home))
        }
        (Some(sock), None) => {
            let sock = expand_tilde(sock)?;
            let parent = sock
                .parent()
                .ok_or_else(|| eyre::eyre!("VMM socket path has no parent: {}", sock.display()))?;
            let derived_home = if parent.file_name().and_then(|s| s.to_str()) == Some("run") {
                parent.parent().unwrap_or(parent).to_path_buf()
            } else {
                parent.to_path_buf()
            };
            Ok((sock, derived_home))
        }
        (None, None) => Ok((default_vmm_socket()?, default_vmm_home()?)),
    }
}

#[derive(Clone, Debug)]
pub struct CreateLocalVmOptions {
    pub name: String,
    pub owner_tag: Option<String>,
    pub image_dir: PathBuf,
    pub image_name: Option<String>,
    pub flavor: String,
    pub disk_source: PathBuf,
    /// Optional staging name under `<state-dir>/staging/<name>` (`--disk-staging <name>`).
    /// When set, `create_local_vm*` holds `lock_staging(name)` BEFORE `lock_vm` across
    /// resolution, SHA-256 hashing, `CreateVm`, disk conversion, and optional consumption so the
    /// hashed bytes are guaranteed to match the converted bytes.
    pub disk_staging_name: Option<String>,
    /// When `true` (requires `disk_staging_name.is_some()`), deletes `<state-dir>/staging/<name>`
    /// ONLY after `LocalVmPhase::Ready` is durably saved.
    pub consume_staging: bool,
    pub vcpu: u32,
    pub memory_gib: u32,
    pub disk_gib: u32,
    pub bridge: String,
    pub app_compose_path: Option<PathBuf>,
    pub intended_ip: Option<Ipv4Addr>,
    pub no_tee: bool,
    pub vmm_socket: PathBuf,
    pub vmm_home: PathBuf,
    /// Allow issuing a new `CreateVm` when a prior `Creating` intent did not appear within the
    /// grace polling window.
    pub retry_create: bool,
    /// Optional overall timeout bounding lock acquisition, recovery grace polling, and RPCs.
    pub timeout: Option<Duration>,
    /// Grace polling window when resolving an existing `Creating` intent (default 30s, bounded by
    /// `timeout` if set).
    pub recovery_grace: Duration,
    /// Poll interval used during `Creating` grace polling.
    pub poll_interval: Duration,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CreateLocalVmOutput {
    pub name: String,
    pub vm_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_tag: Option<String>,
    pub image_name: String,
    pub os_image_hash: String,
    pub image_reused: bool,
    pub mac: String,
    pub bridge: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intended_ip: Option<Ipv4Addr>,
    pub vcpu: u32,
    pub memory_gib: u32,
    pub disk_gib: u32,
    pub phase: LocalVmPhase,
    #[serde(default)]
    pub staging_consumed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    pub state_file: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StartLocalVmOutput {
    pub name: String,
    pub vm_id: String,
    pub already_running: bool,
    pub vmm_status: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StopLocalVmOutput {
    pub name: String,
    pub vm_id: String,
    pub already_stopped: bool,
    pub forced: bool,
    /// True when `--force` sent `StopVm` on an already-`exited` VM to clear dstack's `.started`
    /// flag and prevent automatic restart.
    #[serde(default)]
    pub cleared_started_flag: bool,
    pub vmm_status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LocalVmStatusOutput {
    pub name: String,
    pub vm_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_tag: Option<String>,
    pub phase: LocalVmPhase,
    pub vmm_status: String,
    pub running: bool,
    pub uptime: String,
    pub mac: String,
    pub ip: Option<Ipv4Addr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_ip: Option<Ipv4Addr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dhcp_ip: Option<Ipv4Addr>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intended_ip: Option<Ipv4Addr>,
    pub image_name: String,
    pub os_image_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_source_sha256: Option<String>,
    pub app_id: String,
    pub no_tee: bool,
    pub vcpu: u32,
    pub memory_gib: u32,
    pub disk_gib: u32,
    pub bridge: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boot_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LocalVmLogsOutput {
    pub name: String,
    pub vm_id: String,
    pub lines: usize,
    pub log: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoveLocalVmOutput {
    pub name: String,
    pub vm_id: String,
    pub state_file: PathBuf,
    pub forced: bool,
    #[serde(default)]
    pub forgot_intent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// Create one stopped local VM, publish its guest image under a digest-qualified name,
/// stage its qcow2 data disk locally, and journal every transition to its per-VM state file.
pub fn create_local_vm<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    options: &CreateLocalVmOptions,
) -> Result<CreateLocalVmOutput> {
    let cancel = AtomicBool::new(false);
    create_local_vm_cancellable(store, vmm, options, &cancel)
}

/// Same as [`create_local_vm`], honoring a signal-driven cancellation flag across lock wait,
/// pre-RPC checkpoints, recovery grace polling, and `qemu-img` execution.
pub fn create_local_vm_cancellable<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    options: &CreateLocalVmOptions,
    cancel: &AtomicBool,
) -> Result<CreateLocalVmOutput> {
    create_local_vm_with_disk_stager_cancellable(
        store,
        vmm,
        options,
        Some(cancel),
        |vmm_home, vm_id, raw_disk, disk_gib| {
            stage_local_disk_cancellable(vmm_home, vm_id, raw_disk, disk_gib, cancel)
        },
    )
}

/// Same as [`create_local_vm`] with an injectable disk stager for fast stub tests.
pub fn create_local_vm_with_disk_stager<T, F>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    options: &CreateLocalVmOptions,
    disk_stager: F,
) -> Result<CreateLocalVmOutput>
where
    T: VmmTransport,
    F: FnOnce(&Path, &str, &Path, u32) -> Result<PathBuf>,
{
    create_local_vm_with_disk_stager_cancellable(store, vmm, options, None, disk_stager)
}

/// Same as [`create_local_vm_with_disk_stager`] with an optional cancellation flag.
pub fn create_local_vm_with_disk_stager_cancellable<T, F>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    options: &CreateLocalVmOptions,
    cancel: Option<&AtomicBool>,
    disk_stager: F,
) -> Result<CreateLocalVmOutput>
where
    T: VmmTransport,
    F: FnOnce(&Path, &str, &Path, u32) -> Result<PathBuf>,
{
    let op_deadline = options.timeout.map(|t| Instant::now() + t);

    check_canceled(cancel, "validating create inputs")?;
    validate_vm_name(&options.name)?;
    if let Some(tag) = &options.owner_tag {
        validate_owner_tag(tag)?;
    }
    if options.consume_staging {
        ensure!(
            options.disk_staging_name.is_some(),
            "--consume-staging requires --disk-staging"
        );
    }
    if let Some(staging_name) = &options.disk_staging_name {
        validate_vm_name(staging_name)?;
    }
    validate_iface_name(&options.bridge, "bridge")?;
    validate_flavor(&options.flavor)?;
    ensure!(options.vcpu > 0, "--vcpu must be positive");
    ensure!(options.memory_gib > 0, "--memory-gib must be positive");
    ensure!(options.disk_gib > 0, "--disk-gib must be positive");

    // Lock order: staging lock FIRST (when `--disk-staging` is set), then per-VM lock, then
    // per-image lock. `disk prepare` and `disk clean` take only the staging lock, so this order
    // is strictly acyclic and prevents any concurrent prepare/clean from swapping `data.img`
    // between SHA-256 hashing and `qemu-img convert`.
    let (effective_disk_source, staging_held) = match &options.disk_staging_name {
        Some(staging_name) => {
            verify_existing_private_dir(&store.root, store.fix_permissions, "state directory")?;
            verify_existing_private_dir(
                &store.staging_root(),
                store.fix_permissions,
                "staging root directory",
            )?;
            let staging_lock =
                store.lock_staging_with_options(staging_name, op_deadline, cancel)?;
            let resolved = store.resolve_staged_disk(staging_name)?;
            (resolved, Some((staging_name.clone(), staging_lock)))
        }
        None => {
            ensure!(
                options.disk_source.is_file(),
                "raw disk image does not exist: {}",
                options.disk_source.display()
            );
            (options.disk_source.clone(), None)
        }
    };

    let compose_file = match &options.app_compose_path {
        Some(path) => {
            let content = fs::read_to_string(path)
                .wrap_err_with(|| format!("read app-compose file {}", path.display()))?;
            let _: serde_json::Value = serde_json::from_str(&content)
                .wrap_err_with(|| format!("invalid JSON in app-compose file {}", path.display()))?;
            content
        }
        None => APP_COMPOSE_JSON.to_owned(),
    };
    let app_id = calc_app_id(&compose_file);
    check_canceled(cancel, "hashing raw disk image")?;
    let disk_source_sha256 = sha256_sparse_file(&effective_disk_source)
        .wrap_err_with(|| format!("hash raw disk image {}", effective_disk_source.display()))?;

    let guard = store.lock_vm_with_options(&options.name, op_deadline, cancel)?;

    check_canceled(cancel, "publishing local guest image")?;
    let published_locked = publish_local_image_locked_cancellable(
        &options.vmm_home,
        &options.image_dir,
        options.image_name.as_deref(),
        &options.flavor,
        op_deadline,
        cancel,
    )?;
    let published = published_locked.published;
    let image_lock = published_locked.lock;

    let existing = store.load_optional_locked(&guard, &options.name)?;
    let mut state = match existing {
        Some(mut prev) => {
            ensure!(
                prev.phase != LocalVmPhase::Ready,
                "VM `{}` ({}) already exists in {}; run `outbe-up vm remove --name {}` first",
                prev.name,
                prev.vm_id,
                store.state_path(&prev.name)?.display(),
                prev.name
            );
            ensure!(
                prev.phase != LocalVmPhase::Removing,
                "VM `{}` ({}) is mid-removal; finish cleanup with `outbe-up vm remove --name {}`",
                prev.name,
                prev.vm_id,
                prev.name
            );
            ensure_immutable_create_params_match(
                &prev,
                options,
                &effective_disk_source,
                &published.name,
                &published.digest,
                &app_id,
                &disk_source_sha256,
            )?;

            if prev.phase == LocalVmPhase::Creating {
                let token = prev
                    .create_token
                    .clone()
                    .ok_or_else(|| eyre::eyre!("creating state missing create_token"))?;
                let grace_deadline = compute_grace_deadline(options.recovery_grace, op_deadline);
                if let Some(recovered) = poll_for_owned_creating_vm(
                    vmm,
                    &prev,
                    &token,
                    grace_deadline,
                    options.poll_interval,
                    cancel,
                )? {
                    verify_adopted_vm_matches_state(&recovered, &prev)?;
                    recovered.ensure_never_started_and_stopped()?;
                    prev.vm_id = recovered.id.clone();
                    prev.phase = LocalVmPhase::Created;
                    prev.record_step(
                        "recovered-creating",
                        Some(format!("vm_id={}", recovered.id)),
                    );
                    store.save_locked(&guard, &prev)?;
                } else {
                    ensure!(
                        options.retry_create,
                        "VM `{}` has an unresolved `Creating` intent (token {}) and did not appear on the local VMM within {}s; an earlier CreateVm request may still be completing. Inspect the VMM, wait, or re-run with `--retry-create` to issue a new CreateVm (or `outbe-up vm remove --name {} --forget-intent` to discard the intent)",
                        prev.name,
                        token,
                        options.recovery_grace.as_secs(),
                        prev.name
                    );
                    check_canceled(cancel, "re-checking collisions before --retry-create")?;
                    ensure_no_foreign_name_collision(vmm, &prev.name, op_deadline)?;

                    let new_token = generate_create_token()?;
                    prev.create_token = Some(new_token.clone());
                    prev.record_step("retry-create", Some(format!("token={new_token}")));
                    store.save_locked(&guard, &prev)?;

                    check_canceled(cancel, "issuing CreateVm")?;
                    let created_id = issue_create_vm(
                        vmm,
                        options,
                        &published.name,
                        &compose_file,
                        &app_id,
                        &new_token,
                        op_deadline,
                    )?;
                    prev.vm_id = created_id.clone();
                    prev.phase = LocalVmPhase::Created;
                    prev.record_step("created", Some(format!("vm_id={created_id}")));
                    persist_created_or_cleanup(store, &guard, vmm, &prev)?;
                }
            } else {
                check_canceled(cancel, "querying partial VM info")?;
                let info_resp = vmm.get_info_with_deadline(&prev.vm_id, op_deadline)?;
                ensure!(
                    !info_resp.is_gone(),
                    "recorded partial VM `{}` ({}) is missing from VMM; run `outbe-up vm remove --name {}` to clean up",
                    prev.name,
                    prev.vm_id,
                    prev.name
                );
                let info = info_resp.info.unwrap();
                ensure!(
                    info.name == prev.name,
                    "recorded partial VM {} now has unexpected VMM name `{}`; run `outbe-up vm remove --name {}`",
                    prev.vm_id,
                    info.name,
                    prev.name
                );
                info.ensure_never_started_and_stopped()?;
            }
            prev
        }
        None => {
            check_canceled(cancel, "checking VMM name collisions")?;
            // Refuse name collisions with any foreign VM already registered on the local VMM.
            ensure_no_foreign_name_collision(vmm, &options.name, op_deadline)?;

            check_canceled(cancel, "recording Creating intent before CreateVm")?;
            // Journal durable `Creating` intent with a unique annotation token BEFORE CreateVm
            // so a crash or lost response never strands an unowned VM.
            let create_token = generate_create_token()?;
            let mut intent = LocalVmState {
                version: LOCAL_VM_STATE_VERSION,
                name: options.name.clone(),
                vm_id: String::new(),
                create_token: Some(create_token.clone()),
                owner_tag: options.owner_tag.clone(),
                vmm_socket: options.vmm_socket.clone(),
                vmm_home: options.vmm_home.clone(),
                image_name: published.name.clone(),
                os_image_hash: published.digest.clone(),
                disk_source: effective_disk_source.clone(),
                disk_source_sha256: Some(disk_source_sha256.clone()),
                vcpu: options.vcpu,
                memory_gib: options.memory_gib,
                disk_gib: options.disk_gib,
                bridge: options.bridge.clone(),
                intended_ip: options.intended_ip,
                mac: String::new(),
                app_id: app_id.clone(),
                no_tee: options.no_tee,
                phase: LocalVmPhase::Creating,
                journal: Vec::new(),
            };
            intent.record_step("creating", Some(format!("token={create_token}")));
            store.save_locked(&guard, &intent)?;

            check_canceled(cancel, "issuing CreateVm")?;
            let created_id = issue_create_vm(
                vmm,
                options,
                &published.name,
                &compose_file,
                &app_id,
                &create_token,
                op_deadline,
            )?;

            intent.vm_id = created_id.clone();
            intent.phase = LocalVmPhase::Created;
            intent.record_step("created", Some(format!("vm_id={created_id}")));
            persist_created_or_cleanup(store, &guard, vmm, &intent)?;
            intent
        }
    };
    // Release the per-image lock only after CreateVm has returned and the VM is registered on VMM
    // and persisted locally as `Created` (or higher), preventing a concurrent `image remove` from
    // deleting the image in the window between publication and VMM registration.
    drop(image_lock);

    if state.phase == LocalVmPhase::Created {
        check_canceled(cancel, "fetching VM info")?;
        state.record_step("fetching-info", None);
        store.save_locked(&guard, &state)?;

        let info_resp = vmm.get_info_with_deadline(&state.vm_id, op_deadline)?;
        ensure!(
            !info_resp.is_gone(),
            "VMM GetInfo reported VM {} not found immediately after CreateVm",
            state.vm_id
        );
        let info = info_resp.info.unwrap();
        info.ensure_never_started_and_stopped()?;
        let mac = extract_primary_mac(&info)?;

        state.mac = mac.clone();
        state.phase = LocalVmPhase::MacRecorded;
        state.record_step("mac-recorded", Some(format!("mac={mac}")));
        store.save_locked(&guard, &state)?;
    }

    if state.phase == LocalVmPhase::MacRecorded {
        check_canceled(cancel, "staging hda.img")?;
        // Re-verify right before touching hda.img that no external VMM client started this VM.
        let info_resp = vmm.get_info_with_deadline(&state.vm_id, op_deadline)?;
        ensure!(
            !info_resp.is_gone(),
            "VMM GetInfo reported VM {} missing before disk staging",
            state.vm_id
        );
        let info = info_resp.info.unwrap();
        ensure!(
            info.name == state.name,
            "VMM VM {} has unexpected name `{}` (expected `{}`)",
            state.vm_id,
            info.name,
            state.name
        );
        info.ensure_never_started_and_stopped()?;

        check_canceled(cancel, "starting disk conversion")?;
        state.record_step(
            "staging-disk",
            Some(format!(
                "source={} sha256={disk_source_sha256}",
                state.disk_source.display()
            )),
        );
        store.save_locked(&guard, &state)?;

        let hda_path = disk_stager(
            &state.vmm_home,
            &state.vm_id,
            &state.disk_source,
            state.disk_gib,
        )?;
        state.phase = LocalVmPhase::DiskStaged;
        state.record_step("disk-staged", Some(format!("hda={}", hda_path.display())));
        store.save_locked(&guard, &state)?;
    }

    if state.phase == LocalVmPhase::DiskStaged {
        state.phase = LocalVmPhase::Ready;
        state.record_step("ready", None);
    }
    let state_file = store.save_locked(&guard, &state)?;

    // Consume the staged raw disk ONLY AFTER `LocalVmPhase::Ready` has been durably saved so any
    // failure or interruption before `Ready` preserves the staged disk for retry/resume. If
    // consumption fails after durable Ready, emit a warning and `staging_consumed: false` (exit 0)
    // so callers can clean it up explicitly with `disk clean --staging-name`.
    let mut staging_consumed = false;
    let mut warning = None;
    if options.consume_staging {
        if let Some((staging_name, staging_lock)) = staging_held.as_ref() {
            match store.clean_staging_dir_locked(staging_lock, staging_name) {
                Ok(cleaned) => {
                    staging_consumed = cleaned.removed;
                }
                Err(err) => {
                    warning = Some(format!(
                        "WARNING: VM `{}` reached Ready, but failed to remove staged disk `{staging_name}` ({err:#}); run `outbe-up disk clean --staging-name {staging_name}`",
                        state.name
                    ));
                }
            }
        }
    }
    drop(staging_held);

    Ok(CreateLocalVmOutput {
        name: state.name,
        vm_id: state.vm_id,
        owner_tag: state.owner_tag,
        image_name: state.image_name,
        os_image_hash: state.os_image_hash,
        image_reused: published.reused,
        mac: state.mac,
        bridge: state.bridge,
        intended_ip: state.intended_ip,
        vcpu: state.vcpu,
        memory_gib: state.memory_gib,
        disk_gib: state.disk_gib,
        phase: state.phase,
        staging_consumed,
        warning,
        state_file,
    })
}

fn compute_grace_deadline(grace: Duration, op_deadline: Option<Instant>) -> Instant {
    let by_grace = Instant::now() + grace;
    match op_deadline {
        Some(op) => by_grace.min(op),
        None => by_grace,
    }
}

fn ensure_immutable_create_params_match(
    prev: &LocalVmState,
    options: &CreateLocalVmOptions,
    effective_disk_source: &Path,
    published_image_name: &str,
    published_image_hash: &str,
    app_id: &str,
    current_disk_sha256: &str,
) -> Result<()> {
    let mut mismatches = Vec::new();
    if prev.image_name != published_image_name {
        mismatches.push(format!(
            "image_name (`{}` vs `{published_image_name}`)",
            prev.image_name
        ));
    }
    if prev.os_image_hash != published_image_hash {
        mismatches.push(format!(
            "os_image_hash (`{}` vs `{published_image_hash}`)",
            prev.os_image_hash
        ));
    }
    if prev.bridge != options.bridge {
        mismatches.push(format!(
            "bridge (`{}` vs `{}`)",
            prev.bridge, options.bridge
        ));
    }
    if prev.disk_gib != options.disk_gib {
        mismatches.push(format!(
            "disk_gib ({} vs {})",
            prev.disk_gib, options.disk_gib
        ));
    }
    if prev.vcpu != options.vcpu {
        mismatches.push(format!("vcpu ({} vs {})", prev.vcpu, options.vcpu));
    }
    if prev.memory_gib != options.memory_gib {
        mismatches.push(format!(
            "memory_gib ({} vs {})",
            prev.memory_gib, options.memory_gib
        ));
    }
    if prev.no_tee != options.no_tee {
        mismatches.push(format!("no_tee ({} vs {})", prev.no_tee, options.no_tee));
    }
    if prev.app_id != app_id {
        mismatches.push(format!("app_id (`{}` vs `{app_id}`)", prev.app_id));
    }
    if prev.owner_tag != options.owner_tag {
        mismatches.push(format!(
            "owner_tag ({:?} vs {:?})",
            prev.owner_tag, options.owner_tag
        ));
    }
    if prev.intended_ip != options.intended_ip {
        mismatches.push(format!(
            "intended_ip ({:?} vs {:?})",
            prev.intended_ip, options.intended_ip
        ));
    }
    if prev.vmm_socket != options.vmm_socket {
        mismatches.push(format!(
            "vmm_socket (`{}` vs `{}`)",
            prev.vmm_socket.display(),
            options.vmm_socket.display()
        ));
    }
    if prev.vmm_home != options.vmm_home {
        mismatches.push(format!(
            "vmm_home (`{}` vs `{}`)",
            prev.vmm_home.display(),
            options.vmm_home.display()
        ));
    }
    if prev.disk_source != effective_disk_source {
        mismatches.push(format!(
            "disk_source (`{}` vs `{}`)",
            prev.disk_source.display(),
            effective_disk_source.display()
        ));
    }
    match prev.disk_source_sha256.as_deref() {
        Some(recorded_sha) if recorded_sha == current_disk_sha256 => {}
        Some(recorded_sha) => {
            mismatches.push(format!(
                "disk_source_sha256 (`{recorded_sha}` vs `{current_disk_sha256}`)"
            ));
        }
        None => {
            mismatches.push("disk_source_sha256 (missing in partial state)".to_owned());
        }
    }
    ensure!(
        mismatches.is_empty(),
        "partial state for VM `{}` has mismatched immutable parameters ({}); retry with original inputs or run `outbe-up vm remove --name {}` first",
        prev.name,
        mismatches.join(", "),
        prev.name
    );
    Ok(())
}

/// Paginate brief `Status` filtered by `keyword = name` and return deduplicated candidate VM IDs
/// whose exact name equals `name`.
///
/// Uses `brief = true` and `page_size = 100` so unrelated VMs and large compose/env blobs on the
/// host never bloat a single `Status` response past the 32 MiB HTTP cap.
fn list_exact_name_candidate_ids<T: VmmTransport>(
    vmm: &VmmClient<T>,
    name: &str,
    deadline: Option<Instant>,
) -> Result<Vec<String>> {
    let mut candidate_ids = Vec::new();
    let mut seen_ids = BTreeSet::new();

    for page in 1..=MAX_STATUS_DISCOVERY_PAGES {
        let resp = vmm.status_with_deadline(
            &StatusRequest {
                brief: true,
                keyword: name.to_owned(),
                page,
                page_size: STATUS_DISCOVERY_PAGE_SIZE,
                ..StatusRequest::default()
            },
            deadline,
        )?;
        let count = resp.vms.len() as u32;
        for vm in resp.vms {
            if vm.name == name && !vm.id.is_empty() && seen_ids.insert(vm.id.clone()) {
                candidate_ids.push(vm.id);
            }
        }
        if count < STATUS_DISCOVERY_PAGE_SIZE {
            return Ok(candidate_ids);
        }
        if resp.total > 0 && page.saturating_mul(STATUS_DISCOVERY_PAGE_SIZE) >= resp.total {
            return Ok(candidate_ids);
        }
    }

    bail!(
        "VMM Status pagination exceeded hard cap ({} pages / {} entries) while querying keyword `{name}`",
        MAX_STATUS_DISCOVERY_PAGES,
        MAX_STATUS_DISCOVERY_PAGES * STATUS_DISCOVERY_PAGE_SIZE
    );
}

fn ensure_no_foreign_name_collision<T: VmmTransport>(
    vmm: &VmmClient<T>,
    name: &str,
    deadline: Option<Instant>,
) -> Result<()> {
    let candidates = list_exact_name_candidate_ids(vmm, name, deadline)?;
    if let Some(collision_id) = candidates.first() {
        bail!(
            "refusing to create VM `{name}`: a VM with the same name ({collision_id}) already exists on the local VMM"
        );
    }
    Ok(())
}

/// Search filtered brief `Status` pages for exact-name candidates, then fetch full `GetInfo` ONLY
/// for those candidates and match `(name, CREATE_TOKEN_ANNOTATION)`.
///
/// Security note: `CREATE_TOKEN_ANNOTATION` is a bearer recovery marker readable by any client
/// with access to the VMM Unix socket; callers MUST also pass any returned [`VmInfo`] through
/// [`verify_adopted_vm_matches_state`] before adopting it.
fn find_owned_creating_vm<T: VmmTransport>(
    vmm: &VmmClient<T>,
    name: &str,
    expected_token: &str,
    deadline: Option<Instant>,
) -> Result<Option<VmInfo>> {
    let candidate_ids = list_exact_name_candidate_ids(vmm, name, deadline)?;
    let mut matches = Vec::new();
    for id in candidate_ids {
        let resp = vmm.get_info_with_deadline(&id, deadline)?;
        if let Some(info) = resp.info {
            if info.name == name && info.annotation(CREATE_TOKEN_ANNOTATION) == Some(expected_token)
            {
                matches.push(info);
            }
        }
    }
    ensure!(
        matches.len() <= 1,
        "multiple VMM entries matched create token for VM `{name}`; resolve manually"
    );
    Ok(matches.into_iter().next())
}

/// Poll `Status`/`GetInfo` until `grace_deadline` for a token-matched VM created by an interrupted
/// `CreateVm` request, returning `Ok(None)` if still absent when the grace window expires.
fn poll_for_owned_creating_vm<T: VmmTransport>(
    vmm: &VmmClient<T>,
    state: &LocalVmState,
    expected_token: &str,
    grace_deadline: Instant,
    poll_interval: Duration,
    cancel: Option<&AtomicBool>,
) -> Result<Option<VmInfo>> {
    loop {
        check_canceled(cancel, "polling VMM for in-flight Creating VM")?;
        if let Some(found) =
            find_owned_creating_vm(vmm, &state.name, expected_token, Some(grace_deadline))?
        {
            return Ok(Some(found));
        }
        let now = Instant::now();
        if now >= grace_deadline {
            return Ok(None);
        }
        let remaining = grace_deadline.saturating_duration_since(now);
        let sleep_step = if cancel.is_some() {
            remaining.min(poll_interval).min(LOCK_POLL_INTERVAL)
        } else {
            remaining.min(poll_interval)
        };
        thread::sleep(sleep_step);
    }
}

/// Verify that a candidate VM recovered via `CREATE_TOKEN_ANNOTATION` matches our journaled
/// `name`, `image`, `vcpu`, `memory`, and `app_id` before adopting it.
fn verify_adopted_vm_matches_state(info: &VmInfo, state: &LocalVmState) -> Result<()> {
    let mut mismatches = Vec::new();
    if info.name != state.name {
        mismatches.push(format!("name (`{}` vs `{}`)", info.name, state.name));
    }
    let Some(cfg) = &info.configuration else {
        bail!(
            "refusing to adopt VM {} for `{}`: VMM GetInfo omitted configuration block",
            info.id,
            state.name
        );
    };
    if cfg.image != state.image_name {
        mismatches.push(format!("image (`{}` vs `{}`)", cfg.image, state.image_name));
    }
    if cfg.vcpu != state.vcpu {
        mismatches.push(format!("vcpu ({} vs {})", cfg.vcpu, state.vcpu));
    }
    let expected_memory_mb = state.memory_gib * 1024;
    if cfg.memory != expected_memory_mb {
        mismatches.push(format!(
            "memory ({} MiB vs {} MiB)",
            cfg.memory, expected_memory_mb
        ));
    }
    match info.effective_app_id() {
        Some(actual_app_id) if actual_app_id == state.app_id => {}
        Some(actual_app_id) => {
            mismatches.push(format!("app_id (`{actual_app_id}` vs `{}`)", state.app_id));
        }
        None => {
            mismatches.push(format!("app_id (missing vs `{}`)", state.app_id));
        }
    }
    if info.annotation(OWNER_TAG_ANNOTATION) != state.owner_tag.as_deref() {
        mismatches.push(format!(
            "owner_tag ({:?} vs {:?})",
            info.annotation(OWNER_TAG_ANNOTATION),
            state.owner_tag.as_deref()
        ));
    }
    ensure!(
        mismatches.is_empty(),
        "refusing to adopt token-matched VM {} for `{}`: VMM configuration does not match journal ({})",
        info.id,
        state.name,
        mismatches.join(", ")
    );
    Ok(())
}

fn issue_create_vm<T: VmmTransport>(
    vmm: &VmmClient<T>,
    options: &CreateLocalVmOptions,
    image_name: &str,
    compose_file: &str,
    app_id: &str,
    create_token: &str,
    deadline: Option<Instant>,
) -> Result<String> {
    let mut annotations = BTreeMap::new();
    annotations.insert(CREATE_TOKEN_ANNOTATION.to_owned(), create_token.to_owned());
    if let Some(owner_tag) = &options.owner_tag {
        annotations.insert(OWNER_TAG_ANNOTATION.to_owned(), owner_tag.clone());
    }
    vmm.create_vm_with_deadline(
        &CreateVmRequest {
            name: options.name.clone(),
            image: image_name.to_owned(),
            compose_file: compose_file.to_owned(),
            vcpu: options.vcpu,
            memory: options.memory_gib * 1024,
            disk_size: options.disk_gib,
            ports: Vec::new(),
            app_id: Some(app_id.to_owned()),
            user_config: String::new(),
            hugepages: false,
            pin_numa: false,
            stopped: true,
            no_tee: options.no_tee,
            networking: Some(NetworkingConfig {
                mode: "bridge".to_owned(),
                bridge_name: options.bridge.clone(),
            }),
            annotations,
        },
        deadline,
    )
}

fn persist_created_or_cleanup<T: VmmTransport>(
    store: &LocalVmStore,
    guard: &VmLockGuard,
    vmm: &VmmClient<T>,
    state: &LocalVmState,
) -> Result<()> {
    if let Err(save_err) = store.save_locked(guard, state) {
        let created_id = &state.vm_id;
        let cleanup_note = match vmm.remove_vm(created_id) {
            Ok(()) => format!("removed newly created VM {created_id}"),
            Err(rm_err) => format!(
                "failed to remove newly created VM {created_id} ({rm_err:#}); remove it manually"
            ),
        };
        bail!("failed to journal newly created VM {created_id} ({save_err:#}); {cleanup_note}");
    }
    Ok(())
}

fn verify_expected_vm_id(state: &LocalVmState, expect_vm_id: Option<&str>) -> Result<()> {
    let Some(expected) = expect_vm_id else {
        return Ok(());
    };
    validate_vm_id(expected).wrap_err("invalid --expect-vm-id")?;
    ensure!(
        !state.vm_id.is_empty() && state.vm_id == expected,
        "vm_id_mismatch for VM `{}`: expected `{expected}`, recorded `{}`",
        state.name,
        if state.vm_id.is_empty() {
            "<none (creating)>"
        } else {
            &state.vm_id
        }
    );
    Ok(())
}

/// Start one locally recorded VM.
pub fn start_local_vm<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    name: &str,
) -> Result<StartLocalVmOutput> {
    start_local_vm_with_expected_id(store, vmm, name, None)
}

/// Start one locally recorded VM, verifying an optional `--expect-vm-id` under the per-VM lock
/// before issuing any VMM RPC.
pub fn start_local_vm_with_expected_id<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    name: &str,
    expect_vm_id: Option<&str>,
) -> Result<StartLocalVmOutput> {
    if let Some(expected) = expect_vm_id {
        validate_vm_id(expected).wrap_err("invalid --expect-vm-id")?;
    }
    let guard = store.lock_vm(name)?;
    let mut state = store.load_locked(&guard, name)?;
    verify_expected_vm_id(&state, expect_vm_id)?;
    ensure!(
        state.phase == LocalVmPhase::Ready,
        "VM `{name}` ({}) is in phase `{:?}`, not `Ready`; resume `outbe-up vm create` or clean up with `outbe-up vm remove --name {name}`",
        state.vm_id,
        state.phase
    );

    let info = require_owned_vmm_info(vmm, &state, None)?;
    if info.running {
        return Ok(StartLocalVmOutput {
            name: state.name,
            vm_id: state.vm_id,
            already_running: true,
            vmm_status: info.status,
        });
    }

    state.record_step("starting", None);
    store.save_locked(&guard, &state)?;

    vmm.start_vm(&state.vm_id)
        .wrap_err_with(|| format!("start VM `{name}` ({})", state.vm_id))?;

    let post_status = vmm
        .get_info(&state.vm_id)
        .ok()
        .and_then(|resp| resp.info)
        .map(|i| i.status)
        .unwrap_or_else(|| VmInfo::STATUS_RUNNING.to_owned());

    state.record_step("started", Some(format!("status={post_status}")));
    store.save_locked(&guard, &state)?;

    Ok(StartLocalVmOutput {
        name: state.name,
        vm_id: state.vm_id,
        already_running: false,
        vmm_status: post_status,
    })
}

/// Stop one locally recorded VM gracefully (`ShutdownVm`), or forcefully (`StopVm`) only when
/// `force == true`.
///
/// The total `timeout` bounds the entire operation, including per-VM lock acquisition, initial
/// `GetInfo`, the stop RPC, and all polling calls. When `force == true` and the VM is already
/// `exited` (guest powered off while dstack's `.started` flag remains set), `StopVm` is still
/// sent so dstack clears `.started`, disables automatic restart, and transitions the VM to
/// `stopped`. Never falls back from graceful shutdown to forced stop.
pub fn stop_local_vm<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    name: &str,
    force: bool,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<StopLocalVmOutput> {
    stop_local_vm_with_expected_id(store, vmm, name, None, force, timeout, poll_interval)
}

/// Same as [`stop_local_vm`], verifying an optional `--expect-vm-id` under the per-VM lock before
/// issuing any VMM RPC.
pub fn stop_local_vm_with_expected_id<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    name: &str,
    expect_vm_id: Option<&str>,
    force: bool,
    timeout: Duration,
    poll_interval: Duration,
) -> Result<StopLocalVmOutput> {
    if let Some(expected) = expect_vm_id {
        validate_vm_id(expected).wrap_err("invalid --expect-vm-id")?;
    }
    let deadline = Instant::now() + timeout;
    let guard = store.lock_vm_with_options(name, Some(deadline), None)?;
    let mut state = store.load_locked(&guard, name)?;
    verify_expected_vm_id(&state, expect_vm_id)?;
    ensure!(
        state.phase != LocalVmPhase::Creating,
        "VM `{name}` was interrupted during creation (no vm_id recorded); resume `outbe-up vm create` or clean up with `outbe-up vm remove --name {name}`"
    );
    let info = require_owned_vmm_info(vmm, &state, Some(deadline))?;

    if !force && info.is_stopped() {
        return Ok(StopLocalVmOutput {
            name: state.name,
            vm_id: state.vm_id,
            already_stopped: true,
            forced: false,
            cleared_started_flag: false,
            vmm_status: info.status,
            warning: None,
        });
    }
    if force && info.is_administratively_stopped() {
        return Ok(StopLocalVmOutput {
            name: state.name,
            vm_id: state.vm_id,
            already_stopped: true,
            forced: false,
            cleared_started_flag: false,
            vmm_status: info.status,
            warning: None,
        });
    }

    let was_exited = !info.running && info.status == VmInfo::STATUS_EXITED;
    let warning = force.then(|| FORCE_STOP_DKG_WARNING.to_owned());
    if force {
        state.record_step(
            if was_exited {
                "clearing-started-flag"
            } else {
                "force-stopping"
            },
            None,
        );
        store.save_locked(&guard, &state)?;
        vmm.stop_vm_with_deadline(&state.vm_id, Some(deadline))
            .wrap_err_with(|| format!("force-stop VM `{name}` ({})", state.vm_id))?;
    } else {
        state.record_step("shutting-down", None);
        store.save_locked(&guard, &state)?;
        vmm.shutdown_vm_with_deadline(&state.vm_id, Some(deadline))
            .wrap_err_with(|| format!("gracefully shut down VM `{name}` ({})", state.vm_id))?;
    }

    let final_status = wait_until_stopped(vmm, &state, deadline, poll_interval, force)?;

    state.record_step(
        if force {
            if was_exited {
                "cleared-started-flag"
            } else {
                "force-stopped"
            }
        } else {
            "stopped"
        },
        Some(format!("status={final_status}")),
    );
    store.save_locked(&guard, &state)?;

    Ok(StopLocalVmOutput {
        name: state.name,
        vm_id: state.vm_id,
        already_stopped: false,
        forced: force,
        cleared_started_flag: force && was_exited,
        vmm_status: final_status,
        warning,
    })
}

/// Query status for one locally recorded VM, reading DHCP leases best-effort for IP lookup.
pub fn status_local_vm<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    name: &str,
    dhcp_leases_path: &Path,
) -> Result<LocalVmStatusOutput> {
    status_local_vm_with_expected_id(store, vmm, name, None, dhcp_leases_path)
}

/// Same as [`status_local_vm`], verifying an optional `--expect-vm-id` under the per-VM lock
/// before issuing any VMM RPC.
pub fn status_local_vm_with_expected_id<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    name: &str,
    expect_vm_id: Option<&str>,
    dhcp_leases_path: &Path,
) -> Result<LocalVmStatusOutput> {
    if let Some(expected) = expect_vm_id {
        validate_vm_id(expected).wrap_err("invalid --expect-vm-id")?;
    }
    let guard = store.lock_vm(name)?;
    let state = store.load_locked(&guard, name)?;
    verify_expected_vm_id(&state, expect_vm_id)?;
    let leases_content = fs::read_to_string(dhcp_leases_path).ok();
    query_status_from_state(vmm, &state, leases_content.as_deref())
}

/// Query status for all locally recorded VMs in `store`.
pub fn status_all_local_vms<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    dhcp_leases_path: &Path,
) -> Result<Vec<LocalVmStatusOutput>> {
    let names = store.list_vm_names()?;
    let leases_content = fs::read_to_string(dhcp_leases_path).ok();
    let mut results = Vec::with_capacity(names.len());
    for name in names {
        let guard = store.lock_vm(&name)?;
        if let Some(state) = store.load_optional_locked(&guard, &name)? {
            results.push(query_status_from_state(
                vmm,
                &state,
                leases_content.as_deref(),
            )?);
        }
    }
    Ok(results)
}

fn query_status_from_state<T: VmmTransport>(
    vmm: &VmmClient<T>,
    state: &LocalVmState,
    leases_content: Option<&str>,
) -> Result<LocalVmStatusOutput> {
    let service_ip = state.intended_ip;
    if state.phase == LocalVmPhase::Creating && state.vm_id.is_empty() {
        return Ok(LocalVmStatusOutput {
            name: state.name.clone(),
            vm_id: String::new(),
            owner_tag: state.owner_tag.clone(),
            phase: state.phase,
            vmm_status: "creating".to_owned(),
            running: false,
            uptime: String::new(),
            mac: state.mac.clone(),
            ip: service_ip,
            service_ip,
            dhcp_ip: None,
            intended_ip: state.intended_ip,
            image_name: state.image_name.clone(),
            os_image_hash: state.os_image_hash.clone(),
            disk_source_sha256: state.disk_source_sha256.clone(),
            app_id: state.app_id.clone(),
            no_tee: state.no_tee,
            vcpu: state.vcpu,
            memory_gib: state.memory_gib,
            disk_gib: state.disk_gib,
            bridge: state.bridge.clone(),
            boot_error: None,
            error: Some(
                "VM creation was interrupted before VMM ID was recorded; resume `vm create` or clean up with `vm remove`"
                    .to_owned(),
            ),
        });
    }

    let resp = vmm.get_info(&state.vm_id)?;
    if resp.is_gone() {
        let dhcp_ip = leases_content.and_then(|raw| lookup_lease_ip_by_mac(raw, &state.mac));
        return Ok(LocalVmStatusOutput {
            name: state.name.clone(),
            vm_id: state.vm_id.clone(),
            owner_tag: state.owner_tag.clone(),
            phase: state.phase,
            vmm_status: "missing".to_owned(),
            running: false,
            uptime: String::new(),
            mac: state.mac.clone(),
            ip: service_ip.or(dhcp_ip),
            service_ip,
            dhcp_ip,
            intended_ip: state.intended_ip,
            image_name: state.image_name.clone(),
            os_image_hash: state.os_image_hash.clone(),
            disk_source_sha256: state.disk_source_sha256.clone(),
            app_id: state.app_id.clone(),
            no_tee: state.no_tee,
            vcpu: state.vcpu,
            memory_gib: state.memory_gib,
            disk_gib: state.disk_gib,
            bridge: state.bridge.clone(),
            boot_error: None,
            error: Some(format!("VM {} not found on local VMM", state.vm_id)),
        });
    }

    let info = resp.info.unwrap();
    let mac = extract_primary_mac(&info).unwrap_or_else(|_| state.mac.clone());
    let dhcp_ip = leases_content.and_then(|raw| lookup_lease_ip_by_mac(raw, &mac));
    let image_name = info
        .image_name()
        .map(str::to_owned)
        .unwrap_or_else(|| state.image_name.clone());
    let boot_error = (!info.boot_error.is_empty()).then_some(info.boot_error);
    let error = (info.name != state.name).then(|| {
        format!(
            "VMM reports name `{}` for {}, expected `{}`",
            info.name, state.vm_id, state.name
        )
    });

    Ok(LocalVmStatusOutput {
        name: state.name.clone(),
        vm_id: state.vm_id.clone(),
        owner_tag: state.owner_tag.clone(),
        phase: state.phase,
        vmm_status: info.status,
        running: info.running,
        uptime: info.uptime,
        mac,
        ip: service_ip.or(dhcp_ip),
        service_ip,
        dhcp_ip,
        intended_ip: state.intended_ip,
        image_name,
        os_image_hash: state.os_image_hash.clone(),
        disk_source_sha256: state.disk_source_sha256.clone(),
        app_id: state.app_id.clone(),
        no_tee: state.no_tee,
        vcpu: state.vcpu,
        memory_gib: state.memory_gib,
        disk_gib: state.disk_gib,
        bridge: state.bridge.clone(),
        boot_error,
        error,
    })
}

/// Read the tail of the VM serial console log for one locally recorded VM.
pub fn logs_local_vm<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    name: &str,
    lines: usize,
) -> Result<LocalVmLogsOutput> {
    logs_local_vm_with_expected_id(store, vmm, name, None, lines)
}

/// Same as [`logs_local_vm`], verifying an optional `--expect-vm-id` under the per-VM lock before
/// reading the serial log.
pub fn logs_local_vm_with_expected_id<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    name: &str,
    expect_vm_id: Option<&str>,
    lines: usize,
) -> Result<LocalVmLogsOutput> {
    if let Some(expected) = expect_vm_id {
        validate_vm_id(expected).wrap_err("invalid --expect-vm-id")?;
    }
    let guard = store.lock_vm(name)?;
    let state = store.load_locked(&guard, name)?;
    verify_expected_vm_id(&state, expect_vm_id)?;
    ensure!(
        state.phase != LocalVmPhase::Creating && !state.vm_id.is_empty(),
        "VM `{name}` has no recorded VMM ID yet (phase: {:?})",
        state.phase
    );
    let log = vmm.serial_log_tail(&state.vm_id, lines)?;
    Ok(LocalVmLogsOutput {
        name: state.name,
        vm_id: state.vm_id,
        lines,
        log,
    })
}

/// Options controlling `remove_local_vm` behavior.
#[derive(Clone, Debug)]
pub struct RemoveLocalVmOptions {
    pub force: bool,
    /// Allow deleting an unresolved `Creating` intent when no token-matched VM appears within the
    /// grace polling window.
    pub forget_intent: bool,
    /// Optional expected VM UUID verified under the per-VM exclusive lock before any VMM RPC.
    pub expect_vm_id: Option<String>,
    pub timeout: Duration,
    pub recovery_grace: Duration,
    pub poll_interval: Duration,
}

impl Default for RemoveLocalVmOptions {
    fn default() -> Self {
        Self {
            force: false,
            forget_intent: false,
            expect_vm_id: None,
            timeout: Duration::from_secs(120),
            recovery_grace: DEFAULT_CREATE_RECOVERY_GRACE,
            poll_interval: DEFAULT_POLL_INTERVAL,
        }
    }
}

/// Remove one locally recorded VM, waiting for asynchronous VMM removal before deleting the
/// state file. Guest OS images are never deleted.
///
/// If the VM is in `Creating` phase (interrupted before `CreateVm` returned an ID), polls
/// filtered `Status` + `GetInfo` during the grace window and verifies both
/// `CREATE_TOKEN_ANNOTATION` and journaled config before removing; if still absent, refuses to
/// delete the intent unless `options.forget_intent == true`. The total `options.timeout` bounds
/// lock acquisition, recovery grace polling, and all RPCs.
pub fn remove_local_vm<T: VmmTransport>(
    store: &LocalVmStore,
    vmm: &VmmClient<T>,
    name: &str,
    options: RemoveLocalVmOptions,
) -> Result<RemoveLocalVmOutput> {
    if let Some(expected) = options.expect_vm_id.as_deref() {
        validate_vm_id(expected).wrap_err("invalid --expect-vm-id")?;
    }
    let deadline = Instant::now() + options.timeout;
    let guard = store.lock_vm_with_options(name, Some(deadline), None)?;
    let mut state = store.load_locked(&guard, name)?;
    verify_expected_vm_id(&state, options.expect_vm_id.as_deref())?;

    // Recover an interrupted `Creating` record strictly by our unique annotation token + config.
    if state.phase == LocalVmPhase::Creating && state.vm_id.is_empty() {
        let token = state
            .create_token
            .clone()
            .ok_or_else(|| eyre::eyre!("creating state missing create_token"))?;
        let grace_deadline = compute_grace_deadline(options.recovery_grace, Some(deadline));
        match poll_for_owned_creating_vm(
            vmm,
            &state,
            &token,
            grace_deadline,
            options.poll_interval,
            None,
        )? {
            Some(recovered) => {
                verify_adopted_vm_matches_state(&recovered, &state)?;
                state.vm_id = recovered.id;
                state.phase = LocalVmPhase::Removing;
                state.record_step("recovered-creating-for-remove", None);
                store.save_locked(&guard, &state)?;
            }
            None => {
                ensure!(
                    options.forget_intent,
                    "VM `{name}` has an unresolved `Creating` intent (token {token}) and did not appear on the local VMM within {}s; refusing to delete the intent because an earlier CreateVm may still complete. Re-run with `--forget-intent` once you have confirmed on the VMM that no VM was created",
                    options.recovery_grace.as_secs()
                );
                let state_file = store.delete_locked(&guard, name)?;
                return Ok(RemoveLocalVmOutput {
                    name: state.name,
                    vm_id: String::new(),
                    state_file,
                    forced: false,
                    forgot_intent: true,
                    warning: None,
                });
            }
        }
    } else {
        state.phase = LocalVmPhase::Removing;
        state.record_step("removing", None);
        store.save_locked(&guard, &state)?;
    }

    let mut warning: Option<String> = None;

    let info_resp = vmm.get_info_with_deadline(&state.vm_id, Some(deadline))?;
    if !info_resp.is_gone() {
        let info = info_resp.info.unwrap();
        ensure!(
            info.name == state.name,
            "refusing to remove VM {}: VMM reports name `{}`, expected `{}`",
            state.vm_id,
            info.name,
            state.name
        );

        if info.running && info.status != VmInfo::STATUS_REMOVING {
            if options.force {
                warning = Some(FORCE_STOP_DKG_WARNING.to_owned());
                state.record_step("force-stopping-for-remove", None);
                store.save_locked(&guard, &state)?;
                vmm.stop_vm_with_deadline(&state.vm_id, Some(deadline))
                    .wrap_err_with(|| format!("force-stop VM `{name}` ({})", state.vm_id))?;
            } else {
                state.record_step("shutting-down-for-remove", None);
                store.save_locked(&guard, &state)?;
                vmm.shutdown_vm_with_deadline(&state.vm_id, Some(deadline))
                    .wrap_err_with(|| {
                        format!("gracefully shut down VM `{name}` ({})", state.vm_id)
                    })?;
            }
            wait_until_stopped_or_gone(
                vmm,
                &state,
                deadline,
                options.poll_interval,
                options.force,
            )?;
        }

        let post_stop = vmm.get_info_with_deadline(&state.vm_id, Some(deadline))?;
        if !post_stop.is_gone() {
            let status = post_stop
                .info
                .as_ref()
                .map(|i| i.status.as_str())
                .unwrap_or("");
            if status != VmInfo::STATUS_REMOVING {
                vmm.remove_vm_with_deadline(&state.vm_id, Some(deadline))
                    .wrap_err_with(|| format!("remove VM `{name}` ({})", state.vm_id))?;
            }
            wait_until_gone(vmm, &state, deadline, options.poll_interval)?;
        }
    }

    let state_file = store.delete_locked(&guard, name)?;
    Ok(RemoveLocalVmOutput {
        name: state.name,
        vm_id: state.vm_id,
        state_file,
        forced: options.force && warning.is_some(),
        forgot_intent: false,
        warning,
    })
}

fn require_owned_vmm_info<T: VmmTransport>(
    vmm: &VmmClient<T>,
    state: &LocalVmState,
    deadline: Option<Instant>,
) -> Result<VmInfo> {
    let resp = vmm.get_info_with_deadline(&state.vm_id, deadline)?;
    ensure!(
        !resp.is_gone(),
        "VM `{}` ({}) was not found on local VMM",
        state.name,
        state.vm_id
    );
    let info = resp.info.unwrap();
    ensure!(
        info.name == state.name,
        "VMM VM {} has unexpected name `{}` (expected `{}`)",
        state.vm_id,
        info.name,
        state.name
    );
    Ok(info)
}

fn wait_until_stopped<T: VmmTransport>(
    vmm: &VmmClient<T>,
    state: &LocalVmState,
    deadline: Instant,
    poll_interval: Duration,
    force: bool,
) -> Result<String> {
    loop {
        let resp = vmm.get_info_with_deadline(&state.vm_id, Some(deadline))?;
        ensure!(
            !resp.is_gone(),
            "VM `{}` ({}) disappeared from local VMM while waiting for stop",
            state.name,
            state.vm_id
        );
        let info = resp.info.unwrap();
        let reached = if force {
            info.is_administratively_stopped()
        } else {
            info.is_stopped()
        };
        if reached {
            return Ok(info.status);
        }
        if Instant::now() >= deadline {
            if force {
                bail!(
                    "VM `{}` ({}) did not reach stopped state before timeout (last status: {}, running: {})",
                    state.name,
                    state.vm_id,
                    info.status,
                    info.running
                );
            }
            bail!(
                "VM `{}` ({}) did not finish graceful shutdown before timeout (last status: {}, running: {}); did not force-stop (use --force only if you accept the pending-DKG snapshot risk in /home/ubuntu/tdx/12-defect-pending-dkg-snapshot.md)",
                state.name,
                state.vm_id,
                info.status,
                info.running
            );
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        thread::sleep(remaining.min(poll_interval));
    }
}

fn wait_until_stopped_or_gone<T: VmmTransport>(
    vmm: &VmmClient<T>,
    state: &LocalVmState,
    deadline: Instant,
    poll_interval: Duration,
    force: bool,
) -> Result<()> {
    loop {
        let resp = vmm.get_info_with_deadline(&state.vm_id, Some(deadline))?;
        if resp.is_gone() {
            return Ok(());
        }
        let info = resp.info.unwrap();
        let reached = if force {
            info.is_administratively_stopped()
        } else {
            info.is_stopped()
        };
        if reached {
            return Ok(());
        }
        if Instant::now() >= deadline {
            if force {
                bail!(
                    "VM `{}` ({}) did not stop before removal timeout (keeping state file for retry; last status: {})",
                    state.name,
                    state.vm_id,
                    info.status
                );
            }
            bail!(
                "VM `{}` ({}) did not finish graceful shutdown before removal timeout (keeping state file for retry; last status: {}); re-run with --force only if you accept the pending-DKG snapshot risk",
                state.name,
                state.vm_id,
                info.status
            );
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        thread::sleep(remaining.min(poll_interval));
    }
}

fn wait_until_gone<T: VmmTransport>(
    vmm: &VmmClient<T>,
    state: &LocalVmState,
    deadline: Instant,
    poll_interval: Duration,
) -> Result<()> {
    loop {
        let resp = vmm.get_info_with_deadline(&state.vm_id, Some(deadline))?;
        if resp.is_gone() {
            return Ok(());
        }
        let status = resp
            .info
            .map(|i| i.status)
            .unwrap_or_else(|| "unknown".to_owned());
        if Instant::now() >= deadline {
            bail!(
                "VM `{}` ({}) did not finish asynchronous VMM removal before timeout (keeping state file for retry; last status: {status})",
                state.name,
                state.vm_id
            );
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        thread::sleep(remaining.min(poll_interval));
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{Arc, Mutex},
        thread,
    };

    use serde_json::{json, Value};

    use super::*;

    const UUID_OWNED: &str = "550e8400-e29b-41d4-a716-446655440010";
    const UUID_FOREIGN: &str = "550e8400-e29b-41d4-a716-446655440099";

    fn make_image_and_disk(dir: &Path) -> (PathBuf, PathBuf) {
        let image_dir = dir.join("guest-image");
        fs::create_dir_all(&image_dir).unwrap();
        let digest = "0123456789abcdef".repeat(4);
        fs::write(image_dir.join("metadata.json"), "{}").unwrap();
        fs::write(image_dir.join("digest.txt"), format!("{digest}\n")).unwrap();
        let raw_disk = dir.join("dev.img");
        fs::write(&raw_disk, b"raw-disk-placeholder").unwrap();
        (image_dir, raw_disk)
    }

    #[derive(Default)]
    struct FakeVmmState {
        vms: BTreeMap<String, FakeVm>,
        calls: Vec<String>,
        status_requests: Vec<Value>,
        remove_polls_before_gone: usize,
        drop_create_response_once: bool,
        delay_registration_polls: usize,
        pending_delayed_vm: Option<FakeVm>,
    }

    #[derive(Clone)]
    struct FakeVm {
        id: String,
        name: String,
        image: String,
        vcpu: u32,
        memory: u32,
        app_id: String,
        status: String,
        running: bool,
        stopped_flag: bool,
        boot_progress: String,
        mac: String,
        annotations: BTreeMap<String, String>,
        remove_polls_left: usize,
    }

    #[derive(Clone)]
    struct StubVmm {
        state: Arc<Mutex<FakeVmmState>>,
    }

    impl StubVmm {
        fn new() -> Self {
            Self {
                state: Arc::new(Mutex::new(FakeVmmState::default())),
            }
        }
    }

    impl VmmTransport for StubVmm {
        fn prpc_post(&self, method: &str, payload: &[u8]) -> Result<String> {
            let req: Value = serde_json::from_slice(payload)?;
            let mut guard = self.state.lock().unwrap();
            guard.calls.push(method.to_owned());
            match method {
                "Status" => {
                    guard.status_requests.push(req.clone());
                    // Simulate delayed server-side VM registration appearing after N Status polls.
                    if let Some(delayed) = guard.pending_delayed_vm.clone() {
                        if guard.delay_registration_polls == 0 {
                            guard.pending_delayed_vm = None;
                            guard.vms.insert(delayed.id.clone(), delayed);
                        } else {
                            guard.delay_registration_polls -= 1;
                        }
                    }

                    let brief = req["brief"].as_bool().unwrap_or(false);
                    let keyword = req["keyword"].as_str().unwrap_or("");
                    let page = req["page"].as_u64().unwrap_or(0) as usize;
                    let page_size = req["page_size"].as_u64().unwrap_or(0) as usize;

                    let filtered: Vec<&FakeVm> = guard
                        .vms
                        .values()
                        .filter(|vm| {
                            keyword.is_empty()
                                || vm.name.contains(keyword)
                                || vm.id.contains(keyword)
                                || vm.app_id.contains(keyword)
                                || vm.image.contains(keyword)
                        })
                        .collect();
                    let total = filtered.len() as u32;
                    let page_slice: Vec<&FakeVm> = if page == 0 || page_size == 0 {
                        filtered
                    } else {
                        filtered
                            .into_iter()
                            .skip((page - 1) * page_size)
                            .take(page_size)
                            .collect()
                    };
                    let vms: Vec<Value> = page_slice
                        .into_iter()
                        .map(|vm| {
                            let config = if brief {
                                Value::Null
                            } else {
                                json!({
                                    "image": vm.image,
                                    "vcpu": vm.vcpu,
                                    "memory": vm.memory,
                                    "app_id": vm.app_id,
                                    "stopped": vm.stopped_flag,
                                    "annotations": vm.annotations,
                                })
                            };
                            json!({
                                "id": vm.id,
                                "name": vm.name,
                                "status": vm.status,
                                "running": vm.running,
                                "app_id": vm.app_id,
                                "uptime": if vm.running { "1m 5s" } else { "" },
                                "boot_progress": vm.boot_progress,
                                "configuration": config,
                                "interfaces": [{ "mode": "bridge", "mac": vm.mac }],
                            })
                        })
                        .collect();
                    Ok(json!({ "vms": vms, "total": total }).to_string())
                }
                "CreateVm" => {
                    let name = req["name"].as_str().unwrap().to_owned();
                    let image = req["image"].as_str().unwrap().to_owned();
                    let vcpu = req["vcpu"].as_u64().unwrap() as u32;
                    let memory = req["memory"].as_u64().unwrap() as u32;
                    let app_id = req["app_id"].as_str().unwrap_or("").to_owned();
                    let annotations: BTreeMap<String, String> = req["annotations"]
                        .as_object()
                        .map(|m| {
                            m.iter()
                                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                                .collect()
                        })
                        .unwrap_or_default();
                    let id = UUID_OWNED.to_owned();
                    let remove_polls_left = guard.remove_polls_before_gone;
                    let vm = FakeVm {
                        id: id.clone(),
                        name,
                        image,
                        vcpu,
                        memory,
                        app_id,
                        status: VmInfo::STATUS_STOPPED.into(),
                        running: false,
                        stopped_flag: true,
                        boot_progress: String::new(),
                        mac: "52:54:00:aa:bb:cc".into(),
                        annotations,
                        remove_polls_left,
                    };
                    if guard.delay_registration_polls > 0 {
                        guard.pending_delayed_vm = Some(vm);
                    } else {
                        guard.vms.insert(id.clone(), vm);
                    }
                    if guard.drop_create_response_once {
                        guard.drop_create_response_once = false;
                        return Err(eyre::eyre!("simulated lost CreateVm response (EOF)"));
                    }
                    Ok(json!({ "id": id }).to_string())
                }
                "GetInfo" => {
                    let id = req["id"].as_str().unwrap();
                    let mut remove_now = false;
                    let resp = match guard.vms.get_mut(id) {
                        Some(vm) => {
                            if vm.status == VmInfo::STATUS_REMOVING {
                                if vm.remove_polls_left == 0 {
                                    remove_now = true;
                                    json!({ "found": false, "info": null })
                                } else {
                                    vm.remove_polls_left -= 1;
                                    json!({
                                        "found": true,
                                        "info": {
                                            "id": vm.id,
                                            "name": vm.name,
                                            "status": vm.status,
                                            "app_id": vm.app_id,
                                            "running": false,
                                            "uptime": "",
                                            "boot_progress": vm.boot_progress,
                                            "configuration": {
                                                "image": vm.image,
                                                "vcpu": vm.vcpu,
                                                "memory": vm.memory,
                                                "app_id": vm.app_id,
                                                "stopped": vm.stopped_flag,
                                                "annotations": vm.annotations,
                                            },
                                            "interfaces": [{ "mode": "bridge", "mac": vm.mac }],
                                        }
                                    })
                                }
                            } else {
                                json!({
                                    "found": true,
                                    "info": {
                                        "id": vm.id,
                                        "name": vm.name,
                                        "status": vm.status,
                                        "app_id": vm.app_id,
                                        "running": vm.running,
                                        "uptime": if vm.running { "1m 5s" } else { "" },
                                        "boot_progress": vm.boot_progress,
                                        "configuration": {
                                            "image": vm.image,
                                            "vcpu": vm.vcpu,
                                            "memory": vm.memory,
                                            "app_id": vm.app_id,
                                            "stopped": vm.stopped_flag,
                                            "annotations": vm.annotations,
                                        },
                                        "interfaces": [{ "mode": "bridge", "mac": vm.mac }],
                                    }
                                })
                            }
                        }
                        None => json!({ "found": false, "info": null }),
                    };
                    if remove_now {
                        guard.vms.remove(id);
                    }
                    Ok(resp.to_string())
                }
                "StartVm" => {
                    let id = req["id"].as_str().unwrap();
                    let vm = guard.vms.get_mut(id).unwrap();
                    vm.running = true;
                    vm.stopped_flag = false;
                    vm.boot_progress = "done".into();
                    vm.status = VmInfo::STATUS_RUNNING.into();
                    Ok("{}".into())
                }
                "ShutdownVm" => {
                    let id = req["id"].as_str().unwrap();
                    let vm = guard.vms.get_mut(id).unwrap();
                    vm.running = false;
                    // Exact dstack VMM status after graceful ShutdownVm (.started remains true).
                    vm.status = VmInfo::STATUS_EXITED.into();
                    Ok("{}".into())
                }
                "StopVm" => {
                    let id = req["id"].as_str().unwrap();
                    let vm = guard.vms.get_mut(id).unwrap();
                    vm.running = false;
                    vm.stopped_flag = true;
                    vm.status = VmInfo::STATUS_STOPPED.into();
                    Ok("{}".into())
                }
                "RemoveVm" => {
                    let id = req["id"].as_str().unwrap();
                    let vm = guard.vms.get_mut(id).unwrap();
                    vm.running = false;
                    vm.status = VmInfo::STATUS_REMOVING.into();
                    Ok("{}".into())
                }
                other => Err(eyre::eyre!("unexpected method {other}")),
            }
        }

        fn read_serial_log(&self, vm_id: &str, lines: usize) -> Result<String> {
            Ok(format!("boot line for {vm_id} ({lines})\n"))
        }
    }

    fn base_opts(dir: &Path, image_dir: &Path, raw_disk: &Path) -> CreateLocalVmOptions {
        let vmm_home = dir.join("vmm");
        CreateLocalVmOptions {
            name: "dev".into(),
            owner_tag: None,
            image_dir: image_dir.to_path_buf(),
            image_name: None,
            flavor: "dev".into(),
            disk_source: raw_disk.to_path_buf(),
            disk_staging_name: None,
            consume_staging: false,
            vcpu: 3,
            memory_gib: 5,
            disk_gib: 10,
            bridge: "obr0".into(),
            app_compose_path: None,
            intended_ip: Some(Ipv4Addr::new(10, 77, 0, 88)),
            no_tee: false,
            vmm_socket: vmm_home.join("run/vmm.sock"),
            vmm_home,
            retry_create: false,
            timeout: None,
            recovery_grace: Duration::from_millis(80),
            poll_interval: Duration::from_millis(10),
        }
    }

    fn fast_remove_opts() -> RemoveLocalVmOptions {
        RemoveLocalVmOptions {
            force: false,
            forget_intent: false,
            expect_vm_id: None,
            timeout: Duration::from_secs(2),
            recovery_grace: Duration::from_millis(80),
            poll_interval: Duration::from_millis(10),
        }
    }

    #[test]
    fn f1_creating_grace_window_refuses_blind_retry_and_forget_without_explicit_flags() {
        let dir = tempfile::tempdir().unwrap();
        let (image_dir, raw_disk) = make_image_and_disk(dir.path());
        let store = LocalVmStore::new(dir.path().join("state"));
        let stub = StubVmm::new();
        let vmm = VmmClient::new(stub.clone(), "stub");
        let opts = base_opts(dir.path(), &image_dir, &raw_disk);

        // 1. Seed a `Creating` intent whose CreateVm never registered on the server.
        let guard = store.lock_vm("dev").unwrap();
        store
            .save_locked(
                &guard,
                &LocalVmState {
                    version: LOCAL_VM_STATE_VERSION,
                    name: "dev".into(),
                    vm_id: String::new(),
                    create_token: Some("b".repeat(32)),
                    owner_tag: None,
                    vmm_socket: opts.vmm_socket.clone(),
                    vmm_home: opts.vmm_home.clone(),
                    image_name: "outbe-vm-dev-0123456789abcdef".into(),
                    os_image_hash: "0123456789abcdef".repeat(4),
                    disk_source: raw_disk.clone(),
                    disk_source_sha256: Some(sha256_sparse_file(&raw_disk).unwrap()),
                    vcpu: 3,
                    memory_gib: 5,
                    disk_gib: 10,
                    bridge: "obr0".into(),
                    intended_ip: Some(Ipv4Addr::new(10, 77, 0, 88)),
                    mac: String::new(),
                    app_id: calc_app_id(APP_COMPOSE_JSON),
                    no_tee: false,
                    phase: LocalVmPhase::Creating,
                    journal: Vec::new(),
                },
            )
            .unwrap();
        drop(guard);

        // Retry WITHOUT --retry-create must poll grace window, NOT issue CreateVm, and refuse.
        let err = create_local_vm_with_disk_stager(&store, &vmm, &opts, |home, id, _, _| {
            Ok(home.join("vm").join(id).join("hda.img"))
        })
        .unwrap_err();
        assert!(err.to_string().contains("--retry-create"), "{err}");
        assert!(
            !stub
                .state
                .lock()
                .unwrap()
                .calls
                .contains(&"CreateVm".to_owned()),
            "must NOT resend CreateVm without --retry-create"
        );
        // Verify Status discovery used brief=true, keyword="dev", and pagination (item c).
        let status_reqs = stub.state.lock().unwrap().status_requests.clone();
        assert!(!status_reqs.is_empty());
        for sr in &status_reqs {
            assert_eq!(sr["brief"], json!(true));
            assert_eq!(sr["keyword"], json!("dev"));
            assert_eq!(sr["page_size"], json!(STATUS_DISCOVERY_PAGE_SIZE));
        }

        // Remove WITHOUT --forget-intent must NOT delete the Creating intent.
        let err = remove_local_vm(&store, &vmm, "dev", fast_remove_opts()).unwrap_err();
        assert!(err.to_string().contains("--forget-intent"), "{err}");
        assert!(store.state_path("dev").unwrap().exists());

        // Remove WITH --forget-intent deletes the intent cleanly.
        let mut forget_opts = fast_remove_opts();
        forget_opts.forget_intent = true;
        let removed = remove_local_vm(&store, &vmm, "dev", forget_opts).unwrap();
        assert!(removed.forgot_intent);
        assert!(!store.state_path("dev").unwrap().exists());

        // 2. Delayed registration that arrives during the grace window is adopted cleanly with 1 CreateVm.
        let store2 = LocalVmStore::new(dir.path().join("state-delayed"));
        let stub2 = StubVmm::new();
        {
            let mut g = stub2.state.lock().unwrap();
            g.drop_create_response_once = true;
            g.delay_registration_polls = 2;
        }
        let vmm2 = VmmClient::new(stub2.clone(), "stub2");
        assert!(
            create_local_vm_with_disk_stager(&store2, &vmm2, &opts, |home, id, _, _| {
                Ok(home.join("vm").join(id).join("hda.img"))
            })
            .is_err()
        );

        let mut grace_opts = opts.clone();
        grace_opts.recovery_grace = Duration::from_secs(2);
        let recovered =
            create_local_vm_with_disk_stager(&store2, &vmm2, &grace_opts, |home, id, _, _| {
                Ok(home.join("vm").join(id).join("hda.img"))
            })
            .unwrap();
        assert_eq!(recovered.vm_id, UUID_OWNED);
        assert_eq!(
            stub2
                .state
                .lock()
                .unwrap()
                .calls
                .iter()
                .filter(|c| c.as_str() == "CreateVm")
                .count(),
            1,
            "delayed CreateVm must be adopted during grace window without re-issuing CreateVm"
        );

        // 3. Explicit --retry-create when truly absent issues CreateVm and succeeds.
        let store3 = LocalVmStore::new(dir.path().join("state-retry"));
        let stub3 = StubVmm::new();
        let vmm3 = VmmClient::new(stub3.clone(), "stub3");
        let g3 = store3.lock_vm("dev").unwrap();
        store3
            .save_locked(
                &g3,
                &LocalVmState {
                    version: LOCAL_VM_STATE_VERSION,
                    name: "dev".into(),
                    vm_id: String::new(),
                    create_token: Some("c".repeat(32)),
                    owner_tag: None,
                    vmm_socket: opts.vmm_socket.clone(),
                    vmm_home: opts.vmm_home.clone(),
                    image_name: "outbe-vm-dev-0123456789abcdef".into(),
                    os_image_hash: "0123456789abcdef".repeat(4),
                    disk_source: raw_disk.clone(),
                    disk_source_sha256: Some(sha256_sparse_file(&raw_disk).unwrap()),
                    vcpu: 3,
                    memory_gib: 5,
                    disk_gib: 10,
                    bridge: "obr0".into(),
                    intended_ip: Some(Ipv4Addr::new(10, 77, 0, 88)),
                    mac: String::new(),
                    app_id: calc_app_id(APP_COMPOSE_JSON),
                    no_tee: false,
                    phase: LocalVmPhase::Creating,
                    journal: Vec::new(),
                },
            )
            .unwrap();
        drop(g3);
        let mut retry_opts = opts.clone();
        retry_opts.retry_create = true;
        let created =
            create_local_vm_with_disk_stager(&store3, &vmm3, &retry_opts, |home, id, _, _| {
                Ok(home.join("vm").join(id).join("hda.img"))
            })
            .unwrap();
        assert_eq!(created.vm_id, UUID_OWNED);
        assert_eq!(created.phase, LocalVmPhase::Ready);
    }

    #[test]
    fn f1_adoption_refuses_copied_token_with_mismatched_config() {
        let dir = tempfile::tempdir().unwrap();
        let (image_dir, raw_disk) = make_image_and_disk(dir.path());
        let store = LocalVmStore::new(dir.path().join("state"));
        let stub = StubVmm::new();
        let vmm = VmmClient::new(stub.clone(), "stub");
        let opts = base_opts(dir.path(), &image_dir, &raw_disk);

        let token = "d".repeat(32);
        let g = store.lock_vm("dev").unwrap();
        store
            .save_locked(
                &g,
                &LocalVmState {
                    version: LOCAL_VM_STATE_VERSION,
                    name: "dev".into(),
                    vm_id: String::new(),
                    create_token: Some(token.clone()),
                    owner_tag: None,
                    vmm_socket: opts.vmm_socket.clone(),
                    vmm_home: opts.vmm_home.clone(),
                    image_name: "outbe-vm-dev-0123456789abcdef".into(),
                    os_image_hash: "0123456789abcdef".repeat(4),
                    disk_source: raw_disk.clone(),
                    disk_source_sha256: Some(sha256_sparse_file(&raw_disk).unwrap()),
                    vcpu: 3,
                    memory_gib: 5,
                    disk_gib: 10,
                    bridge: "obr0".into(),
                    intended_ip: Some(Ipv4Addr::new(10, 77, 0, 88)),
                    mac: String::new(),
                    app_id: calc_app_id(APP_COMPOSE_JSON),
                    no_tee: false,
                    phase: LocalVmPhase::Creating,
                    journal: Vec::new(),
                },
            )
            .unwrap();
        drop(g);

        // Foreign VM copied our bearer token + name, but has a different image/vcpu/app_id.
        let mut copied_annotations = BTreeMap::new();
        copied_annotations.insert(CREATE_TOKEN_ANNOTATION.to_owned(), token);
        stub.state.lock().unwrap().vms.insert(
            UUID_FOREIGN.into(),
            FakeVm {
                id: UUID_FOREIGN.into(),
                name: "dev".into(),
                image: "evil-other-image".into(),
                vcpu: 8,
                memory: 5 * 1024,
                app_id: "f".repeat(40),
                status: VmInfo::STATUS_STOPPED.into(),
                running: false,
                stopped_flag: true,
                boot_progress: String::new(),
                mac: "52:54:00:de:ad:01".into(),
                annotations: copied_annotations,
                remove_polls_left: 0,
            },
        );

        let err = create_local_vm_with_disk_stager(&store, &vmm, &opts, |_, _, _, _| {
            panic!("must not stage disk onto mismatched adopted VM")
        })
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("refusing to adopt"), "{msg}");
        assert!(msg.contains("image"), "{msg}");
        assert!(msg.contains("vcpu"), "{msg}");
        assert!(msg.contains("app_id"), "{msg}");

        // `remove` must also refuse to adopt and delete a mismatched copied-token VM.
        let err = remove_local_vm(&store, &vmm, "dev", fast_remove_opts()).unwrap_err();
        assert!(err.to_string().contains("refusing to adopt"), "{err}");
    }

    #[test]
    fn f1_lost_create_response_recovers_via_token_and_never_adopts_foreign_vm() {
        let dir = tempfile::tempdir().unwrap();
        let (image_dir, raw_disk) = make_image_and_disk(dir.path());
        let store = LocalVmStore::new(dir.path().join("state"));
        let stub = StubVmm::new();
        stub.state.lock().unwrap().drop_create_response_once = true;
        let vmm = VmmClient::new(stub.clone(), "stub");
        let opts = base_opts(dir.path(), &image_dir, &raw_disk);

        // Initial CreateVm registers VM on server, then connection drops before response.
        let err = create_local_vm_with_disk_stager(&store, &vmm, &opts, |home, id, _, _| {
            Ok(home.join("vm").join(id).join("hda.img"))
        })
        .unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("simulated lost CreateVm response"), "{msg}");

        // Durable `Creating` state file with create_token already exists on disk.
        let guard = store.lock_vm("dev").unwrap();
        let creating = store.load_locked(&guard, "dev").unwrap();
        assert_eq!(creating.phase, LocalVmPhase::Creating);
        assert!(creating.vm_id.is_empty());
        assert!(creating.create_token.is_some());
        drop(guard);

        // Retrying `create` finds our exact VM by CREATE_TOKEN_ANNOTATION (no second CreateVm)
        // and finishes cleanly.
        let calls_before = stub.state.lock().unwrap().calls.len();
        let created = create_local_vm_with_disk_stager(&store, &vmm, &opts, |home, id, _, _| {
            Ok(home.join("vm").join(id).join("hda.img"))
        })
        .unwrap();
        assert_eq!(created.vm_id, UUID_OWNED);
        assert_eq!(created.phase, LocalVmPhase::Ready);
        let later_calls = &stub.state.lock().unwrap().calls[calls_before..];
        assert!(
            !later_calls.contains(&"CreateVm".to_owned()),
            "retry must adopt token-matched VM instead of calling CreateVm twice: {later_calls:?}"
        );

        // Also verify `remove` cleans up an interrupted `Creating` VM.
        let store2 = LocalVmStore::new(dir.path().join("state-2"));
        let stub2 = StubVmm::new();
        stub2.state.lock().unwrap().drop_create_response_once = true;
        let vmm2 = VmmClient::new(stub2.clone(), "stub2");
        assert!(
            create_local_vm_with_disk_stager(&store2, &vmm2, &opts, |home, id, _, _| {
                Ok(home.join("vm").join(id).join("hda.img"))
            })
            .is_err()
        );
        let removed = remove_local_vm(&store2, &vmm2, "dev", fast_remove_opts()).unwrap();
        assert_eq!(removed.vm_id, UUID_OWNED);
        assert!(stub2.state.lock().unwrap().vms.is_empty());

        // Foreign VM with the same name WITHOUT our token is refused even with --retry-create.
        let store3 = LocalVmStore::new(dir.path().join("state-3"));
        let stub3 = StubVmm::new();
        stub3.state.lock().unwrap().vms.insert(
            UUID_FOREIGN.into(),
            FakeVm {
                id: UUID_FOREIGN.into(),
                name: "dev".into(),
                image: "foreign".into(),
                vcpu: 3,
                memory: 5 * 1024,
                app_id: calc_app_id(APP_COMPOSE_JSON),
                status: VmInfo::STATUS_STOPPED.into(),
                running: false,
                stopped_flag: true,
                boot_progress: String::new(),
                mac: "52:54:00:de:ad:01".into(),
                annotations: BTreeMap::new(),
                remove_polls_left: 0,
            },
        );
        let guard3 = store3.lock_vm("dev").unwrap();
        store3
            .save_locked(
                &guard3,
                &LocalVmState {
                    version: LOCAL_VM_STATE_VERSION,
                    name: "dev".into(),
                    vm_id: String::new(),
                    create_token: Some("a".repeat(32)),
                    owner_tag: None,
                    vmm_socket: opts.vmm_socket.clone(),
                    vmm_home: opts.vmm_home.clone(),
                    image_name: "outbe-vm-dev-0123456789abcdef".into(),
                    os_image_hash: "0123456789abcdef".repeat(4),
                    disk_source: raw_disk.clone(),
                    disk_source_sha256: Some(sha256_sparse_file(&raw_disk).unwrap()),
                    vcpu: 3,
                    memory_gib: 5,
                    disk_gib: 10,
                    bridge: "obr0".into(),
                    intended_ip: Some(Ipv4Addr::new(10, 77, 0, 88)),
                    mac: String::new(),
                    app_id: calc_app_id(APP_COMPOSE_JSON),
                    no_tee: false,
                    phase: LocalVmPhase::Creating,
                    journal: Vec::new(),
                },
            )
            .unwrap();
        drop(guard3);
        let vmm3 = VmmClient::new(stub3.clone(), "stub3");
        let mut retry_opts = opts.clone();
        retry_opts.retry_create = true;
        let err =
            create_local_vm_with_disk_stager(&store3, &vmm3, &retry_opts, |home, id, _, _| {
                Ok(home.join("vm").join(id).join("hda.img"))
            })
            .unwrap_err();
        assert!(
            err.to_string().contains("already exists on the local VMM"),
            "must refuse foreign same-name VM without matching token: {err}"
        );
    }

    #[test]
    fn f8_cancel_aborts_during_lock_wait_without_issuing_vmm_rpcs() {
        let dir = tempfile::tempdir().unwrap();
        let (image_dir, raw_disk) = make_image_and_disk(dir.path());
        let store = LocalVmStore::new(dir.path().join("state"));
        let stub = StubVmm::new();
        let vmm = VmmClient::new(stub.clone(), "stub");
        let opts = base_opts(dir.path(), &image_dir, &raw_disk);

        // Hold the per-VM lock externally.
        let held = store.lock_vm("dev").unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_bg = Arc::clone(&cancel);
        let trigger = thread::spawn(move || {
            thread::sleep(Duration::from_millis(60));
            cancel_bg.store(true, Ordering::SeqCst);
        });

        let started = Instant::now();
        let err = create_local_vm_with_disk_stager_cancellable(
            &store,
            &vmm,
            &opts,
            Some(&cancel),
            |_, _, _, _| panic!("disk stager must not run after cancel"),
        )
        .unwrap_err();
        let elapsed = started.elapsed();
        trigger.join().unwrap();
        drop(held);

        assert!(
            elapsed < Duration::from_secs(1),
            "lock wait did not honor cancellation promptly: {elapsed:?}"
        );
        assert!(err.to_string().contains("interrupted by signal"), "{err}");
        assert!(
            stub.state.lock().unwrap().calls.is_empty(),
            "no VMM RPCs may be sent after pre-lock cancel, saw: {:?}",
            stub.state.lock().unwrap().calls
        );
    }

    #[test]
    fn f2_and_f3_resumed_create_refuses_started_vm_or_changed_immutable_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let (image_dir, raw_disk) = make_image_and_disk(dir.path());
        let store = LocalVmStore::new(dir.path().join("state"));
        let stub = StubVmm::new();
        let vmm = VmmClient::new(stub.clone(), "stub");
        let opts = base_opts(dir.path(), &image_dir, &raw_disk);

        // Fail during disk staging to leave a MacRecorded partial state.
        assert!(
            create_local_vm_with_disk_stager(&store, &vmm, &opts, |_, _, _, _| {
                Err(eyre::eyre!("simulated stage failure"))
            })
            .is_err()
        );

        // F3: retry with a different --disk path must be refused.
        let other_disk = dir.path().join("other.img");
        fs::write(&other_disk, b"different-raw-disk-contents").unwrap();
        let mut diff_disk_opts = opts.clone();
        diff_disk_opts.disk_source = other_disk;
        let err = create_local_vm_with_disk_stager(&store, &vmm, &diff_disk_opts, |_, _, _, _| {
            panic!("disk stager must not run on mismatched disk")
        })
        .unwrap_err();
        assert!(err.to_string().contains("disk_source"), "{err}");

        // F3: mutating bytes at the SAME --disk path must be refused via recorded SHA-256.
        let original_bytes = fs::read(&raw_disk).unwrap();
        fs::write(&raw_disk, b"mutated-raw-disk-bytes").unwrap();
        let err = create_local_vm_with_disk_stager(&store, &vmm, &opts, |_, _, _, _| {
            panic!("disk stager must not run when raw disk sha256 changed")
        })
        .unwrap_err();
        assert!(err.to_string().contains("disk_source_sha256"), "{err}");
        fs::write(&raw_disk, original_bytes).unwrap();

        // F3: retry with a different --ip must be refused.
        let mut diff_ip_opts = opts.clone();
        diff_ip_opts.intended_ip = Some(Ipv4Addr::new(10, 77, 0, 99));
        let err = create_local_vm_with_disk_stager(&store, &vmm, &diff_ip_opts, |_, _, _, _| {
            panic!("disk stager must not run on mismatched intended_ip")
        })
        .unwrap_err();
        assert!(err.to_string().contains("intended_ip"), "{err}");

        // F2: external start before retry must refuse staging hda.img.
        {
            let mut g = stub.state.lock().unwrap();
            let vm = g.vms.get_mut(UUID_OWNED).unwrap();
            vm.running = true;
            vm.stopped_flag = false;
            vm.status = VmInfo::STATUS_RUNNING.into();
        }
        let err = create_local_vm_with_disk_stager(&store, &vmm, &opts, |_, _, _, _| {
            panic!("disk stager must not run on a running VM")
        })
        .unwrap_err();
        assert!(
            err.to_string().contains("refusing to stage hda.img"),
            "{err}"
        );

        // F2: even if stopped again after booting (`boot_progress` set), refuse staging hda.img.
        {
            let mut g = stub.state.lock().unwrap();
            let vm = g.vms.get_mut(UUID_OWNED).unwrap();
            vm.running = false;
            vm.stopped_flag = true;
            vm.status = VmInfo::STATUS_STOPPED.into();
            vm.boot_progress = "done".into();
        }
        let err = create_local_vm_with_disk_stager(&store, &vmm, &opts, |_, _, _, _| {
            panic!("disk stager must not run on a previously booted VM")
        })
        .unwrap_err();
        assert!(err.to_string().contains("already booted"), "{err}");
    }

    #[test]
    fn f6_force_stop_on_exited_vm_sends_stop_vm_to_clear_started_flag() {
        let dir = tempfile::tempdir().unwrap();
        let (image_dir, raw_disk) = make_image_and_disk(dir.path());
        let store = LocalVmStore::new(dir.path().join("state"));
        let stub = StubVmm::new();
        let vmm = VmmClient::new(stub.clone(), "stub");
        let opts = base_opts(dir.path(), &image_dir, &raw_disk);

        create_local_vm_with_disk_stager(&store, &vmm, &opts, |home, id, _, _| {
            Ok(home.join("vm").join(id).join("hda.img"))
        })
        .unwrap();
        start_local_vm(&store, &vmm, "dev").unwrap();

        // Graceful stop moves VM to `exited` (.started remains true).
        let graceful = stop_local_vm(
            &store,
            &vmm,
            "dev",
            false,
            Duration::from_secs(2),
            Duration::from_millis(10),
        )
        .unwrap();
        assert_eq!(graceful.vmm_status, VmInfo::STATUS_EXITED);
        assert!(!graceful.forced);
        assert!(!graceful.cleared_started_flag);

        // Now `stop --force` on the `exited` VM MUST send `StopVm` to clear `.started`.
        let cleared = stop_local_vm(
            &store,
            &vmm,
            "dev",
            true,
            Duration::from_secs(2),
            Duration::from_millis(10),
        )
        .unwrap();
        assert!(!cleared.already_stopped);
        assert!(cleared.forced);
        assert!(cleared.cleared_started_flag);
        assert_eq!(cleared.vmm_status, VmInfo::STATUS_STOPPED);
        assert!(stub
            .state
            .lock()
            .unwrap()
            .calls
            .contains(&"StopVm".to_owned()));

        // A second `stop --force` once already administratively `stopped` is a no-op.
        let noop = stop_local_vm(
            &store,
            &vmm,
            "dev",
            true,
            Duration::from_secs(2),
            Duration::from_millis(10),
        )
        .unwrap();
        assert!(noop.already_stopped);
        assert!(!noop.forced);
        assert_eq!(noop.vmm_status, VmInfo::STATUS_STOPPED);
    }

    #[test]
    fn f7_unsafe_preexisting_state_permissions_are_refused_unless_fix_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let (image_dir, raw_disk) = make_image_and_disk(dir.path());
        let state_dir = dir.path().join("state");
        let store = LocalVmStore::new(&state_dir);
        let stub = StubVmm::new();
        let vmm = VmmClient::new(stub, "stub");
        let opts = base_opts(dir.path(), &image_dir, &raw_disk);

        create_local_vm_with_disk_stager(&store, &vmm, &opts, |home, id, _, _| {
            Ok(home.join("vm").join(id).join("hda.img"))
        })
        .unwrap();

        let vms_dir = store.vms_dir();
        let state_file = store.state_path("dev").unwrap();
        let leases = dir.path().join("dhcp-leases");

        // Group/world-writable state directory must be refused by default.
        fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o777)).unwrap();
        let err = status_local_vm(&store, &vmm, "dev", &leases).unwrap_err();
        assert!(err.to_string().contains("unsafe mode"), "{err}");

        // Group/world-writable state file must also be refused by default.
        fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&state_file, fs::Permissions::from_mode(0o666)).unwrap();
        let err = status_local_vm(&store, &vmm, "dev", &leases).unwrap_err();
        assert!(err.to_string().contains("unsafe mode"), "{err}");

        // With explicit --fix-permissions, owned unsafe paths are tightened to 0700 / 0600.
        fs::set_permissions(&state_dir, fs::Permissions::from_mode(0o777)).unwrap();
        fs::set_permissions(&vms_dir, fs::Permissions::from_mode(0o777)).unwrap();
        fs::set_permissions(&state_file, fs::Permissions::from_mode(0o666)).unwrap();
        let fixing_store = LocalVmStore::new(&state_dir).with_fix_permissions(true);
        status_local_vm(&fixing_store, &vmm, "dev", &leases).unwrap();
        assert_eq!(
            fs::metadata(&state_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&vms_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&state_file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn state_journal_permissions_refusals_and_force_warning() {
        let dir = tempfile::tempdir().unwrap();
        let (image_dir, raw_disk) = make_image_and_disk(dir.path());
        let state_dir = dir.path().join("state");
        let vmm_home = dir.path().join("vmm");
        let store = LocalVmStore::new(&state_dir);
        let stub = StubVmm::new();
        stub.state.lock().unwrap().remove_polls_before_gone = 2;
        let vmm = VmmClient::new(stub.clone(), "stub");

        // Refusal: unowned VM cannot be started, stopped, inspected, or removed.
        assert!(start_local_vm(&store, &vmm, "foreign").is_err());
        assert!(stop_local_vm(
            &store,
            &vmm,
            "foreign",
            false,
            Duration::from_secs(1),
            Duration::from_millis(10)
        )
        .is_err());
        assert!(remove_local_vm(&store, &vmm, "foreign", fast_remove_opts()).is_err());
        assert!(logs_local_vm(&store, &vmm, "foreign", 20).is_err());

        // Refusal: name collision with foreign VM on VMM before CreateVm.
        stub.state.lock().unwrap().vms.insert(
            UUID_FOREIGN.into(),
            FakeVm {
                id: UUID_FOREIGN.into(),
                name: "dev".into(),
                image: "other".into(),
                vcpu: 3,
                memory: 5 * 1024,
                app_id: calc_app_id(APP_COMPOSE_JSON),
                status: VmInfo::STATUS_RUNNING.into(),
                running: true,
                stopped_flag: false,
                boot_progress: "done".into(),
                mac: "52:54:00:de:ad:01".into(),
                annotations: BTreeMap::new(),
                remove_polls_left: 0,
            },
        );
        let opts = base_opts(dir.path(), &image_dir, &raw_disk);
        let err = create_local_vm_with_disk_stager(&store, &vmm, &opts, |home, id, _, _| {
            Ok(home.join("vm").join(id).join("hda.img"))
        })
        .unwrap_err();
        assert!(err.to_string().contains("already exists on the local VMM"));
        stub.state.lock().unwrap().vms.remove(UUID_FOREIGN);

        // Partial failure during disk staging leaves journaled MacRecorded state and resumes cleanly.
        let err = create_local_vm_with_disk_stager(&store, &vmm, &opts, |_, _, _, _| {
            Err(eyre::eyre!("simulated qemu-img failure"))
        })
        .unwrap_err();
        assert!(err.to_string().contains("simulated qemu-img failure"));
        let guard = store.lock_vm("dev").unwrap();
        let partial = store.load_locked(&guard, "dev").unwrap();
        assert_eq!(partial.phase, LocalVmPhase::MacRecorded);
        assert_eq!(partial.vm_id, UUID_OWNED);
        assert_eq!(partial.mac, "52:54:00:aa:bb:cc");
        assert!(partial.journal.iter().any(|j| j.step == "staging-disk"));
        // Verify 0700 dir and 0600 state file permissions.
        assert_eq!(
            fs::metadata(&state_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(store.state_path("dev").unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        drop(guard);

        // Starting an incomplete VM is refused.
        assert!(start_local_vm(&store, &vmm, "dev").is_err());

        // Resume finishes disk staging and reaches Ready.
        let created = create_local_vm_with_disk_stager(&store, &vmm, &opts, |home, id, _, _| {
            let p = home.join("vm").join(id).join("hda.img");
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, b"qcow2").unwrap();
            Ok(p)
        })
        .unwrap();
        assert_eq!(created.phase, LocalVmPhase::Ready);
        assert_eq!(created.intended_ip, Some(Ipv4Addr::new(10, 77, 0, 88)));

        // Start, graceful stop (uses ShutdownVm, NOT StopVm), start again, force stop (prints warning).
        let started = start_local_vm(&store, &vmm, "dev").unwrap();
        assert!(!started.already_running);
        let stopped = stop_local_vm(
            &store,
            &vmm,
            "dev",
            false,
            Duration::from_secs(2),
            Duration::from_millis(10),
        )
        .unwrap();
        assert!(!stopped.forced);
        assert_eq!(stopped.vmm_status, VmInfo::STATUS_EXITED);
        assert!(stopped.warning.is_none());
        assert!(!stub
            .state
            .lock()
            .unwrap()
            .calls
            .contains(&"StopVm".to_owned()));

        // Force stop calls StopVm and returns the DKG defect warning.
        start_local_vm(&store, &vmm, "dev").unwrap();
        let forced = stop_local_vm(
            &store,
            &vmm,
            "dev",
            true,
            Duration::from_secs(2),
            Duration::from_millis(10),
        )
        .unwrap();
        assert!(forced.forced);
        assert_eq!(forced.vmm_status, VmInfo::STATUS_STOPPED);
        assert!(forced
            .warning
            .as_deref()
            .unwrap()
            .contains("12-defect-pending-dkg-snapshot.md"));

        // Status reports both service_ip (recorded intended_ip) and dhcp_ip (from leases file,
        // None when absent).
        let leases_file = dir.path().join("dhcp-leases");
        let status_no_leases = status_local_vm(&store, &vmm, "dev", &leases_file).unwrap();
        assert_eq!(
            status_no_leases.service_ip,
            Some(Ipv4Addr::new(10, 77, 0, 88))
        );
        assert_eq!(status_no_leases.dhcp_ip, None);
        assert_eq!(status_no_leases.ip, Some(Ipv4Addr::new(10, 77, 0, 88)));
        fs::write(
            &leases_file,
            "1795598734 52:54:00:aa:bb:cc 10.77.0.122 dev *\n",
        )
        .unwrap();
        let status_with_leases = status_local_vm(&store, &vmm, "dev", &leases_file).unwrap();
        assert_eq!(
            status_with_leases.service_ip,
            Some(Ipv4Addr::new(10, 77, 0, 88))
        );
        assert_eq!(
            status_with_leases.dhcp_ip,
            Some(Ipv4Addr::new(10, 77, 0, 122))
        );
        assert_eq!(status_with_leases.ip, Some(Ipv4Addr::new(10, 77, 0, 88)));

        // Remove waits across asynchronous polls and deletes only state file, leaving image intact.
        let removed = remove_local_vm(&store, &vmm, "dev", fast_remove_opts()).unwrap();
        assert_eq!(removed.vm_id, UUID_OWNED);
        assert!(!store.state_path("dev").unwrap().exists());
        assert!(vmm_home.join("image").join(&created.image_name).is_dir());
    }

    #[test]
    fn concurrent_vm_state_locks_serialize_without_corrupting_state() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalVmStore::new(dir.path().join("state"));

        // Seed initial state.
        let guard = store.lock_vm("race-vm").unwrap();
        let initial = LocalVmState {
            version: LOCAL_VM_STATE_VERSION,
            name: "race-vm".into(),
            vm_id: UUID_OWNED.into(),
            create_token: None,
            owner_tag: None,
            vmm_socket: PathBuf::from("/tmp/vmm.sock"),
            vmm_home: PathBuf::from("/tmp/vmm"),
            image_name: "outbe-vm-dev-0123456789abcdef".into(),
            os_image_hash: "0123456789abcdef".repeat(4),
            disk_source: PathBuf::from("/tmp/dev.img"),
            disk_source_sha256: Some("0".repeat(64)),
            vcpu: 3,
            memory_gib: 5,
            disk_gib: 10,
            bridge: "obr0".into(),
            intended_ip: None,
            mac: "52:54:00:11:22:33".into(),
            app_id: "a".repeat(40),
            no_tee: false,
            phase: LocalVmPhase::Ready,
            journal: Vec::new(),
        };
        store.save_locked(&guard, &initial).unwrap();
        drop(guard);

        let mut handles = Vec::new();
        for worker in 0..4 {
            let store = store.clone();
            handles.push(thread::spawn(move || {
                for iter in 0..8 {
                    let g = store.lock_vm("race-vm").unwrap();
                    let mut st = store.load_locked(&g, "race-vm").unwrap();
                    st.record_step(format!("w{worker}-i{iter}"), None);
                    store.save_locked(&g, &st).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let g = store.lock_vm("race-vm").unwrap();
        let final_state = store.load_locked(&g, "race-vm").unwrap();
        assert_eq!(final_state.journal.len(), 32);
    }

    #[test]
    fn f5_remove_local_image_waits_for_vm_create_lock_and_refuses_registered_vm() {
        use std::sync::{mpsc, Arc, Mutex};

        use crate::image::{publish_local_image, remove_local_image};

        struct PausingCreateTransport {
            vms: Arc<Mutex<Vec<serde_json::Value>>>,
            create_entered_tx: Mutex<Option<mpsc::Sender<()>>>,
            allow_create_rx: Mutex<Option<mpsc::Receiver<()>>>,
        }

        impl VmmTransport for PausingCreateTransport {
            fn prpc_post(&self, method: &str, payload: &[u8]) -> Result<String> {
                match method {
                    "Status" => {
                        let vms = self.vms.lock().unwrap();
                        Ok(serde_json::json!({
                            "vms": *vms,
                            "total": vms.len()
                        })
                        .to_string())
                    }
                    "CreateVm" => {
                        let req: serde_json::Value = serde_json::from_slice(payload)?;
                        if let Some(tx) = self.create_entered_tx.lock().unwrap().take() {
                            let _ = tx.send(());
                        }
                        if let Some(rx) = self.allow_create_rx.lock().unwrap().take() {
                            let _ = rx.recv();
                        }
                        let info = serde_json::json!({
                            "id": UUID_OWNED,
                            "name": req["name"].as_str().unwrap_or("race-create"),
                            "status": "stopped",
                            "running": false,
                            "boot_progress": "",
                            "boot_error": "",
                            "uptime": "",
                            "interfaces": [
                                {"mode": "bridge", "mac": "52:54:00:ab:cd:ef"}
                            ],
                            "configuration": {
                                "name": req["name"].as_str().unwrap_or("race-create"),
                                "image": req["image"].as_str().unwrap_or(""),
                                "vcpu": req["vcpu"].as_u64().unwrap_or(3),
                                "memory": req["memory"].as_u64().unwrap_or(5120),
                                "disk_size": req["disk_size"].as_u64().unwrap_or(40)
                            }
                        });
                        self.vms.lock().unwrap().push(info);
                        Ok(serde_json::json!({"id": UUID_OWNED}).to_string())
                    }
                    "GetInfo" => {
                        let vms = self.vms.lock().unwrap();
                        if let Some(info) = vms.first() {
                            Ok(serde_json::json!({"found": true, "info": info}).to_string())
                        } else {
                            Ok(r#"{"found":false,"info":null}"#.to_owned())
                        }
                    }
                    other => Err(eyre::eyre!("unexpected VMM method {other}")),
                }
            }

            fn read_serial_log(&self, _vm_id: &str, _lines: usize) -> Result<String> {
                Ok(String::new())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let vmm_home = dir.path().join("vmm");
        let state_dir = dir.path().join("state");
        let src_image = dir.path().join("src-image");
        fs::create_dir_all(&src_image).unwrap();
        let digest = "0123456789abcdef".repeat(4);
        fs::write(src_image.join("digest.txt"), format!("{digest}\n")).unwrap();
        fs::write(src_image.join("metadata.json"), "{}").unwrap();
        fs::write(
            src_image.join("image-archive-manifest.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "version": 1,
                "flavor": "dev",
                "os_image_hash": digest,
                "source_commit": "a".repeat(40),
                "dstack_rev": "b".repeat(40),
                "overlay_commit": "c".repeat(40),
                "files": [
                    {"path": "digest.txt", "size": 65, "sha256": "0".repeat(64), "mode": 0o644},
                    {"path": "metadata.json", "size": 2, "sha256": "1".repeat(64), "mode": 0o644}
                ]
            }))
            .unwrap(),
        )
        .unwrap();

        // Pre-publish the image so a concurrent `image remove` without F5 locking would delete it
        // while `CreateVm` is paused before registering the VM.
        let published = publish_local_image(&vmm_home, &src_image, None, "dev").unwrap();
        let disk_source = dir.path().join("data.raw");
        fs::write(&disk_source, b"fake-raw-disk").unwrap();

        let shared_vms = Arc::new(Mutex::new(Vec::new()));
        let (create_entered_tx, create_entered_rx) = mpsc::channel();
        let (allow_create_tx, allow_create_rx) = mpsc::channel();

        let create_transport = PausingCreateTransport {
            vms: Arc::clone(&shared_vms),
            create_entered_tx: Mutex::new(Some(create_entered_tx)),
            allow_create_rx: Mutex::new(Some(allow_create_rx)),
        };
        let remove_transport = PausingCreateTransport {
            vms: Arc::clone(&shared_vms),
            create_entered_tx: Mutex::new(None),
            allow_create_rx: Mutex::new(None),
        };

        let store = LocalVmStore::new(state_dir);
        let opts = CreateLocalVmOptions {
            name: "race-vm".to_owned(),
            owner_tag: None,
            image_dir: src_image,
            image_name: None,
            flavor: "dev".to_owned(),
            disk_source,
            disk_staging_name: None,
            consume_staging: false,
            bridge: "obr0".to_owned(),
            vcpu: 3,
            memory_gib: 5,
            disk_gib: 10,
            app_compose_path: None,
            intended_ip: None,
            no_tee: false,
            vmm_socket: vmm_home.join("run/vmm.sock"),
            vmm_home: vmm_home.clone(),
            retry_create: false,
            recovery_grace: Duration::from_secs(1),
            poll_interval: Duration::from_millis(10),
            timeout: Some(Duration::from_secs(10)),
        };

        let create_handle = thread::spawn(move || {
            let vmm = VmmClient::new(create_transport, "pausing-create");
            create_local_vm_with_disk_stager(&store, &vmm, &opts, |home, id, _, _| {
                let p = home.join("vm").join(id).join("hda.img");
                fs::create_dir_all(p.parent().unwrap()).unwrap();
                fs::write(&p, b"qcow2").unwrap();
                Ok(p)
            })
        });

        // Wait until `vm create` is inside `CreateVm` (still holding the per-image lock).
        create_entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("vm create did not reach CreateVm");

        let (remove_done_tx, remove_done_rx) = mpsc::channel();
        let vmm_home_for_rm = vmm_home.clone();
        let image_name_for_rm = published.name.clone();
        let remove_handle = thread::spawn(move || {
            let vmm = VmmClient::new(remove_transport, "concurrent-remove");
            let res = remove_local_image(&vmm_home_for_rm, &vmm, &image_name_for_rm);
            let _ = remove_done_tx.send(res);
        });

        // While CreateVm is paused, `image remove` MUST stay blocked on the per-image lock.
        assert!(
            matches!(
                remove_done_rx.recv_timeout(Duration::from_millis(200)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "image remove must block while vm create holds the per-image lock across CreateVm"
        );

        // Allow CreateVm to register the VM and finish persisting `Created`.
        allow_create_tx.send(()).unwrap();
        let created = create_handle.join().unwrap().expect("vm create failed");
        assert_eq!(created.phase, LocalVmPhase::Ready);

        let remove_err = remove_done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("image remove did not finish after CreateVm released lock")
            .expect_err("image remove must refuse image referenced by newly created VM");
        remove_handle.join().unwrap();
        assert!(
            format!("{remove_err:#}").contains("still referenced by VM(s): race-vm"),
            "{remove_err:#}"
        );
        assert!(
            published.target_dir.is_dir(),
            "referenced image directory must remain intact"
        );
    }

    #[test]
    fn c1_owner_tag_persists_in_state_vmm_annotation_status_and_adoption_check() {
        let dir = tempfile::tempdir().unwrap();
        let (image_dir, raw_disk) = make_image_and_disk(dir.path());
        let store = LocalVmStore::new(dir.path().join("state"));
        let stub = StubVmm::new();
        let vmm = VmmClient::new(stub.clone(), "stub");
        let tag = "outbe-net:devnet:01234567:validator-0".to_owned();

        // Invalid owner tags are rejected before any VMM RPC.
        let mut bad_opts = base_opts(dir.path(), &image_dir, &raw_disk);
        bad_opts.owner_tag = Some("../evil tag".to_owned());
        assert!(
            create_local_vm_with_disk_stager(&store, &vmm, &bad_opts, |_, _, _, _| {
                panic!("must not run disk stager on invalid owner tag")
            })
            .is_err()
        );
        assert!(stub.state.lock().unwrap().calls.is_empty());

        // Drop CreateVm response once to verify owner_tag is persisted in Creating state AND
        // sent in CreateVm annotations AND verified on adoption.
        stub.state.lock().unwrap().drop_create_response_once = true;
        let mut opts = base_opts(dir.path(), &image_dir, &raw_disk);
        opts.owner_tag = Some(tag.clone());
        assert!(
            create_local_vm_with_disk_stager(&store, &vmm, &opts, |home, id, _, _| {
                Ok(home.join("vm").join(id).join("hda.img"))
            })
            .is_err()
        );

        // Creating phase status must already expose owner_tag and resource fields.
        let leases = dir.path().join("dhcp-leases");
        let creating_status = status_local_vm(&store, &vmm, "dev", &leases).unwrap();
        assert_eq!(creating_status.phase, LocalVmPhase::Creating);
        assert_eq!(creating_status.owner_tag.as_deref(), Some(tag.as_str()));
        assert_eq!(creating_status.app_id, calc_app_id(APP_COMPOSE_JSON));
        assert!(!creating_status.no_tee);
        assert_eq!(creating_status.vcpu, 3);
        assert_eq!(creating_status.memory_gib, 5);
        assert_eq!(creating_status.disk_gib, 10);

        // VMM must have received OWNER_TAG_ANNOTATION on CreateVm.
        {
            let g = stub.state.lock().unwrap();
            let vm = g.vms.get(UUID_OWNED).unwrap();
            assert_eq!(
                vm.annotations.get(OWNER_TAG_ANNOTATION).map(String::as_str),
                Some(tag.as_str())
            );
        }

        // Retrying with a different owner_tag must be rejected by immutable param check.
        let mut wrong_tag_opts = opts.clone();
        wrong_tag_opts.owner_tag = Some("outbe-net:other:89abcdef:validator-0".to_owned());
        let err = create_local_vm_with_disk_stager(&store, &vmm, &wrong_tag_opts, |_, _, _, _| {
            panic!("must not resume with mismatched owner_tag")
        })
        .unwrap_err();
        assert!(err.to_string().contains("owner_tag"), "{err}");

        // Tampering the VMM VM's owner_tag annotation must cause adoption to refuse.
        stub.state
            .lock()
            .unwrap()
            .vms
            .get_mut(UUID_OWNED)
            .unwrap()
            .annotations
            .insert(
                OWNER_TAG_ANNOTATION.to_owned(),
                "outbe-net:tampered:00000000:validator-0".to_owned(),
            );
        let err = create_local_vm_with_disk_stager(&store, &vmm, &opts, |_, _, _, _| {
            panic!("must not adopt VM with mismatched owner_tag annotation")
        })
        .unwrap_err();
        assert!(err.to_string().contains("owner_tag"), "{err}");

        // Restore exact annotation -> adoption succeeds and Ready status reports all fields.
        stub.state
            .lock()
            .unwrap()
            .vms
            .get_mut(UUID_OWNED)
            .unwrap()
            .annotations
            .insert(OWNER_TAG_ANNOTATION.to_owned(), tag.clone());
        let created = create_local_vm_with_disk_stager(&store, &vmm, &opts, |home, id, _, _| {
            Ok(home.join("vm").join(id).join("hda.img"))
        })
        .unwrap();
        assert_eq!(created.owner_tag.as_deref(), Some(tag.as_str()));
        let ready_status = status_local_vm(&store, &vmm, "dev", &leases).unwrap();
        assert_eq!(ready_status.owner_tag.as_deref(), Some(tag.as_str()));
        assert_eq!(ready_status.app_id, calc_app_id(APP_COMPOSE_JSON));
        assert!(!ready_status.no_tee);
        assert_eq!(ready_status.vcpu, 3);
        assert_eq!(ready_status.memory_gib, 5);
        assert_eq!(ready_status.disk_gib, 10);
    }

    #[test]
    fn c1_expect_vm_id_enforced_before_any_vmm_rpc() {
        let dir = tempfile::tempdir().unwrap();
        let (image_dir, raw_disk) = make_image_and_disk(dir.path());
        let store = LocalVmStore::new(dir.path().join("state"));
        let stub = StubVmm::new();
        let vmm = VmmClient::new(stub.clone(), "stub");
        let opts = base_opts(dir.path(), &image_dir, &raw_disk);

        let created = create_local_vm_with_disk_stager(&store, &vmm, &opts, |home, id, _, _| {
            Ok(home.join("vm").join(id).join("hda.img"))
        })
        .unwrap();
        assert_eq!(created.vm_id, UUID_OWNED);

        let leases = dir.path().join("dhcp-leases");
        let calls_before = stub.state.lock().unwrap().calls.len();

        // Wrong UUID fails closed on every lifecycle verb with ZERO VMM RPCs.
        let err =
            start_local_vm_with_expected_id(&store, &vmm, "dev", Some(UUID_FOREIGN)).unwrap_err();
        assert!(err.to_string().contains("vm_id_mismatch"), "{err}");

        let err = stop_local_vm_with_expected_id(
            &store,
            &vmm,
            "dev",
            Some(UUID_FOREIGN),
            false,
            Duration::from_secs(2),
            Duration::from_millis(10),
        )
        .unwrap_err();
        assert!(err.to_string().contains("vm_id_mismatch"), "{err}");

        let err =
            status_local_vm_with_expected_id(&store, &vmm, "dev", Some(UUID_FOREIGN), &leases)
                .unwrap_err();
        assert!(err.to_string().contains("vm_id_mismatch"), "{err}");

        let err = logs_local_vm_with_expected_id(&store, &vmm, "dev", Some(UUID_FOREIGN), 10)
            .unwrap_err();
        assert!(err.to_string().contains("vm_id_mismatch"), "{err}");

        let mut rm_mismatch = fast_remove_opts();
        rm_mismatch.expect_vm_id = Some(UUID_FOREIGN.to_owned());
        let err = remove_local_vm(&store, &vmm, "dev", rm_mismatch).unwrap_err();
        assert!(err.to_string().contains("vm_id_mismatch"), "{err}");

        assert_eq!(
            stub.state.lock().unwrap().calls.len(),
            calls_before,
            "mismatched --expect-vm-id must issue ZERO VMM RPCs"
        );
        // State file must still be Ready (not mutated to Removing).
        let guard = store.lock_vm("dev").unwrap();
        assert_eq!(
            store.load_locked(&guard, "dev").unwrap().phase,
            LocalVmPhase::Ready
        );
        drop(guard);

        // Matching UUID succeeds across status -> start -> logs -> stop -> remove.
        let st = status_local_vm_with_expected_id(&store, &vmm, "dev", Some(UUID_OWNED), &leases)
            .unwrap();
        assert_eq!(st.vm_id, UUID_OWNED);
        start_local_vm_with_expected_id(&store, &vmm, "dev", Some(UUID_OWNED)).unwrap();
        let logs =
            logs_local_vm_with_expected_id(&store, &vmm, "dev", Some(UUID_OWNED), 5).unwrap();
        assert!(logs.log.contains(UUID_OWNED));
        stop_local_vm_with_expected_id(
            &store,
            &vmm,
            "dev",
            Some(UUID_OWNED),
            false,
            Duration::from_secs(2),
            Duration::from_millis(10),
        )
        .unwrap();
        let mut rm_match = fast_remove_opts();
        rm_match.expect_vm_id = Some(UUID_OWNED.to_owned());
        let removed = remove_local_vm(&store, &vmm, "dev", rm_match).unwrap();
        assert_eq!(removed.vm_id, UUID_OWNED);
    }

    #[test]
    fn c1_staging_store_list_clean_symlink_refusal_and_consume_only_on_ready() {
        let dir = tempfile::tempdir().unwrap();
        let (image_dir, _) = make_image_and_disk(dir.path());
        let store = LocalVmStore::new(dir.path().join("state"));
        let stub = StubVmm::new();
        let vmm = VmmClient::new(stub.clone(), "stub");

        // Empty store returns empty list and idempotent clean.
        assert!(store.list_staged_disks().unwrap().is_empty());
        let noop = store.clean_staging_dir("dev").unwrap();
        assert!(!noop.removed);

        // Populate a valid staged disk (0700 dir, 0600 data.img).
        let staging_dir = store.ensure_staging_dir("dev").unwrap();
        let staged_img = store.staged_disk_path("dev").unwrap();
        fs::write(&staged_img, b"staged-raw-disk-bytes").unwrap();
        fs::set_permissions(&staged_img, fs::Permissions::from_mode(0o600)).unwrap();

        let listed = store.list_staged_disks().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].staging_name, "dev");
        assert_eq!(listed[0].path.as_deref(), Some(staged_img.as_path()));
        assert_eq!(
            listed[0].size_bytes,
            Some(b"staged-raw-disk-bytes".len() as u64)
        );
        assert_eq!(
            listed[0].sha256.as_deref(),
            Some(sha256_sparse_file(&staged_img).unwrap().as_str())
        );
        assert!(listed[0].error.is_none());

        // Symlink staging dir is refused by clean_staging_dir, while list_staged_disks reports it
        // as an errored entry without aborting valid siblings (F5).
        let outside = dir.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        let evil_link = store.staging_root().join("evil");
        std::os::unix::fs::symlink(&outside, &evil_link).unwrap();
        assert!(store.clean_staging_dir("evil").is_err());
        let listed_with_bad = store.list_staged_disks().unwrap();
        assert_eq!(listed_with_bad.len(), 2);
        let dev_entry = listed_with_bad
            .iter()
            .find(|e| e.staging_name == "dev")
            .unwrap();
        assert!(dev_entry.error.is_none());
        let evil_entry = listed_with_bad
            .iter()
            .find(|e| e.staging_name == "evil")
            .unwrap();
        assert!(evil_entry.error.is_some());
        assert!(evil_entry.sha256.is_none());
        fs::remove_file(&evil_link).unwrap();

        // Failed create with disk_staging + consume_staging MUST preserve staged disk for retry!
        let mut opts = base_opts(dir.path(), &image_dir, &staged_img);
        opts.disk_staging_name = Some("dev".to_owned());
        opts.consume_staging = true;
        let err = create_local_vm_with_disk_stager(&store, &vmm, &opts, |_, _, _, _| {
            Err(eyre::eyre!("simulated qemu-img failure"))
        })
        .unwrap_err();
        assert!(
            err.to_string().contains("simulated qemu-img failure"),
            "{err}"
        );
        assert!(
            staged_img.is_file(),
            "staged disk must be preserved when vm create fails before Ready"
        );
        assert_eq!(store.list_staged_disks().unwrap().len(), 1);

        // Successful resume reaches Ready and consumes the staging directory!
        let created = create_local_vm_with_disk_stager(&store, &vmm, &opts, |home, id, _, _| {
            Ok(home.join("vm").join(id).join("hda.img"))
        })
        .unwrap();
        assert_eq!(created.phase, LocalVmPhase::Ready);
        assert!(created.staging_consumed);
        assert!(created.warning.is_none());
        assert!(!staging_dir.exists());
        assert!(store.list_staged_disks().unwrap().is_empty());
    }

    #[test]
    fn c1_f2_vm_create_disk_staging_holds_staging_lock_across_hash_and_convert() {
        use std::sync::mpsc;

        let dir = tempfile::tempdir().unwrap();
        let (image_dir, _) = make_image_and_disk(dir.path());
        let store = LocalVmStore::new(dir.path().join("state"));
        let stub = StubVmm::new();

        store.ensure_staging_dir("dev").unwrap();
        let staged_img = store.staged_disk_path("dev").unwrap();
        fs::write(&staged_img, b"original-staged-bytes").unwrap();
        fs::set_permissions(&staged_img, fs::Permissions::from_mode(0o600)).unwrap();
        let expected_sha = sha256_sparse_file(&staged_img).unwrap();

        let (stager_entered_tx, stager_entered_rx) = mpsc::channel();
        let (allow_stager_tx, allow_stager_rx) = mpsc::channel();

        let store_create = store.clone();
        let vmm_create = VmmClient::new(stub.clone(), "stub");
        let mut opts = base_opts(dir.path(), &image_dir, &staged_img);
        opts.disk_staging_name = Some("dev".to_owned());
        // Even WITHOUT `--consume-staging`, `--disk-staging` MUST hold `lock_staging` across
        // hashing and conversion so a concurrent prepare/clean cannot race or swap `data.img`.
        opts.consume_staging = false;

        let create_handle = thread::spawn(move || {
            create_local_vm_with_disk_stager(
                &store_create,
                &vmm_create,
                &opts,
                |home, id, raw_disk, _| {
                    stager_entered_tx.send(()).unwrap();
                    allow_stager_rx.recv().unwrap();
                    // Verify bytes converted match the exact hashed bytes untouched by races.
                    assert_eq!(fs::read(raw_disk).unwrap(), b"original-staged-bytes");
                    Ok(home.join("vm").join(id).join("hda.img"))
                },
            )
        });

        stager_entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("vm create did not reach disk_stager");

        // Probe the staging lock directly: MUST fail with WOULDBLOCK while vm create is running!
        let lock_path = store.staging_root().join(".dev.lock");
        let probe_fd = openat(
            CWD,
            &lock_path,
            OFlags::RDWR | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();
        assert_eq!(
            flock(&probe_fd, FlockOperation::NonBlockingLockExclusive),
            Err(rustix::io::Errno::WOULDBLOCK),
            "vm create --disk-staging must hold exclusive staging lock across hash and convert"
        );
        drop(probe_fd);

        // Concurrent `clean_staging_dir("dev")` MUST block until vm create releases the lock.
        let (clean_done_tx, clean_done_rx) = mpsc::channel();
        let store_clean = store.clone();
        let clean_handle = thread::spawn(move || {
            let res = store_clean.clean_staging_dir("dev");
            let _ = clean_done_tx.send(res);
        });
        assert!(
            matches!(
                clean_done_rx.recv_timeout(Duration::from_millis(150)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "clean_staging_dir must block while vm create --disk-staging holds staging lock"
        );

        allow_stager_tx.send(()).unwrap();
        let created = create_handle.join().unwrap().expect("vm create failed");
        assert_eq!(created.phase, LocalVmPhase::Ready);

        let cleaned = clean_done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("clean_staging_dir did not finish after vm create released lock")
            .unwrap();
        clean_handle.join().unwrap();
        assert!(cleaned.removed);

        let g = store.lock_vm("dev").unwrap();
        let st = store.load_locked(&g, "dev").unwrap();
        assert_eq!(
            st.disk_source_sha256.as_deref(),
            Some(expected_sha.as_str())
        );
    }

    #[test]
    fn c1_f3_consume_staging_failure_after_ready_returns_ok_with_warning_and_false() {
        let dir = tempfile::tempdir().unwrap();
        let (image_dir, _) = make_image_and_disk(dir.path());
        let store = LocalVmStore::new(dir.path().join("state"));
        let stub = StubVmm::new();
        let vmm = VmmClient::new(stub, "stub");

        let staging_dir = store.ensure_staging_dir("dev").unwrap();
        let staged_img = store.staged_disk_path("dev").unwrap();
        fs::write(&staged_img, b"staged-raw-disk-bytes").unwrap();
        fs::set_permissions(&staged_img, fs::Permissions::from_mode(0o600)).unwrap();

        let outside = dir.path().join("outside-target");
        fs::create_dir_all(&outside).unwrap();

        let mut opts = base_opts(dir.path(), &image_dir, &staged_img);
        opts.disk_staging_name = Some("dev".to_owned());
        opts.consume_staging = true;

        // Simulate a post-conversion filesystem/security failure during staging consumption by
        // replacing `staging/dev` with a symlink after conversion completes. `LocalVmPhase::Ready`
        // is saved durably; consume fails closed on the symlink, so `create` MUST still succeed
        // (exit 0 equivalent) with `staging_consumed: false` and a clear warning.
        let staging_dir_clone = staging_dir.clone();
        let outside_clone = outside.clone();
        let created = create_local_vm_with_disk_stager(&store, &vmm, &opts, |home, id, _, _| {
            fs::remove_dir_all(&staging_dir_clone).unwrap();
            std::os::unix::fs::symlink(&outside_clone, &staging_dir_clone).unwrap();
            Ok(home.join("vm").join(id).join("hda.img"))
        })
        .expect("consume failure after durable Ready must not fail vm create");

        assert_eq!(created.phase, LocalVmPhase::Ready);
        assert!(!created.staging_consumed);
        let warning = created
            .warning
            .as_deref()
            .expect("consume failure must populate warning");
        assert!(
            warning.contains("disk clean --staging-name dev"),
            "{warning}"
        );
        // Outside target was never touched and VM state is durably Ready.
        assert!(outside.is_dir());
        let g = store.lock_vm("dev").unwrap();
        assert_eq!(
            store.load_locked(&g, "dev").unwrap().phase,
            LocalVmPhase::Ready
        );
    }

    #[test]
    fn c1_f5_disk_clean_list_reports_bad_entries_without_aborting_valid_siblings() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalVmStore::new(dir.path().join("state"));

        // Two valid staged disks.
        for name in ["validator-0", "validator-1"] {
            store.ensure_staging_dir(name).unwrap();
            let img = store.staged_disk_path(name).unwrap();
            fs::write(&img, format!("disk-bytes-{name}")).unwrap();
            fs::set_permissions(&img, fs::Permissions::from_mode(0o600)).unwrap();
        }
        // 1. Unsafe mode (0755) directory.
        let bad_mode = store.staging_root().join("bad-mode");
        fs::create_dir_all(&bad_mode).unwrap();
        fs::set_permissions(&bad_mode, fs::Permissions::from_mode(0o755)).unwrap();
        // 2. Symlink entry.
        let outside = dir.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, store.staging_root().join("evil-link")).unwrap();
        // 3. Regular file directly under staging/.
        fs::write(store.staging_root().join("stray-file"), b"junk").unwrap();
        // 4. Valid dir with unsafe-mode (0644) data.img.
        store.ensure_staging_dir("bad-img-mode").unwrap();
        let bad_img = store.staged_disk_path("bad-img-mode").unwrap();
        fs::write(&bad_img, b"loose").unwrap();
        fs::set_permissions(&bad_img, fs::Permissions::from_mode(0o644)).unwrap();

        let entries = store.list_staged_disks().unwrap();
        assert_eq!(entries.len(), 6);

        for ok_name in ["validator-0", "validator-1"] {
            let e = entries.iter().find(|e| e.staging_name == ok_name).unwrap();
            assert!(
                e.error.is_none(),
                "expected valid entry for {ok_name}: {e:?}"
            );
            assert!(e.sha256.as_deref().is_some_and(|s| s.len() == 64));
            assert!(e.size_bytes.is_some());
        }
        for bad_name in ["bad-mode", "evil-link", "stray-file", "bad-img-mode"] {
            let e = entries.iter().find(|e| e.staging_name == bad_name).unwrap();
            assert!(
                e.error.is_some(),
                "expected error reported for {bad_name}: {e:?}"
            );
            assert!(e.sha256.is_none());
            assert!(e.size_bytes.is_none());
        }
    }
}

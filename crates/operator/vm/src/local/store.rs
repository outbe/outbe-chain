//! Per-VM state files under `<state-dir>/vms/` with atomic fsync+rename persistence.

use std::{
    fs,
    io::{ErrorKind, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use eyre::{ensure, Context, Result};

use super::{
    lock::VmLockGuard,
    private_fs::{
        current_uid, ensure_private_dir, ensure_private_regular_file, optional_symlink_metadata,
    },
    validate_vm_name, LocalVmState, LocalVmStore, LOCAL_VM_STATE_VERSION,
};

/// Durable journal writer for one VM while its exclusive per-VM lock is held.
#[derive(Clone, Copy)]
pub(super) struct VmJournal<'a> {
    pub(super) store: &'a LocalVmStore,
    pub(super) guard: &'a VmLockGuard,
}

impl VmJournal<'_> {
    pub(super) fn save(&self, state: &LocalVmState) -> Result<PathBuf> {
        self.store.save_locked(self.guard, state)
    }

    /// Append one journal step to `state` and persist it.
    pub(super) fn record(
        &self,
        state: &mut LocalVmState,
        step: &str,
        detail: Option<String>,
    ) -> Result<()> {
        state.record_step(step, detail);
        self.save(state)?;
        Ok(())
    }
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

    pub(super) fn ensure_dirs(&self) -> Result<()> {
        ensure_private_dir(&self.root, self.fix_permissions)?;
        ensure_private_dir(&self.vms_dir(), self.fix_permissions)?;
        Ok(())
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
        let Some(meta) = optional_symlink_metadata(&path, "VM state")? else {
            return Ok(None);
        };
        ensure_private_regular_file(
            &path,
            &meta,
            current_uid(),
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

        let vms_dir = self.vms_dir();
        if let Ok(meta) = fs::symlink_metadata(&path) {
            ensure_private_regular_file(
                &path,
                &meta,
                current_uid(),
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

        sync_dir(&vms_dir);
        Ok(path)
    }

    /// Remove a VM's state file while holding its exclusive lock.
    pub fn delete_locked(&self, _guard: &VmLockGuard, name: &str) -> Result<PathBuf> {
        let path = self.state_path(name)?;
        match fs::remove_file(&path) {
            Err(err) if err.kind() != ErrorKind::NotFound => {
                return Err(err).wrap_err_with(|| format!("remove VM state {}", path.display()));
            }
            _ => {}
        }
        sync_dir(&self.vms_dir());
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
            let file_name = entry?.file_name();
            if let Some(name) = file_name.to_str().and_then(recorded_vm_name) {
                names.push(name.to_owned());
            }
        }
        names.sort();
        Ok(names)
    }
}

/// Return the VM name of a `<name>.json` state file entry, skipping hidden and invalid names.
fn recorded_vm_name(file_name: &str) -> Option<&str> {
    if file_name.starts_with('.') {
        return None;
    }
    let stem = file_name.strip_suffix(".json")?;
    validate_vm_name(stem).is_ok().then_some(stem)
}

/// Best-effort fsync of a directory after a rename or unlink inside it.
pub(super) fn sync_dir(dir: &Path) {
    if let Ok(dir) = fs::File::open(dir) {
        let _ = dir.sync_all();
    }
}

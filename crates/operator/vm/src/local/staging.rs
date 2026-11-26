//! Staged raw data disks under `<state-dir>/staging/<staging_name>/data.img`.

use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use eyre::{ensure, Context, Result};
use serde::{Deserialize, Serialize};

use super::{
    lock::StagingLockGuard,
    private_fs::{
        current_uid, ensure_private_dir, ensure_private_regular_file, optional_symlink_metadata,
        verify_existing_private_dir,
    },
    store::sync_dir,
    validate_vm_name, LocalVmStore,
};
use crate::disk::sha256_sparse_file;

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

impl StagedDiskInfo {
    fn failed(staging_name: String, path: PathBuf, error: String) -> Self {
        Self {
            staging_name,
            path: Some(path),
            sha256: None,
            size_bytes: None,
            error: Some(error),
        }
    }
}

/// Result of removing one staged disk directory under `<state-dir>/staging/<staging_name>`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CleanStagingOutput {
    pub staging_name: String,
    pub path: PathBuf,
    pub removed: bool,
}

impl CleanStagingOutput {
    fn absent(staging_name: &str, path: PathBuf) -> Self {
        Self {
            staging_name: staging_name.to_owned(),
            path,
            removed: false,
        }
    }
}

impl LocalVmStore {
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

    /// Check that `<state-dir>` and `<state-dir>/staging` are private real directories without
    /// creating them.
    pub(super) fn verify_staging_root(&self) -> Result<()> {
        verify_existing_private_dir(&self.root, self.fix_permissions, "state directory")?;
        verify_existing_private_dir(
            &self.staging_root(),
            self.fix_permissions,
            "staging root directory",
        )
    }

    /// Resolve and security-verify `<state-dir>/staging/<staging_name>/data.img` without creating
    /// directories. Refuses symlinks, non-regular files, foreign UIDs, and unsafe permissions.
    pub fn resolve_staged_disk(&self, staging_name: &str) -> Result<PathBuf> {
        validate_vm_name(staging_name)?;
        let staging_dir = self.staging_dir(staging_name)?;
        let disk_path = self.staged_disk_path(staging_name)?;

        self.verify_staging_root()?;
        verify_existing_private_dir(&staging_dir, self.fix_permissions, "staged disk directory")?;

        let meta = fs::symlink_metadata(&disk_path).wrap_err_with(|| {
            format!(
                "staged disk image for `{staging_name}` does not exist at {}",
                disk_path.display()
            )
        })?;
        ensure_private_regular_file(
            &disk_path,
            &meta,
            current_uid(),
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
        if optional_symlink_metadata(&staging_root, "staging root directory")?.is_none() {
            return Ok(Vec::new());
        }
        verify_existing_private_dir(
            &staging_root,
            self.fix_permissions,
            "staging root directory",
        )?;

        let mut disks = Vec::new();
        for entry in fs::read_dir(&staging_root)
            .wrap_err_with(|| format!("read staging root {}", staging_root.display()))?
        {
            let entry = entry?;
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str().filter(|name| !name.starts_with('.')) else {
                continue;
            };
            disks.extend(self.inspect_staging_entry(name.to_owned(), entry.path()));
        }
        disks.sort_by(|a, b| a.staging_name.cmp(&b.staging_name));
        Ok(disks)
    }

    /// Describe one `<state-dir>/staging/<name>` entry, or return `None` when it holds no
    /// `data.img`.
    fn inspect_staging_entry(
        &self,
        staging_name: String,
        dir_path: PathBuf,
    ) -> Option<StagedDiskInfo> {
        let checked = validate_vm_name(&staging_name).and_then(|()| {
            verify_existing_private_dir(&dir_path, self.fix_permissions, "staged disk directory")
        });
        if let Err(err) = checked {
            return Some(StagedDiskInfo::failed(
                staging_name,
                dir_path,
                format!("{err:#}"),
            ));
        }
        let data_img = dir_path.join("data.img");
        match fs::symlink_metadata(&data_img) {
            Ok(meta) => Some(self.describe_staged_image(staging_name, data_img, &meta)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => Some(StagedDiskInfo::failed(
                staging_name,
                data_img,
                format!("stat staged disk: {err}"),
            )),
        }
    }

    fn describe_staged_image(
        &self,
        staging_name: String,
        data_img: PathBuf,
        meta: &fs::Metadata,
    ) -> StagedDiskInfo {
        if let Err(err) = ensure_private_regular_file(
            &data_img,
            meta,
            current_uid(),
            self.fix_permissions,
            "staged disk image",
        ) {
            return StagedDiskInfo::failed(staging_name, data_img, format!("{err:#}"));
        }
        match sha256_sparse_file(&data_img) {
            Ok(sha256) => StagedDiskInfo {
                staging_name,
                path: Some(data_img),
                sha256: Some(sha256),
                size_bytes: Some(meta.len()),
                error: None,
            },
            Err(err) => {
                StagedDiskInfo::failed(staging_name, data_img, format!("hash staged disk: {err:#}"))
            }
        }
    }

    /// Safely remove `<state-dir>/staging/<staging_name>` under its exclusive staging lock.
    /// Idempotent (returns `removed: false`) when absent; refuses symlinks or foreign ownership.
    pub fn clean_staging_dir(&self, staging_name: &str) -> Result<CleanStagingOutput> {
        validate_vm_name(staging_name)?;
        let staging_root = self.staging_root();
        let dir_path = self.staging_dir(staging_name)?;
        if optional_symlink_metadata(&self.root, "state directory")?.is_none() {
            return Ok(CleanStagingOutput::absent(staging_name, dir_path));
        }
        verify_existing_private_dir(&self.root, self.fix_permissions, "state directory")?;
        if optional_symlink_metadata(&staging_root, "staging root directory")?.is_none() {
            return Ok(CleanStagingOutput::absent(staging_name, dir_path));
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
        let dir_path = self.staging_dir(staging_name)?;
        let Some(meta) = optional_symlink_metadata(&dir_path, "staged disk directory")? else {
            return Ok(CleanStagingOutput::absent(staging_name, dir_path));
        };
        ensure_removable_staging_dir(&dir_path, &meta)?;
        fs::remove_dir_all(&dir_path)
            .wrap_err_with(|| format!("remove staged disk directory {}", dir_path.display()))?;
        sync_dir(&self.staging_root());
        Ok(CleanStagingOutput {
            staging_name: staging_name.to_owned(),
            path: dir_path,
            removed: true,
        })
    }
}

fn ensure_removable_staging_dir(dir_path: &Path, meta: &fs::Metadata) -> Result<()> {
    let current_uid = current_uid();
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
    Ok(())
}

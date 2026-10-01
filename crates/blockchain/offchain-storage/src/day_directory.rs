//! Parent of the shared database and one RocksDB directory per worldwide day.

use std::path::{Path, PathBuf};

use crate::{RocksDbStorage, StorageError};

const SHARED_DIR: &str = "shared";
const TRIBUTE_DAYS_DIR: &str = "tribute-days";
const NOD_DAYS_DIR: &str = "nod-days";

/// Filesystem layout under one off-chain root.
///
/// `shared/` holds the checkpoint. `tribute-days/<day>/` and `nod-days/<day>/`
/// are separate databases. A legacy root whose `CURRENT` sits beside those
/// directories is moved into `shared/` once. A root that already has both is refused.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DayDirectory {
    root: PathBuf,
}

impl DayDirectory {
    /// Create the root if needed and finish a legacy move before any database opens.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, StorageError> {
        let root = root.as_ref();
        std::fs::create_dir_all(root).map_err(StorageError::unavailable)?;
        migrate_legacy_root(root)?;
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    /// Read an existing layout without creating or moving anything.
    #[must_use]
    pub fn inspect(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    #[must_use]
    pub fn shared_path(&self) -> PathBuf {
        self.root.join(SHARED_DIR)
    }

    #[must_use]
    pub fn tribute_day_path(&self, day: u32) -> PathBuf {
        self.day_path(TRIBUTE_DAYS_DIR, day)
    }

    #[must_use]
    pub fn nod_day_path(&self, day: u32) -> PathBuf {
        self.day_path(NOD_DAYS_DIR, day)
    }

    pub fn open_shared(&self) -> Result<RocksDbStorage, StorageError> {
        self.open_database(&self.shared_path())
    }

    pub fn open_tribute_day(&self, day: u32) -> Result<RocksDbStorage, StorageError> {
        self.open_database(&self.tribute_day_path(day))
    }

    pub fn open_nod_day(&self, day: u32) -> Result<RocksDbStorage, StorageError> {
        self.open_database(&self.nod_day_path(day))
    }

    /// Removing a missing day directory succeeds.
    pub fn drop_tribute_day(&self, day: u32) -> Result<(), StorageError> {
        remove_directory_if_present(&self.tribute_day_path(day))
    }

    /// Removing a missing day directory succeeds.
    pub fn drop_nod_day(&self, day: u32) -> Result<(), StorageError> {
        remove_directory_if_present(&self.nod_day_path(day))
    }

    /// Numeric Tribute day directories, ascending. A missing parent is empty.
    pub fn list_tribute_days(&self) -> Result<Vec<u32>, StorageError> {
        list_numbered_dirs(&self.root.join(TRIBUTE_DAYS_DIR))
    }

    /// Numeric Nod day directories, ascending. A missing parent is empty.
    pub fn list_nod_days(&self) -> Result<Vec<u32>, StorageError> {
        list_numbered_dirs(&self.root.join(NOD_DAYS_DIR))
    }

    fn day_path(&self, family: &str, day: u32) -> PathBuf {
        self.root.join(family).join(day.to_string())
    }

    fn open_database(&self, path: &Path) -> Result<RocksDbStorage, StorageError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(StorageError::unavailable)?;
        }
        RocksDbStorage::open(path)
    }
}

fn migrate_legacy_root(root: &Path) -> Result<(), StorageError> {
    let current = root.join("CURRENT");
    let shared = root.join(SHARED_DIR);
    if current.is_file() && shared.exists() {
        return Err(StorageError::Corruption(
            "offchain root has both a legacy database and shared/".into(),
        ));
    }
    if !current.is_file() {
        return Ok(());
    }
    std::fs::create_dir(&shared).map_err(StorageError::unavailable)?;
    let mut current_path = None;
    for entry in std::fs::read_dir(root).map_err(StorageError::unavailable)? {
        let entry = entry.map_err(StorageError::unavailable)?;
        let name = entry.file_name();
        if name == SHARED_DIR || name == TRIBUTE_DAYS_DIR || name == NOD_DAYS_DIR {
            continue;
        }
        if name == "CURRENT" {
            current_path = Some(entry.path());
            continue;
        }
        std::fs::rename(entry.path(), shared.join(name)).map_err(StorageError::unavailable)?;
    }
    if let Some(current_path) = current_path {
        std::fs::rename(current_path, shared.join("CURRENT")).map_err(StorageError::unavailable)?;
    }
    Ok(())
}

fn list_numbered_dirs(path: &Path) -> Result<Vec<u32>, StorageError> {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(StorageError::unavailable(error)),
    };
    let mut days = Vec::new();
    for entry in entries {
        let entry = entry.map_err(StorageError::unavailable)?;
        let Some(name) = entry.file_name().into_string().ok() else {
            continue;
        };
        let Ok(day) = name.parse::<u32>() else {
            continue;
        };
        if entry
            .file_type()
            .map_err(StorageError::unavailable)?
            .is_dir()
        {
            days.push(day);
        }
    }
    days.sort_unstable();
    days.dedup();
    Ok(days)
}

fn remove_directory_if_present(path: &Path) -> Result<(), StorageError> {
    match path.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(StorageError::unavailable(error)),
        Ok(metadata) if metadata.file_type().is_symlink() => Err(StorageError::invalid_argument(
            "refusing to delete a symlinked day directory",
        )),
        Ok(_) => std::fs::remove_dir_all(path).map_err(StorageError::unavailable),
    }
}

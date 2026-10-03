use super::{PartitionId, StorageScope};
use crate::{Namespace, StorageError};
use std::path::{Path, PathBuf};

pub(super) fn relative_path(scope: &StorageScope) -> PathBuf {
    let root = PathBuf::from(&scope.domain);
    match &scope.partition {
        PartitionId::Shared => root.join("shared"),
        PartitionId::Numbered { family, index } => root.join(family).join(index.to_string()),
    }
}

pub(super) fn reject_legacy(root: &Path) -> Result<(), StorageError> {
    for name in ["CURRENT", "shared", "tribute-days", "nod-days"] {
        if root.join(name).exists() {
            return Err(StorageError::Corruption("legacy entity layout: schema 3 requires a fresh root; automatic migration is disabled".into()));
        }
    }
    Ok(())
}
fn directories(path: &Path) -> Result<Vec<String>, StorageError> {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(StorageError::unavailable(error)),
    };
    let mut names = vec![];
    for entry in entries {
        let entry = entry.map_err(StorageError::unavailable)?;
        let kind = entry.file_type().map_err(StorageError::unavailable)?;
        if kind.is_symlink() {
            return Err(StorageError::Corruption(
                "symlinked partition directory".into(),
            ));
        }
        if kind.is_dir() {
            names.push(
                entry
                    .file_name()
                    .into_string()
                    .map_err(|_| StorageError::Corruption("non-UTF8 partition directory".into()))?,
            );
        }
    }
    Ok(names)
}
pub(super) fn enumerate(root: &Path) -> Result<Vec<StorageScope>, StorageError> {
    let mut scopes = vec![];
    for domain in directories(root)? {
        Namespace::new(&*domain)?;
        for family in directories(&root.join(&domain))? {
            if family == "shared" {
                if root.join(&domain).join("shared/CURRENT").is_file() {
                    scopes.push(StorageScope::shared(&domain)?);
                }
                continue;
            }
            for index in directories(&root.join(&domain).join(&family))? {
                let number = index
                    .parse::<u32>()
                    .map_err(|_| StorageError::Corruption("non-numeric entity partition".into()))?;
                if index != number.to_string() {
                    return Err(StorageError::Corruption(
                        "non-canonical entity partition".into(),
                    ));
                }
                let scope = StorageScope::numbered(&domain, &family, number)?;
                if root.join(relative_path(&scope)).join("CURRENT").is_file() {
                    scopes.push(scope);
                }
            }
        }
    }
    scopes.sort();
    Ok(scopes)
}

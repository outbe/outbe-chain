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
        enumerate_domain(root, &domain, &mut scopes)?;
    }
    scopes.sort();
    Ok(scopes)
}
fn enumerate_domain(
    root: &Path,
    domain: &str,
    scopes: &mut Vec<StorageScope>,
) -> Result<(), StorageError> {
    Namespace::new(domain)?;
    for family in directories(&root.join(domain))? {
        if family == "shared" {
            if root.join(domain).join("shared/CURRENT").is_file() {
                scopes.push(StorageScope::shared(domain)?);
            }
            continue;
        }
        enumerate_family(root, domain, &family, scopes)?;
    }
    Ok(())
}
fn enumerate_family(
    root: &Path,
    domain: &str,
    family: &str,
    scopes: &mut Vec<StorageScope>,
) -> Result<(), StorageError> {
    for index in directories(&root.join(domain).join(family))? {
        let number = canonical_partition_index(&index)?;
        let scope = StorageScope::numbered(domain, family, number)?;
        if root.join(relative_path(&scope)).join("CURRENT").is_file() {
            scopes.push(scope);
        }
    }
    Ok(())
}
fn canonical_partition_index(index: &str) -> Result<u32, StorageError> {
    let number = index
        .parse::<u32>()
        .map_err(|_| StorageError::Corruption("non-numeric entity partition".into()))?;
    if index != number.to_string() {
        return Err(StorageError::Corruption(
            "non-canonical entity partition".into(),
        ));
    }
    Ok(number)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn enumeration_only_returns_persisted_scopes_in_sorted_order() {
        let root = tempfile::tempdir().unwrap();
        for path in [
            "nod/wwd/2",
            "nod/wwd/1",
            "nod/shared",
            "nod/other/9",
            "tribute/shared",
            "tribute/wwd/4294967295",
        ] {
            std::fs::create_dir_all(root.path().join(path)).unwrap();
        }
        for path in [
            "nod/wwd/2",
            "nod/wwd/1",
            "nod/shared",
            "tribute/wwd/4294967295",
        ] {
            std::fs::write(root.path().join(path).join("CURRENT"), b"marker").unwrap();
        }
        assert_eq!(
            enumerate(root.path()).unwrap(),
            vec![
                StorageScope::shared("nod").unwrap(),
                StorageScope::numbered("nod", "wwd", 1).unwrap(),
                StorageScope::numbered("nod", "wwd", 2).unwrap(),
                StorageScope::numbered("tribute", "wwd", u32::MAX).unwrap()
            ]
        );
    }
    #[test]
    fn malformed_numbered_directory_is_rejected_before_current_file_check() {
        for index in ["01", "+1", "-1", "4294967296", "x"] {
            let root = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(root.path().join("nod/wwd").join(index)).unwrap();
            let error = enumerate(root.path()).unwrap_err();
            assert!(matches!(error, StorageError::Corruption(_)));
        }
    }
    #[cfg(unix)]
    #[test]
    fn symlinked_partition_is_rejected_and_absent_root_is_empty() {
        let root = tempfile::tempdir().unwrap();
        assert!(enumerate(&root.path().join("missing")).unwrap().is_empty());
        std::os::unix::fs::symlink(root.path(), root.path().join("alias")).unwrap();
        assert_eq!(
            enumerate(root.path()).unwrap_err().to_string(),
            StorageError::Corruption("symlinked partition directory".into()).to_string()
        );
    }
}

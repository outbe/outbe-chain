//! Locate the system checkpoint database in the entity-partitioned projection root.
use std::path::{Path, PathBuf};
pub(crate) fn projection_database(root: &Path) -> PathBuf {
    root.join("system/shared")
}

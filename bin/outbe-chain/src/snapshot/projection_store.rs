//! Locate the RocksDB directory inside an off-chain projection root.

use std::path::{Path, PathBuf};

/// Shared database directory for an off-chain root.
///
/// A day layout stores that database in `shared/`. A root that still has its
/// `CURRENT` file beside the day directories keeps the single database.
pub(crate) fn projection_database(root: &Path) -> PathBuf {
    let shared = root.join("shared");
    if shared.join("CURRENT").is_file() {
        shared
    } else {
        root.to_path_buf()
    }
}

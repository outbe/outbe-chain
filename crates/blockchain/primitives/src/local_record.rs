//! Shared metadata checks for already opened private local records.

use std::{
    fs::Metadata,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
};

/// Checks file type, owner, permission bits and link count without performing I/O.
#[must_use]
pub fn is_private_single_link_file(
    metadata: &Metadata,
    owner_uid: u32,
    required_mode: u32,
) -> bool {
    metadata.file_type().is_file()
        && metadata.uid() == owner_uid
        && metadata.permissions().mode() & 0o777 == required_mode
        && metadata.nlink() == 1
}

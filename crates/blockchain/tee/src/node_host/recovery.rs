mod replacement;

use super::{path_exists, remove_file_if_exists, NodeHostPaths};
use crate::TransportError;
use std::fs::File;
use std::path::Path;

pub(super) use replacement::{reconcile_replacement_state, replacement_authorization};

fn remove_torn_scratch(paths: &NodeHostPaths, scratch: &Path) -> Result<(), TransportError> {
    if path_exists(scratch)? {
        remove_file_if_exists(scratch)?;
        File::open(&paths.root)?.sync_all()?;
    }
    Ok(())
}

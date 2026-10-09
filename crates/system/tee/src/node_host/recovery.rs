mod committed_join;
mod replacement;

use super::{path_exists, remove_file_if_exists, NodeHostPaths};
use crate::TransportError;
use std::fs::File;
use std::path::Path;

pub(super) use committed_join::{
    reconcile_committed_join_state, reconcile_finalized_join_admission_anchor,
};
pub(super) use replacement::{reconcile_replacement_state, replacement_authorization};

fn remove_torn_scratch(paths: &NodeHostPaths, scratch: &Path) -> Result<(), TransportError> {
    if path_exists(scratch)? {
        remove_file_if_exists(scratch)?;
        File::open(&paths.root)?.sync_all()?;
    }
    Ok(())
}

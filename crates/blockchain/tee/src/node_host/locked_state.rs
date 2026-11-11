//! The locked entry to NodeHost state that every state transition uses.

use super::{ensure_private_directory, path_exists, NodeHostPaths, NodeHostStateLock};
use crate::TransportError;
use std::path::Path;

/// Create the private NodeHost state directory if it does not exist, then take
/// the exclusive state lock. Keep the returned lock in a named binding for the
/// whole state transition. A `_` binding releases the lock at once.
pub(super) fn lock_node_host_state(
    node_data_dir: &Path,
) -> Result<(NodeHostPaths, NodeHostStateLock), TransportError> {
    let paths = NodeHostPaths::new(node_data_dir);
    ensure_private_directory(&paths.root)?;
    let state_lock = NodeHostStateLock::acquire(&paths.state_lock)?;
    Ok((paths, state_lock))
}

/// Require the committed manifest and the persistent Noise key. A missing file
/// gives `missing_error`. This check does not read either file.
pub(super) fn require_committed_node_host_state(
    paths: &NodeHostPaths,
    missing_error: &'static str,
) -> Result<(), TransportError> {
    if !path_exists(&paths.manifest)? || !path_exists(&paths.noise_key)? {
        return Err(TransportError::Codec(missing_error.into()));
    }
    Ok(())
}

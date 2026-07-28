use outbe_primitives::{error::Result, storage::StorageHandle};
use outbe_update::ProtocolVersion;

/// Resolves the Outbe protocol version from the exact execution state supplied
/// to the current top-level, nested or historical call.
///
pub(crate) fn resolve(storage: &StorageHandle<'_>) -> Result<ProtocolVersion> {
    outbe_update::api::resolve_active_version(storage.clone())
}

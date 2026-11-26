//! Chain reads behind the `outbe_*` TEE RPC methods.
//!
//! The handler keeps the RPC wire types. This trait supplies the finalized
//! chain facts needed by TEE renewal scheduling and upgrade admission.

#[cfg(test)]
use outbe_primitives::error::PrecompileError;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_primitives::validator_dto::EpochSnapshot;
use outbe_tee::dcap_protocol::DcapOnboardingContextV1;

/// Object-safe chain reads for the RPC handler.
pub trait OutbeRpcStateReader: Send + Sync {
    /// Epoch metadata used by the TEE renewal schedule.
    fn epoch_snapshot(&self, storage: StorageHandle<'_>) -> Result<EpochSnapshot>;

    /// Whether `context` is the live finalized upgrade candidate.
    fn live_upgrade_candidate(
        &self,
        storage: StorageHandle<'_>,
        context: &DcapOnboardingContextV1,
        timestamp: u64,
        legacy_direct_dev_source: bool,
    ) -> Result<bool>;
}

#[cfg(test)]
pub(crate) struct ClosedRpcReader;

#[cfg(test)]
fn closed<T>() -> Result<T> {
    Err(PrecompileError::storage(
        "rpc state reader is not installed",
    ))
}

#[cfg(test)]
impl OutbeRpcStateReader for ClosedRpcReader {
    fn epoch_snapshot(&self, _: StorageHandle<'_>) -> Result<EpochSnapshot> {
        closed()
    }

    fn live_upgrade_candidate(
        &self,
        _: StorageHandle<'_>,
        _: &DcapOnboardingContextV1,
        _: u64,
        _: bool,
    ) -> Result<bool> {
        closed()
    }
}

//! Resident state-key derivation that keeps the in-process cause.

use outbe_core_common::confidential_tee::Domain;
use outbe_tee::in_process::InProcessTeeContext;
use outbe_tee::resident::StateKeyHandle;
use outbe_tee::TransportError;

/// Derive `domain`'s state key. A missing activation stays
/// [`outbe_tee::InProcessDispatchError::Resident`], which the witness reducer
/// records as local unavailability rather than an unsigned enclave refusal.
pub(crate) fn resident_state_key(
    context: &InProcessTeeContext,
    domain: &Domain,
    epoch: u64,
) -> Result<StateKeyHandle, TransportError> {
    context
        .state_key_handle(domain.state_info(), epoch)
        .map_err(|err| outbe_tee::InProcessDispatchError::Resident(err).into())
}

//! Shared witness call, chain check, and resident key lookup.

use alloy_primitives::U256;
use outbe_core_common::confidential_tee::{
    consensus_witness_call, session_witness_call, ResponseAuthSpec, WitnessSchema, NOD, TRIBUTE,
    TRIBUTE_OFFER_ATTESTATION,
};
use outbe_tee::in_process::InProcessTeeContext;
use outbe_tee::resident::{OfferKeyHandle, StateKeyHandle};
use outbe_tee::{CallMeta, TransportError};
use thiserror::Error;

/// Resident chain or missing-runtime failure.
#[derive(Debug, Error)]
pub(crate) enum HostError {
    /// The request chain is not the resident manifest chain.
    #[error("request chain differs from resident chain")]
    ChainMismatch,
    /// The resident chain identifier does not fit in `u64`.
    #[error("resident chain identifier exceeds u64")]
    ChainOverflow,
    /// `session_witness_call` found no installed runtime.
    #[error("in-process TEE runtime is not installed")]
    RuntimeMissing,
}

pub(crate) fn recorded<T: WitnessSchema>(
    label: &'static str,
    execute: impl FnOnce(&InProcessTeeContext) -> Result<T, TransportError>,
    authenticate: impl Fn(&[u8; 32], &T) -> Result<(), TransportError>,
) -> Result<T, TransportError> {
    consensus_witness_call(CallMeta::recorded(label), execute, authenticate)
}

pub(crate) fn session<T>(
    label: &'static str,
    execute: impl FnOnce(&InProcessTeeContext) -> Result<T, TransportError>,
) -> Result<T, TransportError> {
    session_witness_call(CallMeta::session_only(label), execute)
        .ok_or_else(|| TransportError::unavailable(HostError::RuntimeMissing))?
}

pub(crate) fn tribute_state_key(
    context: &InProcessTeeContext,
) -> Result<StateKeyHandle, TransportError> {
    state_key(context, TRIBUTE.state_info())
}

pub(crate) fn nod_state_key(
    context: &InProcessTeeContext,
) -> Result<StateKeyHandle, TransportError> {
    state_key(context, NOD.state_info())
}

fn state_key(
    context: &InProcessTeeContext,
    label: &'static [u8],
) -> Result<StateKeyHandle, TransportError> {
    context
        .state_key_handle(label, 0)
        .map_err(|err| TransportError::from(outbe_tee::InProcessDispatchError::Resident(err)))
}

pub(crate) fn with_offer<T>(
    context: &InProcessTeeContext,
    operation: impl FnOnce(&OfferKeyHandle) -> Result<T, TransportError>,
) -> Result<T, TransportError> {
    context
        .with_offer_key_handle(operation)
        .map_err(|err| TransportError::from(outbe_tee::InProcessDispatchError::Resident(err)))?
}

pub(crate) fn require_chain(
    context: &InProcessTeeContext,
    requested: u64,
) -> Result<(), TransportError> {
    if chain_u64(context)? != requested {
        return Err(TransportError::enclave_error(HostError::ChainMismatch));
    }
    Ok(())
}

pub(crate) fn chain_u64(context: &InProcessTeeContext) -> Result<u64, TransportError> {
    let resident = U256::from_be_bytes(context.manifest().chain_id);
    u64::try_from(resident).map_err(|_| TransportError::enclave_error(HostError::ChainOverflow))
}

pub(crate) fn sign(context: &InProcessTeeContext, preimage: &[u8]) -> Vec<u8> {
    outbe_core_common::confidential_tee::sign_attestation_tag(context, preimage).to_vec()
}

pub(crate) fn auth_spec<'a>(
    key: &'a [u8; 32],
    expected: alloy_primitives::B256,
    actual: alloy_primitives::B256,
    tag: &'a [u8],
) -> ResponseAuthSpec<'a> {
    ResponseAuthSpec {
        attestation_pub: key,
        descriptor: TRIBUTE_OFFER_ATTESTATION,
        expected_inputs_hash: expected,
        actual_inputs_hash: actual,
        tag,
    }
}

pub(crate) fn reject(error: impl std::error::Error + Send + Sync + 'static) -> TransportError {
    TransportError::enclave_error(error)
}

pub(crate) fn same_identity(matches: bool) -> Result<(), TransportError> {
    if matches {
        Ok(())
    } else {
        Err(TransportError::UnexpectedResponse)
    }
}

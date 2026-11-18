//! Ed25519 checks for Fidelity cohort, snapshot, and query attestations.

use alloy_primitives::B256;
use outbe_tee::tee_call::verify_enclave_attestation_lazy;
use outbe_tee::TransportError;

pub use outbe_core_common::confidential_tee::FIDELITY_ATTESTATION;

use crate::protocol::{
    fidelity_cohort_attestation_preimage, fidelity_query_attestation_preimage,
    fidelity_snapshot_attestation_preimage, FidelityCohortResult, FidelityLeagueEntry,
    FidelityQueryResult,
};

fn verify(
    attestation_pub: &[u8; 32],
    tag: &[u8],
    preimage: impl FnOnce() -> Result<Vec<u8>, serde_json::Error>,
) -> Result<(), TransportError> {
    verify_enclave_attestation_lazy(attestation_pub, tag, preimage)
        .map_err(|cause| TransportError::app_attestation(FIDELITY_ATTESTATION, cause))
}

/// Verify a standalone cohort-op attestation tag.
pub fn verify_fidelity_cohort_attestation(
    attestation_pub: &[u8; 32],
    inputs_canonical_hash: B256,
    result: &FidelityCohortResult,
    tag: &[u8],
) -> Result<(), TransportError> {
    verify(attestation_pub, tag, || {
        fidelity_cohort_attestation_preimage(inputs_canonical_hash, result)
    })
}

/// Verify a league-snapshot attestation tag. The signed body is `leagues`.
pub fn verify_fidelity_snapshot_attestation(
    attestation_pub: &[u8; 32],
    inputs_canonical_hash: B256,
    leagues: &[FidelityLeagueEntry],
    tag: &[u8],
) -> Result<(), TransportError> {
    verify(attestation_pub, tag, || {
        fidelity_snapshot_attestation_preimage(inputs_canonical_hash, leagues)
    })
}

/// Verify an index-query attestation tag.
pub fn verify_fidelity_query_attestation(
    attestation_pub: &[u8; 32],
    inputs_canonical_hash: B256,
    result: &FidelityQueryResult,
    tag: &[u8],
) -> Result<(), TransportError> {
    verify(attestation_pub, tag, || {
        fidelity_query_attestation_preimage(inputs_canonical_hash, result)
    })
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::FidelityQueryResult;
    use alloy_primitives::U256;
    use ed25519_dalek::{Signer, SigningKey};
    use outbe_tee::TeeAttestationError;

    #[test]
    fn query_tag_rejects_a_foreign_domain_and_names_fidelity_attestation() {
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let pk = sk.verifying_key().to_bytes();
        let hash = B256::repeat_byte(0xAB);
        let fidelity = FidelityQueryResult {
            rcfi: U256::from(5u64),
            efficiency: U256::from(6u64),
            league: 7,
            inputs_canonical_hash: hash,
            attestation_tag: Vec::new(),
        };
        let tag = sk
            .sign(&fidelity_query_attestation_preimage(hash, &fidelity).unwrap())
            .to_bytes();
        verify_fidelity_query_attestation(&pk, hash, &fidelity, &tag).unwrap();
        let mut foreign = b"outbe/tee/gratis-attestation/v1".to_vec();
        foreign.extend_from_slice(hash.as_slice());
        foreign.extend_from_slice(&0u32.to_be_bytes());
        let foreign_tag = sk.sign(&foreign).to_bytes();
        let cross =
            verify_fidelity_query_attestation(&pk, hash, &fidelity, &foreign_tag).unwrap_err();
        assert_eq!(cross.metric_class(), "fidelity_attestation");
        assert!(matches!(
            &cross,
            TransportError::AppAttestation {
                descriptor,
                source: TeeAttestationError::SignatureMismatch(_),
            } if *descriptor == FIDELITY_ATTESTATION
        ));
    }
}

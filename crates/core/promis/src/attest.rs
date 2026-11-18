//! Ed25519 checks for Promis and Promis-to-Gratis attestations.
//!
//! Both tags use the Promis attestation descriptor. The preimage domain string
//! is what separates them.

use alloy_primitives::B256;
use outbe_tee::tee_call::verify_enclave_attestation_lazy;
use outbe_tee::TransportError;

pub use outbe_core_common::confidential_tee::PROMIS_OP_ATTESTATION;

use crate::protocol::{
    mine_gratis_from_promis_attestation_preimage, promis_op_attestation_preimage,
    MineGratisFromPromisResultV1, PromisOpResult,
};

/// Verify a Promis-op attestation tag against the peer attestation key.
pub fn verify_promis_op_attestation(
    attestation_pub: &[u8; 32],
    inputs_canonical_hash: B256,
    result: &PromisOpResult,
    tag: &[u8],
) -> Result<(), TransportError> {
    verify_enclave_attestation_lazy(attestation_pub, tag, || {
        promis_op_attestation_preimage(inputs_canonical_hash, result)
    })
    .map_err(|cause| TransportError::app_attestation(PROMIS_OP_ATTESTATION, cause))
}

/// Verify a paired `MineGratisFromPromisV1` attestation tag.
pub fn verify_mine_gratis_from_promis_attestation(
    attestation_pub: &[u8; 32],
    inputs_canonical_hash: B256,
    result: &MineGratisFromPromisResultV1,
    tag: &[u8],
) -> Result<(), TransportError> {
    verify_enclave_attestation_lazy(attestation_pub, tag, || {
        mine_gratis_from_promis_attestation_preimage(inputs_canonical_hash, result)
    })
    .map_err(|cause| TransportError::app_attestation(PROMIS_OP_ATTESTATION, cause))
}

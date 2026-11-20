//! Ed25519 check for a Gratis operation attestation.

use alloy_primitives::B256;
use outbe_tee::tee_call::verify_enclave_attestation_lazy;
use outbe_tee::TransportError;

pub use outbe_core_common::confidential_tee::GRATIS_OP_ATTESTATION;

use crate::protocol::{gratis_op_attestation_preimage, GratisOpResult};

/// Verify a Gratis-op attestation tag against the peer attestation key.
pub fn verify_gratis_op_attestation(
    attestation_pub: &[u8; 32],
    inputs_canonical_hash: B256,
    result: &GratisOpResult,
    tag: &[u8],
) -> Result<(), TransportError> {
    verify_enclave_attestation_lazy(attestation_pub, tag, || {
        gratis_op_attestation_preimage(inputs_canonical_hash, result)
    })
    .map_err(|cause| TransportError::app_attestation(GRATIS_OP_ATTESTATION, cause))
}

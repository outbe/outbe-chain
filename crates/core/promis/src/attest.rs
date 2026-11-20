//! Ed25519 checks for Promis operation attestations.

use alloy_primitives::B256;
use outbe_core_common::confidential_tee::verify_app_attestation;
use outbe_tee::TransportError;

pub use outbe_core_common::confidential_tee::PROMIS_OP_ATTESTATION;

use crate::protocol::{promis_op_attestation_preimage, PromisOpResult};

/// Verify a Promis-op attestation tag against the peer attestation key.
pub fn verify_promis_op_attestation(
    attestation_pub: &[u8; 32],
    inputs_canonical_hash: B256,
    result: &PromisOpResult,
    tag: &[u8],
) -> Result<(), TransportError> {
    verify_app_attestation(PROMIS_OP_ATTESTATION, attestation_pub, tag, || {
        promis_op_attestation_preimage(inputs_canonical_hash, result)
    })
}

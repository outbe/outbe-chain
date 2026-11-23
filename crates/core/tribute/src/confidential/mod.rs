//! Tribute engine, day arithmetic, and the Tribute-to-NOD bridge.

mod day_protocol;
mod encrypted_tribute_offer;
mod errors;
mod nod_bridge;
mod process;
mod protocol;
mod source;
mod tribute_day;
mod tribute_encryption;
mod tribute_v2;

pub mod host;

pub use day_protocol::{
    day_fractions_read_attestation_preimage, day_fractions_read_inputs_hash,
    day_operation_attestation_preimage, day_operation_inputs_hash, day_read_attestation_preimage,
    day_read_inputs_hash, lysis_fractions_attestation_preimage, lysis_fractions_inputs_hash,
    LysisFractionsRequestV1, LysisFractionsResultV1, LysisSeriesLoadOutputV1, LysisTributeInputV1,
    TributeDayOpRequestV2, TributeDayOperationV2,
};
pub use encrypted_tribute_offer::process_encrypted_tribute_offer_batch;
pub use nod_bridge::{
    encrypt_nod_for_tribute as encrypt_nod_for_tribute_pure,
    issue_nod_from_tribute as issue_nod_from_tribute_pure, IssueNodFromTributeOutcomeV1,
    IssueNodFromTributeRequestV1,
};
pub use outbe_nod::offer::compute::compute_token_id;
pub use process::{process_tribute_offer_batch, TributeOfferKeyMaterial};
pub use protocol::{
    inputs_canonical_hash, tribute_offer_attestation_preimage, EncryptedTributeOffer,
    TributeOfferResult, TributeOfferStatus, TributePublicInputs, TributeZkContext,
    TributeZkExpectedHashes, WorldwideDay,
};
pub use source::{
    proof_limits, source_binding, TributeSourceCheckpoint, TributeSourceSubject, TributeStateSource,
};
pub use tribute_day::{
    apply_day_operation, execute_lysis_fractions, read_day_amount, read_day_fractions,
    seal_day_fractions,
};
pub use tribute_encryption::{
    decrypt_tribute, derive_tribute_state_key, derive_view_key, encrypt_tribute,
};
pub use tribute_v2::{
    encrypted_offer_attestation_preimage, encrypted_offer_inputs_hash,
    tribute_read_attestation_preimage, tribute_read_inputs_hash, verify_attestation,
    EncryptedTributeOfferResultV2,
};

pub use outbe_core_common::confidential_tee::{
    decrypt_tribute_for_owner, TributeSourceError, TRIBUTE_OFFER_ATTESTATION,
    TRIBUTE_SOURCE_REJECTED,
};

use alloy_primitives::B256;
use outbe_core_common::confidential_tee::verify_response_attestation;
use outbe_tee::TransportError;

/// Verify a tag-0 offer attestation before the host uses the public result.
pub fn verify_tribute_offer_attestation(
    attestation_pub: &[u8; 32],
    inputs_canonical_hash: B256,
    results: &[TributeOfferResult],
    tag: &[u8],
) -> Result<(), TransportError> {
    let preimage = tribute_offer_attestation_preimage(inputs_canonical_hash, results)
        .map_err(TransportError::codec)?;
    verify_response_attestation(
        &outbe_core_common::confidential_tee::ResponseAuthSpec {
            attestation_pub,
            descriptor: TRIBUTE_OFFER_ATTESTATION,
            expected_inputs_hash: inputs_canonical_hash,
            actual_inputs_hash: inputs_canonical_hash,
            tag,
        },
        &preimage,
    )
}

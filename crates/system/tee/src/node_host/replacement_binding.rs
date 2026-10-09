//! Exact finalized Registry view and binding checks for replacement authority.
//! The caller has already verified durable candidate evidence under the lock.

use super::codec_error;
use super::FinalizedReplacementBindingV1;
use crate::TransportError;
use alloy_primitives::B256;
use outbe_primitives::tee_attestation_v1::RegistrationIntentV1;

pub(super) fn validate_finalized_replacement_binding(
    intent: &RegistrationIntentV1,
    finalized: &FinalizedReplacementBindingV1,
) -> Result<(), TransportError> {
    let node_id_hash = intent.node_id.node_id_hash().map_err(codec_error)?;
    let intent_hash = intent.intent_hash().map_err(codec_error)?;
    let expected_chain_id = intent.chain_id;
    let view_is_well_formed =
        finalized_view_identity_present(finalized) && finalized_view_finality_present(finalized);
    let binding_is_well_formed = finalized_binding_ids_present(finalized)
        && finalized_binding_versions_valid(finalized)
        && finalized_binding_keys_present(finalized);
    let exact_match =
        finalized_binding_identity_matches(intent, finalized, expected_chain_id, node_id_hash)
            && finalized_binding_intent_matches(intent, finalized, intent_hash)
            && finalized_binding_keys_match(intent, finalized);
    if !finalized_binding_is_accepted(view_is_well_formed, binding_is_well_formed, exact_match) {
        return Err(TransportError::Codec(
            "finalized Registry binding does not match the durable replacement intent".into(),
        ));
    }
    Ok(())
}

fn finalized_binding_is_accepted(view: bool, binding: bool, exact_match: bool) -> bool {
    view && binding && exact_match
}

fn finalized_view_identity_present(finalized: &FinalizedReplacementBindingV1) -> bool {
    finalized.view.chain_id != [0; 32] && !finalized.view.genesis_hash.is_zero()
}

fn finalized_view_finality_present(finalized: &FinalizedReplacementBindingV1) -> bool {
    finalized.view.block_number != 0
        && !finalized.view.block_hash.is_zero()
        && !finalized.view.state_root.is_zero()
        && finalized.view.consensus_timestamp != 0
}

fn finalized_binding_ids_present(finalized: &FinalizedReplacementBindingV1) -> bool {
    !finalized.node_id_hash.is_zero()
        && !finalized.enclave_id.is_zero()
        && !finalized.binding_id.is_zero()
        && !finalized.intent_hash.is_zero()
}

fn finalized_binding_versions_valid(finalized: &FinalizedReplacementBindingV1) -> bool {
    finalized.binding_version != 0
        && finalized.registration_version != 0
        && finalized.valid_until > finalized.view.consensus_timestamp
}

fn finalized_binding_keys_present(finalized: &FinalizedReplacementBindingV1) -> bool {
    finalized.recipient_x25519 != [0; 32]
        && finalized.attestation_ed25519 != [0; 32]
        && finalized.noise_responder_x25519 != [0; 32]
        && !finalized.node_host_authorization_hash.is_zero()
}

fn finalized_binding_identity_matches(
    intent: &RegistrationIntentV1,
    finalized: &FinalizedReplacementBindingV1,
    expected_chain_id: [u8; 32],
    node_id_hash: B256,
) -> bool {
    finalized_view_matches_intent(intent, finalized, expected_chain_id)
        && finalized_participant_matches_intent(intent, finalized, node_id_hash)
}

fn finalized_view_matches_intent(
    intent: &RegistrationIntentV1,
    finalized: &FinalizedReplacementBindingV1,
    expected_chain_id: [u8; 32],
) -> bool {
    finalized.view.chain_id == expected_chain_id
        && finalized.view.genesis_hash == intent.genesis_hash
}

fn finalized_participant_matches_intent(
    intent: &RegistrationIntentV1,
    finalized: &FinalizedReplacementBindingV1,
    node_id_hash: B256,
) -> bool {
    finalized.node_id_hash == node_id_hash
        && finalized.enclave_id == intent.enclave_id
        && finalized.binding_id == intent.binding_id
}

fn finalized_binding_intent_matches(
    intent: &RegistrationIntentV1,
    finalized: &FinalizedReplacementBindingV1,
    intent_hash: B256,
) -> bool {
    finalized.intent_hash == intent_hash
        && finalized.binding_version == intent.binding_version
        && finalized.registration_version == intent.registration_version
        && finalized.valid_until == intent.requested_valid_until
}

fn finalized_binding_keys_match(
    intent: &RegistrationIntentV1,
    finalized: &FinalizedReplacementBindingV1,
) -> bool {
    finalized.recipient_x25519 == intent.recipient_x25519
        && finalized.attestation_ed25519 == intent.attestation_ed25519
        && finalized.noise_responder_x25519 == intent.noise_responder_x25519
        && finalized.node_host_authorization_hash == intent.node_host_authorization_hash
}

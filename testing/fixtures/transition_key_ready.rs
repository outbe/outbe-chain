//! Signed transition key-ready proof for the TEE attestation tests.

use alloy_primitives::B256;
use ed25519_dalek::Signer as _;
use outbe_primitives::tee_attestation_v1::{RegistrationIntentV1, TransitionKeyReadyProofV1};

/// Make a transition key-ready proof for `intent` and sign it with `signer`.
/// The chain id, the genesis hash and the transition nonce come from `intent`.
pub(crate) fn signed_transition_key_ready_proof(
    intent: &RegistrationIntentV1,
    intent_hash: B256,
    candidate_manifest_hash: B256,
    resident_offer_public: [u8; 32],
    signer: &ed25519_dalek::SigningKey,
) -> TransitionKeyReadyProofV1 {
    let mut proof = TransitionKeyReadyProofV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        transition_intent_hash: intent_hash,
        candidate_manifest_hash,
        transition_nonce: intent.transition_nonce,
        resident_offer_public,
        candidate_attestation_signature: [0; 64],
    };
    proof.candidate_attestation_signature = signer
        .sign(proof.signing_hash().unwrap().as_slice())
        .to_bytes();
    proof
}

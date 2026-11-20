use super::*;
use crate::in_process::admission::FinalizedOnboardingV1;

pub(super) async fn ingest<P: LifecycleChainPort, S: std::error::Error + 'static>(
    port: &P,
    live: &InProcessTeeContext,
    view: &EnrollmentPreflightV1,
    intent: &RegistrationIntentV1,
    node_data_dir: &Path,
) -> Result<EnrollmentOutcome, EnrollmentError<P::Error, S>> {
    let material = port
        .onboarding_proof(view.registry.checkpoint, intent.intent_hash()?)
        .await?;
    material.proof.validate()?;
    validate_proof_checkpoint::<P::Error, S>(&material.proof.admission, view.registry.checkpoint)?;
    let transitions = material
        .proof
        .committee_transitions
        .iter()
        .map(|b| b.to_vec())
        .collect::<Vec<_>>();
    let authorization = live.authorization_witness()?;
    let verified = live.ingest_finalized_onboarding_v1(FinalizedOnboardingV1 {
        artifact: &material.artifact,
        intent,
        authorization: &authorization,
        anchor_outcome: &material.proof.anchor_outcome,
        committee_transitions: &transitions,
        admission_witness: &material.proof.admission,
        expected_network_encryption_public: view.registry.network_encryption_public_key.into(),
        expected_key_epoch: view.registry.key_epoch,
        expected_network_encryption_epoch: view.registry.network_encryption_epoch,
    })?;
    let at = view.registry.checkpoint;
    let future = verified.block_number > at.height || verified.consensus_timestamp > at.timestamp;
    let canonical_head = verified.block_number != at.height
        || (verified.block_hash, verified.state_root) == (at.hash, at.state_root);
    let key_matches =
        live.network_encryption_public()? == view.registry.network_encryption_public_key.0;
    if future || !canonical_head || !key_matches {
        return Err(EnrollmentError::IdentityMismatch);
    }
    let anchor = crate::FinalizedJoinAdmissionAnchorV1 {
        chain_id: intent.chain_id,
        genesis_hash: intent.genesis_hash,
        node_id_hash: intent.node_id.node_id_hash()?,
        enclave_id: intent.enclave_id,
        intent_hash: intent.intent_hash()?,
        finalized_height: verified.block_number,
        finalized_hash: verified.block_hash,
        finalized_state_root: verified.state_root,
        finalized_consensus_timestamp: verified.consensus_timestamp,
    };
    // Disk evidence never substitutes for the cryptographic resident import above.
    persist_live_join_admission_anchor_v1(node_data_dir, live, anchor)?;
    Ok(EnrollmentOutcome::Admitted {
        anchor: Box::new(anchor),
    })
}

fn validate_proof_checkpoint<E: std::error::Error + 'static, S: std::error::Error + 'static>(
    encoded: &[u8],
    at: super::super::LifecycleCheckpointV1,
) -> Result<(), EnrollmentError<E, S>> {
    use alloy_rlp::Decodable;
    let witness =
        crate::finalized_admission::FinalizedAdmissionWitnessV1::decode_canonical(encoded)?;
    let mut rlp = witness.admission.header.as_slice();
    let header = alloy_consensus::Header::decode(&mut rlp)?;
    if !rlp.is_empty()
        || header.number > at.height
        || header.timestamp > at.timestamp
        || (header.number == at.height
            && (header.hash_slow(), header.state_root) != (at.hash, at.state_root))
    {
        return Err(EnrollmentError::IdentityMismatch);
    }
    Ok(())
}

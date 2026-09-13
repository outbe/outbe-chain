use alloy_primitives::Address;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::error::Result;

pub(super) fn prepare_tee_bootstrap(
    ctx: &BlockRuntimeContext,
    payload: &outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2,
    state: &crate::tee_attestation_activation::TeeAttestationChainSpecStateV1,
) -> Result<()> {
    let activation = state.activation().map_err(|error| {
        PrecompileError::Fatal(format!("invalid TEE ChainSpec authority: {error}"))
    })?;
    if ctx.block.block_number < activation.manifest.activation_height {
        return Err(PrecompileError::Revert(
            "OST3 is not active at the current block".into(),
        ));
    }
    let expected_policy = activation
        .policy_at(ctx.block.block_number)
        .map_err(|error| PrecompileError::Fatal(format!("invalid active TEE policy: {error}")))?;
    if &payload.policy != expected_policy {
        return Err(PrecompileError::Revert(
            "OST3 policy does not match the active ChainSpec schedule".into(),
        ));
    }
    outbe_teeregistry::TeeRegistry::new(ctx.storage.clone())
        .install_initial_policy_v1(expected_policy)?;
    Ok(())
}

/// DCAP block-1 bootstrap. The canonical payload already proves bounded shape;
/// this handler binds it to the exact active committee and epoch-0 snapshot,
/// verifies every validator through the production enclave-resident QVL path,
/// and only then finalizes the existing offer-key bootstrap state.
pub(crate) fn run_tee_bootstrap_v1(
    ctx: &BlockRuntimeContext,
    payload: &outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2,
) -> Result<()> {
    use outbe_primitives::tee_signatures::recover_signer;
    use outbe_teeregistry::{TeeBootstrapData, TeeRegistry};
    use std::collections::BTreeSet;

    let mut registry = TeeRegistry::new(ctx.storage.clone());
    if registry.is_bootstrapped()? {
        return Err(PrecompileError::Revert(
            "TeeBootstrapV2: registry already bootstrapped".into(),
        ));
    }
    if payload.committee_snapshot_block != ctx.block.block_number {
        return Err(PrecompileError::Revert(format!(
            "TeeBootstrapV2: committee snapshot block {} does not equal current block {}",
            payload.committee_snapshot_block, ctx.block.block_number
        )));
    }

    let active_policy = registry.active_policy_v1()?;
    if active_policy != payload.policy {
        return Err(PrecompileError::Revert(
            "TeeBootstrapV2: payload policy is not the authoritative active V1 policy".into(),
        ));
    }

    let committee: BTreeSet<Address> =
        outbe_validatorset::contract::ValidatorSet::new(ctx.storage.clone())
            .get_active_consensus_set()?
            .into_iter()
            .map(|record| record.validator_address)
            .collect();
    if committee.is_empty() {
        return Err(PrecompileError::Revert(
            "TeeBootstrapV2: active consensus committee is empty".into(),
        ));
    }
    let participant_validators = payload
        .participants
        .iter()
        .map(|participant| Address::from(participant.validator_binding.validator))
        .collect::<BTreeSet<_>>();
    if participant_validators != committee || payload.participants.len() != committee.len() {
        return Err(PrecompileError::Revert(
            "TeeBootstrapV2: participants must equal the complete active consensus committee"
                .into(),
        ));
    }

    let signing_hash = payload.signing_hash().map_err(|error| {
        PrecompileError::Fatal(format!(
            "canonical TeeBootstrapV2 cannot produce its signing hash: {error}"
        ))
    })?;
    for signature in &payload.committee_signatures {
        let recovered = recover_signer(&signing_hash, &signature.signature)?;
        if recovered != signature.validator || !committee.contains(&signature.validator) {
            return Err(PrecompileError::Revert(format!(
                "TeeBootstrapV2: invalid committee signature for {}",
                signature.validator
            )));
        }
    }

    let snapshot =
        outbe_validatorset::state::read_committee_snapshot_for_epoch(ctx.storage.clone(), 0)?
            .ok_or_else(|| {
                PrecompileError::Revert(
                    "TeeBootstrapV2: epoch-0 committee snapshot is missing".into(),
                )
            })?;
    let committee_snapshot_hash = outbe_validatorset::committee_set_hash_v2(0, &snapshot);
    if payload.committee_snapshot_hash != committee_snapshot_hash {
        return Err(PrecompileError::Revert(
            "TeeBootstrapV2: committee snapshot hash mismatch".into(),
        ));
    }

    for (index, participant) in payload.participants.iter().enumerate() {
        let evidence = payload
            .logical_evidence(index)
            .and_then(|evidence| evidence.encode_canonical())
            .map_err(|error| {
                PrecompileError::Fatal(format!(
                    "canonical TeeBootstrapV2 cannot reconstruct evidence {index}: {error}"
                ))
            })?;
        registry.register_enclave_v1(
            Address::from(participant.validator_binding.validator),
            &evidence,
            &participant.node_signature,
            &participant.enclave_signature,
            &participant.validator_binding,
            &participant.validator_signature,
            &participant.node_binding_signature,
        )?;
    }

    let policy_hash = payload.policy.policy_hash().map_err(|error| {
        PrecompileError::Fatal(format!(
            "authoritative TeeBootstrapV2 policy cannot be hashed: {error}"
        ))
    })?;
    registry.write_bootstrap(&TeeBootstrapData {
        tribute_offer_public_key: payload.tribute_offer_public_key,
        policy_hash,
        key_epoch: payload.key_epoch,
        tribute_offer_epoch: payload.tribute_offer_epoch,
        dkg_transcript_hash: payload.dkg_transcript_hash,
        committee_snapshot_block: payload.committee_snapshot_block,
        committee_snapshot_hash,
        tribute_offer_group_public_key: payload.tribute_offer_group_public_key.clone(),
    })
}

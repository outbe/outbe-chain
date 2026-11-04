//! Block 1 (`GenesisBootstrap`) TEE / DCAP bootstrap lifecycle module.
//!
//! Consolidates all Block-1 `OST3` (`TeeBootstrapV2`) validation, proposer
//! attestation key extraction for the Block-1 execution witness, and EVM
//! `TeeRegistry` bootstrap execution behind a single module boundary.

use std::collections::BTreeSet;

use alloy_consensus::Transaction as _;
use alloy_primitives::Address;
use outbe_primitives::{
    block::{BlockHeightRegime, BlockRuntimeContext},
    error::Result,
    system_tx::SystemTx,
    tee_attestation_v1::TeePolicyV1,
    tee_bootstrap_v2::TeeBootstrapV2,
    tee_signatures::recover_signer,
};
use outbe_teeregistry::{
    v1::{EnclaveEvidenceV1, NodeHostAssociationV1},
    TeeBootstrapData, TeeRegistry,
};
use reth_ethereum::TransactionSigned;
use reth_primitives_traits::Recovered;

use crate::{
    errors::{BeginBlockPrecompileError, SystemTxBuildError},
    tee_attestation_activation::TeeAttestationChainSpecStateV1,
};

/// Preflights a `TeeBootstrapV2` payload and verifies its policy against the
/// `ChainSpec` activation schedule when building Block-1 system transactions.
pub fn validate_tee_bootstrap_for_build(
    block_number: u64,
    payload: &TeeBootstrapV2,
    state: &TeeAttestationChainSpecStateV1,
) -> std::result::Result<(), SystemTxBuildError> {
    payload
        .preflight()
        .map_err(SystemTxBuildError::invalid_block1_ost3_bootstrap_payload)?;
    let activation = state
        .activation()
        .map_err(SystemTxBuildError::invalid_mandatory_tee_chain_spec_authority)?;
    let expected_policy = activation
        .policy_at(block_number)
        .map_err(SystemTxBuildError::from)?;
    if &payload.authority.policy != expected_policy {
        return Err(SystemTxBuildError::Block1Ost3PolicyScheduleMismatch);
    }
    Ok(())
}

/// Extracts the Block-1 proposer's Ed25519 enclave attestation public key from
/// the `TeeBootstrapV2` payload before `TeeBootstrap` commits to `TeeRegistry`.
pub fn extract_block_one_proposer_attestation_key(
    regime: BlockHeightRegime,
    pending_tee_bootstrap: Option<&TeeBootstrapV2>,
    expected_begin_system_txs: &[Recovered<TransactionSigned>],
    proposer: Address,
) -> Option<[u8; 32]> {
    if !regime.is_genesis_bootstrap() {
        return None;
    }
    if let Some(payload) = pending_tee_bootstrap {
        if let Some(key) = attestation_for_proposer(payload, proposer) {
            return Some(key);
        }
    }
    for tx in expected_begin_system_txs {
        let Ok(SystemTx::TeeBootstrap { payload }) = SystemTx::decode(tx.input().as_ref()) else {
            continue;
        };
        if let Some(key) = attestation_for_proposer(&payload, proposer) {
            return Some(key);
        }
    }
    None
}

/// Returns the Ed25519 attestation public key for `proposer` inside `payload`.
pub fn attestation_for_proposer(payload: &TeeBootstrapV2, proposer: Address) -> Option<[u8; 32]> {
    payload
        .participants
        .iter()
        .find(|participant| Address::from(participant.validator_binding.validator) == proposer)
        .map(|participant| participant.intent.attestation_ed25519)
}

/// Installs the initial authoritative TEE policy and executes the Block-1 DCAP bootstrap.
pub fn execute_tee_bootstrap(
    ctx: &BlockRuntimeContext,
    payload: &TeeBootstrapV2,
    state: &TeeAttestationChainSpecStateV1,
) -> Result<()> {
    prepare_tee_bootstrap(ctx, payload, state)?;
    run_tee_bootstrap_v1(ctx, payload)
}

/// Validates the `ChainSpec` TEE activation schedule and installs the initial policy.
pub fn prepare_tee_bootstrap(
    ctx: &BlockRuntimeContext,
    payload: &TeeBootstrapV2,
    state: &TeeAttestationChainSpecStateV1,
) -> Result<()> {
    let expected_policy = resolve_expected_policy(ctx.block.block_number, payload, state)?;
    TeeRegistry::new(ctx.storage.clone()).install_initial_policy_v1(&expected_policy)?;
    Ok(())
}

fn resolve_expected_policy(
    block_number: u64,
    payload: &TeeBootstrapV2,
    state: &TeeAttestationChainSpecStateV1,
) -> Result<TeePolicyV1> {
    let activation = state
        .activation()
        .map_err(BeginBlockPrecompileError::invalid_tee_chain_spec_authority)?;
    if block_number < activation.manifest.activation_height {
        return Err(BeginBlockPrecompileError::Ost3NotActive.into());
    }
    let expected_policy = activation
        .policy_at(block_number)
        .map_err(BeginBlockPrecompileError::invalid_active_tee_policy)?;
    if &payload.authority.policy != expected_policy {
        return Err(BeginBlockPrecompileError::Ost3PolicyScheduleMismatch.into());
    }
    Ok(expected_policy.clone())
}

/// DCAP Block-1 bootstrap handler:
/// 1. Binds `payload` to the active consensus committee and epoch-0 snapshot.
/// 2. Verifies each validator's enclave evidence and node-host association.
/// 3. Commits the initial [`TeeBootstrapData`] to [`TeeRegistry`].
pub fn run_tee_bootstrap_v1(ctx: &BlockRuntimeContext, payload: &TeeBootstrapV2) -> Result<()> {
    let mut registry = TeeRegistry::new(ctx.storage.clone());
    if registry.is_bootstrapped()? {
        return Err(BeginBlockPrecompileError::TeeRegistryAlreadyBootstrapped.into());
    }
    if payload.authority.committee_snapshot_block != ctx.block.block_number {
        return Err(
            BeginBlockPrecompileError::TeeCommitteeSnapshotBlockMismatch {
                snapshot_block: payload.authority.committee_snapshot_block,
                current_block: ctx.block.block_number,
            }
            .into(),
        );
    }

    let active_policy = registry.active_policy_v1()?;
    if active_policy != payload.authority.policy {
        return Err(BeginBlockPrecompileError::TeePayloadPolicyNotAuthoritative.into());
    }

    let committee = read_bootstrap_committee(ctx, payload)?;

    let signing_hash = payload
        .signing_hash()
        .map_err(BeginBlockPrecompileError::tee_cannot_produce_signing_hash)?;
    for signature in &payload.committee_signatures {
        let recovered = recover_signer(&signing_hash, &signature.signature)?;
        if recovered != signature.validator || !committee.contains(&signature.validator) {
            return Err(BeginBlockPrecompileError::TeeInvalidCommitteeSignature {
                validator: signature.validator,
            }
            .into());
        }
    }

    let snapshot =
        outbe_validatorset::state::read_committee_snapshot_for_epoch(ctx.storage.clone(), 0)?
            .ok_or(BeginBlockPrecompileError::TeeMissingEpoch0CommitteeSnapshot)?;
    let committee_snapshot_hash = outbe_validatorset::committee_set_hash_v2(0, &snapshot);
    if payload.authority.committee_snapshot_hash != committee_snapshot_hash {
        return Err(BeginBlockPrecompileError::TeeCommitteeSnapshotHashMismatch.into());
    }

    for (index, participant) in payload.participants.iter().enumerate() {
        let evidence = payload
            .logical_evidence(index)
            .and_then(|evidence| evidence.encode_canonical())
            .map_err(|error| {
                BeginBlockPrecompileError::tee_cannot_reconstruct_evidence(index, error)
            })?;
        registry.register_enclave_v1(
            EnclaveEvidenceV1 {
                caller: Address::from(participant.validator_binding.validator),
                evidence: &evidence,
                node_signature: &participant.node_signature,
                enclave_signature: &participant.enclave_signature,
            },
            NodeHostAssociationV1 {
                binding: &participant.validator_binding,
                validator_signature: &participant.validator_signature,
                node_binding_signature: &participant.node_binding_signature,
            },
        )?;
    }

    let policy_hash = payload
        .authority
        .policy
        .policy_hash()
        .map_err(BeginBlockPrecompileError::tee_cannot_hash_authoritative_policy)?;
    registry.write_bootstrap(&TeeBootstrapData {
        tribute_offer_public_key: payload.authority.tribute_offer_public_key,
        policy_hash,
        key_epoch: payload.authority.key_epoch,
        tribute_offer_epoch: payload.authority.tribute_offer_epoch,
        dkg_transcript_hash: payload.authority.dkg_transcript_hash,
        committee_snapshot_block: payload.authority.committee_snapshot_block,
        committee_snapshot_hash,
        tribute_offer_group_public_key: payload.authority.tribute_offer_group_public_key.clone(),
    })
}

fn read_bootstrap_committee(
    ctx: &BlockRuntimeContext,
    payload: &TeeBootstrapV2,
) -> Result<BTreeSet<Address>> {
    let committee: BTreeSet<Address> =
        outbe_validatorset::contract::ValidatorSet::new(ctx.storage.clone())
            .get_active_consensus_set()?
            .into_iter()
            .map(|record| record.validator_address)
            .collect();
    if committee.is_empty() {
        return Err(BeginBlockPrecompileError::TeeEmptyActiveConsensusCommittee.into());
    }
    let participant_validators = payload
        .participants
        .iter()
        .map(|participant| Address::from(participant.validator_binding.validator))
        .collect::<BTreeSet<_>>();
    if participant_validators != committee || payload.participants.len() != committee.len() {
        return Err(BeginBlockPrecompileError::TeeParticipantsMismatchActiveCommittee.into());
    }

    Ok(committee)
}

//! Block-1 TEE bootstrap checks that do not depend on the chain-spec schedule.
//!
//! The caller resolves the authoritative [`TeePolicyV1`] from the chain spec.
//! This module checks that payload against the resolved policy, the active
//! consensus committee, and the epoch-0 snapshot, then registers the enclaves.

use std::collections::BTreeSet;
use std::fmt::Display;

use alloy_primitives::{Address, B256};
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::tee_attestation_v1::TeePolicyV1;
use outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2;
use outbe_primitives::tee_signatures::recover_signer;
use thiserror::Error;

use crate::runtime::TeeBootstrapData;
use crate::schema::TeeRegistry;

/// Payload failures found while a block-1 system transaction is being built.
///
/// These stay build errors. They do not convert into a precompile revert.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TeeBootstrapBuildError {
    /// Canonical preflight rejected the payload.
    #[error("invalid block-1 OST3 bootstrap payload: {0}")]
    InvalidBlock1Ost3BootstrapPayload(String),
    /// The payload policy is not the policy the caller resolved for this block.
    #[error("block-1 OST3 policy does not match the immutable ChainSpec schedule")]
    Block1Ost3PolicyScheduleMismatch,
}

impl TeeBootstrapBuildError {
    /// Wraps a preflight failure with the block-1 payload message.
    pub fn invalid_payload(error: impl Display) -> Self {
        Self::InvalidBlock1Ost3BootstrapPayload(error.to_string())
    }
}

/// Begin-block failures of the block-1 DCAP bootstrap.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TeeBootstrapError {
    /// The registry already holds a bootstrap record.
    #[error("TeeBootstrapV2: registry already bootstrapped")]
    TeeRegistryAlreadyBootstrapped,
    /// The payload names a snapshot block other than the block being executed.
    #[error(
        "TeeBootstrapV2: committee snapshot block {snapshot_block} does not equal current block {current_block}"
    )]
    TeeCommitteeSnapshotBlockMismatch {
        /// Snapshot height carried by the payload.
        snapshot_block: u64,
        /// Height of the block under execution.
        current_block: u64,
    },
    /// The payload policy is not the installed authoritative V1 policy.
    #[error("TeeBootstrapV2: payload policy is not the authoritative active V1 policy")]
    TeePayloadPolicyNotAuthoritative,
    /// The payload policy is not the policy the caller resolved for this block.
    #[error("OST3 policy does not match the active ChainSpec schedule")]
    Ost3PolicyScheduleMismatch,
    /// The payload cannot produce its committee signing hash.
    #[error("canonical TeeBootstrapV2 cannot produce its signing hash: {0}")]
    TeeCannotProduceSigningHash(String),
    /// A committee signature does not recover to a member of the active set.
    #[error("TeeBootstrapV2: invalid committee signature for {validator}")]
    TeeInvalidCommitteeSignature {
        /// Validator address recovered from the rejected signature.
        validator: Address,
    },
    /// Epoch 0 has no stored committee snapshot.
    #[error("TeeBootstrapV2: epoch-0 committee snapshot is missing")]
    TeeMissingEpoch0CommitteeSnapshot,
    /// The payload committee hash does not match the stored epoch-0 snapshot.
    #[error("TeeBootstrapV2: committee snapshot hash mismatch")]
    TeeCommitteeSnapshotHashMismatch,
    /// One participant's logical evidence cannot be reconstructed.
    #[error("canonical TeeBootstrapV2 cannot reconstruct evidence {index}: {reason}")]
    TeeCannotReconstructEvidence {
        /// Index of the participant whose evidence failed.
        index: usize,
        /// Codec failure text.
        reason: String,
    },
    /// The authoritative policy cannot be hashed.
    #[error("authoritative TeeBootstrapV2 policy cannot be hashed: {0}")]
    TeeCannotHashAuthoritativePolicy(String),
    /// The active consensus set is empty.
    #[error("TeeBootstrapV2: active consensus committee is empty")]
    TeeEmptyActiveConsensusCommittee,
    /// Participants are not exactly the active consensus set.
    #[error("TeeBootstrapV2: participants must equal the complete active consensus committee")]
    TeeParticipantsMismatchActiveCommittee,
    /// Enclave registration is compiled only with `tee-attestation-v1`.
    #[cfg(not(feature = "tee-attestation-v1"))]
    #[error("block-1 TEE bootstrap registration requires tee-attestation-v1")]
    RegistrationRequiresTeeAttestationV1,
}

impl TeeBootstrapError {
    fn cannot_produce_signing_hash(error: impl Display) -> Self {
        Self::TeeCannotProduceSigningHash(error.to_string())
    }

    #[cfg(feature = "tee-attestation-v1")]
    fn cannot_reconstruct_evidence(index: usize, error: impl Display) -> Self {
        Self::TeeCannotReconstructEvidence {
            index,
            reason: error.to_string(),
        }
    }

    fn cannot_hash_authoritative_policy(error: impl Display) -> Self {
        Self::TeeCannotHashAuthoritativePolicy(error.to_string())
    }
}

impl From<TeeBootstrapError> for PrecompileError {
    fn from(error: TeeBootstrapError) -> Self {
        Self::Revert(error.to_string())
    }
}

/// Preflights `payload` and checks its policy against the caller-resolved schedule.
pub fn validate_tee_bootstrap_for_build(
    payload: &TeeBootstrapV2,
    expected_policy: &TeePolicyV1,
) -> std::result::Result<(), TeeBootstrapBuildError> {
    payload
        .preflight()
        .map_err(TeeBootstrapBuildError::invalid_payload)?;
    if &payload.authority.policy != expected_policy {
        return Err(TeeBootstrapBuildError::Block1Ost3PolicyScheduleMismatch);
    }
    Ok(())
}

/// Binds `payload` to the active committee and writes the founding bootstrap.
pub fn run_tee_bootstrap_v1(
    ctx: &BlockRuntimeContext<'_>,
    payload: &TeeBootstrapV2,
    expected_policy: &TeePolicyV1,
) -> Result<()> {
    let mut registry = TeeRegistry::new(ctx.storage.clone());
    if registry.is_bootstrapped()? {
        return Err(TeeBootstrapError::TeeRegistryAlreadyBootstrapped.into());
    }
    if payload.authority.committee_snapshot_block != ctx.block.block_number {
        return Err(TeeBootstrapError::TeeCommitteeSnapshotBlockMismatch {
            snapshot_block: payload.authority.committee_snapshot_block,
            current_block: ctx.block.block_number,
        }
        .into());
    }
    if &payload.authority.policy != expected_policy {
        return Err(TeeBootstrapError::Ost3PolicyScheduleMismatch.into());
    }
    let active_policy = registry.active_policy_v1()?;
    if active_policy != payload.authority.policy {
        return Err(TeeBootstrapError::TeePayloadPolicyNotAuthoritative.into());
    }

    let committee = read_bootstrap_committee(ctx, payload)?;
    verify_committee_signatures(payload, &committee)?;
    let committee_snapshot_hash = epoch0_snapshot_hash(ctx, payload)?;
    register_bootstrap_participants(&mut registry, payload)?;
    let policy_hash = payload
        .authority
        .policy
        .policy_hash()
        .map_err(TeeBootstrapError::cannot_hash_authoritative_policy)?;
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

fn verify_committee_signatures(
    payload: &TeeBootstrapV2,
    committee: &BTreeSet<Address>,
) -> Result<()> {
    let signing_hash = payload
        .signing_hash()
        .map_err(TeeBootstrapError::cannot_produce_signing_hash)?;
    for signature in &payload.committee_signatures {
        let recovered = recover_signer(&signing_hash, &signature.signature)?;
        if recovered != signature.validator || !committee.contains(&signature.validator) {
            return Err(TeeBootstrapError::TeeInvalidCommitteeSignature {
                validator: signature.validator,
            }
            .into());
        }
    }
    Ok(())
}

fn epoch0_snapshot_hash(ctx: &BlockRuntimeContext<'_>, payload: &TeeBootstrapV2) -> Result<B256> {
    let snapshot =
        outbe_validatorset::state::read_committee_snapshot_for_epoch(ctx.storage.clone(), 0)?
            .ok_or(TeeBootstrapError::TeeMissingEpoch0CommitteeSnapshot)?;
    let committee_snapshot_hash = outbe_validatorset::committee_set_hash_v2(0, &snapshot);
    if payload.authority.committee_snapshot_hash != committee_snapshot_hash {
        return Err(TeeBootstrapError::TeeCommitteeSnapshotHashMismatch.into());
    }
    Ok(committee_snapshot_hash)
}

fn read_bootstrap_committee(
    ctx: &BlockRuntimeContext<'_>,
    payload: &TeeBootstrapV2,
) -> Result<BTreeSet<Address>> {
    let committee: BTreeSet<Address> =
        outbe_validatorset::contract::ValidatorSet::new(ctx.storage.clone())
            .get_active_consensus_set()?
            .into_iter()
            .map(|record| record.validator_address)
            .collect();
    if committee.is_empty() {
        return Err(TeeBootstrapError::TeeEmptyActiveConsensusCommittee.into());
    }
    let participant_validators = payload
        .participants
        .iter()
        .map(|participant| Address::from(participant.validator_binding.validator))
        .collect::<BTreeSet<_>>();
    if participant_validators != committee || payload.participants.len() != committee.len() {
        return Err(TeeBootstrapError::TeeParticipantsMismatchActiveCommittee.into());
    }
    Ok(committee)
}

#[cfg(feature = "tee-attestation-v1")]
fn register_bootstrap_participants(
    registry: &mut TeeRegistry<'_>,
    payload: &TeeBootstrapV2,
) -> Result<()> {
    use crate::v1::{EnclaveEvidenceV1, NodeHostAssociationV1};

    for (index, participant) in payload.participants.iter().enumerate() {
        let evidence = payload
            .logical_evidence(index)
            .and_then(|item| item.encode_canonical())
            .map_err(|error| TeeBootstrapError::cannot_reconstruct_evidence(index, error))?;
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
    Ok(())
}

#[cfg(not(feature = "tee-attestation-v1"))]
fn register_bootstrap_participants(
    registry: &mut TeeRegistry<'_>,
    payload: &TeeBootstrapV2,
) -> Result<()> {
    let _ = (registry, payload);
    Err(TeeBootstrapError::RegistrationRequiresTeeAttestationV1.into())
}

//! Block-1 TEE bootstrap checks that do not depend on the chain-spec schedule.
//!
//! The caller resolves the authoritative [`TeePolicyV1`] from the chain spec.
//! This module checks that payload against the resolved policy, the active
//! consensus committee, and the epoch-0 snapshot, then registers the enclaves.

use std::collections::BTreeSet;
use std::fmt::Display;

use alloy_primitives::{Address, Bytes, B256};
use outbe_primitives::block::{BlockRuntimeContext, ProtocolModule};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::system_tx_context::{
    SystemTxBuildContext, SystemTxPayloadBuildError, SystemTxPayloadValidationError,
    SystemTxValidationContext,
};
use outbe_primitives::tee_attestation_v1::TeePolicyV1;
use outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2;
use outbe_primitives::tee_signatures::recover_signer;
use thiserror::Error;

use crate::runtime::TeeBootstrapData;
use crate::schema::TeeRegistry;

/// Installs the authoritative block-1 policy and founding enclave bindings.
pub struct TeeBootstrapModule;

impl ProtocolModule for TeeBootstrapModule {
    const NAME: &'static str = "tee_bootstrap";

    fn build_payload(
        ctx: &SystemTxBuildContext<'_>,
    ) -> std::result::Result<Bytes, SystemTxPayloadBuildError> {
        let payload = ctx
            .pending_tee_bootstrap
            .ok_or(SystemTxPayloadBuildError::MissingBlock1TeeBootstrapPayload)?;
        if let Some(expected_policy) = ctx.expected_tee_policy {
            validate_tee_bootstrap_for_build(payload, expected_policy).map_err(
                |error| match error {
                    TeeBootstrapBuildError::Block1Ost3PolicyScheduleMismatch => {
                        SystemTxPayloadBuildError::Block1TeeBootstrapPolicyScheduleMismatch
                    }
                    TeeBootstrapBuildError::InvalidBlock1Ost3BootstrapPayload(error) => {
                        SystemTxPayloadBuildError::InvalidBlock1TeeBootstrapPayload(error)
                    }
                },
            )?;
        } else {
            payload.preflight()?;
        }
        Ok(outbe_primitives::tee_bootstrap_v2::codec::encode_canonical(
            payload,
        )?)
    }

    fn validate_payload(
        payload: &[u8],
        ctx: &SystemTxValidationContext<'_>,
    ) -> std::result::Result<(), SystemTxPayloadValidationError> {
        outbe_primitives::system_tx::binding::validate_tee_bootstrap_payload(
            payload,
            ctx.block_number,
        )?;
        Ok(())
    }

    fn begin_block(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        let sys_ctx = ctx.system_tx()?;
        let payload =
            outbe_primitives::tee_bootstrap_v2::codec::decode_canonical(sys_ctx.payload.as_ref())
                .map_err(|error| {
                crate::errors::TeeRegistryError::codec_revert("TeeBootstrapV2", error)
            })?;
        let expected_policy = sys_ctx
            .chain_spec_tee_policy
            .as_ref()
            .ok_or(TeeBootstrapError::TeePayloadPolicyNotAuthoritative)?;
        TeeRegistry::new(ctx.storage.clone()).install_initial_policy_v1(expected_policy)?;
        run_tee_bootstrap_v1(ctx, &payload, expected_policy)
    }
}

/// Payload failures found while a block-1 system transaction is being built.
///
/// These stay build errors. They do not convert into a precompile revert.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TeeBootstrapBuildError {
    /// Canonical preflight rejected the payload.
    #[error("invalid block-1 OST3 bootstrap payload: {0}")]
    InvalidBlock1Ost3BootstrapPayload(#[from] outbe_primitives::tee_attestation_v1::CodecError),
    /// The payload policy is not the policy the caller resolved for this block.
    #[error("block-1 OST3 policy does not match the immutable ChainSpec schedule")]
    Block1Ost3PolicyScheduleMismatch,
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
    payload.preflight()?;
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
        tribute_offer_public_key: payload.authority.network_encryption_public_key,
        policy_hash,
        key_epoch: payload.authority.key_epoch,
        tribute_offer_epoch: payload.authority.network_encryption_epoch,
        dkg_transcript_hash: payload.authority.dkg_transcript_hash,
        committee_snapshot_block: payload.authority.committee_snapshot_block,
        committee_snapshot_hash,
        tribute_offer_group_public_key: payload
            .authority
            .network_encryption_group_public_key
            .clone(),
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

#[cfg(test)]
mod protocol_module_tests {
    use super::*;
    use outbe_primitives::{
        block::{BlockContext, BlockHeightRegime},
        reshare_artifact::OutbeBlockArtifacts,
        storage::hashmap::HashMapStorageProvider,
        system_tx_context::SystemTxContext,
    };

    #[test]
    fn module_requires_bootstrap_payload_and_rejects_invalid_bytes() {
        let artifacts = OutbeBlockArtifacts::default();
        let build = SystemTxBuildContext {
            regime: BlockHeightRegime::GenesisBootstrap,
            block_number: 1,
            parent_hash: B256::ZERO,
            artifacts: &artifacts,
            parent_consensus_metadata: None,
            pending_tee_bootstrap: None,
            expected_tee_policy: None,
        };
        assert!(matches!(
            TeeBootstrapModule::build_payload(&build),
            Err(SystemTxPayloadBuildError::MissingBlock1TeeBootstrapPayload)
        ));
        let validation = SystemTxValidationContext {
            regime: build.regime,
            block_number: 1,
            parent_hash: B256::ZERO,
            artifacts: &artifacts,
        };
        assert!(matches!(
            TeeBootstrapModule::validate_payload(&[], &validation),
            Err(SystemTxPayloadValidationError::TeeBootstrap {
                block_number: 1,
                ..
            })
        ));
        let mut provider = HashMapStorageProvider::new(1);
        provider.enter(|storage| {
            let mut ctx = BlockRuntimeContext::new(
                BlockContext {
                    block_number: 1,
                    timestamp: 1,
                    chain_id: 1,
                    genesis_hash: B256::ZERO,
                    genesis_timestamp: 1,
                    proposer: Address::ZERO,
                    validators: vec![],
                    block_gas_limit: 0,
                    user_gas_used: 0,
                },
                storage,
            );
            assert!(TeeBootstrapModule::begin_block(&ctx).is_err());
            ctx.system_tx = Some(SystemTxContext {
                payload: Bytes::from_static(&[0xff]),
                ..Default::default()
            });
            assert!(TeeBootstrapModule::begin_block(&ctx).is_err());
            assert!(!TeeRegistry::new(ctx.storage.clone())
                .is_bootstrapped()
                .unwrap());
        });
        assert!(provider.storage.is_empty());
    }
}

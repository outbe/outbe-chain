//! Storage-level Phase 1 finalization and late-credit settlement.
//!
//! Certificate verification that needs a Reth database stays with the executor.
//! These functions apply the checks and writes that use protocol storage.

use std::collections::BTreeSet;
use std::fmt::Display;

use alloy_primitives::{Address, Bytes, B256};
use outbe_primitives::block::{BlockRuntimeContext, MetadosisEntitlementContext, ProtocolModule};
use outbe_primitives::consensus::LATE_FINALIZE_WINDOW_K;
use outbe_primitives::consensus_metadata::CertifiedParentAccountingMetadata;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::proof::verify_late_finalize_proof;
use outbe_primitives::reshare_artifact::{
    decode_late_finalize_credits_artifact, encode_late_finalize_credits_artifact,
    LateFinalizeCreditsArtifact, PerBlockCredit,
};
use outbe_primitives::storage::{
    metadosis_late_settlement_binding, MetadosisMutationEntitlements, MetadosisMutationPurposeTag,
    StorageHandle,
};
use outbe_primitives::system_tx_context::{
    SystemTxBuildContext, SystemTxExecutionContext, SystemTxPayloadBuildError,
    SystemTxPayloadValidationError, SystemTxValidationContext,
};
use outbe_validatorset::state::{committee_snapshot_key, read_committee_snapshot};

use crate::accounting::record_phase1_progress;
use crate::block_fees::{self, WindowCloseInfo};
use crate::errors::FinalizationError;
use crate::runtime::{self, MetadataFingerprintOutcome};
use crate::schema::Rewards;

/// Applies certified parent accounting from the consensus payload.
pub struct ParentAccountingModule;

impl ProtocolModule for ParentAccountingModule {
    const NAME: &'static str = "certified_parent_accounting";

    fn build_payload(
        ctx: &SystemTxBuildContext<'_>,
    ) -> std::result::Result<Bytes, SystemTxPayloadBuildError> {
        let metadata = ctx.parent_consensus_metadata.ok_or(
            SystemTxPayloadBuildError::MissingParentConsensusMetadata {
                block_number: ctx.block_number,
            },
        )?;
        if metadata.finalized_block_hash != ctx.parent_hash {
            return Err(
                SystemTxPayloadBuildError::CertifiedParentAccountingHashMismatch {
                    parent_hash: ctx.parent_hash,
                    metadata_hash: metadata.finalized_block_hash,
                },
            );
        }
        Ok(metadata.encode()?)
    }

    fn validate_payload(
        payload: &[u8],
        ctx: &SystemTxValidationContext<'_>,
    ) -> std::result::Result<(), SystemTxPayloadValidationError> {
        outbe_primitives::system_tx::binding::validate_parent_accounting_payload(
            payload,
            ctx.parent_hash,
        )?;
        Ok(())
    }

    fn begin_block(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        let sys_ctx = ctx.system_tx()?;
        let metadata = CertifiedParentAccountingMetadata::decode(sys_ctx.payload.as_ref())?;
        if metadata.encode()?.as_ref() != sys_ctx.payload.as_ref() {
            return Err(PrecompileError::revert(
                "non-canonical parent accounting payload",
            ));
        }
        run_finalization_and_slashing(ctx, &metadata, &sys_ctx.to_execution_context(&ctx.block))
    }
}

/// Applies canonical late-finality credits under their exact mutation binding.
pub struct LateFinalizeCreditsModule;

impl ProtocolModule for LateFinalizeCreditsModule {
    const NAME: &'static str = "late_finalize_credits";

    fn build_payload(
        ctx: &SystemTxBuildContext<'_>,
    ) -> std::result::Result<Bytes, SystemTxPayloadBuildError> {
        Ok(encode_late_finalize_credits_artifact(
            &ctx.artifacts
                .late_finalize_credits
                .clone()
                .unwrap_or_default(),
        )?)
    }

    fn validate_payload(
        payload: &[u8],
        ctx: &SystemTxValidationContext<'_>,
    ) -> std::result::Result<(), SystemTxPayloadValidationError> {
        outbe_primitives::system_tx::binding::validate_late_finalize_credits_payload(
            payload,
            ctx.artifacts,
        )?;
        Ok(())
    }

    fn mutation_entitlements(
        ctx: MetadosisEntitlementContext<'_>,
    ) -> MetadosisMutationEntitlements {
        let Ok(artifact) = decode_late_finalize_credits_artifact(ctx.payload) else {
            return MetadosisMutationEntitlements::NONE;
        };
        let Ok(reencoded) = encode_late_finalize_credits_artifact(&artifact.unwrap_or_default())
        else {
            return MetadosisMutationEntitlements::NONE;
        };
        if reencoded.as_ref() != ctx.payload {
            return MetadosisMutationEntitlements::NONE;
        }
        MetadosisMutationEntitlements::exact(
            MetadosisMutationPurposeTag::CertifiedFinality,
            metadosis_late_settlement_binding(ctx.chain_id, ctx.block_number, ctx.timestamp),
        )
    }

    fn begin_block(ctx: &BlockRuntimeContext<'_>) -> Result<()> {
        let sys_ctx = ctx.system_tx()?;
        let artifact =
            decode_late_finalize_credits_artifact(sys_ctx.payload.as_ref())?.unwrap_or_default();
        if encode_late_finalize_credits_artifact(&artifact)?.as_ref() != sys_ctx.payload.as_ref() {
            return Err(PrecompileError::revert(
                "non-canonical late-finalize credits payload",
            ));
        }
        run_late_finalize_credits(
            ctx,
            &artifact,
            Some(&sys_ctx.to_execution_context(&ctx.block)),
        )
    }
}

/// Checks committee shape, registration, missed proposers, and the signer bitmap.
pub fn validate_finalized_metadata(
    storage: StorageHandle<'_>,
    metadata: &CertifiedParentAccountingMetadata,
) -> Result<()> {
    if metadata.signer_bitmap.len() != metadata.ordered_committee.len() {
        return Err(FinalizationError::SignerBitmapLengthMismatch.into());
    }
    let committee_set: BTreeSet<Address> = metadata.ordered_committee.iter().copied().collect();
    if committee_set.len() != metadata.ordered_committee.len() {
        return Err(FinalizationError::DuplicateCommitteeAddresses.into());
    }
    validate_committee_members(storage, metadata, &committee_set)?;
    validate_binary_signer_bitmap(&metadata.signer_bitmap)
}

/// Applies the parent finalization facts, fees, and proposer slashing.
pub fn run_finalization_and_slashing(
    ctx: &BlockRuntimeContext<'_>,
    metadata: &CertifiedParentAccountingMetadata,
    execution: &SystemTxExecutionContext,
) -> Result<()> {
    let expected_parent_number = validate_accounting_parent(ctx, metadata)?;
    validate_finalized_metadata(ctx.storage.clone(), metadata)?;
    let finalized = execution.finalized_summary.as_ref().ok_or(
        FinalizationError::MissingPreloadedExecutionSummary {
            block_number: metadata.finalized_block_number,
            block_hash: metadata.finalized_block_hash,
        },
    )?;
    let is_fresh = verify_metadata_progress(
        ctx,
        metadata,
        finalized.summary.validator_fee_sum,
        execution.canonical_vrf_proof_hash,
        expected_parent_number,
    )?;
    if !is_fresh {
        return Ok(());
    }
    let voters = collect_signing_voters(metadata);
    crate::finalized_metadata_hook::record_votes_and_fees(
        ctx,
        metadata,
        finalized.summary.validator_fee_sum,
        finalized.timestamp,
        &voters,
    )?;
    let missed_validators: Vec<Address> = metadata
        .missed_proposers
        .iter()
        .map(|missed| missed.validator)
        .collect();
    outbe_slashindicator::hooks::slash_window_proposers(
        ctx.storage.clone(),
        metadata.finalized_block_hash,
        &missed_validators,
    )?;
    outbe_slashindicator::hooks::prune_slash_guards(
        ctx.storage.clone(),
        metadata.finalized_block_hash,
    )?;
    record_phase1_progress(ctx, expected_parent_number)
}

/// Checks a late credit against the escrow stored for its finalized block.
pub fn authenticate_late_credit(
    storage: &StorageHandle<'_>,
    credit: &PerBlockCredit,
) -> Result<()> {
    let rewards = storage.contract::<Rewards<'_>>();
    let fb_number = credit.fb_number;
    authenticate_late_credit_hash(&rewards, credit)?;
    ensure_equal(
        rewards.pending_epoch_at.read(&fb_number)?,
        credit.epoch,
        |escrowed_epoch, credit_epoch| FinalizationError::LateCreditEpochMismatch {
            fb_number,
            escrowed_epoch,
            credit_epoch,
        },
    )?;
    ensure_equal(
        rewards.pending_committee_set_hash_at.read(&fb_number)?,
        credit.committee_set_hash,
        |escrowed_csh, credit_csh| FinalizationError::LateCreditCommitteeSetHashMismatch {
            fb_number,
            escrowed_csh,
            credit_csh,
        },
    )?;
    ensure_equal(
        rewards.pending_view_at.read(&fb_number)?,
        credit.view,
        |escrowed_view, credit_view| FinalizationError::LateCreditViewMismatch {
            fb_number,
            escrowed_view,
            credit_view,
        },
    )?;
    ensure_equal(
        rewards.pending_parent_view_at.read(&fb_number)?,
        credit.parent_view,
        |escrowed_parent_view, credit_parent_view| {
            FinalizationError::LateCreditParentViewMismatch {
                fb_number,
                escrowed_parent_view,
                credit_parent_view,
            }
        },
    )
}

/// Records late voters, window-close absentees, and the matured reward window.
pub fn run_late_finalize_credits(
    ctx: &BlockRuntimeContext<'_>,
    artifact: &LateFinalizeCreditsArtifact,
    execution: Option<&SystemTxExecutionContext>,
) -> Result<()> {
    let block_number = ctx.block.block_number;
    let preverified = execution.and_then(|context| context.verified_late_credit_signers.as_deref());
    for (batch_idx, credit) in artifact.batches.iter().enumerate() {
        let cached_batch = preverified
            .and_then(|batches| batches.get(batch_idx))
            .map(Vec::as_slice);
        record_single_late_credit_batch(ctx, block_number, credit, cached_batch)?;
    }
    record_window_close_absentees(ctx, block_number)?;
    block_fees::seal_block_votes(ctx, block_number, LATE_FINALIZE_WINDOW_K)?;
    Ok(())
}

fn validate_committee_members(
    storage: StorageHandle<'_>,
    metadata: &CertifiedParentAccountingMetadata,
    committee_set: &BTreeSet<Address>,
) -> Result<()> {
    let validators = outbe_validatorset::contract::ValidatorSet::new(storage);
    for address in &metadata.ordered_committee {
        if !validators.is_validator(*address)? {
            return Err(
                FinalizationError::UnregisteredCommitteeMember { address: *address }.into(),
            );
        }
    }
    for missed in &metadata.missed_proposers {
        if !committee_set.contains(&missed.validator) {
            return Err(FinalizationError::MissedProposerNotInCommittee {
                validator: missed.validator,
                view: missed.view,
            }
            .into());
        }
    }
    Ok(())
}

fn validate_binary_signer_bitmap(signer_bitmap: &[u8]) -> Result<()> {
    for entry in signer_bitmap {
        if *entry > 1 {
            return Err(FinalizationError::NonBinarySignerBitmapEntry.into());
        }
    }
    Ok(())
}

fn validate_accounting_parent(
    ctx: &BlockRuntimeContext<'_>,
    metadata: &CertifiedParentAccountingMetadata,
) -> Result<u64> {
    if !ctx.block.regime().has_parent_finalization_accounting() {
        return Err(FinalizationError::RequiresBlockNumberAtLeastTwo.into());
    }
    let expected_parent_number = ctx
        .block
        .block_number
        .checked_sub(1)
        .ok_or(FinalizationError::BlockNumberUnderflow)?;
    if metadata.finalized_block_number != expected_parent_number {
        return Err(FinalizationError::TargetImmediateParentMismatch {
            expected: expected_parent_number,
            actual: metadata.finalized_block_number,
        }
        .into());
    }
    if metadata.finalized_block_hash.is_zero() {
        return Err(FinalizationError::ZeroFinalizedBlockHash.into());
    }
    Ok(expected_parent_number)
}

fn verify_metadata_progress(
    ctx: &BlockRuntimeContext<'_>,
    metadata: &CertifiedParentAccountingMetadata,
    validator_fee_sum: alloy_primitives::U256,
    canonical_vrf_proof_hash: B256,
    expected_parent_number: u64,
) -> Result<bool> {
    let last_accounted = crate::accounting::read_last_accounted_block_number(ctx)?;
    match runtime::check_and_record_metadata_fingerprint(
        ctx,
        metadata,
        validator_fee_sum,
        canonical_vrf_proof_hash,
    )? {
        MetadataFingerprintOutcome::IdenticalReplay => {
            if last_accounted != expected_parent_number {
                return Err(FinalizationError::IdenticalReplayUnexpectedProgress {
                    last_accounted,
                    expected: expected_parent_number,
                }
                .into());
            }
            tracing::warn!(
                target: "outbe::system_tx",
                block_number = ctx.block.block_number,
                parent_block_number = expected_parent_number,
                "CertifiedParentAccounting identical replay accepted",
            );
            Ok(false)
        }
        MetadataFingerprintOutcome::Fresh => {
            let required_previous = expected_parent_number.saturating_sub(1);
            if last_accounted != required_previous {
                return Err(FinalizationError::ProgressGap {
                    last_accounted,
                    expected_previous: required_previous,
                    accounting_parent: expected_parent_number,
                }
                .into());
            }
            Ok(true)
        }
    }
}

fn collect_signing_voters(metadata: &CertifiedParentAccountingMetadata) -> Vec<Address> {
    metadata
        .ordered_committee
        .iter()
        .copied()
        .zip(metadata.signer_bitmap.iter().copied())
        .filter(|(_, did_sign)| *did_sign == 1)
        .map(|(address, _)| address)
        .collect()
}

fn authenticate_late_credit_hash(rewards: &Rewards<'_>, credit: &PerBlockCredit) -> Result<()> {
    let escrowed_hash = rewards.pending_fb_hash_at.read(&credit.fb_number)?;
    if escrowed_hash == B256::ZERO {
        return Err(FinalizationError::LateCreditMissingEscrow {
            fb_number: credit.fb_number,
            fb_hash: credit.fb_hash,
        }
        .into());
    }
    if escrowed_hash != credit.fb_hash {
        return Err(FinalizationError::LateCreditFbHashMismatch {
            fb_number: credit.fb_number,
            escrowed_hash,
            credit_hash: credit.fb_hash,
        }
        .into());
    }
    Ok(())
}

fn ensure_equal<T: PartialEq>(
    escrowed: T,
    credit_value: T,
    mismatch: impl FnOnce(T, T) -> FinalizationError,
) -> Result<()> {
    if escrowed != credit_value {
        return Err(mismatch(escrowed, credit_value).into());
    }
    Ok(())
}

fn record_single_late_credit_batch(
    ctx: &BlockRuntimeContext<'_>,
    block_number: u64,
    credit: &PerBlockCredit,
    cached_signers: Option<&[usize]>,
) -> Result<()> {
    let distance = validate_inclusion_window(block_number, credit.fb_number)?;
    authenticate_late_credit(&ctx.storage, credit)?;
    let snapshot_key = committee_snapshot_key(credit.epoch, credit.committee_set_hash);
    let snapshot = read_committee_snapshot(ctx.storage.clone(), snapshot_key)?.ok_or(
        FinalizationError::LateCreditMissingCommitteeSnapshot {
            epoch: credit.epoch,
            snapshot_key,
        },
    )?;
    let signer_indices = match cached_signers {
        Some(cached) => cached.to_vec(),
        None => verify_late_finalize_proof(&snapshot, credit)
            .map_err(|error| proof_verify_failed(credit.fb_hash, error))?,
    };
    for idx in signer_indices {
        let voter = snapshot
            .committee
            .get(idx)
            .map(|entry| entry.address)
            .ok_or(FinalizationError::LateCreditSignerIndexOutOfRange { idx })?;
        block_fees::record_late_vote(ctx, credit.fb_hash, voter, distance)?;
    }
    Ok(())
}

fn validate_inclusion_window(block_number: u64, fb_number: u64) -> Result<u8> {
    let distance = block_number.checked_sub(fb_number).ok_or(
        FinalizationError::LateCreditFbNumberAtOrAboveBlock {
            fb_number,
            block_number,
        },
    )?;
    if distance == 0 || distance > LATE_FINALIZE_WINDOW_K {
        return Err(FinalizationError::LateCreditOutsideInclusionWindow {
            fb_number,
            distance,
            window_k: LATE_FINALIZE_WINDOW_K,
            block_number,
        }
        .into());
    }
    u8::try_from(distance).map_err(|_| {
        FinalizationError::LateCreditInclusionDistanceExceedsU8 {
            distance,
            block_number,
        }
        .into()
    })
}

fn proof_verify_failed(fb_hash: B256, error: impl Display) -> PrecompileError {
    FinalizationError::LateCreditProofVerifyFailed {
        fb_hash,
        reason: error.to_string(),
    }
    .into()
}

fn record_window_close_absentees(ctx: &BlockRuntimeContext<'_>, block_number: u64) -> Result<()> {
    let Some(fb_number) = block_number
        .checked_sub(LATE_FINALIZE_WINDOW_K)
        .filter(|&height| height > 0)
    else {
        return Ok(());
    };
    let Some(info) = block_fees::window_close_credited(ctx, fb_number)? else {
        return Ok(());
    };
    let Some(absentees) = absentees_for_window(ctx, &info)? else {
        tracing::warn!(
            target: "outbe::slashing",
            fb_number,
            epoch = info.epoch,
            "window-close absentee pass: committee snapshot missing; skipping",
        );
        return Ok(());
    };
    if absentees.is_empty() {
        return Ok(());
    }
    outbe_validatorset::hooks::record_finalized_participation(
        ctx.storage.clone(),
        info.fb_hash,
        &[],
        &absentees,
    )?;
    crate::slash_absent_voters(ctx.storage.clone(), info.fb_hash, &absentees)
}

fn absentees_for_window(
    ctx: &BlockRuntimeContext<'_>,
    info: &WindowCloseInfo,
) -> Result<Option<Vec<Address>>> {
    let snapshot_key = committee_snapshot_key(info.epoch, info.committee_set_hash);
    let Some(snapshot) = read_committee_snapshot(ctx.storage.clone(), snapshot_key)? else {
        return Ok(None);
    };
    let credited: BTreeSet<Address> = info.credited.iter().copied().collect();
    let validators = outbe_validatorset::contract::ValidatorSet::new(ctx.storage.clone());
    let mut absentees = Vec::new();
    for entry in &snapshot.committee {
        if !credited.contains(&entry.address) && validators.is_validator(entry.address)? {
            absentees.push(entry.address);
        }
    }
    Ok(Some(absentees))
}

#[cfg(test)]
mod protocol_module_tests {
    use super::*;
    use outbe_primitives::{
        block::{BlockHeightRegime, MetadosisEntitlementContext, ProtocolModule},
        reshare_artifact::{encode_late_finalize_credits_artifact, OutbeBlockArtifacts},
        storage::{
            metadosis_late_settlement_binding, MetadosisMutationEntitlements,
            MetadosisMutationPurposeTag,
        },
        system_tx_context::{
            SystemTxBuildContext, SystemTxPayloadBuildError, SystemTxValidationContext,
        },
    };

    #[test]
    fn parent_module_binds_payload_to_parent() {
        let artifacts = OutbeBlockArtifacts::default();
        let metadata = crate::test_support::meta_with_hash(B256::repeat_byte(1), 1);
        let mut build = SystemTxBuildContext {
            regime: BlockHeightRegime::SteadyState,
            block_number: 2,
            parent_hash: metadata.finalized_block_hash,
            artifacts: &artifacts,
            parent_consensus_metadata: None,
            pending_tee_bootstrap: None,
            expected_tee_policy: None,
        };
        assert!(matches!(
            ParentAccountingModule::build_payload(&build),
            Err(SystemTxPayloadBuildError::MissingParentConsensusMetadata { block_number: 2 })
        ));
        build.parent_consensus_metadata = Some(&metadata);
        let payload = ParentAccountingModule::build_payload(&build).unwrap();
        let mut validation = SystemTxValidationContext {
            regime: build.regime,
            block_number: 2,
            parent_hash: build.parent_hash,
            artifacts: &artifacts,
        };
        ParentAccountingModule::validate_payload(&payload, &validation).unwrap();
        validation.parent_hash = B256::ZERO;
        assert!(ParentAccountingModule::validate_payload(&payload, &validation).is_err());
        build.parent_hash = B256::ZERO;
        assert!(matches!(
            ParentAccountingModule::build_payload(&build),
            Err(SystemTxPayloadBuildError::CertifiedParentAccountingHashMismatch { .. })
        ));
    }

    #[test]
    fn late_module_grants_only_canonical_payload_entitlements() {
        let payload =
            encode_late_finalize_credits_artifact(&LateFinalizeCreditsArtifact::default()).unwrap();
        let mut context = MetadosisEntitlementContext {
            payload: &payload,
            chain_id: 1,
            block_number: 2,
            timestamp: 100,
            cycle_active_utc_day: None,
        };
        assert_eq!(
            LateFinalizeCreditsModule::mutation_entitlements(context),
            MetadosisMutationEntitlements::exact(
                MetadosisMutationPurposeTag::CertifiedFinality,
                metadosis_late_settlement_binding(1, 2, 100)
            )
        );
        context.payload = &[0xff];
        assert_eq!(
            LateFinalizeCreditsModule::mutation_entitlements(context),
            MetadosisMutationEntitlements::NONE
        );
    }

    #[test]
    fn late_module_rejects_unrelated_canonical_artifact_records() {
        let artifacts = OutbeBlockArtifacts {
            radicle_publication_enabled: true,
            ..Default::default()
        };
        let payload =
            outbe_primitives::reshare_artifact::encode_outbe_block_artifacts(&artifacts).unwrap();
        assert!(
            outbe_primitives::reshare_artifact::decode_late_finalize_credits_artifact(&payload)
                .unwrap()
                .is_none()
        );
        let context = MetadosisEntitlementContext {
            payload: &payload,
            chain_id: 1,
            block_number: 2,
            timestamp: 100,
            cycle_active_utc_day: None,
        };
        assert_eq!(
            LateFinalizeCreditsModule::mutation_entitlements(context),
            MetadosisMutationEntitlements::NONE
        );
        crate::test_support::with_block(2, crate::test_support::GENESIS_TS + 60, |mut ctx| {
            ctx.system_tx = Some(outbe_primitives::system_tx_context::SystemTxContext {
                payload,
                ..Default::default()
            });
            assert!(LateFinalizeCreditsModule::begin_block(&ctx).is_err());
        });
    }

    #[test]
    fn late_module_builds_and_validates_empty_default() {
        let artifacts = OutbeBlockArtifacts::default();
        let build = SystemTxBuildContext {
            regime: BlockHeightRegime::SteadyState,
            block_number: 2,
            parent_hash: B256::ZERO,
            artifacts: &artifacts,
            parent_consensus_metadata: None,
            pending_tee_bootstrap: None,
            expected_tee_policy: None,
        };
        let payload = LateFinalizeCreditsModule::build_payload(&build).unwrap();
        let validation = SystemTxValidationContext {
            regime: build.regime,
            block_number: build.block_number,
            parent_hash: build.parent_hash,
            artifacts: &artifacts,
        };
        LateFinalizeCreditsModule::validate_payload(&payload, &validation).unwrap();
        assert!(LateFinalizeCreditsModule::validate_payload(&[0xff], &validation).is_err());
    }

    #[test]
    fn modules_reject_missing_context_and_malformed_execution_payloads() {
        crate::test_support::with_block(2, crate::test_support::GENESIS_TS + 60, |mut ctx| {
            assert!(ParentAccountingModule::begin_block(&ctx).is_err());
            assert!(LateFinalizeCreditsModule::begin_block(&ctx).is_err());
            ctx.system_tx = Some(outbe_primitives::system_tx_context::SystemTxContext {
                payload: vec![0xff].into(),
                ..Default::default()
            });
            assert!(ParentAccountingModule::begin_block(&ctx).is_err());
            assert!(LateFinalizeCreditsModule::begin_block(&ctx).is_err());
        });
    }
}

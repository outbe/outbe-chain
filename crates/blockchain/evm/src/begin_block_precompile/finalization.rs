use crate::executor::validate_finalized_metadata;
use alloy_primitives::Address;
use alloy_primitives::B256;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::consensus_metadata::CertifiedParentAccountingMetadata;
use outbe_primitives::error::Result;

use super::{current_preloaded_system_tx_context, read_preloaded_finalized_summary};

/// CertifiedParentAccounting system tx: apply the immediate parent's finalization
/// facts, participation, fee settlement, and deterministic slashing.
pub(crate) fn run_finalization_and_slashing(
    ctx: &BlockRuntimeContext,
    metadata: &CertifiedParentAccountingMetadata,
) -> Result<()> {
    let expected_parent_number = validate_accounting_parent(ctx, metadata)?;

    validate_finalized_metadata(ctx.storage.clone(), metadata)?;

    let finalized = read_preloaded_finalized_summary(&ctx.storage)?.ok_or(
        crate::errors::BeginBlockPrecompileError::MissingPreloadedExecutionSummary {
            block_number: metadata.finalized_block_number,
            block_hash: metadata.finalized_block_hash,
        },
    )?;

    // the V3 Rewards fingerprint binds the canonical VRF proof
    // hash from the verified parent certificate. The executor's Phase 1
    // preflight (`apply_pre_execution_changes::verify_phase1_in_preexec`)
    // captured this value from `outbe_consensus::proof::VerifiedProof::vrf_proof_hash`
    // and stashed it in the preloaded context. A zero hash here would
    // pass the gate but produces a degenerate fingerprint. In production
    // the preflight always populates a real value for `block_number >= 2`.
    let canonical_vrf_proof_hash = current_preloaded_system_tx_context()
        .map(|context| context.canonical_vrf_proof_hash)
        .unwrap_or(B256::ZERO);

    let last_accounted = outbe_accounting::read_last_accounted_block_number(ctx)?;
    match outbe_rewards::runtime::check_and_record_metadata_fingerprint(
        ctx,
        metadata,
        finalized.summary.validator_fee_sum,
        canonical_vrf_proof_hash,
    )? {
        outbe_rewards::runtime::MetadataFingerprintOutcome::IdenticalReplay => {
            if last_accounted != expected_parent_number {
                return Err(
                    crate::errors::BeginBlockPrecompileError::IdenticalReplayUnexpectedProgress {
                        last_accounted,
                        expected: expected_parent_number,
                    }
                    .into(),
                );
            }
            tracing::warn!(
                target: "outbe::system_tx",
                block_number = ctx.block.block_number,
                parent_block_number = expected_parent_number,
                "CertifiedParentAccounting identical replay accepted",
            );
            return Ok(());
        }
        outbe_rewards::runtime::MetadataFingerprintOutcome::Fresh => {
            let required_previous = expected_parent_number.saturating_sub(1);
            if last_accounted != required_previous {
                return Err(crate::errors::BeginBlockPrecompileError::ProgressGap {
                    last_accounted,
                    expected_previous: required_previous,
                    accounting_parent: expected_parent_number,
                }
                .into());
            }
        }
    }

    // Base voters = the k=0 quorum (direct-parent signers). They seed the fee
    // escrow at k=0. This step defers the FULL absentee set and its miss / slashing
    // accounting to the inclusion-window close at N+K (`record_window_close_absentees`
    // in the LateFinalizeCredits phase). Thus a slow-but-honest validator credited at
    // k=1..K is not counted "missed" or slashed.
    let mut voters = Vec::new();
    for (addr, did_sign) in metadata
        .ordered_committee
        .iter()
        .copied()
        .zip(metadata.signer_bitmap.iter().copied())
    {
        if did_sign == 1 {
            voters.push(addr);
        }
    }

    outbe_rewards::finalized_metadata_hook::on_finalized_metadata(
        ctx,
        metadata,
        finalized.summary.validator_fee_sum,
        finalized.timestamp,
        &voters,
    )?;

    // Missed-proposer slashing: idempotent + bounded via the per-`fb_hash`
    // `proposer_window_slashed` guard. The whole event list for this
    // finalized parent is processed atomically. The one pass slashes each
    // duplicate proposer across skipped views.
    let missed_validators: Vec<Address> = metadata
        .missed_proposers
        .iter()
        .map(|m| m.validator)
        .collect();
    outbe_slashindicator::hooks::slash_window_proposers(
        ctx.storage.clone(),
        metadata.finalized_block_hash,
        &missed_validators,
    )?;

    // advance the slash-guard prune ring once per finalized block. Phase 1
    // sees every finalized block exactly once (as a direct parent), so this
    // bounds both window guards to the last SLASH_GUARD_RETAIN finalized blocks.
    outbe_slashindicator::hooks::prune_slash_guards(
        ctx.storage.clone(),
        metadata.finalized_block_hash,
    )?;

    outbe_accounting::record_phase1_progress(ctx, expected_parent_number)?;

    Ok(())
}

fn validate_accounting_parent(
    ctx: &BlockRuntimeContext,
    metadata: &CertifiedParentAccountingMetadata,
) -> Result<u64> {
    if ctx.block.block_number < 2 {
        return Err(crate::errors::BeginBlockPrecompileError::RequiresBlockNumberAtLeastTwo.into());
    }
    let expected_parent_number = ctx
        .block
        .block_number
        .checked_sub(1)
        .ok_or(crate::errors::BeginBlockPrecompileError::BlockNumberUnderflow)?;
    if metadata.finalized_block_number != expected_parent_number {
        return Err(
            crate::errors::BeginBlockPrecompileError::TargetImmediateParentMismatch {
                expected: expected_parent_number,
                actual: metadata.finalized_block_number,
            }
            .into(),
        );
    }
    if metadata.finalized_block_hash.is_zero() {
        return Err(crate::errors::BeginBlockPrecompileError::ZeroFinalizedBlockHash.into());
    }

    Ok(expected_parent_number)
}

use crate::executor::validate_finalized_metadata;
use alloy_primitives::Address;
use alloy_primitives::B256;
use outbe_primitives::block::BlockRuntimeContext;
use outbe_primitives::consensus_metadata::CertifiedParentAccountingMetadata;
use outbe_primitives::error::PrecompileError;
use outbe_primitives::error::Result;

use super::{current_preloaded_system_tx_context, read_preloaded_finalized_summary};

/// CertifiedParentAccounting system tx: apply the immediate parent's finalization
/// facts, participation, fee settlement, and deterministic slashing.
pub(crate) fn run_finalization_and_slashing(
    ctx: &BlockRuntimeContext,
    metadata: &CertifiedParentAccountingMetadata,
) -> Result<()> {
    if ctx.block.block_number < 2 {
        return Err(PrecompileError::Fatal(
            "CertifiedParentAccounting system tx requires block_number >= 2".into(),
        ));
    }
    let expected_parent_number = ctx
        .block
        .block_number
        .checked_sub(1)
        .ok_or_else(|| PrecompileError::Fatal("block number underflow".into()))?;
    if metadata.finalized_block_number != expected_parent_number {
        return Err(PrecompileError::Fatal(format!(
            "CertifiedParentAccounting metadata must target immediate parent: expected {}, got {}",
            expected_parent_number, metadata.finalized_block_number
        )));
    }
    if metadata.finalized_block_hash.is_zero() {
        return Err(PrecompileError::Fatal(
            "CertifiedParentAccounting metadata has zero finalized block hash".into(),
        ));
    }

    validate_finalized_metadata(ctx.storage.clone(), metadata)?;

    let finalized = read_preloaded_finalized_summary(&ctx.storage)?.ok_or_else(|| {
        PrecompileError::Fatal(format!(
            "missing preloaded execution summary for finalized block {} ({})",
            metadata.finalized_block_number, metadata.finalized_block_hash
        ))
    })?;

    // OCOMP finality is derived only from an actual consensus finalization
    // certificate for the exact request parent. Certified notarization remains
    // sufficient for ordinary parent accounting, but cannot create a JobId or
    // open a result-vote window.
    if metadata.proof_kind
        == outbe_primitives::consensus_metadata::ParentParticipationProof::Finalization
    {
        let finalized_state_root = finalized.state_root.ok_or_else(|| {
            PrecompileError::Fatal(
                "certified Metadosis finality requires the verified parent state root".into(),
            )
        })?;
        let certified = outbe_primitives::storage::MetadosisCertifiedFinalityBinding::new(
            ctx.block.chain_id,
            ctx.block.block_number,
            metadata.finalized_block_number,
            metadata.finalized_block_hash,
            finalized_state_root,
        );
        outbe_metadosis::commands::record_certified_parent_finality(ctx, &certified)?;
    }

    // the V3 Rewards fingerprint binds the canonical VRF proof
    // hash from the verified parent certificate. The executor's Phase 1
    // preflight (`apply_pre_execution_changes::verify_phase1_in_preexec`)
    // captured this value from `outbe_consensus::proof::VerifiedProof::vrf_proof_hash`
    // and stashed it in the preloaded context. A zero hash here would
    // pass the gate but produces a degenerate fingerprint; in production
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
                return Err(PrecompileError::Fatal(format!(
                    "CertifiedParentAccounting identical replay at unexpected progress: last_accounted={last_accounted}, expected={expected_parent_number}"
                )));
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
                return Err(PrecompileError::Fatal(format!(
                    "CertifiedParentAccounting progress gap: last_accounted={last_accounted}, expected_previous={required_previous}, accounting_parent={expected_parent_number}"
                )));
            }
        }
    }

    // Base voters = the k=0 quorum (direct-parent signers); they seed the fee
    // escrow at k=0. The FULL absentee set and its miss / slashing accounting are
    // deferred to the inclusion-window close at N+K (`record_window_close_absentees`
    // in the LateFinalizeCredits phase), so a slow-but-honest validator credited at
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
    // finalized parent is processed atomically; duplicate proposers across
    // skipped views are each slashed within the one pass.
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

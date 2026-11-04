//! `LateFinalizeCredits` verification, authentication, and matured-window settlement.
//!
//! Unifies pre-execution BLS aggregate verification and the `LateFinalizeCredits`
//! begin-block precompile execution so the BLS pairing is performed once per
//! block and reused when the precompile executes.

use std::collections::BTreeSet;

use alloy_evm::block::{BlockExecutionError, StateDB};
use alloy_primitives::{Address, B256};
use outbe_consensus::proof::verify_late_finalize_proof;
use outbe_primitives::{
    block::{BlockContext, BlockRuntimeContext},
    consensus::LATE_FINALIZE_WINDOW_K,
    error::Result as OutbeResult,
    reshare_artifact::{LateFinalizeCreditsArtifact, PerBlockCredit},
    storage::StorageHandle,
};
use outbe_validatorset::state::{committee_snapshot_key, read_committee_snapshot};

use crate::{
    begin_block_precompile::current_preloaded_system_tx_context,
    errors::{BeginBlockPrecompileError, ExecutorValidationError},
    executor::context,
};

#[derive(Debug, Clone, Copy)]
enum LateCreditWindowError {
    FbNumberAtOrAboveBlock {
        fb_number: u64,
        block_number: u64,
    },
    OutsideInclusionWindow {
        fb_number: u64,
        distance: u64,
        window_k: u64,
        block_number: u64,
    },
    DistanceExceedsU8 {
        distance: u64,
        block_number: u64,
    },
}

impl From<LateCreditWindowError> for BeginBlockPrecompileError {
    fn from(error: LateCreditWindowError) -> Self {
        match error {
            LateCreditWindowError::FbNumberAtOrAboveBlock {
                fb_number,
                block_number,
            } => Self::LateCreditFbNumberAtOrAboveBlock {
                fb_number,
                block_number,
            },
            LateCreditWindowError::OutsideInclusionWindow {
                fb_number,
                distance,
                window_k,
                block_number,
            } => Self::LateCreditOutsideInclusionWindow {
                fb_number,
                distance,
                window_k,
                block_number,
            },
            LateCreditWindowError::DistanceExceedsU8 {
                distance,
                block_number,
            } => Self::LateCreditInclusionDistanceExceedsU8 {
                distance,
                block_number,
            },
        }
    }
}

impl From<LateCreditWindowError> for ExecutorValidationError {
    fn from(error: LateCreditWindowError) -> Self {
        match error {
            LateCreditWindowError::FbNumberAtOrAboveBlock {
                fb_number,
                block_number,
            } => Self::LateFinalizeFbNumberAtOrAboveBlock {
                fb_number,
                block_number,
            },
            LateCreditWindowError::OutsideInclusionWindow {
                fb_number,
                distance,
                window_k,
                block_number,
            } => Self::LateFinalizeOutsideInclusionWindow {
                fb_number,
                distance,
                window_k,
                block_number,
            },
            LateCreditWindowError::DistanceExceedsU8 {
                distance,
                block_number,
            } => Self::LateFinalizeOutsideInclusionWindow {
                fb_number: block_number.saturating_sub(distance),
                distance,
                window_k: LATE_FINALIZE_WINDOW_K,
                block_number,
            },
        }
    }
}

fn validate_inclusion_window(
    block_number: u64,
    fb_number: u64,
) -> Result<u8, LateCreditWindowError> {
    let distance = block_number.checked_sub(fb_number).ok_or(
        LateCreditWindowError::FbNumberAtOrAboveBlock {
            fb_number,
            block_number,
        },
    )?;
    if distance == 0 || distance > LATE_FINALIZE_WINDOW_K {
        return Err(LateCreditWindowError::OutsideInclusionWindow {
            fb_number,
            distance,
            window_k: LATE_FINALIZE_WINDOW_K,
            block_number,
        });
    }
    u8::try_from(distance).map_err(|_| LateCreditWindowError::DistanceExceedsU8 {
        distance,
        block_number,
    })
}

/// Authenticates a late-finalize `credit`'s proposer-supplied
/// `fb_number` / `epoch` / `committee_set_hash` / `view` / `parent_view`
/// against the canonical binding escrowed for that finalized block in `Rewards`.
pub fn authenticate_late_credit(
    storage: &StorageHandle,
    credit: &PerBlockCredit,
) -> OutbeResult<()> {
    let rewards = storage.contract::<outbe_rewards::contract::Rewards<'_>>();
    let fb_number = credit.fb_number;
    authenticate_late_credit_hash(&rewards, credit)?;
    ensure_escrowed_eq(
        rewards.pending_epoch_at.read(&fb_number)?,
        credit.epoch,
        |escrowed_epoch, credit_epoch| BeginBlockPrecompileError::LateCreditEpochMismatch {
            fb_number,
            escrowed_epoch,
            credit_epoch,
        },
    )?;
    ensure_escrowed_eq(
        rewards.pending_committee_set_hash_at.read(&fb_number)?,
        credit.committee_set_hash,
        |escrowed_csh, credit_csh| BeginBlockPrecompileError::LateCreditCommitteeSetHashMismatch {
            fb_number,
            escrowed_csh,
            credit_csh,
        },
    )?;
    ensure_escrowed_eq(
        rewards.pending_view_at.read(&fb_number)?,
        credit.view,
        |escrowed_view, credit_view| BeginBlockPrecompileError::LateCreditViewMismatch {
            fb_number,
            escrowed_view,
            credit_view,
        },
    )?;
    ensure_escrowed_eq(
        rewards.pending_parent_view_at.read(&fb_number)?,
        credit.parent_view,
        |escrowed_parent_view, credit_parent_view| {
            BeginBlockPrecompileError::LateCreditParentViewMismatch {
                fb_number,
                escrowed_parent_view,
                credit_parent_view,
            }
        },
    )
}

fn ensure_escrowed_eq<T: PartialEq>(
    escrowed: T,
    credit_value: T,
    mismatch: impl FnOnce(T, T) -> BeginBlockPrecompileError,
) -> OutbeResult<()> {
    if escrowed != credit_value {
        return Err(mismatch(escrowed, credit_value).into());
    }
    Ok(())
}

fn authenticate_late_credit_hash(
    rewards: &outbe_rewards::contract::Rewards<'_>,
    credit: &PerBlockCredit,
) -> OutbeResult<()> {
    let escrowed_hash = rewards.pending_fb_hash_at.read(&credit.fb_number)?;
    if escrowed_hash == B256::ZERO {
        return Err(BeginBlockPrecompileError::LateCreditMissingEscrow {
            fb_number: credit.fb_number,
            fb_hash: credit.fb_hash,
        }
        .into());
    }
    if escrowed_hash != credit.fb_hash {
        return Err(BeginBlockPrecompileError::LateCreditFbHashMismatch {
            fb_number: credit.fb_number,
            escrowed_hash,
            credit_hash: credit.fb_hash,
        }
        .into());
    }
    Ok(())
}

/// Executes the `LateFinalizeCredits` system transaction:
/// 1. Authenticates each credit batch against `Rewards` escrow and records late voters.
///    When preloaded signer indices are present in the system-tx context, reuses
///    those pre-verified indices so the BLS pairing is not repeated.
/// 2. Records window-close absentees and applies voter slashing for `block_number - K`.
/// 3. Settles the matured reward window at `block_number - K`.
pub fn run_late_finalize_credits(
    ctx: &BlockRuntimeContext,
    artifact: &LateFinalizeCreditsArtifact,
) -> OutbeResult<()> {
    let block_number = ctx.block.block_number;
    let preverified_signers = current_preloaded_system_tx_context()
        .and_then(|context| context.verified_late_credit_signers);

    for (batch_idx, credit) in artifact.batches.iter().enumerate() {
        let cached_batch = preverified_signers
            .as_ref()
            .and_then(|batches| batches.get(batch_idx))
            .map(Vec::as_slice);
        record_single_late_credit_batch(ctx, block_number, credit, cached_batch)?;
    }

    record_window_close_absentees(ctx, block_number)?;
    outbe_rewards::late_settlement::settle_matured(ctx, block_number, LATE_FINALIZE_WINDOW_K)?;

    Ok(())
}

fn record_single_late_credit_batch(
    ctx: &BlockRuntimeContext,
    block_number: u64,
    credit: &PerBlockCredit,
    cached_signers: Option<&[usize]>,
) -> OutbeResult<()> {
    let k = validate_inclusion_window(block_number, credit.fb_number)
        .map_err(BeginBlockPrecompileError::from)?;

    authenticate_late_credit(&ctx.storage, credit)?;

    let snapshot_key = committee_snapshot_key(credit.epoch, credit.committee_set_hash);
    let snapshot = read_committee_snapshot(ctx.storage.clone(), snapshot_key)?.ok_or(
        BeginBlockPrecompileError::LateCreditMissingCommitteeSnapshot {
            epoch: credit.epoch,
            snapshot_key,
        },
    )?;

    let signer_indices = match cached_signers {
        Some(cached) => cached.to_vec(),
        None => verify_late_finalize_proof(&snapshot, credit).map_err(|error| {
            BeginBlockPrecompileError::late_credit_proof_verify_failed(credit.fb_hash, error)
        })?,
    };

    for idx in signer_indices {
        let voter = snapshot
            .committee
            .get(idx)
            .map(|entry| entry.address)
            .ok_or(BeginBlockPrecompileError::LateCreditSignerIndexOutOfRange { idx })?;
        outbe_rewards::late_settlement::record_late_credit(ctx, credit.fb_hash, voter, k)?;
    }
    Ok(())
}

fn record_window_close_absentees(ctx: &BlockRuntimeContext, block_number: u64) -> OutbeResult<()> {
    let Some(fb_number) = block_number
        .checked_sub(LATE_FINALIZE_WINDOW_K)
        .filter(|&height| height > 0)
    else {
        return Ok(());
    };
    let Some(info) = outbe_rewards::late_settlement::window_close_credited(ctx, fb_number)? else {
        return Ok(());
    };

    let snapshot_key = committee_snapshot_key(info.epoch, info.committee_set_hash);
    let Some(snapshot) = read_committee_snapshot(ctx.storage.clone(), snapshot_key)? else {
        tracing::warn!(
            target: "outbe::slashing",
            fb_number,
            epoch = info.epoch,
            "window-close absentee pass: committee snapshot missing; skipping",
        );
        return Ok(());
    };

    let credited: BTreeSet<Address> = info.credited.into_iter().collect();
    let absentees =
        collect_window_close_absentees(ctx.storage.clone(), &snapshot.committee, &credited)?;
    if absentees.is_empty() {
        return Ok(());
    }

    outbe_validatorset::hooks::record_finalized_participation(
        ctx.storage.clone(),
        info.fb_hash,
        &[],
        &absentees,
    )?;

    outbe_slashindicator::hooks::slash_window_voters(
        ctx.storage.clone(),
        info.fb_hash,
        &absentees,
    )?;

    Ok(())
}

fn collect_window_close_absentees(
    storage: StorageHandle,
    committee: &[outbe_validatorset::state::CommitteeEntry],
    credited: &BTreeSet<Address>,
) -> OutbeResult<Vec<Address>> {
    let vs = outbe_validatorset::contract::ValidatorSet::new(storage);
    let mut absentees: Vec<Address> = Vec::new();
    for entry in committee {
        if !credited.contains(&entry.address) && vs.is_validator(entry.address)? {
            absentees.push(entry.address);
        }
    }
    Ok(absentees)
}

/// Pre-execution verification of a block's late-finalize credit batches.
///
/// Verifies the inclusion window (`1 <= block_number - fb_number <= K`) and
/// the BLS aggregate proof once, returning the verified signer indices per batch
/// so [`run_late_finalize_credits`] does not repeat the BLS pairing.
pub(crate) fn verify_late_finalize_credits_batches<DB>(
    db: &mut DB,
    block_context: BlockContext,
    artifact: &LateFinalizeCreditsArtifact,
) -> Result<Vec<Vec<usize>>, BlockExecutionError>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
{
    let mut verified_batches = Vec::with_capacity(artifact.batches.len());
    for credit in &artifact.batches {
        verified_batches.push(verify_single_late_credit_batch(
            db,
            block_context.clone(),
            credit,
        )?);
    }
    Ok(verified_batches)
}

fn verify_single_late_credit_batch<DB>(
    db: &mut DB,
    block_context: BlockContext,
    credit: &PerBlockCredit,
) -> Result<Vec<usize>, BlockExecutionError>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
{
    let block_number = block_context.block_number;
    validate_inclusion_window(block_number, credit.fb_number)
        .map_err(ExecutorValidationError::from)?;

    let snapshot_key = committee_snapshot_key(credit.epoch, credit.committee_set_hash);
    let snapshot = context::read_committee_snapshot(db, block_context, snapshot_key, |error| {
        ExecutorValidationError::late_finalize_read_committee_snapshot(
            credit.epoch,
            snapshot_key,
            error,
        )
    })?;
    let Some(snapshot) = snapshot else {
        return Err(
            ExecutorValidationError::LateFinalizeMissingCommitteeSnapshot {
                epoch: credit.epoch,
                snapshot_key,
                block_number,
            }
            .into(),
        );
    };

    verify_late_finalize_proof(&snapshot, credit).map_err(|error| {
        BlockExecutionError::from(ExecutorValidationError::late_finalize_proof_rejected(
            credit.fb_hash,
            block_number,
            error,
        ))
    })
}

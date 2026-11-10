//! `LateFinalizeCredits` verification, authentication, and matured-window settlement.
//!
//! Unifies pre-execution BLS aggregate verification and the `LateFinalizeCredits`
//! begin-block precompile execution so the BLS pairing is performed once per
//! block and reused when the precompile executes.

use alloy_evm::block::{BlockExecutionError, StateDB};
use outbe_consensus::proof::verify_late_finalize_proof;
use outbe_primitives::committee_snapshot::committee_snapshot_key;
use outbe_primitives::{
    block::{BlockContext, BlockRuntimeContext},
    consensus::LATE_FINALIZE_WINDOW_K,
    error::Result as OutbeResult,
    reshare_artifact::{LateFinalizeCreditsArtifact, PerBlockCredit},
    storage::StorageHandle,
};

use crate::{
    errors::{BeginBlockPrecompileError, ExecutorValidationError},
    executor::{context, current_preloaded_system_tx_context},
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
    host: &dyn crate::EvmRuntimeHost,
    storage: &StorageHandle,
    credit: &PerBlockCredit,
) -> OutbeResult<()> {
    host.authenticate_late_credit(storage, credit)
}

/// Executes the `LateFinalizeCredits` system transaction.
///
/// Authenticates each credit, records late voters, records window-close
/// absentees, and settles the matured reward window.
pub fn run_late_finalize_credits(
    host: &dyn crate::EvmRuntimeHost,
    ctx: &BlockRuntimeContext,
    artifact: &LateFinalizeCreditsArtifact,
) -> OutbeResult<()> {
    let execution = current_preloaded_system_tx_context();
    host.run_late_finalize_credits(ctx, artifact, execution.as_ref())
}

/// Pre-execution verification of a block's late-finalize credit batches.
///
/// Verifies the inclusion window (`1 <= block_number - fb_number <= K`) and
/// the BLS aggregate proof once, returning the verified signer indices per batch
/// so [`run_late_finalize_credits`] does not repeat the BLS pairing.
pub(crate) fn verify_late_finalize_credits_batches<DB>(
    host: &dyn crate::EvmRuntimeHost,
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
            host,
            db,
            block_context.clone(),
            credit,
        )?);
    }
    Ok(verified_batches)
}

fn verify_single_late_credit_batch<DB>(
    host: &dyn crate::EvmRuntimeHost,
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
    let snapshot =
        context::read_committee_snapshot(host, db, block_context, snapshot_key, |error| {
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

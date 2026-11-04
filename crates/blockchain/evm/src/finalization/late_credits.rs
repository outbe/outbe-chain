//! `LateFinalizeCredits` verification, authentication, and matured-window settlement.
//!
//! Unifies pre-execution BLS aggregate verification and the `LateFinalizeCredits`
//! begin-block precompile execution so the BLS pairing is performed once per
//! block and reused when the precompile executes.

use std::{collections::BTreeSet, sync::Arc};

use alloy_evm::{
    block::{BlockExecutionError, StateDB},
    revm::context::Block as _,
};
use alloy_primitives::{Address, B256};
use outbe_consensus::proof::verify_late_finalize_proof;
use outbe_primitives::{
    block::{BlockHeightRegime, BlockRuntimeContext},
    consensus::LATE_FINALIZE_WINDOW_K,
    error::Result as OutbeResult,
    reshare_artifact::{LateFinalizeCreditsArtifact, OutbeBlockArtifacts, PerBlockCredit},
    storage::StorageHandle,
};
use outbe_validatorset::state::{committee_snapshot_key, read_committee_snapshot};
use reth_ethereum::evm::{primitives::Evm, revm::context::TxEnv};

use crate::{
    begin_block_precompile::current_preloaded_system_tx_context,
    errors::{BeginBlockPrecompileError, ExecutorValidationError},
    executor::{context, OutbeBlockExecutor, ZeroFeeCfgAccess},
};

/// Authenticates a late-finalize `credit`'s proposer-supplied
/// `fb_number` / `epoch` / `committee_set_hash` / `view` / `parent_view`
/// against the canonical binding escrowed for that finalized block in `Rewards`.
pub fn authenticate_late_credit(
    storage: &StorageHandle,
    credit: &PerBlockCredit,
) -> OutbeResult<()> {
    let rewards = storage.contract::<outbe_rewards::contract::Rewards<'_>>();
    authenticate_late_credit_hash(&rewards, credit)?;
    let escrowed_epoch = rewards.pending_epoch_at.read(&credit.fb_number)?;
    if escrowed_epoch != credit.epoch {
        return Err(BeginBlockPrecompileError::LateCreditEpochMismatch {
            fb_number: credit.fb_number,
            escrowed_epoch,
            credit_epoch: credit.epoch,
        }
        .into());
    }
    let escrowed_csh = rewards
        .pending_committee_set_hash_at
        .read(&credit.fb_number)?;
    if escrowed_csh != credit.committee_set_hash {
        return Err(
            BeginBlockPrecompileError::LateCreditCommitteeSetHashMismatch {
                fb_number: credit.fb_number,
                escrowed_csh,
                credit_csh: credit.committee_set_hash,
            }
            .into(),
        );
    }
    let escrowed_view = rewards.pending_view_at.read(&credit.fb_number)?;
    if escrowed_view != credit.view {
        return Err(BeginBlockPrecompileError::LateCreditViewMismatch {
            fb_number: credit.fb_number,
            escrowed_view,
            credit_view: credit.view,
        }
        .into());
    }
    let escrowed_parent_view = rewards.pending_parent_view_at.read(&credit.fb_number)?;
    if escrowed_parent_view != credit.parent_view {
        return Err(BeginBlockPrecompileError::LateCreditParentViewMismatch {
            fb_number: credit.fb_number,
            escrowed_parent_view,
            credit_parent_view: credit.parent_view,
        }
        .into());
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
///    When called from `OutbeBlockExecutor`, reuses the signer indices verified in
///    `verify_late_finalize_credits_in_preexec` so the BLS pairing is not repeated.
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
        let k_u64 = block_number.checked_sub(credit.fb_number).ok_or(
            BeginBlockPrecompileError::LateCreditFbNumberAtOrAboveBlock {
                fb_number: credit.fb_number,
                block_number,
            },
        )?;
        if k_u64 == 0 || k_u64 > LATE_FINALIZE_WINDOW_K {
            return Err(
                BeginBlockPrecompileError::LateCreditOutsideInclusionWindow {
                    fb_number: credit.fb_number,
                    distance: k_u64,
                    window_k: LATE_FINALIZE_WINDOW_K,
                    block_number,
                }
                .into(),
            );
        }
        let k = u8::try_from(k_u64).map_err(|_| {
            BeginBlockPrecompileError::LateCreditInclusionDistanceExceedsU8 {
                distance: k_u64,
                block_number,
            }
        })?;

        authenticate_late_credit(&ctx.storage, credit)?;

        let snapshot_key = committee_snapshot_key(credit.epoch, credit.committee_set_hash);
        let snapshot = read_committee_snapshot(ctx.storage.clone(), snapshot_key)?.ok_or(
            BeginBlockPrecompileError::LateCreditMissingCommitteeSnapshot {
                epoch: credit.epoch,
                snapshot_key,
            },
        )?;

        let signer_indices = if let Some(cached) = preverified_signers
            .as_ref()
            .and_then(|batches| batches.get(batch_idx))
        {
            cached.clone()
        } else {
            verify_late_finalize_proof(&snapshot, credit).map_err(|error| {
                BeginBlockPrecompileError::late_credit_proof_verify_failed(credit.fb_hash, error)
            })?
        };

        for idx in signer_indices {
            let voter = snapshot
                .committee
                .get(idx)
                .map(|entry| entry.address)
                .ok_or(BeginBlockPrecompileError::LateCreditSignerIndexOutOfRange { idx })?;
            outbe_rewards::late_settlement::record_late_credit(ctx, credit.fb_hash, voter, k)?;
        }
    }

    record_window_close_absentees(ctx, block_number)?;
    outbe_rewards::late_settlement::settle_matured(ctx, block_number, LATE_FINALIZE_WINDOW_K)?;

    Ok(())
}

fn record_window_close_absentees(ctx: &BlockRuntimeContext, block_number: u64) -> OutbeResult<()> {
    let Some(fb_number) = block_number.checked_sub(LATE_FINALIZE_WINDOW_K) else {
        return Ok(());
    };
    if fb_number == 0 {
        return Ok(());
    }
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
    let vs = outbe_validatorset::contract::ValidatorSet::new(ctx.storage.clone());
    let mut absentees: Vec<Address> = Vec::new();
    for entry in &snapshot.committee {
        if credited.contains(&entry.address) {
            continue;
        }
        if vs.is_validator(entry.address)? {
            absentees.push(entry.address);
        }
    }
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

#[allow(private_bounds)]
impl<DB, E> OutbeBlockExecutor<'_, E>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
    E: Evm<DB = DB, Tx = TxEnv> + ZeroFeeCfgAccess,
    E::Error: std::fmt::Display,
{
    /// Pre-execution verification of the block's late-finalize credits.
    ///
    /// Verifies the inclusion window (`1 <= block_number - fb_number <= K`) and
    /// the BLS aggregate proof once, caching the verified signer indices so
    /// [`run_late_finalize_credits`] does not repeat the BLS pairing.
    pub(crate) fn verify_late_finalize_credits_in_preexec(
        &mut self,
        block_number: u64,
        block_artifacts: &OutbeBlockArtifacts,
    ) -> Result<(), BlockExecutionError> {
        self.verified_late_credit_signers = None;
        if !BlockHeightRegime::for_block(block_number).has_parent_finalization_accounting() {
            return Ok(());
        }
        let Some(artifact) = block_artifacts.late_finalize_credits.as_ref() else {
            return Ok(());
        };
        if artifact.batches.is_empty() {
            return Ok(());
        }

        let timestamp = outbe_primitives::time::timestamp_secs(self.inner.evm.block().timestamp())
            .map_err(ExecutorValidationError::from)?;
        let proposer = self
            .begin_zone_proposer(block_number)?
            .unwrap_or_else(|| self.inner.evm.block().beneficiary());

        let mut verified_batches = Vec::with_capacity(artifact.batches.len());
        for credit in &artifact.batches {
            let distance = block_number.checked_sub(credit.fb_number).ok_or(
                ExecutorValidationError::LateFinalizeFbNumberAtOrAboveBlock {
                    fb_number: credit.fb_number,
                    block_number,
                },
            )?;
            if distance == 0 || distance > LATE_FINALIZE_WINDOW_K {
                return Err(
                    ExecutorValidationError::LateFinalizeOutsideInclusionWindow {
                        fb_number: credit.fb_number,
                        distance,
                        window_k: LATE_FINALIZE_WINDOW_K,
                        block_number,
                    }
                    .into(),
                );
            }

            let snapshot_key = committee_snapshot_key(credit.epoch, credit.committee_set_hash);
            let ctx = self.block_context(block_number, timestamp, proposer);
            let snapshot = context::read_committee_snapshot(
                self.inner.evm.db_mut(),
                ctx,
                snapshot_key,
                |error| {
                    ExecutorValidationError::late_finalize_read_committee_snapshot(
                        credit.epoch,
                        snapshot_key,
                        error,
                    )
                },
            )?;
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

            let signer_indices =
                verify_late_finalize_proof(&snapshot, credit).map_err(|error| {
                    BlockExecutionError::from(
                        ExecutorValidationError::late_finalize_proof_rejected(
                            credit.fb_hash,
                            block_number,
                            error,
                        ),
                    )
                })?;
            verified_batches.push(signer_indices);
        }

        self.verified_late_credit_signers = Some(Arc::new(verified_batches));
        Ok(())
    }
}

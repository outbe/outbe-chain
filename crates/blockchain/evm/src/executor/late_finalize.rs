use super::*;
use outbe_primitives::block::BlockContextInput;

#[allow(private_bounds)]
impl<DB, E> OutbeBlockExecutor<'_, E>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
    E: Evm<DB = DB, Tx = TxEnv> + ZeroFeeCfgAccess,
    E::Error: std::fmt::Display,
{
    /// FATAL pre-exec verification of the block's late-finalize
    /// credits. Each batch in `header.extra_data`'s
    /// `LateFinalizeCreditsArtifact` carries a BLS aggregate over a recently
    /// finalized block's individual finalize votes. This check runs on the same
    /// pre-exec path as [`Self::verify_phase1_in_preexec`]. It is synchronous
    /// and does not mutate state. An `Err` aborts the block before any
    /// begin-zone state diff reaches Reth's state-root task. For every batch,
    /// the check enforces these conditions:
    ///
    /// - The target sits inside the inclusion window: `1 <= block - fb <= K`.
    /// - The committee snapshot for `(epoch, committee_set_hash)` exists.
    /// - The aggregate verifies against that snapshot (no quorum/VRF floor:
    ///   late credits are the sub-quorum tail, see
    ///   [`outbe_consensus::proof::verify_late_finalize_proof`]).
    ///
    /// Both proposer (its own gathered credits) and validator (proposer-
    /// supplied) verify, so a buggy proposer or a forged batch is rejected
    /// identically. Block 0 / block 1 (genesis bootstrap) skip verification.
    /// A `None` or empty artifact is a no-op. There is no test opt-out.
    pub(in crate::executor) fn verify_late_finalize_credits_in_preexec(
        &mut self,
        block_number: u64,
        block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
    ) -> Result<(), BlockExecutionError> {
        use outbe_consensus::proof::verify_late_finalize_proof;
        use outbe_primitives::consensus::LATE_FINALIZE_WINDOW_K;
        use outbe_validatorset::state::{committee_snapshot_key, read_committee_snapshot};

        if block_number <= crate::system_tx::GENESIS_BOOTSTRAP_BLOCK_NUMBER {
            return Ok(());
        }
        // No test opt-out: a `None`/empty artifact early-returns below, so tests
        // that don't carry credits are unaffected. Tests that do carry credits
        // (and seed the matching committee snapshot) exercise the real verifier.
        let Some(artifact) = block_artifacts.late_finalize_credits.as_ref() else {
            return Ok(());
        };
        if artifact.batches.is_empty() {
            return Ok(());
        }

        let timestamp = outbe_primitives::time::timestamp_secs(self.inner.evm.block().timestamp())
            .map_err(crate::errors::ExecutorValidationError::from)?;
        let chain_id = self.inner.evm.chain_id();
        let proposer = self
            .begin_zone_proposer(block_number)?
            .unwrap_or_else(|| self.inner.evm.block().beneficiary());

        for credit in &artifact.batches {
            use crate::errors::ExecutorValidationError;

            // Inclusion window: 1 <= block_number - fb_number <= K.
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

            // NOTE: the canonical-binding authentication (fb_number/epoch/
            // committee_set_hash vs the escrow) is intentionally NOT done here.
            // THIS block's CPA writes the escrow for the closest in-window target
            // (block N-1). The CPA executes in pre-execution AFTER this gate
            // (`apply_phase1_commit_in_preexec`), so the binding is not yet
            // present when this gate runs. The
            // authentication therefore lives in the begin-zone body
            // (`run_late_finalize_credits`, after the CPA), where a mismatch is
            // FATAL and aborts the block. This pre-exec gate covers the BLS proof
            // (committee snapshot exists from the epoch boundary).
            let snapshot_key = committee_snapshot_key(credit.epoch, credit.committee_set_hash);
            let snapshot = {
                let db = self.inner.evm.db_mut();
                let ctx = BlockContext::new_with_genesis_hash(BlockContextInput {
                    block_number,
                    timestamp,
                    chain_id,
                    genesis_hash: self.genesis_hash,
                    proposer,
                    validators: Vec::new(),
                });
                let mut provider = DirectStorageProvider::new(db, ctx);
                let storage = StorageHandle::new(&mut provider);
                read_committee_snapshot(storage, snapshot_key).map_err(|error| {
                    if error.is_node_local() || matches!(error, PrecompileError::Fatal(_)) {
                        BlockExecutionError::other(error)
                    } else {
                        ExecutorValidationError::late_finalize_read_committee_snapshot(
                            credit.epoch,
                            snapshot_key,
                            error,
                        )
                        .into()
                    }
                })?
            };
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
            })?;
        }

        Ok(())
    }
}

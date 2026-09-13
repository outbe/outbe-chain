use super::*;

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
    /// finalized block's individual finalize votes. This runs on the same
    /// pre-exec path as [`Self::verify_phase1_in_preexec`] - synchronous, no
    /// state mutation, `Err` aborts the block before any begin-zone state diff
    /// reaches Reth's state-root task - and enforces, for every batch:
    ///
    /// - the target sits inside the inclusion window: `1 <= block - fb <= K`;
    /// - the committee snapshot for `(epoch, committee_set_hash)` exists;
    /// - the aggregate verifies against that snapshot (no quorum/VRF floor -
    ///   late credits are the sub-quorum tail, see
    ///   [`outbe_consensus::proof::verify_late_finalize_proof`]).
    ///
    /// Both proposer (its own gathered credits) and validator (proposer-
    /// supplied) verify, so a buggy proposer or a forged batch is rejected
    /// identically. Block 0 / block 1 (genesis bootstrap) and the test-only
    /// `PHASE1_VERIFY_DISABLED` opt-out skip verification; a `None` or empty
    /// artifact is a no-op.
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
        // that don't carry credits are unaffected; tests that do carry credits
        // (and seed the matching committee snapshot) exercise the real verifier.
        let Some(artifact) = block_artifacts.late_finalize_credits.as_ref() else {
            return Ok(());
        };
        if artifact.batches.is_empty() {
            return Ok(());
        }

        let timestamp = self.inner.evm.block().timestamp().saturating_to::<u64>();
        let chain_id = self.inner.evm.chain_id();
        let proposer = self
            .begin_zone_proposer(block_number)?
            .unwrap_or_else(|| self.inner.evm.block().beneficiary());

        for credit in &artifact.batches {
            // Inclusion window: 1 <= block_number - fb_number <= K.
            let distance = block_number.checked_sub(credit.fb_number).ok_or_else(|| {
                BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                    format!(
                        "LateFinalizeCredits pre-exec: fb_number {} >= block {block_number}",
                        credit.fb_number
                    )
                    .into(),
                ))
            })?;
            if distance == 0 || distance > LATE_FINALIZE_WINDOW_K {
                return Err(BlockExecutionError::Internal(
                    InternalBlockExecutionError::Other(
                        format!(
                            "LateFinalizeCredits pre-exec: fb_number {} outside inclusion window \
                             (distance {distance}, K={LATE_FINALIZE_WINDOW_K}) for block {block_number}",
                            credit.fb_number
                        )
                        .into(),
                    ),
                ));
            }

            // NOTE: the canonical-binding authentication (fb_number/epoch/
            // committee_set_hash vs the escrow) is intentionally NOT done here.
            // The escrow for the closest in-window target (block N-1) is written
            // by THIS block's CPA, which runs in the body AFTER this pre-exec
            // gate - so the binding is not yet present at pre-exec. The
            // authentication therefore lives in the begin-zone body
            // (`run_late_finalize_credits`, after the CPA), where a mismatch is
            // FATAL and aborts the block. This pre-exec gate covers the BLS proof
            // (committee snapshot exists from the epoch boundary).
            let snapshot_key = committee_snapshot_key(credit.epoch, credit.committee_set_hash);
            let snapshot = {
                let db = self.inner.evm.db_mut();
                let ctx = BlockContext::new_with_genesis_hash(
                    block_number,
                    timestamp,
                    chain_id,
                    self.genesis_hash,
                    proposer,
                    Vec::new(),
                );
                let mut provider = DirectStorageProvider::new(db, ctx);
                let storage = StorageHandle::new(&mut provider);
                read_committee_snapshot(storage, snapshot_key).map_err(|error| {
                    BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                        format!(
                            "LateFinalizeCredits pre-exec: read committee snapshot epoch={} \
                             key={snapshot_key}: {error}",
                            credit.epoch
                        )
                        .into(),
                    ))
                })?
            };
            let Some(snapshot) = snapshot else {
                return Err(BlockExecutionError::Internal(
                    InternalBlockExecutionError::Other(
                        format!(
                            "LateFinalizeCredits pre-exec: missing committee snapshot epoch={} \
                             key={snapshot_key} for block {block_number}",
                            credit.epoch
                        )
                        .into(),
                    ),
                ));
            };

            verify_late_finalize_proof(&snapshot, credit).map_err(|error| {
                BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                    format!(
                        "LateFinalizeCredits pre-exec: proof rejected for fb={} at block \
                         {block_number}: {error}",
                        credit.fb_hash
                    )
                    .into(),
                ))
            })?;
        }

        Ok(())
    }
}

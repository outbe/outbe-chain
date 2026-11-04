//! Executor orchestration for parent finalization accounting and late-finalize credits.

use super::*;

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
    /// [`crate::finalization::late_credits::run_late_finalize_credits`] does not
    /// repeat the BLS pairing.
    pub(in crate::executor) fn verify_late_finalize_credits_in_preexec(
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
            .map_err(crate::errors::ExecutorValidationError::from)?;
        let proposer = self
            .begin_zone_proposer(block_number)?
            .unwrap_or_else(|| self.inner.evm.block().beneficiary());
        let ctx = self.block_context(block_number, timestamp, proposer);
        let verified_batches =
            crate::finalization::late_credits::verify_late_finalize_credits_batches(
                self.inner.evm.db_mut(),
                ctx,
                artifact,
            )?;
        self.verified_late_credit_signers = Some(Arc::new(verified_batches));
        Ok(())
    }
}
